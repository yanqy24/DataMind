<script setup lang="ts">
/* Toast 渲染器 */
import { toasts } from "../composables/ui";
import { icon } from "../composables/ui";
</script>

<template>
  <div id="toastRoot" class="toasts">
    <div
      v-for="t in toasts"
      :key="t.id"
      class="toast"
      :class="{ 'toast--ok': t.type === 'ok', 'toast--err': t.type === 'err' }"
    >
      <span
        v-html="
          icon(t.type === 'ok' ? 'check' : t.type === 'err' ? 'alert' : 'spark', 14)
        "
      ></span>
      <span>{{ t.msg }}</span>
    </div>
  </div>
</template>
