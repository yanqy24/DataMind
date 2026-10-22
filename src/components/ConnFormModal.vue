<script setup lang="ts">
/* 数据库连接表单：新建 / 编辑 / 测试连接 */
import { computed, reactive, ref, watch } from "vue";
import { connFormState } from "../composables/modals";
import { newConnDraft, saveConnection, testConnection } from "../composables/store";
import { beErr } from "../lib/backend";
import { icon, toast } from "../composables/ui";
import ModalShell from "./ModalShell.vue";
import VSelect from "./VSelect.vue";
import type { ConnConfig } from "../types";

const isNew = computed(() => !connFormState.editing);
const draft = reactive<ConnConfig>(JSON.parse(JSON.stringify(connFormState.editing ?? newConnDraft())));
const testing = ref(false);

watch(
  () => connFormState.open,
  (open) => {
    if (open) Object.assign(draft, JSON.parse(JSON.stringify(connFormState.editing ?? newConnDraft())));
  },
);

function close() {
  connFormState.open = false;
}

async function onTest() {
  if (!draft.host) {
    toast("请填写主机 IP", "err");
    return;
  }
  testing.value = true;
  try {
    const msg = await testConnection({ ...draft });
    toast(msg, "ok");
  } catch (e) {
    const err = beErr(e);
    toast(`连接失败：${err.message}${err.hint ? `（${err.hint}）` : ""}`, "err");
  } finally {
    testing.value = false;
  }
}

async function onSubmit() {
  if (!draft.host) {
    toast("请填写主机 IP", "err");
    return;
  }
  if (!draft.name.trim()) draft.name = draft.host;
  try {
    await saveConnection({ ...draft }, isNew.value);
    close();
  } catch (e) {
    toast(`保存失败：${beErr(e).message}`, "err");
  }
}
</script>

<template>
  <ModalShell v-if="connFormState.open" :title="isNew ? '新建数据库连接' : '编辑数据库连接'"
    icon-name="database" size="lg" @close="close">
    <div style="display:flex;flex-direction:column;gap:12px">
      <div class="field-row">
        <div class="field">
          <label>数据库类型</label>
          <VSelect :model-value="draft.type" :options="[
            { value: 'MySQL', label: 'MySQL' },
            { value: 'PostgreSQL', label: 'PostgreSQL', muted: true, sub: '敬请期待' },
          ]" width="160px" @update:model-value="v => { if (v === 'MySQL') draft.type = 'MySQL' }" />
          <span class="field__hint">当前仅支持 MySQL，后续版本将拓展 PostgreSQL / ClickHouse</span>
        </div>
        <div class="field">
          <label>连接名称</label>
          <input v-model="draft.name" class="input" placeholder="例如：电商生产库" />
        </div>
      </div>
      <div class="field-row">
        <div class="field">
          <label>主机 IP / 域名</label>
          <input v-model="draft.host" class="input" placeholder="127.0.0.1 或 db.example.com" />
        </div>
        <div class="field" style="max-width:110px">
          <label>端口</label>
          <input v-model.number="draft.port" class="input" type="number" />
        </div>
      </div>
      <div class="field-row">
        <div class="field">
          <label>用户名</label>
          <input v-model="draft.user" class="input" placeholder="root" />
        </div>
        <div class="field">
          <label>密码</label>
          <input v-model="draft.password" class="input" type="password" placeholder="••••••••" />
        </div>
      </div>

      <div class="setrow">
        <div class="setrow__main">
          <div class="setrow__title">使用 SSH 隧道</div>
          <div class="setrow__desc">通过跳板机访问内网数据库</div>
        </div>
        <div class="switch" :class="{ 'is-on': draft.ssh.enabled }" @click="draft.ssh.enabled = !draft.ssh.enabled"></div>
      </div>

      <div v-if="draft.ssh.enabled"
        style="display:flex;flex-direction:column;gap:10px;padding:12px;border:1px solid var(--border);border-radius:var(--r);background:var(--surface-2)">
        <div class="field-row">
          <div class="field">
            <label>SSH 主机</label>
            <input v-model="draft.ssh.host" class="input" placeholder="jump.company.com" />
          </div>
          <div class="field" style="max-width:110px">
            <label>SSH 端口</label>
            <input v-model.number="draft.ssh.port" class="input" type="number" />
          </div>
        </div>
        <div class="field-row">
          <div class="field">
            <label>SSH 用户</label>
            <input v-model="draft.ssh.user" class="input" placeholder="ops" />
          </div>
          <div class="field">
            <label>认证方式</label>
            <VSelect v-model="draft.ssh.auth" :options="[
              { value: 'password', label: '密码' },
              { value: 'key', label: '私钥文件' },
            ]" width="140px" />
          </div>
        </div>
        <div class="field">
          <label>{{ draft.ssh.auth === "password" ? "SSH 密码" : "私钥文件路径" }}</label>
          <input v-model="draft.ssh.secret" class="input input--mono"
            :placeholder="draft.ssh.auth === 'password' ? '••••••••' : 'C:\\Users\\you\\.ssh\\id_rsa'" />
        </div>
      </div>
    </div>

    <template #foot>
      <button class="btn" :disabled="testing" @click="onTest">
        <span v-html="icon('link', 12)"></span> {{ testing ? "测试中…" : "测试连接" }}
      </button>
      <button class="btn" @click="close">取消</button>
      <button class="btn btn--primary" @click="onSubmit">{{ isNew ? "创建" : "保存" }}</button>
    </template>
  </ModalShell>
</template>
