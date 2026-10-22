<script setup lang="ts">
/* 数据展示栏表格：多选 / 列宽拖拽 / 右键复制（文本 & Markdown） */
import { computed, reactive, ref, watch } from "vue";
import type { QueryResult } from "../types";
import { copyText, openCtxMenu, icon } from "../composables/ui";

const props = defineProps<{ result: QueryResult | null }>();

type Sel = Record<string, true>;
const sel = reactive<Sel>({});
const anchor = ref<{ r: number; c: number } | null>(null);
let dragging = false;
/** 每列显式宽度（索引 -> px），未设置的列自适应 */
const widths = reactive<Record<number, number>>({});
const tableEl = ref<HTMLTableElement | null>(null);

const key = (r: number, c: number) => `${r}:${c}`;

watch(
  () => props.result,
  () => {
    clearSel();
    for (const k of Object.keys(widths)) delete (widths as Record<string, number>)[k];
  },
);

function clearSel() {
  for (const k of Object.keys(sel)) delete sel[k];
}
function paint() {
  /* 选中态由模板响应式渲染，无需手动 paint */
}
function setRange(r1: number, c1: number, r2: number, c2: number) {
  clearSel();
  const ra = Math.min(r1, r2), rb = Math.max(r1, r2);
  const ca = Math.min(c1, c2), cb = Math.max(c1, c2);
  for (let r = ra; r <= rb; r++) for (let c = ca; c <= cb; c++) sel[key(r, c)] = true;
  paint();
}

function onCellDown(e: MouseEvent, r: number, c: number) {
  const td = (e.target as HTMLElement).closest("td[data-r]");
  if (!td) return;
  if (e.button === 2) {
    if (!sel[key(r, c)]) {
      clearSel();
      sel[key(r, c)] = true;
    }
    return;
  }
  e.preventDefault();
  if (anchor.value) {
    anchor.value.r = r;
    anchor.value.c = c;
  } else {
    anchor.value = { r, c };
  }
  if (e.ctrlKey || e.metaKey) {
    sel[key(r, c)] = true;
    dragging = false;
  } else {
    clearSel();
    sel[key(r, c)] = true;
    dragging = true;
  }
}
function onCellMove(e: MouseEvent, r: number, c: number) {
  if (!dragging) return;
  setRange(anchor.value?.r ?? r, anchor.value?.c ?? c, r, c);
}
function onMouseUp() {
  dragging = false;
}
function onCellClick(e: MouseEvent, r: number, c: number) {
  if (e.shiftKey) setRange(anchor.value?.r ?? r, anchor.value?.c ?? c, r, c);
}

/** 选中单元格的行列矩阵（去重、升序） */
function selectedMatrix(): { rows: number[]; cols: number[] } | null {
  const keys = Object.keys(sel);
  if (!keys.length || !props.result) return null;
  const rs = new Set<number>();
  const cs = new Set<number>();
  keys.forEach((k) => {
    const [r, c] = k.split(":").map(Number);
    rs.add(r);
    cs.add(c);
  });
  return { rows: [...rs].sort((a, b) => a - b), cols: [...cs].sort((a, b) => a - b) };
}

function cellText(r: number, c: number): string {
  const res = props.result;
  if (!res) return "";
  const v = res.rows[r]?.[c];
  return v === null || v === undefined ? "" : String(v);
}

function buildTSV(): string {
  const mt = selectedMatrix();
  if (!mt) return "";
  return mt.rows.map((r) => mt.cols.map((c) => cellText(r, c)).join("\t")).join("\n");
}
function buildMarkdown(): string {
  const mt = selectedMatrix();
  const res = props.result;
  if (!mt || !res) return "";
  const head = mt.cols.map((c) => res.columns[c].name);
  const lines = [
    `| ${head.join(" | ")} |`,
    `| ${head.map(() => "---").join(" | ")} |`,
  ];
  mt.rows.forEach((r) => {
    lines.push(`| ${mt.cols.map((c) => cellText(r, c).replace(/\|/g, "\\|")).join(" | ")} |`);
  });
  return lines.join("\n");
}

function onCtx(e: MouseEvent, r: number, c: number) {
  if (!(e.target as HTMLElement).closest("td[data-r]")) return;
  e.preventDefault();
  const n = Object.keys(sel).length;
  openCtxMenu(e.clientX, e.clientY, [
    { header: true, label: `已选中 ${n} 个单元格` },
    {
      label: "复制",
      icon: "copy",
      short: "Ctrl+C",
      onClick: () => copyText(buildTSV(), `已复制 ${n} 个单元格`),
    },
    { label: "复制为 Markdown 表格", icon: "copy", onClick: () => copyText(buildMarkdown(), "已复制 Markdown 表格") },
    "sep",
    {
      label: "复制列名",
      icon: "copy",
      onClick: () => {
        const mt = selectedMatrix();
        const res = props.result;
        if (mt && res) copyText(mt.cols.map((c) => res.columns[c].name).join("\t"));
      },
    },
    {
      label: "全选结果",
      icon: "check",
      onClick: () => {
        const res = props.result;
        if (!res) return;
        clearSel();
        res.rows.forEach((_, r) => res.columns.forEach((_, c) => (sel[key(r, c)] = true)));
      },
    },
    { label: "取消选中", icon: "x", onClick: clearSel },
  ]);
}

function onKeyCopy(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && e.key === "c" && Object.keys(sel).length) {
    e.preventDefault();
    copyText(buildTSV(), `已复制 ${Object.keys(sel).length} 个单元格`);
  }
}

/* 列宽拖拽 */
function startResize(e: MouseEvent, ci: number) {
  e.preventDefault();
  e.stopPropagation();
  const startX = e.clientX;
  const th = (e.target as HTMLElement).closest("th") as HTMLElement;
  const startW = th.getBoundingClientRect().width;
  function move(ev: MouseEvent) {
    widths[ci] = Math.max(56, Math.round(startW + (ev.clientX - startX)));
  }
  function up() {
    document.removeEventListener("mousemove", move);
    document.removeEventListener("mouseup", up);
    document.body.classList.remove("is-resizing");
  }
  document.body.classList.add("is-resizing");
  document.addEventListener("mousemove", move);
  document.addEventListener("mouseup", up);
}
function autoFit(ci: number) {
  delete widths[ci];
}
function anyFixed(): boolean {
  return Object.keys(widths).length > 0;
}

const isNum = (v: string | null): boolean =>
  v !== null && v !== "" && /^-?\d+(\.\d+)?$/.test(v);
</script>

<template>
  <div class="grid-wrap" @keydown="onKeyCopy">
    <div v-if="!result || !result.rows.length" class="result-empty">
      {{ result ? "查询无结果" : "执行 SQL 后在此展示结果集" }}
    </div>
    <table
      v-else
      ref="tableEl"
      class="dgrid"
      :class="{ 'is-fixed': anyFixed() }"
      @mouseup="onMouseUp"
      @contextmenu="onCtx($event, 0, 0)"
    >
      <thead>
        <tr>
          <th class="rownum">#</th>
          <th v-for="(c, ci) in result.columns" :key="ci" :style="widths[ci] ? `width:${widths[ci]}px` : ''">
            <div class="th-in">
              <span class="th-name" :title="c.name">{{ c.name }}</span>
              <span v-if="c.type" class="th-type">{{ c.type }}</span>
              <span
                class="th-resize"
                title="拖动调整列宽，双击自适应"
                @mousedown="startResize($event, ci)"
                @dblclick.stop="autoFit(ci)"
              ></span>
            </div>
          </th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="(row, ri) in result.rows" :key="ri">
          <td class="rownum">{{ ri + 1 }}</td>
          <td
            v-for="(c, ci) in result.columns"
            :key="ci"
            :class="{
              num: isNum(row[ci]),
              null: row[ci] === null || row[ci] === '',
              'is-sel': !!sel[`${ri}:${ci}`],
            }"
            :data-r="ri"
            :data-c="ci"
            :title="row[ci] === null ? 'NULL' : String(row[ci] ?? '')"
            @mousedown="onCellDown($event, ri, ci)"
            @mousemove="onCellMove($event, ri, ci)"
            @click="onCellClick($event, ri, ci)"
          >
            {{ row[ci] === null || row[ci] === undefined || row[ci] === "" ? "NULL" : row[ci] }}
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
