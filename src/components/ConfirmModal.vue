<script setup lang="ts">
/* 通用确认对话框 */
import { confirmState } from "../composables/ui";
import { icon } from "../composables/ui";
import ModalShell from "./ModalShell.vue";

function onOk() {
  confirmState.open = false;
  confirmState.onOk?.();
  confirmState.onOk = null;
}
</script>

<template>
  <ModalShell
    v-if="confirmState.open"
    :title="confirmState.title"
    icon-name="alert"
    @close="confirmState.open = false"
  >
    <template #default>
      <div style="line-height:1.7">{{ confirmState.msg }}</div>
    </template>
    <template #foot>
      <button class="btn" @click="confirmState.open = false">取消</button>
      <button
        class="btn"
        :class="confirmState.danger ? 'btn--danger' : 'btn--primary'"
        @click="onOk"
      >
        {{ confirmState.okText }}
      </button>
    </template>
  </ModalShell>
</template>
