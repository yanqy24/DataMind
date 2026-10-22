<script setup lang="ts">
/* 查询窗口 Tab 切换栏 */
import { state, tabTitle, createTab, activateTab, closeTab, duplicateTab } from "../composables/store";
import { icon, openCtxMenu } from "../composables/ui";
import type { QueryTab } from "../types";

function ctx(t: QueryTab, x: number, y: number) {
  openCtxMenu(x, y, [
    { header: true, label: tabTitle(t) },
    {
      label: "重命名",
      icon: "edit",
      onClick: () => {
        const v = prompt("窗口名称", tabTitle(t));
        if (v !== null && v.trim()) {
          t.title = v.trim();
          state.tabs = [...state.tabs];
        }
      },
    },
    { label: "复制标签", icon: "copy", onClick: () => duplicateTab(t) },
    "sep",
    { label: "关闭", icon: "x", onClick: () => closeTab(t.id) },
    {
      label: "关闭其他",
      icon: "x",
      onClick: () => {
        state.tabs = [t];
        activateTab(t.id);
      },
    },
  ]);
}
</script>

<template>
  <div class="tabs" id="tabBar">
    <div
      v-for="t in state.tabs"
      :key="t.id"
      class="tab"
      :class="{ 'is-active': t.id === state.activeTab }"
      @click="activateTab(t.id)"
      @dblclick="ctx(t, $event.clientX, $event.clientY)"
      @contextmenu.prevent="ctx(t, $event.clientX, $event.clientY)"
    >
      <span class="tab__dot" :class="{ 'is-running': !!t.running }"></span>
      <span class="tab__name">{{ tabTitle(t) }}</span>
      <button class="tab__close" title="关闭" @click.stop="closeTab(t.id)" v-html="icon('x', 11)"></button>
    </div>
    <button class="tabs__add" title="新建查询窗口 (Ctrl+T)" @click="createTab()" v-html="icon('plus', 14)"></button>
  </div>
</template>
