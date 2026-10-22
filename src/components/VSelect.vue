<script setup lang="ts">
/* 自定义下拉选择器（对应 .select 样式体系） */
import { computed, onBeforeUnmount, onMounted, ref } from "vue";

export interface VSelectOption {
  value: string;
  label: string;
  sub?: string;
  muted?: boolean;
}

const props = defineProps<{
  modelValue: string;
  options: VSelectOption[];
  placeholder?: string;
  clearable?: boolean;
  width?: string;
}>();
const emit = defineEmits<{ (e: "update:modelValue", v: string): void }>();

const open = ref(false);
const root = ref<HTMLElement | null>(null);

const current = computed(() => props.options.find((o) => o.value === props.modelValue));
const label = computed(() => current.value?.label ?? "");
const empty = computed(() => !current.value);

function toggle() {
  open.value = !open.value;
}
function pick(v: string) {
  open.value = false;
  if (v !== props.modelValue) emit("update:modelValue", v);
}
function onDoc(e: MouseEvent) {
  if (root.value && !root.value.contains(e.target as Node)) open.value = false;
}
onMounted(() => document.addEventListener("mousedown", onDoc));
onBeforeUnmount(() => document.removeEventListener("mousedown", onDoc));
</script>

<template>
  <div ref="root" class="select" :class="{ 'is-open': open }" :style="width ? `min-width:${width}` : ''">
    <button type="button" @click="toggle">
      <span class="select__val" :class="{ 'is-empty': empty }">
        {{ empty ? placeholder || "请选择" : label }}
      </span>
      <svg viewBox="0 0 24 24" width="12" height="12" style="color:var(--text-3)"><use href="#i-down" /></svg>
    </button>
    <div class="select__menu">
      <div
        v-if="clearable"
        class="select__opt select__opt--muted"
        :class="{ 'is-sel': !modelValue }"
        @click="pick('')"
      >
        （不选 / 多表联查）
      </div>
      <div
        v-for="o in options"
        :key="o.value"
        class="select__opt"
        :class="{ 'is-sel': o.value === modelValue, 'select__opt--muted': o.muted }"
        @click="pick(o.value)"
      >
        <span>{{ o.label }}</span>
        <span v-if="o.sub" class="select__sub">{{ o.sub }}</span>
      </div>
      <div v-if="!options.length" class="select__opt select__opt--muted">暂无数据</div>
    </div>
  </div>
</template>
