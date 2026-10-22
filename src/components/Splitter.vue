<script setup lang="ts">
/* 通用拖拽分割条：轴 x/y，v-model 代理尺寸值（px） */
import { onBeforeUnmount, onMounted, ref } from "vue";

const props = defineProps<{
  axis: "x" | "y";
  /** 反向拖拽（右栏：向左拖数值变大） */
  reverse?: boolean;
  min?: number;
  maxFn?: () => number;
}>();

const value = defineModel<number>({ required: true });
const emit = defineEmits<{ (e: "change"): void }>();

const bar = ref<HTMLElement | null>(null);
let dragging = false;
let start = 0;
let base = 0;

function onMove(e: MouseEvent) {
  if (!dragging) return;
  const cur = props.axis === "x" ? e.clientX : e.clientY;
  const delta = props.reverse ? start - cur : cur - start;
  const min = props.min ?? 160;
  const max = props.maxFn ? props.maxFn() : 100000;
  value.value = Math.round(Math.min(max, Math.max(min, base + delta)));
}
function onUp() {
  if (!dragging) return;
  dragging = false;
  bar.value?.classList.remove("is-active");
  document.body.classList.remove("is-resizing", "is-resizing-v");
  document.removeEventListener("mousemove", onMove);
  document.removeEventListener("mouseup", onUp);
  emit("change");
}
function onDown(e: MouseEvent) {
  e.preventDefault();
  dragging = true;
  start = props.axis === "x" ? e.clientX : e.clientY;
  base = value.value;
  bar.value?.classList.add("is-active");
  document.body.classList.add(props.axis === "x" ? "is-resizing" : "is-resizing-v");
  document.addEventListener("mousemove", onMove);
  document.addEventListener("mouseup", onUp);
}
onMounted(() => bar.value?.addEventListener("mousedown", onDown));
onBeforeUnmount(onUp);
</script>

<template>
  <div ref="bar" class="splitter" :class="axis === 'x' ? 'splitter--v' : 'splitter--h'"></div>
</template>
