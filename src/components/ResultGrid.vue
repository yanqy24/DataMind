<script setup lang="ts">
/* 数据展示栏表格：多选 / 列宽拖拽 / 右键复制（文本 & Markdown） / 大结果集分页 */
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

/* ---------- 分页（结果集超过单页行数时出现，单页上限 1000 行） ---------- */
/** 可选单页行数，最大 1000 */
const PAGE_SIZES = [100, 200, 500, 1000] as const;
const pageSize = ref<number>(1000);
const page = ref(1);

const total = computed(() => props.result?.rows.length ?? 0);
const pageCount = computed(() => Math.max(1, Math.ceil(total.value / pageSize.value)));
const start = computed(() => (page.value - 1) * pageSize.value);
/** 结果集超过一页时才展示分页条 */
const showPager = computed(() => total.value > pageSize.value);
/** 当前页行：`gi` 为全局行号（跨页连续，行号与选中态都以它为准） */
const pageRows = computed(() => {
  const res = props.result;
  if (!res) return [] as { gi: number; row: (string | null)[] }[];
  const s = start.value;
  const out: { gi: number; row: (string | null)[] }[] = [];
  const end = Math.min(s + pageSize.value, res.rows.length);
  for (let i = s; i < end; i++) out.push({ gi: i, row: res.rows[i] });
  return out;
});
const pageFrom = computed(() => (total.value === 0 ? 0 : start.value + 1));
const pageTo = computed(() => Math.min(start.value + pageSize.value, total.value));

function go(p: number) {
  page.value = Math.min(Math.max(1, Math.trunc(p) || 1), pageCount.value);
}
function setPageSize(n: number) {
  const first = start.value; // 保持当前页首行大致可见
  pageSize.value = n;
  page.value = Math.min(Math.max(1, Math.floor(first / n) + 1), Math.max(1, Math.ceil(total.value / n)));
}
/** 千分位，便于阅读大结果集行数 */
const fmt = (n: number): string => n.toLocaleString("en-US");

watch(
  () => props.result,
  () => {
    clearSel();
    page.value = 1;
    for (const k of Object.keys(widths)) delete (widths as Record<string, number>)[k];
  },
);
// 结果集变化（行数变少）时把页码夹回合法范围
watch(pageCount, (n) => {
  if (page.value > n) page.value = n;
});

const key = (r: number, c: number) => `${r}:${c}`;

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
      label: showPager.value ? "全选本页" : "全选结果",
      icon: "check",
      onClick: () => {
        const res = props.result;
        if (!res) return;
        clearSel();
        pageRows.value.forEach(({ gi }) =>
          res.columns.forEach((_, c) => (sel[key(gi, c)] = true)),
        );
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
  <div class="grid-pane">
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
          <tr v-for="item in pageRows" :key="item.gi">
            <td class="rownum">{{ item.gi + 1 }}</td>
            <td
              v-for="(c, ci) in result.columns"
              :key="ci"
              :class="{
                num: isNum(item.row[ci]),
                null: item.row[ci] === null || item.row[ci] === '',
                'is-sel': !!sel[`${item.gi}:${ci}`],
              }"
              :data-r="item.gi"
              :data-c="ci"
              :title="item.row[ci] === null ? 'NULL' : String(item.row[ci] ?? '')"
              @mousedown="onCellDown($event, item.gi, ci)"
              @mousemove="onCellMove($event, item.gi, ci)"
              @click="onCellClick($event, item.gi, ci)"
            >
              {{ item.row[ci] === null || item.row[ci] === undefined || item.row[ci] === "" ? "NULL" : item.row[ci] }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- 分页：结果集超过单页行数时出现（数据展示栏右下角） -->
    <div v-if="showPager" class="grid-pager">
      <span class="grid-pager__info">
        第 {{ fmt(pageFrom) }}-{{ fmt(pageTo) }} 条 / 共 {{ fmt(total) }} 条
      </span>
      <select class="grid-pager__size" :value="pageSize" title="每页行数（最大 1000）"
        @change="setPageSize(Number(($event.target as HTMLSelectElement).value))">
        <option v-for="n in PAGE_SIZES" :key="n" :value="n">{{ n }} 条/页</option>
      </select>
      <button class="pbtn pbtn--edge" :disabled="page <= 1" title="首页" @click="go(1)">«</button>
      <button class="pbtn" :disabled="page <= 1" title="上一页" @click="go(page - 1)" v-html="icon('left', 12)"></button>
      <span class="grid-pager__page">
        <input class="grid-pager__input" type="number" min="1" :max="pageCount" :value="page"
          @keydown.enter.prevent="go(Number(($event.target as HTMLInputElement).value))"
          @blur="go(Number(($event.target as HTMLInputElement).value))" />
        / {{ fmt(pageCount) }}
      </span>
      <button class="pbtn" :disabled="page >= pageCount" title="下一页" @click="go(page + 1)" v-html="icon('right', 12)"></button>
      <button class="pbtn pbtn--edge" :disabled="page >= pageCount" title="末页" @click="go(pageCount)">»</button>
    </div>
  </div>
</template>
