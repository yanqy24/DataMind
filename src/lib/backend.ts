/* ============================================================
 * backend.ts — Tauri invoke 封装（全部后端命令的类型化入口）
 * ============================================================ */
import { invoke } from "@tauri-apps/api/core";
import type {
  AgentStep,
  AppSettings,
  BackendError,
  ConnConfig,
  ConnMemory,
  ModelConfig,
  QueryResult,
  SqlLogItem,
  TableDetail,
  ChatTurn,
} from "../types";

/** 后端抛错时 invoke 会 reject 出该结构 */
export function beErr(e: unknown): BackendError {
  if (typeof e === "object" && e !== null && "code" in e && "message" in e) {
    return e as BackendError;
  }
  return { code: "UNKNOWN", message: e instanceof Error ? e.message : String(e), hint: null };
}

/* ---------- 连接 ---------- */
export const apiConn = {
  list: () => invoke<ConnConfig[]>("conn_list"),
  save: (conn: ConnConfig) => invoke<ConnConfig>("conn_save", { conn }),
  remove: (id: string) => invoke<void>("conn_delete", { id }),
  test: (conn: ConnConfig) =>
    invoke<{ ok: boolean; ms: number; message: string }>("conn_test", { conn }),
  /** 连接 + 全量读取 DDL/关系/取样，重建本地记忆（带 sync-progress 事件） */
  sync: (id: string) => invoke<ConnMemory>("conn_sync", { id }),
  memory: (id: string) => invoke<ConnMemory | null>("memory_get", { id }),
};

/* ---------- 单表详情 ---------- */
export const apiTable = {
  /** 实时读取建表语句 + 取样（绕过本地记忆，注释按当前库的字符集解码） */
  detail: (connId: string, db: string, table: string) =>
    invoke<TableDetail>("table_detail", { connId, db, table }),
};

/* ---------- SQL 执行 ---------- */
export const apiSql = {
  execute: (p: {
    runId: string;
    connId: string;
    db: string;
    sql: string;
    timeoutSecs: number;
  }) => invoke<QueryResult>("sql_execute", p),
  cancel: (runId: string) => invoke<void>("sql_cancel", { runId }),
  /** 模型推理生成 SQL；mode=fix 为错误修复（runId 供取消用） */
  generate: (p: {
    runId: string;
    connId: string;
    db: string;
    table: string;
    prompt: string;
    mode: "new" | "fix";
    prevSql: string | null;
    error: string | null;
    agent?: boolean;
    /** 多轮上下文：本窗口历史对话（首轮为空数组） */
    history?: ChatTurn[];
  }) =>
    invoke<{ sql: string; note: string; steps: AgentStep[] }>("sql_generate", p),
};

/* ---------- 结果导出 ---------- */
export const apiExport = {
  /** 按 path + format 导出当前结果集（路径由「另存为」对话框给出） */
  toFile: (p: {
    path: string;
    format: "xlsx" | "json";
    payload: {
      columns: { name: string; type: string }[];
      rows: (string | null)[][];
      source?: { conn: string; db: string; sql: string };
    };
  }) =>
    invoke<{ path: string; format: string; rows: number; columns: number; bytes: number }>(
      "result_export",
      p,
    ),
};

/* ---------- 模型 ---------- */
export const apiModel = {
  list: () => invoke<ModelConfig[]>("model_list"),
  save: (model: ModelConfig) => invoke<ModelConfig>("model_save", { model }),
  remove: (id: string) => invoke<void>("model_delete", { id }),
  activate: (id: string) => invoke<void>("model_activate", { id }),
};

/* ---------- SQL 日志 ---------- */
export const apiLog = {
  list: () => invoke<SqlLogItem[]>("log_list"),
  clear: () => invoke<void>("log_clear"),
};

/* ---------- 设置 ---------- */
export const apiSettings = {
  get: () => invoke<AppSettings>("settings_get"),
  set: (s: AppSettings) => invoke<void>("settings_set", { settings: s }),
};

/* ---------- 版本 / 诊断 ---------- */
export const apiApp = {
  version: () => invoke<{ version: string; build: string }>("app_version"),
  /** 运行环境诊断：WebView2 是否存活、是否有冲突的第三方输入法 */
  diag: () => invoke<{ webviewAlive: boolean; ime: string | null }>("env_diag"),
  /** 界面崩溃后重建（重置自动重试计数） */
  restartWebview: () => invoke<void>("webview_restart"),
};
