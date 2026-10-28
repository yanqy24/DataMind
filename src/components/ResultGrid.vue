<script setup lang="ts">
/* 数据展示栏表格：多选 / 列宽拖拽 / 右键复制（文本 & Markdown） / 大结果集分页 */
import { computed, reactive, ref, watch } from "vue";
import type { QueryResult } from "../types";
import { copyText, openCtxMenu, icon } from "../composables/ui";

const props = defineProps<{
  result: QueryResult | null;
  /** 服务端分页状态（null = 当前结果未分页，如用户 SQL 自带 LIMIT） */
  paging: { page: number; pageSize: number; total: number | null } | null;
  /** 正在翻页查询中（表格保留上一页内容，仅置灰） */
  loading?: boolean;
}>();

const emit = defineEmits<{
  /** 跳转到指定页（父组件会用同一条 SQL 重新查询该页） */
  goto: [page: number];
  /** 修改单页行数（回到第 1 页重新查询） */
  size: [size: number];
}>();

type Sel = Record<string, true>;
const sel = reactive<Sel>({});
const anchor = ref<{ r: number; c: number } | null>(null);
let dragging = false;
/** 每列显式宽度（索引 -> px），未设置的列自适应 */
const widths = reactive<Record<number, number>>({});
const tableEl = ref<HTMLTableElement | null>(null);

/* ---------- 服务端分页：本组件只负责展示当前页与翻页交互 ---------- */
const PAGE_SIZES = [100, 200, 500, 1000] as const;

const page = computed(() => props.paging?.page ?? 1);
const pageSize = computed(() => props.paging?.pageSize ?? 1000);
const total = computed(() => props.paging?.total ?? null);
const rowCount = computed(() => props.result?.rows.length ?? 0);
/** 总页数；总数未知时按「当前页是否满页」推算是否还有下一页 */
const pageCount = computed(() =>
  total.value != null ? Math.max(1, Math.ceil(total.value / pageSize.value)) : null,
);
/** 无总数时：本页满页即认为可能还有下一页 */
const maybeMore = computed(() => total.value == null && rowCount.value >= pageSize.value);
/** 需要分页才显示分页条：总数超过一页，或总数未知但本页满页 */
const showPager = computed(
  () => !!props.paging && (total.value == null ? maybeMore.value : total.value > pageSize.value),
);
const start = computed(() => (page.value - 1) * pageSize.value);
const pageFrom = computed(() => (rowCount.value === 0 ? 0 : start.value + 1));
const pageTo = computed(() => start.value + rowCount.value);
/** 当前页行：`gi` 为页内序号（服务端分页，每次只有本页数据） */
const pageRows = computed(() => props.result?.rows.map((row, gi) => ({ gi, row })) ?? []);

/** 下一页是否可点 */
const canNext = computed(() =>
  pageCount.value != null ? page.value < pageCount.value : maybeMore.value,
);

function go(p: number) {
  const max = pageCount.value ?? Number.MAX_SAFE_INTEGER;
  const target = Math.min(Math.max(1, Math.trunc(p) || 1), max);
  if (target !== page.value && !props.loading) emit("goto", target);
}
/** 千分位，便于阅读大结果集行数 */
const fmt = (n: number): string => n.toLocaleString("en-US");

watch(
  () => props.result,
  () => {
    clearSel();
    for (const k of Object.keys(widths)) delete (widths as Record<string, number>)[k];
  },
);

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
      label: "全选本页",
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
    <div class="grid-wrap" :class="{ 'is-loading': loading }" @keydown="onKeyCopy">
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
            <td class="rownum">{{ start + item.gi + 1 }}</td>
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

    <!-- 分页（右下角）：点页码会用同一条 SQL 重新查询该页（服务端分页） -->
    <div v-if="showPager" class="grid-pager">
      <span v-if="loading" class="grid-pager__loading">正在查询…</span>
      <span class="grid-pager__info">
        第 {{ fmt(pageFrom) }}-{{ fmt(pageTo) }} 条<template v-if="total != null"> / 共 {{ fmt(total) }} 条</template>
      </span>
      <select class="grid-pager__size" :value="pageSize" :disabled="loading"
        title="每页行数（修改后回到第 1 页重新查询，最大 1000）"
        @change="emit('size', Number(($event.target as HTMLSelectElement).value))">
        <option v-for="n in PAGE_SIZES" :key="n" :value="n">{{ n }} 条/页</option>
      </select>
      <button class="pbtn pbtn--edge" :disabled="page <= 1 || loading" title="首页" @click="go(1)">«</button>
      <button class="pbtn" :disabled="page <= 1 || loading" title="上一页" @click="go(page - 1)" v-html="icon('left', 12)"></button>
      <span class="grid-pager__page">
        <input class="grid-pager__input" type="number" min="1" :max="pageCount ?? page" :value="page" :disabled="loading"
          @keydown.enter.prevent="go(Number(($event.target as HTMLInputElement).value))" />
        <template v-if="pageCount != null"> / {{ fmt(pageCount) }}</template>
      </span>
      <button class="pbtn" :disabled="!canNext || loading" title="下一页（重新执行 SQL 取该页）" @click="go(page + 1)" v-html="icon('right', 12)"></button>
      <button v-if="pageCount != null" class="pbtn pbtn--edge" :disabled="page >= pageCount || loading" title="末页"
        @click="go(pageCount)">»</button>
    </div>
  </div>
</template>
