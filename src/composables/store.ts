/* ============================================================
 * store.ts — 全局状态中心：连接 / 模型 / 记忆 / 查询窗口 / SQL 执行
 * ============================================================ */
import { reactive, computed } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type {
  AppSettings,
  ChatTurn,
  ConnConfig,
  ConnMemory,
  ModelConfig,
  QueryTab,
  QueryResult,
  SqlItem,
  SqlLogItem,
  DatabaseMeta,
  TableMeta,
} from "../types";
import { apiConn, apiModel, apiSettings, apiSql, apiLog, apiExport, beErr } from "../lib/backend";
import { listen } from "@tauri-apps/api/event";
import { toast } from "./ui";

/* ---------- 工具 ---------- */
let seed = 0;
export function uid(prefix: string): string {
  return `${prefix}_${(++seed).toString(36)}${Date.now().toString(36).slice(-5)}`;
}
export function now(): string {
  const d = new Date();
  const p = (n: number) => (n < 10 ? "0" + n : "" + n);
  return `${p(d.getHours())}:${p(d.getMinutes())}`;
}

const UI_KEY = "datamind.ui.v1";

/** 结构记忆是否是在错误字符集下采集的：注释/DDL 里成片出现 '???' 就说明中文已被服务端转成问号 */
function memoryLooksBroken(mem?: ConnMemory): boolean {
  if (!mem) return false;
  for (const db of mem.databases) {
    for (const t of db.tables) {
      if (t.ddl.includes("???")) return true;
      if (t.columns.some((c) => c.comment.includes("???"))) return true;
    }
  }
  return false;
}

/* ---------- 状态 ---------- */

interface UiPersist {
  collapsed: { left: boolean; right: boolean };
  sizes: { left: number; right: number };
  tabs: QueryTab[];
  activeTab: string | null;
}

export const state = reactive({
  ready: false,
  settings: { theme: "dark", sqlTimeout: 60, agentMode: true } as AppSettings,
  connections: [] as ConnConfig[],
  /** connId -> 本地结构记忆 */
  memories: {} as Record<string, ConnMemory>,
  /** connId -> 正在同步的步骤描述 */
  syncing: {} as Record<string, string>,
  models: [] as ModelConfig[],
  tabs: [] as QueryTab[],
  activeTab: null as string | null,
  collapsed: { left: false, right: false },
  sizes: { left: 272, right: 296 },
  sqlLog: [] as SqlLogItem[],
  /** 正在等待模型推理的 tabId 集合 */
  generating: {} as Record<string, boolean>,
  /** tabId -> 本轮生成的 runId（用于「停止」） */
  genRun: {} as Record<string, string>,
  /** 当前生成阶段（两阶段 Agent 流水线的进度提示） */
  genStage: { key: "", label: "" } as { key: string; label: string },
});

/* ---------- 便捷计算 ---------- */
export const activeTab = computed<QueryTab | null>(() =>
  state.tabs.find((t) => t.id === state.activeTab) ?? null,
);
export const activeModel = computed<ModelConfig | null>(
  () => state.models.find((m) => m.active) ?? null,
);
export function connOf(id: string | null): ConnConfig | undefined {
  return state.connections.find((c) => c.id === id);
}
export function memoryOf(id: string | null): ConnMemory | undefined {
  return id ? state.memories[id] : undefined;
}
export function dbOf(connId: string | null, dbName: string): DatabaseMeta | undefined {
  return memoryOf(connId)?.databases.find((d) => d.name === dbName);
}
export function tableOf(
  connId: string | null,
  dbName: string,
  tableName: string,
): TableMeta | undefined {
  return dbOf(connId, dbName)?.tables.find((t) => t.name === tableName);
}

/* ---------- UI 状态持久化（localStorage） ---------- */

/** 结果集超过该行数时不写入本地持久化：结果集现已不截断，大结果会撑爆 localStorage 配额，
 *  连带丢失窗口布局与 SQL 卡片（写入失败被 catch 吞掉，用户完全无感知）。 */
const PERSIST_MAX_RESULT_ROWS = 2000;

export function saveUi() {
  try {
    const tabs = state.tabs.map((t) =>
      t.result && t.result.rows.length > PERSIST_MAX_RESULT_ROWS ? { ...t, result: null } : t,
    );
    const p: UiPersist = {
      collapsed: state.collapsed,
      sizes: state.sizes,
      tabs,
      activeTab: state.activeTab,
    };
    localStorage.setItem(UI_KEY, JSON.stringify(p));
  } catch {
    /* 忽略存储异常 */
  }
}

function loadUi() {
  try {
    const raw = localStorage.getItem(UI_KEY);
    if (!raw) return;
    const p = JSON.parse(raw) as UiPersist;
    if (p.collapsed) state.collapsed = p.collapsed;
    if (p.sizes) state.sizes = p.sizes;
    if (Array.isArray(p.tabs)) {
      // 恢复时清理运行态
      state.tabs = p.tabs.map((t) => ({
        ...t,
        sqls: t.sqls.map((s) =>
          s.status === "running" ? { ...s, status: "cancelled" as const } : s,
        ),
        running: null,
        messages: t.messages ?? [],
        result: t.result ?? null,
      }));
    }
    state.activeTab = p.activeTab && state.tabs.some((t) => t.id === p.activeTab)
      ? p.activeTab
      : state.tabs[0]?.id ?? null;
  } catch {
    /* 忽略 */
  }
}

/* ---------- 主题 ---------- */

export function setTheme(theme: "dark" | "light") {
  state.settings.theme = theme;
  document.documentElement.dataset.theme = theme;
  apiSettings.set(state.settings).catch(() => undefined);
}

/** 生成模式：true = Agent 模式（分析→探查→设计→生成→校验），false = 快速模式（单次直出） */
export function setAgentMode(on: boolean) {
  state.settings.agentMode = on;
  apiSettings.set(state.settings).catch(() => undefined);
}

export function setCollapsed(side: "left" | "right", val: boolean) {
  state.collapsed[side] = val;
  saveUi();
}

/* ---------- 初始化 ---------- */

export async function initApp() {
  // 记忆同步进度事件：驱动树上「同步中」步骤提示
  listen<{ connId: string; step: string }>("sync-progress", (e) => {
    if (state.syncing[e.payload.connId]) state.syncing[e.payload.connId] = e.payload.step;
  }).catch(() => undefined);

  // 两阶段生成进度：分析 → 探查 → 设计 → 生成 → 校验
  listen<{ stage: string; label: string }>("sqlgen-stage", (e) => {
    state.genStage = { key: e.payload.stage, label: e.payload.label || e.payload.stage };
  }).catch(() => undefined);

  const [settings, conns, models] = await Promise.all([
    apiSettings.get(),
    apiConn.list(),
    apiModel.list(),
  ]);
  state.settings = settings;
  document.documentElement.dataset.theme = settings.theme;
  state.connections = conns;
  state.models = models;
  // 恢复本地记忆缓存（连接树可立即渲染）
  for (const c of conns) {
    try {
      const mem = await apiConn.memory(c.id);
      if (mem) state.memories[c.id] = mem;
    } catch {
      /* 单个连接记忆读取失败不影响启动 */
    }
  }
  // 旧版本在错误字符集下采集的记忆里，注释会成片变成 '?'（无法还原）。
  // 提示一次：重新读取结构即可用修复后的解码逻辑重建。
  const stale = conns.find((c) => memoryLooksBroken(state.memories[c.id]));
  if (stale) {
    toast(`「${stale.name}」的结构记忆是字符集修复前采集的，注释可能仍是 ?，建议刷新结构记忆`, "err");
  }
  loadUi();
  if (!state.tabs.length) createTab();
  state.ready = true;
}

/* ---------- 连接管理 ---------- */

export function newConnDraft(): ConnConfig {
  return {
    id: "",
    name: "",
    type: "MySQL",
    host: "",
    port: 3306,
    user: "",
    password: "",
    ssh: { enabled: false, host: "", port: 22, user: "", auth: "password", secret: "" },
  };
}

export async function saveConnection(conn: ConnConfig, isNew: boolean): Promise<ConnConfig> {
  const saved = await apiConn.save(conn);
  const i = state.connections.findIndex((c) => c.id === saved.id);
  if (i >= 0) state.connections[i] = saved;
  else state.connections.push(saved);
  toast(isNew ? "连接已创建，正在读取结构" : "连接已更新，正在重建本地记忆", "ok");
  void syncConnection(saved.id);
  return saved;
}

export async function testConnection(conn: ConnConfig): Promise<string> {
  const r = await apiConn.test(conn);
  if (!r.ok) throw new Error(r.message);
  return `连接成功（${r.ms} ms${conn.ssh.enabled ? " · SSH 隧道" : ""}）`;
}

/** 同步连接结构记忆（DDL / 关系 / 取样），期间通过 syncing[connId] 暴露步骤 */
export async function syncConnection(connId: string, silent = false): Promise<void> {
  const conn = connOf(connId);
  if (!conn) return;
  if (state.syncing[connId]) return;
  state.syncing[connId] = "连接数据库";
  if (!silent) toast(`正在读取 ${conn.name} 的表结构…`);
  try {
    const mem = await apiConn.sync(connId);
    state.memories[connId] = mem;
    const tables = mem.databases.reduce((s, d) => s + d.tables.length, 0);
    const rels = mem.databases.reduce((s, d) => s + d.relations.length, 0);
    toast(`已更新本地记忆：${tables} 张表 / ${rels} 组关系`, "ok");
  } catch (e) {
    toast(`同步失败：${beErr(e).message}`, "err");
  } finally {
    delete state.syncing[connId];
  }
}

export function removeConnection(connId: string) {
  const conn = connOf(connId);
  if (!conn) return;
  void apiConn.remove(connId).catch((e) => toast(beErr(e).message, "err"));
  delete state.memories[connId];
  state.connections = state.connections.filter((c) => c.id !== connId);
  state.tabs.forEach((t) => {
    if (t.connId === connId) t.connId = null;
  });
  saveUi();
  toast("已删除连接", "ok");
}

/* ---------- 模型管理 ---------- */

export function newModelDraft(): ModelConfig {
  return {
    id: "",
    name: "",
    provider: "deepseek",
    apiKey: "",
    modelId: "deepseek-chat",
    baseUrl: "https://api.deepseek.com",
    active: false,
  };
}

export async function saveModel(model: ModelConfig, isNew: boolean): Promise<ModelConfig> {
  const saved = await apiModel.save(model);
  const i = state.models.findIndex((m) => m.id === saved.id);
  if (i >= 0) state.models[i] = saved;
  else {
    state.models.push(saved);
    await apiModel.activate(saved.id);
    state.models.forEach((m) => (m.active = m.id === saved.id));
  }
  toast(isNew ? "已新增并设为使用中" : "已保存", "ok");
  return saved;
}

export async function activateModel(id: string) {
  await apiModel.activate(id);
  state.models.forEach((m) => (m.active = m.id === id));
  const cur = activeModel.value;
  toast(`已切换到 ${cur?.name ?? ""}`, "ok");
}

export function removeModel(id: string) {
  void apiModel.remove(id).catch((e) => toast(beErr(e).message, "err"));
  state.models = state.models.filter((m) => m.id !== id);
  if (state.models.length && !state.models.some((m) => m.active)) {
    state.models[0].active = true;
  }
  toast("已删除模型配置", "ok");
}

export async function refreshSqlLog() {
  try {
    state.sqlLog = await apiLog.list();
  } catch {
    /* 忽略 */
  }
}

/* ---------- 查询窗口（Tab） ---------- */

export function tabTitle(t: QueryTab): string {
  if (t.title) return t.title;
  const conn = connOf(t.connId);
  if (!conn) return "未连接";
  return (t.db || conn.name) + (t.table ? " · " + t.table : "");
}

export function createTab(opts: { connId?: string; db?: string; table?: string; title?: string } = {}): QueryTab {
  const connId = opts.connId ?? state.connections[0]?.id ?? null;
  const mem = memoryOf(connId);
  const t: QueryTab = {
    id: uid("tab"),
    title: opts.title ?? "",
    connId: connId ?? null,
    db: opts.db ?? mem?.databases[0]?.name ?? "",
    table: opts.table ?? "",
    chatH: null,
    chatW: null,
    messages: [],
    sqls: [],
    result: null,
    running: null,
  };
  state.tabs.push(t);
  state.activeTab = t.id;
  saveUi();
  // 必须返回响应式代理（而非原始对象）：调用方若拿原始对象做后续修改，界面不会更新
  return state.tabs[state.tabs.length - 1];
}

/** 双击数据表：新建该表的查询窗口并立即查询整表（结果集不截断，由界面分页展示） */
export async function previewTable(connId: string, db: string, table: string): Promise<void> {
  if (!connId || !db || !table) return;
  const conn = connOf(connId);
  const tab = createTab({
    connId,
    db,
    table,
    title: (db || conn?.name || "查询") + " · " + table,
  });
  const item: SqlItem = {
    id: uid("sql"),
    // 不追加 LIMIT：结果集完整返回，界面按单页 1000 行分页展示
    sql: `SELECT * FROM \`${table.replace(/`/g, "")}\``,
    status: "idle",
    error: null,
    ms: null,
    rows: null,
    ts: now(),
  };
  tab.sqls.push(item);
  // 同样取回响应式代理，否则执行过程中卡片状态与计时器不会刷新
  const liveItem = tab.sqls[tab.sqls.length - 1];
  saveUi();
  await runSql(tab, liveItem);
}

export function closeTab(id: string) {
  const i = state.tabs.findIndex((t) => t.id === id);
  if (i < 0) return;
  const t = state.tabs[i];
  if (t.running) void apiSql.cancel(`${id}:${t.running}`).catch(() => undefined);
  // 关窗时同时停掉还在跑的生成任务，避免后台白烧模型调用
  if (state.genRun[id]) void apiSql.cancel(state.genRun[id]).catch(() => undefined);
  state.tabs.splice(i, 1);
  if (state.activeTab === id) {
    state.activeTab = state.tabs[Math.max(0, i - 1)]?.id ?? null;
  }
  saveUi();
}

export function duplicateTab(t: QueryTab) {
  const copy: QueryTab = JSON.parse(JSON.stringify(t));
  copy.id = uid("tab");
  copy.running = null;
  copy.sqls = copy.sqls.map((s) => (s.status === "running" ? { ...s, status: "cancelled" as const } : s));
  state.tabs.push(copy);
  state.activeTab = copy.id;
  saveUi();
}

export function activateTab(id: string) {
  state.activeTab = id;
  saveUi();
}

export function touchTab() {
  saveUi();
}

/* ---------- 对话生成 SQL ---------- */

export async function generateSql(tab: QueryTab, prompt: string): Promise<void> {
  if (!tab.connId || !tab.db) {
    toast("请先选择连接与数据库", "err");
    return;
  }
  if (!activeModel.value) {
    toast("请先在右侧配置并启用一个模型", "err");
    return;
  }
  // 多轮上下文：本轮之前的历史对话 + 上一轮生成的 SQL（首轮两者为空）
  const history: ChatTurn[] = tab.messages.slice(-10).map((m) => ({ role: m.role, text: m.text }));
  const prevSql = tab.sqls.length ? tab.sqls[tab.sqls.length - 1].sql : null;
  const isFollowUp = history.length > 0 && !!prevSql;

  tab.messages.push({ role: "user", text: prompt, ts: now() });
  const runId = `gen:${tab.id}:${uid("r")}`;
  state.genRun[tab.id] = runId;
  state.generating[tab.id] = true;
  try {
    state.genStage = { key: "schema", label: "分析表结构" };
    const r = await apiSql.generate({
      runId,
      connId: tab.connId,
      db: tab.db,
      table: tab.table,
      prompt,
      mode: "new",
      prevSql: isFollowUp ? prevSql : null,
      error: null,
      agent: state.settings.agentMode !== false,
      history,
    });
    const dbMem = dbOf(tab.connId, tab.db);
    const relCount = dbMem?.relations.length ?? 0;
    const explore = r.steps?.find((s) => s.key === "explore");
    tab.messages.push({
      role: "ai",
      text:
        (isFollowUp ? "已结合历史对话与上一轮 SQL 迭代生成：" : "已生成 SQL：") +
        `（本地记忆 ${tab.db}：${dbMem?.tables.length ?? 0} 张表结构 / ${relCount} 组外键关系）` +
        (explore?.summary ? `\n数据探查：${explore.summary.split("\n").slice(0, 4).join("；")}` : "") +
        (r.note ? `\n${r.note}` : ""),
      ts: now(),
    });
    tab.sqls.push({
      id: uid("sql"),
      sql: r.sql,
      status: "idle",
      error: null,
      ms: null,
      rows: null,
      ts: now(),
      trace: r.steps?.length ? r.steps : undefined,
    });
  } catch (e) {
    const err = beErr(e);
    if (err.code === "CANCELLED") {
      tab.messages.push({ role: "ai", text: "已停止生成", ts: now() });
      toast("已停止生成", "err");
    } else {
      tab.messages.push({ role: "ai", text: `生成失败：${err.message}`, ts: now() });
      toast(`生成失败：${err.message}`, "err");
    }
  } finally {
    delete state.generating[tab.id];
    delete state.genRun[tab.id];
    state.genStage = { key: "", label: "" };
    saveUi();
  }
}

/** 停止本窗口正在进行的 SQL 生成：模型请求与库内探查会立即中断 */
export async function cancelGenerate(tab: QueryTab): Promise<void> {
  const runId = state.genRun[tab.id];
  if (!runId) return;
  try {
    await apiSql.cancel(runId);
  } catch {
    /* 任务可能已结束 */
  }
}

/** 快速修复：把上一条 SQL 与错误信息交给模型修复 */
export async function quickFixSql(tab: QueryTab, item: SqlItem): Promise<void> {
  if (!activeModel.value) {
    toast("请先在右侧配置并启用一个模型", "err");
    return;
  }
  const runId = `gen:${tab.id}:${uid("r")}`;
  state.genRun[tab.id] = runId;
  state.generating[tab.id] = true;
  try {
    const r = await apiSql.generate({
      runId,
      connId: tab.connId ?? "",
      db: tab.db,
      table: tab.table,
      prompt: "修复以下 SQL 的错误，保持原意图不变",
      mode: "fix",
      prevSql: item.sql,
      error: item.error
        ? `[${item.error.code}] ${item.error.message}${item.error.hint ? `（提示：${item.error.hint}）` : ""}`
        : "",
      history: tab.messages.slice(-10).map((m) => ({ role: m.role, text: m.text })),
    });
    tab.sqls.push({
      id: uid("sql"),
      sql: r.sql,
      status: "idle",
      error: null,
      ms: null,
      rows: null,
      ts: now(),
      fixed: true,
    });
    tab.messages.push({
      role: "ai",
      text: `已根据错误提示${r.note ? `（${r.note}）` : ""}修复 SQL，请确认后运行。`,
      ts: now(),
    });
    toast("已生成修复后的 SQL", "ok");
  } catch (e) {
    const err = beErr(e);
    toast(err.code === "CANCELLED" ? "已停止生成" : `修复失败：${err.message}`, "err");
  } finally {
    delete state.generating[tab.id];
    delete state.genRun[tab.id];
    saveUi();
  }
}

/* ---------- SQL 执行 ---------- */

/**
 * 手写 / 粘贴 SQL：只是新增一条卡片，执行仍走 runSql
 * （后端 AST 只读校验 + 超时自动取消 + 手动取消，与生成的 SQL 完全同一条链路）。
 * 返回响应式代理对象，保证执行过程中卡片状态与计时器能刷新。
 */
export function addManualSql(tab: QueryTab, sql: string): SqlItem {
  const item: SqlItem = {
    id: uid("sql"),
    sql,
    status: "idle",
    error: null,
    ms: null,
    rows: null,
    ts: now(),
    manual: true,
  };
  tab.sqls.push(item);
  saveUi();
  return tab.sqls[tab.sqls.length - 1];
}

export async function runSql(tab: QueryTab, item: SqlItem): Promise<void> {
  if (!tab.connId || !tab.db) {
    toast("请先选择连接与数据库", "err");
    return;
  }
  if (tab.running) {
    toast("当前窗口已有查询在执行", "err");
    return;
  }
  const runId = `${tab.id}:${item.id}`;
  item.status = "running";
  item.error = null;
  item.ms = null;
  tab.running = item.id;
  const start = Date.now();
  saveUi();
  try {
    const result = await apiSql.execute({
      runId,
      connId: tab.connId,
      db: tab.db,
      sql: item.sql,
      timeoutSecs: state.settings.sqlTimeout,
    });
    item.status = "success";
    item.ms = result.ms;
    item.rows = result.rows.length;
    tab.result = result as QueryResult;
    toast(`查询完成 · ${result.rows.length} 条`, "ok");
  } catch (e) {
    const err = beErr(e);
    const ms = Date.now() - start;
    if (err.code === "TIMEOUT" || err.code === "CANCELLED") {
      item.status = "cancelled";
      item.ms = ms;
      toast(err.code === "TIMEOUT" ? `执行超过 ${state.settings.sqlTimeout}s，已自动取消` : "已取消执行", "err");
    } else {
      item.status = "error";
      item.ms = ms;
      item.error = { code: err.code, message: err.message, hint: err.hint ?? undefined, pos: undefined };
      toast("SQL 执行失败", "err");
    }
  } finally {
    tab.running = null;
    saveUi();
  }
}

export async function cancelSql(tab: QueryTab, item: SqlItem): Promise<void> {
  const runId = `${tab.id}:${item.id}`;
  try {
    await apiSql.cancel(runId);
  } catch {
    /* 任务可能已结束 */
  }
}

/** 删除单条 SQL 卡片（若正在运行则先取消） */
export async function deleteSql(tab: QueryTab, item: SqlItem): Promise<void> {
  if (item.status === "running") await cancelSql(tab, item);
  const i = tab.sqls.findIndex((s) => s.id === item.id);
  if (i >= 0) tab.sqls.splice(i, 1);
  if (tab.running === item.id) tab.running = null;
  saveUi();
}

/** 清空本窗口全部 SQL 卡片 */
export async function clearSqls(tab: QueryTab): Promise<void> {
  const runningItem = tab.sqls.find((s) => s.id === tab.running);
  if (runningItem) await cancelSql(tab, runningItem);
  tab.sqls = [];
  tab.running = null;
  saveUi();
}

/* ---------- 结果导出 ---------- */

/** 超过该行数时导出提示里额外给出行数（大结果集写盘需要一点时间） */
const EXPORT_CONFIRM_ROWS = 5000;

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** 文件名安全化：去掉 Windows / Excel 不友好的字符 */
function safeName(s: string): string {
  return (s || "result").replace(/[\\/:*?"<>|]/g, "_").slice(0, 40);
}

function stamp(): string {
  const d = new Date();
  const p = (n: number) => (n < 10 ? "0" + n : "" + n);
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
}

/**
 * 导出当前展示的结果集为 Excel（xlsx）或 JSON。
 * 内容与界面一致：全部列（含类型）+ 全部结果行；路径由系统「另存为」对话框给出。
 */
export async function exportResult(tab: QueryTab, kind: "xlsx" | "json"): Promise<void> {
  const r = tab.result;
  if (!r || !r.rows.length) {
    toast("当前没有可导出的结果，请先执行查询", "err");
    return;
  }
  const ext = kind === "xlsx" ? "xlsx" : "json";
  const def = `datamind-${safeName(tab.db || "result")}${tab.table ? "-" + safeName(tab.table) : ""}-${stamp()}.${ext}`;
  let path: string | null = null;
  try {
    // 动态引入：浏览器（npm run dev）下没有 Tauri 环境时给出明确提示
    const { save } = await import("@tauri-apps/plugin-dialog");
    path = await save({
      defaultPath: def,
      filters: [
        {
          name: kind === "xlsx" ? "Excel 工作簿" : "JSON 文件",
          extensions: [ext],
        },
      ],
    });
  } catch {
    toast("当前环境不支持保存对话框，请在桌面客户端中使用导出", "err");
    return;
  }
  if (!path) return; // 用户取消

  // 导出始终作用于完整结果集，与界面分页无关（分页只影响渲染）
  if (r.rows.length >= EXPORT_CONFIRM_ROWS) {
    toast(`正在导出全部 ${r.rows.length} 行（不受分页影响，行数较多请稍候）…`);
  } else {
    toast(`正在导出 ${r.rows.length} 行…`);
  }

  const src = [...tab.sqls].reverse().find((s) => s.status === "success");
  try {
    const stat = await apiExport.toFile({
      path,
      format: kind,
      payload: {
        columns: r.columns.map((c) => ({ name: c.name, type: c.type })),
        rows: r.rows,
        source: {
          conn: connOf(tab.connId)?.name ?? "",
          db: tab.db,
          sql: src?.sql ?? "",
        },
      },
    });
    toast(
      `已导出 ${stat.rows} 行 × ${stat.columns} 列 · ${fmtBytes(stat.bytes)}`,
      "ok",
    );
  } catch (e) {
    const err = beErr(e);
    toast(`导出失败：${err.message}${err.hint ? `（${err.hint}）` : ""}`, "err");
  }
}
