<script setup lang="ts">
/* SQL 编辑器弹窗：
   1. edit 模式：查看 / 修改已生成的 SQL 卡片，保存后直接运行
   2. new  模式：手写或粘贴一条 SELECT，创建卡片后运行
   两种模式共用同一条执行链路（runSql → 后端 AST 只读校验 → 超时/取消），不新增任何执行权限 */
import { computed, ref, watch } from "vue";
import { sqlEditorState } from "../composables/modals";
import { state, runSql, touchTab, addManualSql } from "../composables/store";
import { icon, toast } from "../composables/ui";
import { formatSQL, looksLikeSelect } from "../lib/sql";
import ModalShell from "./ModalShell.vue";

const text = ref("");

const isNew = computed(() => sqlEditorState.mode === "new");
const tab = computed(() => state.tabs.find((t) => t.id === sqlEditorState.tabId) ?? null);
const item = computed(() =>
  isNew.value ? null : tab.value?.sqls.find((s) => s.id === sqlEditorState.sqlId) ?? null,
);

watch(
  () => [sqlEditorState.open, sqlEditorState.mode] as const,
  ([open]) => {
    if (!open) return;
    text.value = isNew.value ? sqlEditorState.initial : formatSQL(item.value?.sql ?? "");
  },
  { immediate: true },
);

/** 前端轻量预检（后端仍有 AST 级校验，此处只做即时提示，避免明显的写语句白跑一趟） */
const precheck = computed(() => {
  const s = text.value.trim();
  if (!s) return "";
  return looksLikeSelect(s) ? "" : "仅支持 SELECT / WITH 开头的只读查询，写操作会被只读保护拦截";
});

function close() {
  sqlEditorState.open = false;
}
function doFormat() {
  text.value = formatSQL(text.value);
}
function saveAndRun() {
  const sql = text.value.trim();
  if (!sql) {
    toast("请输入 SQL", "err");
    return;
  }
  if (!looksLikeSelect(sql)) {
    toast("仅支持 SELECT / WITH 开头的只读查询", "err");
    return;
  }
  if (!tab.value) return;
  if (isNew.value) {
    // 新建卡片 → 走与生成卡片完全相同的 runSql（含取消 / 超时 / 只读校验）
    const created = addManualSql(tab.value, sql);
    close();
    void runSql(tab.value, created);
    return;
  }
  const target = item.value;
  if (!target) return;
  target.sql = sql;
  target.status = "idle";
  target.error = null;
  touchTab();
  close();
  void runSql(tab.value, target);
}
</script>

<template>
  <ModalShell v-if="sqlEditorState.open" :title="isNew ? '新建 SQL（手写 / 粘贴）' : '查看 / 编辑 SQL'"
    icon-name="terminal" size="lg" @close="close">
    <div style="display:flex;flex-direction:column;gap:12px">
      <div class="field">
        <label>{{ isNew ? "手写或粘贴一条 SELECT 语句" : "SQL 语句（支持修改后直接运行）" }}</label>
        <textarea v-model="text" class="textarea mono" style="min-height:220px" spellcheck="false"
          :placeholder="isNew
            ? 'SELECT * FROM orders WHERE ctime >= UNIX_TIMESTAMP(DATE_SUB(CURDATE(), INTERVAL 7 DAY))'
            : ''"
          @keydown.enter.ctrl.prevent="saveAndRun" @keydown.enter.meta.prevent="saveAndRun"></textarea>
      </div>
      <div style="display:flex;gap:8px;align-items:center">
        <button class="btn btn--sm" @click="doFormat">
          <span v-html="icon('edit', 12)"></span> 格式化
        </button>
        <span v-if="precheck" class="composer__hint" style="color:var(--danger)">
          <span v-html="icon('alert', 11)"></span> {{ precheck }}
        </span>
        <span v-else class="composer__hint">
          仅支持 SELECT 查询；超时 {{ state.settings.sqlTimeout }}s 自动取消 · Ctrl + Enter 运行
        </span>
      </div>
    </div>
    <template #foot>
      <button class="btn" @click="close">取消</button>
      <button class="btn btn--primary" :disabled="!text.trim()" @click="saveAndRun">
        <span v-html="icon('play', 12)"></span> {{ isNew ? "创建并运行" : "保存并运行" }}
      </button>
    </template>
  </ModalShell>
</template>
