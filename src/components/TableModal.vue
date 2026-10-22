<script setup lang="ts">
/* 单表查看弹窗：建表语句 + 取样数据（均实时读取，本地记忆只作兜底） */
import { computed, ref, watch } from "vue";
import { tableState } from "../composables/modals";
import { copyText, icon } from "../composables/ui";
import { apiTable, beErr } from "../lib/backend";
import type { TableDetail } from "../types";
import ModalShell from "./ModalShell.vue";

const t = computed(() => tableState.table);

/** 实时读取结果：记忆里的可能是字符集修复前采集的（注释已变成 '?'），不能作为显示依据 */
const live = ref<TableDetail | null>(null);
const loading = ref(false);
const liveErr = ref("");

watch(
  () => [tableState.open, tableState.connId, tableState.db, t.value?.name] as const,
  async ([open, connId, db, name]) => {
    if (!open) return;
    live.value = null;
    liveErr.value = "";
    if (!connId || !db || !name) return;
    loading.value = true;
    try {
      live.value = await apiTable.detail(connId, db, name);
      // 顺手回填内存中的记忆副本：后续生成 SQL 的上下文也能用到正确注释（不落盘）
      if (t.value) {
        t.value.ddl = live.value.ddl;
        if (live.value.sample.length) t.value.sample = live.value.sample;
      }
    } catch (e) {
      // 读取失败时回落到本地记忆，但要明确告知这份可能过期
      liveErr.value = beErr(e).message;
    } finally {
      loading.value = false;
    }
  },
  { immediate: true },
);

const ddlText = computed(() => live.value?.ddl || t.value?.ddl || "");
const sample = computed(() => live.value?.sample ?? t.value?.sample ?? []);
const ddlSource = computed(() =>
  live.value ? "实时读取" : loading.value ? "读取中…" : "本地缓存",
);
</script>

<template>
  <ModalShell v-if="tableState.open && t" :title="`${tableState.db}.${t.name}`" icon-name="table"
    size="lg" @close="tableState.open = false">
    <div class="modal__section-title" v-html="icon('edit', 13) + ' 建表语句'"></div>
    <div class="memcard">
      <div class="memcard__head">
        <span v-html="icon('table', 13)"></span>
        <span>{{ t.name }}</span>
        <span class="tag tag--ghost" :title="liveErr || ''">{{ ddlSource }}</span>
        <span class="tag tag--ghost" style="margin-left:auto">{{ t.comment }}</span>
        <button class="btn btn--ghost btn--sm" :disabled="!ddlText"
          @click="copyText(ddlText, '建表语句已复制')">复制</button>
      </div>
      <div class="memcard__body" :class="{ 'memcard__body--muted': loading }">{{ ddlText }}</div>
      <div v-if="liveErr" class="memcard__foot">
        实时读取失败（{{ liveErr }}），当前显示的是本地缓存版本，可能已过期
      </div>
    </div>
    <div class="modal__section-title" v-html="icon('database', 13) + ` 取样数据（${sample.length} 行）`"></div>
    <div class="memcard">
      <div class="memcard__body">{{ JSON.stringify(sample, null, 2) }}</div>
    </div>
  </ModalShell>
</template>
