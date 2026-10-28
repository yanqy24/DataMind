<script setup lang="ts">
/* 设置：版本信息 / 外观(深色模式) / 通用 / SQL 日志 */
import { computed, ref, watch } from "vue";
import { settingsState } from "../composables/modals";
import { state, setTheme, setAgentMode, refreshSqlLog } from "../composables/store";
import { confirmDialog, icon, copyText, toast } from "../composables/ui";
import { apiLog, apiApp, apiSettings } from "../lib/backend";
import { highlightSQL } from "../lib/sql";
import ModalShell from "./ModalShell.vue";

type Nav = "about" | "appearance" | "general" | "log";
const nav = ref<Nav>("about");
const keyword = ref("");
const version = ref<{ version: string; build: string }>({ version: "1.0.0", build: "" });

watch(
  () => settingsState.open,
  (open) => {
    if (open) {
      nav.value = "about";
      apiApp
        .version()
        .then((v) => (version.value = v))
        .catch(() => undefined);
      void refreshSqlLog();
    }
  },
);

const filteredLog = computed(() => {
  const kw = keyword.value.trim().toLowerCase();
  return state.sqlLog.filter((l) => !kw || l.sql.toLowerCase().includes(kw));
});

function statusInfo(l: { status: string }): { cls: string; label: string } {
  switch (l.status) {
    case "success": return { cls: "tag tag--ok", label: "成功" };
    case "error": return { cls: "tag tag--err", label: "失败" };
    case "running": return { cls: "tag tag--warn", label: "运行中" };
    case "cancelled": return { cls: "tag tag--warn", label: "已取消" };
    default: return { cls: "tag tag--ghost", label: "生成" };
  }
}

function onTimeoutInput(e: Event) {
  const v = parseInt((e.target as HTMLInputElement).value, 10);
  if (!Number.isNaN(v) && v >= 1) {
    state.settings.sqlTimeout = Math.min(600, v);
  }
}
function saveTimeout() {
  void apiSettings.set(state.settings);
  toast("已保存", "ok");
}

function toggleAgent() {
  const next = state.settings.agentMode === false;
  setAgentMode(next);
  toast(next ? "已开启 Agent模式" : "已关闭，改为快速模式（单次直出）", "ok");
}

function clearLog() {
  confirmDialog("清空 SQL 日志", `确定清空全部 ${state.sqlLog.length} 条日志？`, () => {
    void apiLog.clear();
    state.sqlLog = [];
    toast("已清空", "ok");
  });
}
</script>

<template>
  <ModalShell v-if="settingsState.open" title="设置" icon-name="settings" size="lg" @close="settingsState.open = false">
    <div class="setwrap">
      <div class="setnav">
        <div v-for="it in [
          { key: 'about', label: '版本信息', ic: 'layers' },
          { key: 'appearance', label: '外观', ic: 'moon' },
          { key: 'general', label: '通用', ic: 'settings' },
          { key: 'log', label: 'SQL 日志', ic: 'terminal' },
        ]" :key="it.key" class="setnav__item" :class="{ 'is-on': nav === it.key }" @click="nav = it.key as Nav">
          <span v-html="icon(it.ic, 13)"></span><span>{{ it.label }}</span>
        </div>
      </div>
      <div class="setpanel">
        <!-- 版本信息 -->
        <template v-if="nav === 'about'">
          <div class="modal__section-title" v-html="icon('layers', 13) + ' 版本信息'"></div>
          <div class="setcard">
            <div class="kv"><span class="kv__k">应用名称</span><span class="kv__v">DataMind</span></div>
            <div class="kv"><span class="kv__k">版本号</span><span class="kv__v">{{ version.version }}</span></div>
            <div class="kv"><span class="kv__k">构建</span><span class="kv__v">{{ version.build || "—" }}</span></div>
            <div class="kv"><span class="kv__k">当前平台</span><span class="kv__v">Windows x64</span></div>
            <div class="kv"><span class="kv__k">运行时</span><span class="kv__v">Tauri 2 / Rust + WebView2</span></div>
            <div class="kv"><span class="kv__k">多端计划</span><span class="kv__v">macOS / Android / iOS</span></div>
          </div>
          <div class="memcard">
            <div class="memcard__head"><span v-html="icon('spark', 13)"></span><span>更新日志</span></div>
            <div class="memcard__body">v1.0.0 · 首个开源版本
· 三栏可拖拽布局 / 面板折叠
· MySQL 连接管理（支持 SSH 隧道）
· 多查询窗口 Tab
· 自然语言生成 SQL + 本地结构记忆
· Agent 模式（Schema 分析 → 数据探查 → SQL 设计 → 生成 → 校验）
· 闲置断连自愈 / 双击表直接查询整表
· 中文字符集自动修复（GBK / cp1252 / 二次编码还原）
· 60s 超时保护 / 只读保护（仅 SELECT）
· 结果集多选复制（文本 / Markdown）
· 深色模式 / SQL 日志</div>
          </div>
        </template>

        <!-- 外观 -->
        <template v-else-if="nav === 'appearance'">
          <div class="modal__section-title" v-html="icon('moon', 13) + ' 外观'"></div>
          <div class="setrow">
            <div class="setrow__main">
              <div class="setrow__title">深色模式</div>
              <div class="setrow__desc">切换后立即生效</div>
            </div>
            <div class="switch" :class="{ 'is-on': state.settings.theme === 'dark' }"
              @click="setTheme(state.settings.theme === 'dark' ? 'light' : 'dark')"></div>
          </div>
        </template>

        <!-- 通用 -->
        <template v-else-if="nav === 'general'">
          <div class="modal__section-title" v-html="icon('settings', 13) + ' 通用'"></div>
          <div class="setrow">
            <div class="setrow__main">
              <div class="setrow__title">SQL 执行超时</div>
              <div class="setrow__desc">超时后自动取消执行（默认 60 秒，最大 600 秒）</div>
            </div>
            <input class="input" type="number" min="1" max="600" style="width:90px;text-align:right"
              :value="state.settings.sqlTimeout" @input="onTimeoutInput" @change="saveTimeout" />
          </div>
          <div class="setrow">
            <div class="setrow__main">
              <div class="setrow__title">Agent模式</div>
              <div class="setrow__desc">
                生成前先分析表结构、主动探查真实数据（时间单位 / 枚举取值 / NULL 比例），
                再设计方案、生成并校验 SQL。关闭则退化为快速模式（单次直出，更快但更依赖记忆质量）
              </div>
            </div>
            <div class="switch" :class="{ 'is-on': state.settings.agentMode !== false }"
              @click="toggleAgent"></div>
          </div>
          <div class="setrow">
            <div class="setrow__main">
              <div class="setrow__title">只读保护</div>
              <div class="setrow__desc">禁止执行 INSERT / UPDATE / DELETE 等写操作</div>
            </div>
            <div class="switch is-on" title="出于安全考虑，写操作始终禁用"
              @click="toast('出于安全考虑，写操作始终禁用')"></div>
          </div>
        </template>

        <!-- SQL 日志 -->
        <template v-else>
          <div class="logbar">
            <input v-model="keyword" class="input" style="flex:1" placeholder="搜索 SQL 关键字" />
            <button class="btn btn--sm btn--danger" @click="clearLog">
              <span v-html="icon('trash', 12)"></span> 清空
            </button>
          </div>
          <div class="loglist">
            <div v-if="!filteredLog.length" class="sql-empty">暂无日志</div>
            <div v-for="l in filteredLog" :key="l.id" class="logitem">
              <div class="logitem__top">
                <span :class="statusInfo(l).cls">{{ statusInfo(l).label }}</span>
                <span>{{ l.ts }}</span>
                <span>{{ l.conn }} / {{ l.db }}</span>
                <span v-if="l.ms != null">{{ l.ms }} ms</span>
                <span style="margin-left:auto;cursor:pointer" @click="copyText(l.sql)">复制</span>
              </div>
              <div class="logitem__sql" v-html="highlightSQL(l.sql)"></div>
            </div>
          </div>
        </template>
      </div>
    </div>
  </ModalShell>
</template>
