<script setup lang="ts">
/* 右侧「模型配置栏」：多模型管理 / 唯一高亮选中 */
import { PROVIDERS, type ModelConfig } from "../types";
import {
  state,
  activeModel,
  activateModel,
  removeModel,
} from "../composables/store";
import { openModelForm } from "../composables/modals";
import { confirmDialog, copyText, icon, openCtxMenu } from "../composables/ui";

function providerName(p: ModelConfig["provider"]): string {
  return PROVIDERS[p]?.name ?? "自定义";
}

function setActive(id: string) {
  void activateModel(id);
}

function menu(m: ModelConfig, x: number, y: number) {
  openCtxMenu(x, y, [
    { header: true, label: m.name },
    { label: "设为使用中", icon: "check", disabled: m.active, onClick: () => setActive(m.id) },
    { label: "编辑配置", icon: "edit", onClick: () => openModelForm(m) },
    { label: "复制 ModelID", icon: "copy", onClick: () => copyText(m.modelId) },
    "sep",
    {
      label: "删除",
      icon: "trash",
      danger: true,
      onClick: () =>
        confirmDialog("删除模型配置", `确定删除「${m.name}」？`, () => removeModel(m.id)),
    },
  ]);
}
</script>

<template>
  <aside class="side side--right" id="sideRight" :class="{ 'is-collapsed': state.collapsed.right }"
    :style="!state.collapsed.right ? `width:${state.sizes.right}px` : ''">
    <div class="side__head">
      <span class="side__title" v-html="icon('spark', 14) + ' 模型配置'"></span>
      <div class="side__acts">
        <button class="iconbtn iconbtn--sm" title="新增模型" @click="openModelForm(null)"
          v-html="icon('plus', 14)"></button>
        <button class="iconbtn iconbtn--sm" title="折叠面板" @click="state.collapsed.right = true"
          v-html="icon('right', 14)"></button>
      </div>
    </div>
    <div class="side__body">
      <div class="side__note">
        当前使用中：<b>{{ activeModel?.name ?? "未选择" }}</b>
      </div>
      <div class="modellist">
        <div
          v-for="m in state.models"
          :key="m.id"
          class="mcard"
          :class="{ 'is-active': m.active }"
          @click="!m.active && setActive(m.id)"
          @contextmenu.prevent="menu(m, $event.clientX, $event.clientY)"
        >
          <span class="mcard__check" v-html="icon('check', 14)"></span>
          <div class="mcard__top">
            <span v-html="icon('spark', 13)"></span>
            <span class="mcard__name">{{ m.name }}</span>
            <span class="tag tag--ghost">{{ providerName(m.provider) }}</span>
          </div>
          <div class="mcard__meta">
            <span :title="'ModelID: ' + m.modelId">ModelID: {{ m.modelId }}</span>
            <span :title="m.baseUrl">{{ m.baseUrl || "—" }}</span>
          </div>
          <div class="mcard__acts">
            <button class="btn btn--sm" :class="{ 'btn--primary': m.active }" @click.stop="setActive(m.id)">
              {{ m.active ? "使用中" : "设为使用" }}
            </button>
            <button class="btn btn--sm btn--icon" title="编辑" @click.stop="openModelForm(m)"
              v-html="icon('edit', 12)"></button>
            <button class="btn btn--sm btn--icon btn--danger" title="删除"
              @click.stop="confirmDialog('删除模型配置', `确定删除「${m.name}」？`, () => removeModel(m.id))"
              v-html="icon('trash', 12)"></button>
          </div>
        </div>
        <div v-if="!state.models.length" class="sql-empty">
          还没有模型配置，
          点击右上角 + 新增
        </div>
      </div>
    </div>
    <div class="side__collapsed">
      <button class="iconbtn iconbtn--sm" title="展开模型栏" @click="state.collapsed.right = false"
        v-html="icon('left', 14)"></button>
      <span class="side__collapsed-text">模型配置</span>
    </div>
  </aside>
</template>
