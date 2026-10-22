<script setup lang="ts">
/* SQL 查看 / 编辑弹窗：格式化展示，可修改后直接运行 */
import { computed, ref, watch } from "vue";
import { sqlEditorState } from "../composables/modals";
import { state, runSql, touchTab } from "../composables/store";
import { icon } from "../composables/ui";
import { formatSQL } from "../lib/sql";
import ModalShell from "./ModalShell.vue";

const text = ref("");

const tab = computed(() => state.tabs.find((t) => t.id === sqlEditorState.tabId) ?? null);
const item = computed(() =>
  tab.value?.sqls.find((s) => s.id === sqlEditorState.sqlId) ?? null,
);

watch(
  () => sqlEditorState.open,
  (open) => {
    if (open && item.value) text.value = formatSQL(item.value.sql);
  },
  { immediate: true },
);

function close() {
  sqlEditorState.open = false;
}
function doFormat() {
  text.value = formatSQL(text.value);
}
function saveAndRun() {
  if (!item.value || !tab.value) return;
  item.value.sql = text.value;
  item.value.status = "idle";
  item.value.error = null;
  touchTab();
  close();
  void runSql(tab.value, item.value);
}
</script>

<template>
  <ModalShell v-if="sqlEditorState.open" title="查看 / 编辑 SQL" icon-name="terminal" size="lg" @close="close">
    <div style="display:flex;flex-direction:column;gap:12px">
      <div class="field">
        <label>SQL 语句（支持修改后直接运行）</label>
        <textarea v-model="text" class="textarea mono" style="min-height:220px" spellcheck="false"></textarea>
      </div>
      <div style="display:flex;gap:8px;align-items:center">
        <button class="btn btn--sm" @click="doFormat">
          <span v-html="icon('edit', 12)"></span> 格式化
        </button>
        <span class="composer__hint">
          仅支持 SELECT 查询；超时 {{ state.settings.sqlTimeout }}s 自动取消
        </span>
      </div>
    </div>
    <template #foot>
      <button class="btn" @click="close">取消</button>
      <button class="btn btn--primary" @click="saveAndRun">
        <span v-html="icon('play', 12)"></span> 保存并运行
      </button>
    </template>
  </ModalShell>
</template>
