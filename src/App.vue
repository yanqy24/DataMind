<script setup lang="ts">
/* App — 应用根组件：三栏工作区装配 / 全局快捷键 / 主题 */
import { computed, onMounted, ref } from "vue";
import {
  state,
  activeTab,
  initApp,
  createTab,
  setTheme,
  setAgentMode,
  setCollapsed,
  saveUi,
} from "./composables/store";
import { settingsState } from "./composables/modals";
import { icon, toast } from "./composables/ui";
import { apiApp } from "./lib/backend";
import { listen } from "@tauri-apps/api/event";
import ConnSidebar from "./components/ConnSidebar.vue";
import ModelSidebar from "./components/ModelSidebar.vue";
import TabBar from "./components/TabBar.vue";
import QueryView from "./components/QueryView.vue";
import Splitter from "./components/Splitter.vue";
import ToastHost from "./components/ToastHost.vue";
import CtxHost from "./components/CtxHost.vue";
import ConfirmModal from "./components/ConfirmModal.vue";
import ConnFormModal from "./components/ConnFormModal.vue";
import ModelFormModal from "./components/ModelFormModal.vue";
import SettingsModal from "./components/SettingsModal.vue";
import SqlEditorModal from "./components/SqlEditorModal.vue";
import MemoryModal from "./components/MemoryModal.vue";
import TableModal from "./components/TableModal.vue";

const workspaceEl = computed<HTMLElement | null>(() => document.getElementById("workspace") as HTMLElement | null);

const MIN_SIDE = 200;
const MAX_SIDE = 560;
const MIN_CENTER = 420;

const leftSize = computed({ get: () => state.sizes.left, set: (v) => (state.sizes.left = v) });
const rightSize = computed({ get: () => state.sizes.right, set: (v) => (state.sizes.right = v) });

const leftMax = () => {
  const other = state.collapsed.right ? 40 : state.sizes.right;
  return Math.max(MIN_SIDE, (workspaceEl.value?.clientWidth ?? 1200) - other - MIN_CENTER);
};
const rightMax = () => {
  const other = state.collapsed.left ? 40 : state.sizes.left;
  return Math.max(MIN_SIDE, (workspaceEl.value?.clientWidth ?? 1200) - other - MIN_CENTER);
};

function onGlobalKey(e: KeyboardEvent) {
  const mod = e.ctrlKey || e.metaKey;
  if (!mod) return;
  if (e.key === "t") {
    e.preventDefault();
    createTab();
  } else if (e.key === ",") {
    e.preventDefault();
    settingsState.open = true;
  } else if (e.key === "b") {
    e.preventDefault();
    setCollapsed("left", !state.collapsed.left);
  }
}

import { invoke } from "@tauri-apps/api/core";

const bootError = ref("");

/* ---------- 生成模式切换（快速模式 ⇄ Agent 模式） ---------- */
const agentMode = computed(() => state.settings.agentMode !== false);
const modeTip = ref(false);

function toggleAgentMode() {
  const next = !agentMode.value;
  setAgentMode(next);
  toast(next ? "已切换为 Agent 模式：生成前先分析探查数据" : "已切换为快速模式：单次直出，响应更快", "ok");
}

/** 界面崩溃提示（WebView2 进程崩溃时由后端守护线程推送） */
const crash = ref<{ ime: string | null; attempt: number; failed: boolean } | null>(null);

function report(msg: string) {
  // 启动自检上报（桌面环境无控制台时辅助定位）
  invoke("diag_log", { payload: msg }).catch(() => undefined);
}

function restartWebview() {
  crash.value = null;
  apiApp
    .restartWebview()
    .catch((e) => {
      crash.value = { ime: null, attempt: 99, failed: true };
      report(`RESTART_FAIL ${e}`);
    });
}

onMounted(() => {
  // 监听后端守护线程的崩溃事件（自动重建会重载页面，此处主要承接"重建失败"）
  listen<{ autoRestart: boolean; attempt: number; ime: string | null }>("webview:crashed", (e) => {
    const p = e.payload ?? ({} as { autoRestart: boolean; attempt: number; ime: string | null });
    crash.value = { ime: p.ime ?? null, attempt: p.attempt ?? 1, failed: !p.autoRestart };
    report(`WEBVIEW_CRASH attempt=${p.attempt} auto=${p.autoRestart} ime=${p.ime ?? "-"}`);
  }).catch(() => undefined);

  initApp()
    .then(() => {
      const appLen = document.getElementById("app")?.innerHTML.length ?? 0;
      report(`READY appLen=${appLen} tabs=${state.tabs.length} conns=${state.connections.length} models=${state.models.length}`);
    })
    .catch((e) => {
      bootError.value = e instanceof Error ? e.message : String(e);
      report(`BOOT_FAIL ${bootError.value}`);
    });
  document.addEventListener("keydown", onGlobalKey);
});
</script>

<template>
  <div class="app" v-if="state.ready">
    <!-- 界面崩溃提示条（仅 WebView2 渲染进程真崩溃时出现） -->
    <div v-if="crash" class="alertbar alertbar--danger">
      <span v-html="icon('alert', 16)"></span>
      <span class="alertbar__text">
        界面渲染进程已崩溃{{
          crash.ime ? `（检测到第三方输入法「${crash.ime}」，已知与 WebView2 冲突）` : ""
        }}。{{ crash.failed ? "自动重建未成功，可手动重试或临时切换为微软拼音。" : "正在自动重建界面…" }}
      </span>
      <button class="btn btn--sm btn--primary" @click="restartWebview">重建界面</button>
      <button class="iconbtn" title="忽略" @click="crash = null" v-html="icon('x', 14)"></button>
    </div>

    <!-- 顶部应用栏 -->
    <header class="appbar">
      <div class="appbar__brand">
        <span class="logo" v-html="icon('layers', 18)"></span>
        <span class="appbar__name">DataMind</span>
        <span class="tag tag--ghost">v1.0.0</span>
      </div>
      <div class="appbar__actions">
        <div class="modeswitch" @mouseenter="modeTip = true" @mouseleave="modeTip = false">
          <button class="modebtn" :class="{ 'is-agent': agentMode }" @click="toggleAgentMode">
            <span class="modebtn__ico" v-html="icon(agentMode ? 'brain' : 'play', 13)"></span>
            <span class="modebtn__label">{{ agentMode ? "Agent模式" : "快速模式" }}</span>
          </button>
          <transition name="tip">
            <div v-if="modeTip" class="tipcard">
              <div class="tipcard__head">生成模式 · 点击切换</div>
              <div class="tipcard__row" :class="{ 'is-on': !agentMode }">
                <div class="tipcard__title">
                  <span v-html="icon('play', 12)"></span>快速模式
                  <span class="tag tag--ghost">快</span>
                </div>
                <div class="tipcard__desc">
                  直接依据本地记忆（表结构 + 表关系）一次生成 SQL。<br />
                  通常 3–8 秒返回；时间单位、枚举取值、NULL 语义等只能靠记忆猜，记忆不准时容易出错。
                </div>
              </div>
              <div class="tipcard__row" :class="{ 'is-on': agentMode }">
                <div class="tipcard__title">
                  <span v-html="icon('brain', 12)"></span>Agent模式
                  <span class="tag tag--accent">准</span>
                </div>
                <div class="tipcard__desc">
                  先分析表结构 → 主动探查真实数据（时间单位 / 枚举取值 / NULL 比例 / 行数）→ 设计方案 →
                  生成 → 校验（只读 + EXPLAIN + 复审）后再交付。<br />
                  准确率显著提升，但需 4–5 次模型调用，通常 15–40 秒。
                </div>
              </div>
            </div>
          </transition>
        </div>
        <button class="iconbtn" :title="state.settings.theme === 'dark' ? '切换为浅色' : '切换为深色'"
          @click="setTheme(state.settings.theme === 'dark' ? 'light' : 'dark')"
          v-html="icon(state.settings.theme === 'dark' ? 'sun' : 'moon', 16)"></button>
        <button class="iconbtn" title="设置 (Ctrl+,)" @click="settingsState.open = true"
          v-html="icon('settings', 16)"></button>
      </div>
    </header>

    <!-- 三栏工作区 -->
    <main class="workspace" id="workspace">
      <ConnSidebar />
      <Splitter v-if="!state.collapsed.left" v-model="leftSize" axis="x" :min="MIN_SIDE" :max-fn="leftMax"
        @change="saveUi" />

      <section class="center">
        <TabBar />
        <div class="views">
          <QueryView v-for="t in state.tabs" :key="t.id" :tab="t" :active="t.id === state.activeTab" />
        </div>
        <div v-if="!state.tabs.length" class="empty-state">
          <svg viewBox="0 0 24 24" width="34" height="34"><use href="#i-terminal" /></svg>
          <p>没有打开的查询窗口</p>
          <button class="btn btn--primary" @click="createTab()">新建查询窗口</button>
        </div>
      </section>

      <Splitter v-if="!state.collapsed.right" v-model="rightSize" axis="x" reverse :min="MIN_SIDE"
        :max-fn="rightMax" @change="saveUi" />
      <ModelSidebar />
    </main>

    <!-- 全局浮层 -->
    <ToastHost />
    <CtxHost />
    <ConfirmModal />
    <ConnFormModal />
    <ModelFormModal />
    <SettingsModal />
    <SqlEditorModal />
    <MemoryModal />
    <TableModal />
  </div>
  <div v-else class="app" style="place-items:center;display:grid;color:var(--text-3)">
    <div v-if="bootError" class="boot-error" style="position:static;max-width:520px">
      DataMind 启动异常：
      {{ bootError }}
    </div>
    <div v-else>DataMind 正在启动…</div>
  </div>
</template>
