<script setup lang="ts">
/* 通用模态窗骨架（对应 .modal 样式体系） */
import { onBeforeUnmount, onMounted } from "vue";
import { icon } from "../composables/ui";

const props = withDefaults(
  defineProps<{
    title: string;
    iconName?: string;
    size?: "md" | "lg";
    maskClose?: boolean;
  }>(),
  { iconName: "", size: "md", maskClose: true },
);
const emit = defineEmits<{ (e: "close"): void }>();

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") close();
}
function close() {
  emit("close");
}
onMounted(() => document.addEventListener("keydown", onKey));
onBeforeUnmount(() => document.removeEventListener("keydown", onKey));
</script>

<template>
  <div class="modal-mask" @mousedown="e => { if (maskClose && e.target === e.currentTarget) close(); }">
    <div class="modal" :class="{ 'modal--lg': size === 'lg' }">
      <div class="modal__head">
        <span v-if="iconName" v-html="icon(iconName, 16)"></span>
        <span>{{ title }}</span>
        <div class="spacer"></div>
        <button class="iconbtn" v-html="icon('x', 16)" @click="close"></button>
      </div>
      <div class="modal__body">
        <slot />
      </div>
      <div v-if="$slots.foot" class="modal__foot">
        <slot name="foot" />
      </div>
    </div>
  </div>
</template>
