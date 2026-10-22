<script setup lang="ts">
/* 左侧「数据库连接栏」：树形管理 / 右键菜单 / 记忆同步 */
import { computed, reactive, ref } from "vue";
import {
  state,
  connOf,
  removeConnection,
  syncConnection,
  createTab,
  previewTable,
  memoryOf,
} from "../composables/store";
import { openConnForm, openMemory, openTable } from "../composables/modals";
import { confirmDialog, icon, openCtxMenu, toast, copyText } from "../composables/ui";
import type { DatabaseMeta, TableMeta } from "../types";

const keyword = ref("");
/** 展开状态（运行时） */
const expanded = reactive<Record<string, boolean>>({});
const selected = ref<string | null>(null);

function match(text: string): boolean {
  return !keyword.value || text.toLowerCase().includes(keyword.value.toLowerCase());
}

interface TreeConn {
  connId: string;
  name: string;
  addr: string;
  badge: string;
  syncing: boolean;
  dbs: DatabaseMeta[];
}

/** 过滤后的树数据 */
const tree = computed<TreeConn[]>(() =>
  state.connections
    .map((c) => {
      const mem = memoryOf(c.id);
      const syncing = !!state.syncing[c.id];
      const dbs = (mem?.databases ?? []).filter(
        (d) => match(d.name) || d.tables.some((t) => match(t.name)),
      );
      const badge = syncing
        ? state.syncing[c.id]
        : mem
          ? `${mem.databases.reduce((s, d) => s + d.tables.length, 0)} 表`
          : "未同步";
      return {
        connId: c.id,
        name: c.name,
        addr: `${c.host}:${c.port}${c.ssh.enabled ? " · SSH" : ""}`,
        badge,
        syncing,
        dbs,
      };
    })
    .filter((t) => match(t.name) || t.dbs.length > 0),
);

function toggle(key: string) {
  expanded[key] = !expanded[key];
  selected.value = key;
}

function newQuery(connId: string, db: string, table: string) {
  const conn = connOf(connId);
  createTab({
    connId,
    db,
    table,
    title: (db || conn?.name || "查询") + (table ? " · " + table : ""),
  });
}

function connMenu(connId: string, x: number, y: number) {
  const conn = connOf(connId);
  if (!conn) return;
  const firstDb = memoryOf(connId)?.databases[0]?.name ?? "";
  openCtxMenu(x, y, [
    { header: true, label: conn.name },
    { label: "新建查询窗口", icon: "terminal", onClick: () => newQuery(connId, firstDb, "") },
    { label: "刷新结构记忆", icon: "refresh", onClick: () => void syncConnection(connId) },
    "sep",
    { label: "编辑连接", icon: "edit", onClick: () => openConnForm(conn) },
    { label: "复制连接信息", icon: "copy", onClick: () => copyText(`${conn.type}://${conn.user}@${conn.host}:${conn.port}`) },
    "sep",
    {
      label: "删除连接",
      icon: "trash",
      danger: true,
      onClick: () =>
        confirmDialog("删除连接", `确定删除「${conn.name}」？本地记忆会一并清除。`, () =>
          removeConnection(connId),
        ),
    },
  ]);
}

function dbMenu(connId: string, d: DatabaseMeta, x: number, y: number) {
  openCtxMenu(x, y, [
    { header: true, label: d.name + (d.comment ? " · " + d.comment : "") },
    { label: "新建该库查询窗口", icon: "terminal", onClick: () => newQuery(connId, d.name, "") },
    { label: "查看结构记忆", icon: "brain", onClick: () => openMemory(connId, d.name) },
    { label: "刷新该库结构", icon: "refresh", onClick: () => void syncConnection(connId) },
    "sep",
    { label: "复制库名", icon: "copy", onClick: () => copyText(d.name) },
  ]);
}

function tableMenu(connId: string, d: DatabaseMeta, t: TableMeta, x: number, y: number) {
  openCtxMenu(x, y, [
    { header: true, label: `${d.name}.${t.name}` },
    { label: "新建该表查询窗口", icon: "terminal", onClick: () => newQuery(connId, d.name, t.name) },
    { label: "预览前 100 条数据", icon: "play", onClick: () => void previewTable(connId, d.name, t.name) },
    { label: "查看建表语句 / 取样", icon: "edit", onClick: () => openTable(connId, d.name, t) },
    "sep",
    { label: "复制表名", icon: "copy", onClick: () => copyText(t.name) },
    { label: "复制 SELECT 模板", icon: "copy", onClick: () => copyText(`SELECT * FROM \`${t.name}\` LIMIT 100;`) },
  ]);
}

function refreshAll() {
  state.connections.forEach((c) => void syncConnection(c.id, true));
  toast("正在同步全部连接的结构记忆…");
}

function rowsBadge(n: number): string {
  if (n >= 100000000) return (n / 100000000).toFixed(1) + "亿";
  if (n >= 10000) return (n / 10000).toFixed(1) + "w";
  return String(n);
}
</script>

<template>
  <aside class="side side--left" id="sideLeft" :class="{ 'is-collapsed': state.collapsed.left }"
    :style="!state.collapsed.left ? `width:${state.sizes.left}px` : ''">
    <div class="side__head">
      <span class="side__title" v-html="icon('database', 14) + ' 数据库连接'"></span>
      <div class="side__acts">
        <button class="iconbtn iconbtn--sm" title="新建连接" @click="openConnForm(null)"
          v-html="icon('plus', 14)"></button>
        <button class="iconbtn iconbtn--sm" title="刷新全部" @click="refreshAll"
          v-html="icon('refresh', 14)"></button>
        <button class="iconbtn iconbtn--sm" title="折叠面板" @click="state.collapsed.left = true"
          v-html="icon('left', 14)"></button>
      </div>
    </div>
    <div class="side__body">
      <div class="side__search">
        <svg viewBox="0 0 24 24" width="14" height="14"><use href="#i-search" /></svg>
        <input v-model="keyword" type="text" placeholder="搜索连接 / 库 / 表" />
      </div>
      <div class="tree">
        <div class="tree__group-title">数据库连接</div>

        <template v-for="tc in tree" :key="tc.connId">
          <div
            class="tnode"
            :class="{ 'is-sel': selected === tc.connId }"
            @click="toggle(tc.connId)"
            @contextmenu.prevent="connMenu(tc.connId, $event.clientX, $event.clientY)"
          >
            <span class="tnode__caret" :class="{ 'is-open': expanded[tc.connId] }"
              v-html="icon('right', 12)"></span>
            <span class="tnode__icon" v-html="icon('database', 13)"></span>
            <span class="tnode__name" :title="tc.addr">{{ tc.name }}</span>
            <span class="tnode__badge">{{ tc.badge }}</span>
            <span class="tnode__acts" v-if="!tc.syncing">
              <button class="iconbtn iconbtn--sm" title="刷新结构记忆"
                @click.stop="syncConnection(tc.connId)" v-html="icon('refresh', 12)"></button>
              <button class="iconbtn iconbtn--sm" title="编辑连接"
                @click.stop="openConnForm(connOf(tc.connId)!)" v-html="icon('edit', 12)"></button>
              <button class="iconbtn iconbtn--sm" title="更多"
                @click.stop="connMenu(tc.connId, $event.clientX, $event.clientY)" v-html="icon('more', 12)"></button>
            </span>
          </div>

          <div v-if="expanded[tc.connId]" class="tnode__children">
            <div v-if="!tc.dbs.length" class="tree__group-title"
              style="text-transform:none;letter-spacing:0;padding-top:4px">
              {{ tc.syncing ? "正在读取结构…" : "尚未同步，点击 ↻ 刷新" }}
            </div>
            <template v-for="d in tc.dbs" :key="d.name">
              <div
                class="tnode"
                :class="{ 'is-sel': selected === tc.connId + '/' + d.name }"
                @click="toggle(tc.connId + '/' + d.name)"
                @contextmenu.prevent="dbMenu(tc.connId, d, $event.clientX, $event.clientY)"
              >
                <span class="tnode__caret" :class="{ 'is-open': expanded[tc.connId + '/' + d.name] }"
                  v-html="icon('right', 12)"></span>
                <span class="tnode__icon" v-html="icon('layers', 13)"></span>
                <span class="tnode__name" :title="d.comment">{{ d.name }}</span>
                <span class="tnode__badge">{{ d.tables.length }}</span>
                <span class="tnode__acts">
                  <button class="iconbtn iconbtn--sm" title="新建该库查询"
                    @click.stop="newQuery(tc.connId, d.name, '')" v-html="icon('terminal', 12)"></button>
                </span>
              </div>
              <div v-if="expanded[tc.connId + '/' + d.name]" class="tnode__children">
                <div
                  v-for="t in d.tables.filter(t => !keyword || match(t.name))"
                  :key="t.name"
                  class="tnode"
                  :class="{ 'is-sel': selected === tc.connId + '/' + d.name + '/' + t.name }"
                  @click="selected = tc.connId + '/' + d.name + '/' + t.name"
                  @dblclick="void previewTable(tc.connId, d.name, t.name)"
                  @contextmenu.prevent="tableMenu(tc.connId, d, t, $event.clientX, $event.clientY)"
                  title="双击：新建该表查询窗口并预览前 100 条"
                >
                  <span class="tnode__caret"></span>
                  <span class="tnode__icon" v-html="icon('table', 13)"></span>
                  <span class="tnode__name" :title="t.comment">{{ t.name }}</span>
                  <span class="tnode__badge">{{ rowsBadge(t.rows) }}</span>
                  <span class="tnode__acts">
                    <button class="iconbtn iconbtn--sm" title="新建该表查询"
                      @click.stop="newQuery(tc.connId, d.name, t.name)" v-html="icon('terminal', 12)"></button>
                  </span>
                </div>
              </div>
            </template>
          </div>
        </template>

        <div v-if="!tree.length" class="tree__group-title" style="text-transform:none;letter-spacing:0">
          {{ keyword ? "没有匹配结果" : "还没有连接，点击 ↗ 新建" }}
        </div>
      </div>
    </div>
    <div class="side__collapsed">
      <button class="iconbtn iconbtn--sm" title="展开连接栏" @click="state.collapsed.left = false"
        v-html="icon('right', 14)"></button>
      <span class="side__collapsed-text">数据库连接</span>
    </div>
  </aside>
</template>
