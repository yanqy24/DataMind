<script setup lang="ts">
/* 本地记忆查看弹窗：DDL / 表关系 一览 */
import { computed } from "vue";
import { memoryState } from "../composables/modals";
import { connOf, memoryOf } from "../composables/store";
import { icon } from "../composables/ui";
import ModalShell from "./ModalShell.vue";

const conn = computed(() => connOf(memoryState.connId));
const mem = computed(() => memoryOf(memoryState.connId));
const dbs = computed(() =>
  memoryState.db
    ? (mem.value?.databases ?? []).filter((d) => d.name === memoryState.db)
    : mem.value?.databases ?? [],
);

function memoryText(d: { relations: { from: string; to: string; desc: string }[]; tables: { ddl: string }[] }): string {
  let txt = "";
  d.relations.forEach((r) => (txt += `-- 关系: ${r.from} -> ${r.to} (${r.desc})\n`));
  txt += "\n";
  d.tables.forEach((t) => (txt += t.ddl + "\n\n"));
  return txt.trim();
}
</script>

<template>
  <ModalShell v-if="memoryState.open"
    :title="`本地记忆 · ${conn?.name ?? ''}${memoryState.db ? '/' + memoryState.db : ''}`"
    icon-name="brain" size="lg" @close="memoryState.open = false">
    <div class="memtree">
      <div v-if="!dbs.length" class="sql-empty">尚未同步结构记忆，请先在连接树上点击「刷新结构记忆」</div>
      <div v-for="d in dbs" :key="d.name" class="memcard">
        <div class="memcard__head">
          <span v-html="icon('layers', 13)"></span>
          <span>{{ d.name }}</span>
          <span class="tag tag--ghost" style="margin-left:auto">{{ d.tables.length }} 表</span>
        </div>
        <div class="memcard__body">{{ memoryText(d) }}</div>
      </div>
    </div>
  </ModalShell>
</template>
