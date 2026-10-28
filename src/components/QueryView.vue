<script setup lang="ts">
/* 中间「数据查询栏」单窗口视图：联动下拉 / 对话生成 SQL / 执行与结果 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import type { QueryTab, SqlItem } from "../types";
import {
  state,
  connOf,
  memoryOf,
  dbOf,
  activeModel,
  generateSql,
  cancelGenerate,
  quickFixSql,
  runSql,
  cancelSql,
  deleteSql,
  clearSqls,
  syncConnection,
  touchTab,
  uid,
  exportResult,
} from "../composables/store";
import { openMemory, openSqlEditor, openNewSql } from "../composables/modals";
import { copyText, icon, toast, confirmDialog } from "../composables/ui";
import { formatSQL, highlightSQL } from "../lib/sql";
import Splitter from "./Splitter.vue";
import VSelect from "./VSelect.vue";
import ResultGrid from "./ResultGrid.vue";

const props = defineProps<{ tab: QueryTab; active: boolean }>();

const tab = computed(() => props.tab);

/* ---------- 联动下拉 ---------- */
const connOptions = computed(() =>
  state.connections.map((c) => ({
    value: c.id,
    label: c.name,
    sub: c.host + (c.ssh.enabled ? " ·SSH" : ""),
  })),
);
const dbOptions = computed(() =>
  (memoryOf(tab.value.connId)?.databases ?? []).map((d) => ({
    value: d.name,
    label: d.name,
    sub: `${d.tables.length} 表`,
  })),
);
const tableOptions = computed(() =>
  (dbOf(tab.value.connId, tab.value.db)?.tables ?? []).map((t) => ({
    value: t.name,
    label: t.name,
    sub: t.comment,
  })),
);

function onConnChange(v: string) {
  tab.value.connId = v || null;
  tab.value.db = memoryOf(v)?.databases[0]?.name ?? "";
  tab.value.table = "";
  touchTab();
}
function onDbChange(v: string) {
  tab.value.db = v;
  tab.value.table = "";
  touchTab();
}
function onTableChange(v: string) {
  tab.value.table = v;
  touchTab();
}

/* ---------- 记忆徽标 ---------- */
const memoryChip = computed(() => {
  const mem = memoryOf(tab.value.connId);
  if (!mem) return "记忆未同步";
  const tables = mem.databases.reduce((s, d) => s + d.tables.length, 0);
  const rels = mem.databases.reduce((s, d) => s + d.relations.length, 0);
  return `记忆 ${tables} 表 / ${rels} 关系 · ${mem.syncedAt.slice(5, 16)}`;
});

/* ---------- 对话输入 ---------- */
const composerText = ref("");
function send() {
  const q = composerText.value.trim();
  if (!q) return;
  composerText.value = "";
  void generateSql(tab.value, q);
}

const generating = computed(() => !!state.generating[tab.value.id]);
function stopGenerate() {
  void cancelGenerate(tab.value);
}
const modelLabel = computed(() => activeModel.value?.name ?? "未选择模型");

/* ---------- 多轮上下文 ---------- */
/** 已有对话 + 已有 SQL：下一次生成会带上历史对话与上一轮 SQL 迭代，而非从零生成 */
const followUp = computed(() => tab.value.messages.length > 0 && tab.value.sqls.length > 0);

/* ---------- 手写 SQL ---------- */
function newSql() {
  // 已选中数据表时预填一条可直接改的骨架，未选表则留空让用户自由粘贴
  const t = tab.value.table.replace(/`/g, "");
  openNewSql(tab.value.id, t ? `SELECT * FROM \`${t}\` LIMIT 100` : "");
}

/* ---------- 结果导出 ---------- */
const exporting = ref<"" | "xlsx" | "json">("");
async function doExport(kind: "xlsx" | "json") {
  if (exporting.value) return;
  exporting.value = kind;
  try {
    await exportResult(tab.value, kind);
  } finally {
    exporting.value = "";
  }
}

/* ---------- 两阶段生成：阶段进度 ---------- */
const AGENT_STAGES = [
  { key: "schema", label: "分析表结构" },
  { key: "explore", label: "探查真实数据" },
  { key: "design", label: "设计查询方案" },
  { key: "build", label: "生成 SQL" },
  { key: "verify", label: "校验 SQL" },
];
function stageState(key: string): string {
  const cur = AGENT_STAGES.findIndex((s) => s.key === state.genStage.key);
  if (cur < 0) return "";
  const i = AGENT_STAGES.findIndex((s) => s.key === key);
  if (i < cur) return "is-done";
  if (i === cur) return "is-active";
  return "";
}

/* ---------- SQL 卡片 ---------- */
const openTrace = ref<string | null>(null);
function toggleTrace(id: string) {
  openTrace.value = openTrace.value === id ? null : id;
}
function statusTag(item: SqlItem): { cls: string; label: string } {
  switch (item.status) {
    case "running": return { cls: "tag tag--warn", label: "运行中" };
    case "success": return { cls: "tag tag--ok", label: "成功" };
    case "error": return { cls: "tag tag--err", label: "执行失败" };
    case "cancelled": return { cls: "tag tag--warn", label: "已取消" };
    default: return { cls: "tag tag--ghost", label: "待运行" };
  }
}
function fmt(item: SqlItem): string {
  return formatSQL(item.sql);
}
function hl(item: SqlItem): string {
  return highlightSQL(fmt(item));
}
function doRun(item: SqlItem) {
  if (item.status === "running") void cancelSql(tab.value, item);
  else void runSql(tab.value, item);
}
function openEditor(item: SqlItem) {
  openSqlEditor(tab.value.id, item.id);
}
function doDelete(item: SqlItem) {
  hideTip();
  void deleteSql(tab.value, item);
}
function doClear() {
  if (!tab.value.sqls.length) return;
  confirmDialog(
    "清空 SQL",
    `确定删除本窗口全部 ${tab.value.sqls.length} 条 SQL 吗？删除后不可恢复。`,
    () => void clearSqls(tab.value),
    "全部删除",
  );
}

/* ---------- SQL 列表滚动 ---------- */
const sqlListEl = ref<HTMLElement | null>(null);
watch(
  () => tab.value.sqls.length,
  async () => {
    await nextTick();
    if (sqlListEl.value) sqlListEl.value.scrollTo({ top: sqlListEl.value.scrollHeight });
  },
);

/* 悬浮完整 SQL */
const tip = ref<{ show: boolean; html: string; x: number; y: number }>({
  show: false,
  html: "",
  x: 0,
  y: 0,
});
function showTip(item: SqlItem) {
  tip.value = { show: true, html: highlightSQL(fmt(item)), x: 0, y: 0 };
}
function moveTip(e: MouseEvent) {
  if (!tip.value.show) return;
  const el = document.getElementById("sqlTip");
  const w = el?.offsetWidth ?? 400;
  const h = el?.offsetHeight ?? 200;
  tip.value.x = Math.min(e.clientX + 16, window.innerWidth - w - 12);
  tip.value.y = Math.min(e.clientY + 16, window.innerHeight - h - 12);
}
function hideTip() {
  tip.value.show = false;
}

/* ---------- 快速修复 ---------- */
function fix(item: SqlItem) {
  void quickFixSql(tab.value, item);
}

/* ---------- 执行计时 ---------- */
const elapsed = ref(0);
let timer: ReturnType<typeof setInterval> | null = null;
watch(
  () => tab.value.running,
  (run) => {
    if (timer) { clearInterval(timer); timer = null; }
    elapsed.value = 0;
    if (run) {
      const start = Date.now();
      timer = setInterval(() => (elapsed.value = (Date.now() - start) / 1000), 100);
    }
  },
  { immediate: true },
);
onBeforeUnmount(() => {
  if (timer) clearInterval(timer);
});

const runningItem = computed(() =>
  tab.value.running
    ? tab.value.sqls.find((s) => s.id === tab.value.running && s.status === "running") ?? null
    : null,
);
const lastOk = computed(() => {
  const ok = tab.value.sqls.filter((s) => s.status === "success");
  return ok.length ? ok[ok.length - 1] : null;
});

function copyAll() {
  const r = tab.value.result;
  if (!r) return;
  const head = r.columns.map((c) => c.name).join("\t");
  const body = r.rows.map((row) => row.join("\t")).join("\n");
  copyText(head + "\n" + body, "已复制全部数据");
}
function copyMd() {
  const r = tab.value.result;
  if (!r) return;
  const head = r.columns.map((c) => c.name);
  const lines = [
    `| ${head.join(" | ")} |`,
    `| ${head.map(() => "---").join(" | ")} |`,
    ...r.rows.map((row) => `| ${row.map((v) => String(v ?? "").replace(/\|/g, "\\|")).join(" | ")} |`),
  ];
  copyText(lines.join("\n"), "已复制 Markdown 表格");
}

/* ---------- 面板尺寸拖拽 ---------- */
const qmainEl = ref<HTMLElement | null>(null);
const chatEl = ref<HTMLElement | null>(null);
const chatLeftEl = ref<HTMLElement | null>(null);

const chatH = computed({
  get: () => tab.value.chatH ?? 260,
  set: (v) => { tab.value.chatH = v; },
});
const chatW = computed({
  get: () => tab.value.chatW ?? 360,
  set: (v) => { tab.value.chatW = v; },
});

onMounted(() => {
  if (tab.value.chatH == null && qmainEl.value) {
    tab.value.chatH = Math.max(200, Math.round(qmainEl.value.clientHeight * 0.48));
  }
  if (tab.value.chatW == null && chatLeftEl.value) {
    tab.value.chatW = Math.round(chatLeftEl.value.clientWidth);
  }
});
function persistSizes() {
  touchTab();
}
</script>

<template>
  <div class="qview" :class="{ 'is-active': active }" v-show="active">
    <!-- 查询选项栏 -->
    <div class="qbar">
      <div class="qbar__group">
        <label>连接</label>
        <VSelect :model-value="tab.connId ?? ''" :options="connOptions" placeholder="选择连接"
          width="150px" @update:model-value="onConnChange" />
        <div class="qbar__sep"></div>
        <label>数据库</label>
        <VSelect :model-value="tab.db" :options="dbOptions" placeholder="选择数据库" width="140px"
          @update:model-value="onDbChange" />
        <div class="qbar__sep"></div>
        <label>数据表</label>
        <VSelect :model-value="tab.table" :options="tableOptions" placeholder="不选（多表联查）"
          width="150px" clearable @update:model-value="onTableChange" />
      </div>
      <div class="qbar__right">
        <button class="tag tag--accent tag--btn" :title="'查看该连接已形成的本地记忆'" @click="openMemory(tab.connId ?? '', tab.db)">
          <span v-html="icon('brain', 12)"></span>
          <span>{{ memoryChip }}</span>
        </button>
        <button class="btn btn--sm" @click="tab.connId && syncConnection(tab.connId)">
          <span v-html="icon('refresh', 12)"></span> 刷新记忆
        </button>
      </div>
    </div>

    <!-- 查询对话栏 + 数据展示栏 -->
    <div class="qmain" ref="qmainEl">
      <div class="qchat" ref="chatEl" :style="`height:${chatH}px`">
        <!-- 左：对话 -->
        <div class="qchat__left" ref="chatLeftEl" :style="`width:${chatW}px`">
          <div class="chat-scroll">
            <div v-if="!tab.messages.length" class="chat-empty">
              <div class="chat-empty__title" v-html="icon('spark', 18)"></div>
              <div class="chat-empty__text">描述你想查的数据，DataMind 会结合本地结构记忆生成 SQL</div>
              <div class="chat-empty__chips">
                <button class="chip" v-for="q in ['统计上个月各城市的订单数和销售额', '查询近 7 天注册且消费过的 VIP 用户', '各商品类目的销量排行']"
                  :key="q" @click="composerText = q">{{ q }}</button>
              </div>
            </div>
            <template v-else>
              <div v-for="(m, i) in tab.messages" :key="i" class="msg" :class="'msg--' + m.role">
                <template v-if="m.role === 'user'">
                  <div class="msg__bubble">{{ m.text }}</div>
                  <div class="msg__time">{{ m.ts }}</div>
                </template>
                <template v-else>
                  <div class="msg__label">
                    <span v-html="icon('spark', 12)"></span><span>DataMind</span>
                  </div>
                  <div class="msg__text">{{ m.text }}</div>
                </template>
              </div>
              <div v-if="generating" class="msg msg--ai">
                <div class="msg__label"><span v-html="icon('spark', 12)"></span><span>DataMind</span></div>
                <div class="msg__text">
                  <div class="agent-now">
                    <span class="agent-dot"></span>{{ state.genStage.label || "准备中" }}…
                  </div>
                  <ol class="agent-steps">
                    <li v-for="s in AGENT_STAGES" :key="s.key" :class="stageState(s.key)">{{ s.label }}</li>
                  </ol>
                </div>
              </div>
            </template>
          </div>
          <div class="composer">
            <div class="composer__box">
              <textarea v-model="composerText"
                :placeholder="followUp
                  ? '结果不对？继续补充说明，例如：只要已支付的订单、按城市分组、改成按周统计'
                  : '用自然语言描述你要查询的数据，例如：统计上个月各城市的订单数和销售额，按销售额倒序'"
                @keydown.enter.ctrl.prevent="send" @keydown.enter.meta.prevent="send"></textarea>
              <div class="composer__bar">
                <span class="composer__hint">
                  {{ followUp ? "本轮将结合历史对话与上一轮 SQL 迭代生成 · " : "" }}Ctrl + Enter 发送 · 仅生成 SELECT
                </span>
                <span class="tag tag--ghost" v-html="icon('spark', 11) + ' ' + modelLabel"></span>
                <div style="flex:1"></div>
                <button v-if="generating" class="btn btn--ghost btn--sm" @click="stopGenerate">
                  停止
                </button>
                <button class="btn btn--primary btn--sm" :disabled="generating" @click="send">
                  <span v-html="icon('spark', 12)"></span> {{ generating ? "推理中…" : "生成 SQL" }}
                </button>
              </div>
            </div>
          </div>
        </div>

        <Splitter v-model="chatW" axis="x" :min="240" :max-fn="() => (chatEl?.clientWidth ?? 600) - 220"
          @change="persistSizes" />

        <!-- 右：生成的 SQL 列表 -->
        <div class="qchat__right">
          <div class="sql-head">
            <span v-html="icon('spark', 12) + ' 生成的 SQL'"></span>
            <span class="tag tag--ghost" style="margin-left:auto">{{ tab.sqls.length }}</span>
            <button class="btn btn--sm sql-head__new" title="手写或粘贴一条 SELECT 后执行"
              @click="newSql">
              <span v-html="icon('plus', 12)"></span> 新建 SQL
            </button>
            <button v-if="tab.sqls.length" class="iconbtn iconbtn--sm sql-head__clear" title="清空全部 SQL"
              @click="doClear" v-html="icon('trash', 13)"></button>
          </div>
          <div class="sql-list" ref="sqlListEl">
            <div v-if="!tab.sqls.length" class="sql-empty">对话生成后，SQL 会出现在这里</div>
            <div v-for="(item, idx) in tab.sqls" :key="item.id" class="sqlcard-wrap">
            <div class="sqlcard"
              :class="{ 'is-running': item.status === 'running', 'is-error': item.status === 'error', 'is-ok': item.status === 'success' }">
              <div class="sqlcard__main" @mouseenter="showTip(item)" @mousemove="moveTip" @mouseleave="hideTip"
                @click="hideTip(); openEditor(item)">
                <div class="sqlcard__head">
                  <span>#{{ idx + 1 }}{{ item.fixed ? " · 修复" : "" }}{{ item.manual ? " · 手写" : "" }}</span>
                  <span>{{ item.ts }}</span>
                  <span :class="statusTag(item).cls">{{ statusTag(item).label }}</span>
                  <span v-if="item.ms != null" class="tag tag--ghost">{{ item.ms }} ms</span>
                </div>
                <div class="sqlcard__code mono" v-html="hl(item)"></div>
                <!-- 错误块 -->
                <div v-if="item.status === 'error' && item.error" class="sqlerr">
                  <div class="sqlerr__top">
                    <span v-html="icon('alert', 13)"></span>
                    <span>MySQL Error {{ item.error.code }}</span>
                  </div>
                  <div class="sqlerr__msg">{{ item.error.message }}</div>
                  <div v-if="item.error.hint" class="sqlerr__msg" style="color:var(--text-2)">建议：{{ item.error.hint }}</div>
                  <div class="sqlerr__acts">
                    <button class="btn btn--sm btn--primary" @click.stop="fix(item)">
                      <span v-html="icon('spark', 12)"></span> 快速修复 SQL
                    </button>
                  </div>
                </div>
              </div>
              <div class="sqlcard__side">
                <button class="sqlcard__run" :class="{ 'is-stop': item.status === 'running' }"
                  :title="item.status === 'running' ? '取消执行' : '运行 SQL'" @click.stop="doRun(item)"
                  v-html="icon(item.status === 'running' ? 'stop' : 'play', 13)"></button>
                <button class="iconbtn iconbtn--sm sqlcard__mini" title="复制 SQL" @click.stop="copyText(fmt(item))"
                  v-html="icon('copy', 13)"></button>
                <button class="iconbtn iconbtn--sm sqlcard__mini" title="编辑 SQL" @click.stop="openEditor(item)"
                  v-html="icon('edit', 13)"></button>
                <button class="iconbtn iconbtn--sm sqlcard__mini sqlcard__del" title="删除此 SQL"
                  @click.stop="doDelete(item)" v-html="icon('trash', 13)"></button>
              </div>
            </div>
            <div v-if="item.trace?.length" class="sqlcard__trace">
              <button class="tracebtn" @click.stop="toggleTrace(item.id)">
                <span v-html="icon('spark', 11)"></span>
                推理过程 · {{ item.trace.length }} 步
                <span class="tracebtn__arrow">{{ openTrace === item.id ? "收起" : "展开" }}</span>
              </button>
              <div v-if="openTrace === item.id" class="trace">
                <div v-for="(s, i) in item.trace" :key="i" class="trace__item" :class="{ 'is-bad': !s.ok }">
                  <span class="trace__idx">{{ i + 1 }}</span>
                  <div class="trace__body">
                    <div class="trace__label">
                      {{ s.label }}
                      <span v-if="!s.ok" class="tag tag--err">降级</span>
                    </div>
                    <div class="trace__sum">{{ s.summary }}</div>
                  </div>
                </div>
              </div>
            </div>
            </div>
          </div>
        </div>
      </div>

      <Splitter v-model="chatH" axis="y" :min="160"
        :max-fn="() => (qmainEl?.clientHeight ?? 600) - 160" @change="persistSizes" />

      <!-- 数据展示栏 -->
      <div class="qresult">
        <div class="qresult__bar">
          <div class="qresult__status">
            <template v-if="runningItem">
              <span class="tag tag--warn">查询中</span>
              <span>{{ elapsed.toFixed(1) }} s</span>
              <span class="composer__hint">超过 {{ state.settings.sqlTimeout }}s 将自动取消</span>
            </template>
            <template v-else-if="tab.result">
              <span class="tag tag--ok">完成</span>
              <span>共 <b>{{ tab.result.rows.length }}</b> 条
                <template v-if="tab.result.truncated">（已截断）</template>
                <template v-if="lastOk?.ms != null"> · {{ lastOk.ms }} ms</template>
              </span>
            </template>
            <span v-else class="composer__hint">尚未执行查询</span>
          </div>
          <div class="qresult__acts">
            <button v-if="runningItem" class="btn btn--sm btn--danger" @click="cancelSql(tab, runningItem)">
              <span v-html="icon('stop', 12)"></span> 取消查询
            </button>
            <template v-else-if="tab.result">
              <button class="btn btn--sm" @click="copyAll"><span v-html="icon('copy', 12)"></span> 复制全部</button>
              <button class="btn btn--sm" @click="copyMd"><span v-html="icon('copy', 12)"></span> 复制为 Markdown</button>
              <button class="btn btn--sm" :disabled="!!exporting" @click="doExport('xlsx')">
                <span v-html="icon('down', 12)"></span> {{ exporting === "xlsx" ? "导出中…" : "导出 Excel" }}
              </button>
              <button class="btn btn--sm" :disabled="!!exporting" @click="doExport('json')">
                <span v-html="icon('down', 12)"></span> {{ exporting === "json" ? "导出中…" : "导出 JSON" }}
              </button>
            </template>
          </div>
        </div>
        <ResultGrid :result="tab.result" />
      </div>
    </div>
  </div>

  <!-- 悬浮完整 SQL 预览 -->
  <div v-if="tip.show" id="sqlTip" class="sqltip" :style="`left:${tip.x}px;top:${tip.y}px`"
    v-html="tip.html"></div>
</template>
