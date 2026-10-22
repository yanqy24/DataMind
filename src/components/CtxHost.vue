<script setup lang="ts">
/* 右键菜单渲染器 */
import { computed, onBeforeUnmount, onMounted } from "vue";
import { closeCtxMenu, ctxMenuState, icon, type CtxItem } from "../composables/ui";

const left = computed(() => Math.min(ctxMenuState.x, window.innerWidth - 210));
const top = computed(() => Math.min(ctxMenuState.y, window.innerHeight - 320));

function onDoc(e: MouseEvent) {
  if (!ctxMenuState.open) return;
  const el = e.target as HTMLElement;
  if (!el.closest(".ctx")) closeCtxMenu();
}
function onBlur() {
  closeCtxMenu();
}
onMounted(() => {
  document.addEventListener("mousedown", onDoc);
  window.addEventListener("blur", onBlur);
});
onBeforeUnmount(() => {
  document.removeEventListener("mousedown", onDoc);
  window.removeEventListener("blur", onBlur);
});

function clickItem(it: CtxItem) {
  if (typeof it === "string") return;
  if (it.header || it.disabled) return;
  closeCtxMenu();
  it.onClick?.();
}
</script>

<template>
  <div
    v-if="ctxMenuState.open"
    class="ctx"
    :style="{ left: left + 'px', top: top + 'px' }"
  >
    <template v-for="(it, i) in ctxMenuState.items" :key="i">
      <div v-if="it === 'sep'" class="ctx__sep"></div>
      <div v-else-if="it.header" class="ctx__label">{{ it.label }}</div>
      <div
        v-else
        class="ctx__item"
        :class="{ 'ctx__item--danger': it.danger, 'is-disabled': it.disabled }"
        @click="clickItem(it)"
      >
        <span v-if="it.icon" v-html="icon(it.icon, 14)"></span>
        <span v-else style="width:14px"></span>
        <span>{{ it.label }}</span>
        <span v-if="it.short" class="ctx__short">{{ it.short }}</span>
      </div>
    </template>
  </div>
</template>
