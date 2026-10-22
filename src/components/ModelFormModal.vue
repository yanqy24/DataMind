<script setup lang="ts">
/* 模型配置表单：供应商 / APIKey / ModelID / 自定义 BaseURL */
import { computed, reactive, ref, watch } from "vue";
import { modelFormState } from "../composables/modals";
import { newModelDraft, saveModel } from "../composables/store";
import { icon, toast } from "../composables/ui";
import { PROVIDERS, type ModelConfig, type Provider } from "../types";
import ModalShell from "./ModalShell.vue";
import VSelect from "./VSelect.vue";

const isNew = computed(() => !modelFormState.editing);
const draft = reactive<ModelConfig>(JSON.parse(JSON.stringify(modelFormState.editing ?? newModelDraft())));
const apikeyInput = ref(draft.apiKey);

watch(
  () => modelFormState.open,
  (open) => {
    if (open) {
      Object.assign(draft, JSON.parse(JSON.stringify(modelFormState.editing ?? newModelDraft())));
      apikeyInput.value = draft.apiKey;
    }
  },
);

function onProviderChange(v: string) {
  // 仅当当前 ID 为空、或属于上一个供应商的预设值时才跟随切换覆盖，避免清掉用户手填的 ID
  const prevPresets = PROVIDERS[draft.provider]?.models ?? [];
  const autoFill = !draft.modelId.trim() || prevPresets.includes(draft.modelId.trim());
  draft.provider = v as Provider;
  if (v !== "custom") {
    draft.baseUrl = PROVIDERS[v as Provider].baseUrl;
    if (autoFill) draft.modelId = PROVIDERS[v as Provider].models[0] ?? draft.modelId;
  } else {
    draft.baseUrl = "";
    if (autoFill) draft.modelId = "";
  }
}

function close() {
  modelFormState.open = false;
}

function onSubmit() {
  draft.apiKey = apikeyInput.value.trim();
  if (!draft.apiKey) {
    toast("请填写 API Key", "err");
    return;
  }
  if (!draft.modelId.trim()) {
    toast("请填写 Model ID", "err");
    return;
  }
  if (!draft.baseUrl.trim()) {
    toast("请填写 Base URL", "err");
    return;
  }
  draft.modelId = draft.modelId.trim();
  if (!draft.name.trim()) draft.name = PROVIDERS[draft.provider]?.name ?? "自定义模型";
  void saveModel({ ...draft }, isNew.value);
  close();
}
</script>

<template>
  <ModalShell v-if="modelFormState.open" :title="isNew ? '新增模型配置' : '编辑模型配置'"
    icon-name="spark" @close="close">
    <div style="display:flex;flex-direction:column;gap:12px">
      <div class="field">
        <label>配置名称</label>
        <input v-model="draft.name" class="input" placeholder="例如：DeepSeek-V3" />
      </div>
      <div class="field">
        <label>模型供应商</label>
        <VSelect :model-value="draft.provider" :options="[
          { value: 'deepseek', label: 'DeepSeek' },
          { value: 'zhipu', label: '智谱 AI' },
          { value: 'custom', label: '自定义（OpenAI 兼容）' },
        ]" width="260px" @update:model-value="onProviderChange" />
      </div>
      <div class="field">
        <label>Base URL</label>
        <input v-model="draft.baseUrl" class="input input--mono"
          :placeholder="draft.provider === 'custom' ? 'http://your-host:8000/v1' : 'https://api.deepseek.com'" />
        <span class="field__hint">选择内置供应商时自动填充，自定义模型可自行输入</span>
      </div>
      <div class="field">
        <label>API Key</label>
        <input v-model="apikeyInput" class="input input--mono" type="password" placeholder="sk-..." />
      </div>
      <div class="field">
        <label>Model ID</label>
        <input v-model="draft.modelId" class="input input--mono"
          placeholder="输入模型 ID，例如 deepseek-chat / qwen2.5-72b-instruct" />
        <span class="field__hint">按供应商要求填写完整模型 ID，无任何取值限制</span>
      </div>
    </div>

    <template #foot>
      <button class="btn" @click="close">取消</button>
      <button class="btn btn--primary" @click="onSubmit">{{ isNew ? "创建" : "保存" }}</button>
    </template>
  </ModalShell>
</template>
