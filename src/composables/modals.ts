/* ============================================================
 * modals.ts — 各业务弹窗的开关与参数状态（由 App.vue 统一渲染）
 * ============================================================ */
import { reactive } from "vue";
import type { ConnConfig, ModelConfig, TableMeta } from "../types";

export const connFormState = reactive({
  open: false,
  editing: null as ConnConfig | null, // null = 新建
});

export const modelFormState = reactive({
  open: false,
  editing: null as ModelConfig | null, // null = 新建
});

export const settingsState = reactive({ open: false });

/** SQL 查看 / 编辑弹窗（edit = 改已有卡片，new = 手写/粘贴新建） */
export const sqlEditorState = reactive({
  open: false,
  mode: "edit" as "edit" | "new",
  tabId: "" as string,
  sqlId: "" as string,
  /** 新建模式下的初始内容（例如从剪贴板/示例带入） */
  initial: "" as string,
});

/** 本地记忆查看弹窗 */
export const memoryState = reactive({
  open: false,
  connId: "" as string,
  db: "" as string, // 空 = 全部库
});

/** 单表 DDL / 取样查看弹窗 */
export const tableState = reactive({
  open: false,
  connId: "" as string,
  db: "" as string,
  table: null as TableMeta | null,
});

export function openConnForm(editing: ConnConfig | null) {
  connFormState.editing = editing;
  connFormState.open = true;
}
export function openModelForm(editing: ModelConfig | null) {
  modelFormState.editing = editing;
  modelFormState.open = true;
}
export function openSqlEditor(tabId: string, sqlId: string) {
  sqlEditorState.mode = "edit";
  sqlEditorState.tabId = tabId;
  sqlEditorState.sqlId = sqlId;
  sqlEditorState.initial = "";
  sqlEditorState.open = true;
}

/** 新建 SQL：手写或从别处粘贴一条 SELECT 后执行（仍走同一套只读校验与执行链路） */
export function openNewSql(tabId: string, initial = "") {
  sqlEditorState.mode = "new";
  sqlEditorState.tabId = tabId;
  sqlEditorState.sqlId = "";
  sqlEditorState.initial = initial;
  sqlEditorState.open = true;
}
export function openMemory(connId: string, db = "") {
  memoryState.connId = connId;
  memoryState.db = db;
  memoryState.open = true;
}
export function openTable(connId: string, db: string, table: TableMeta) {
  tableState.connId = connId;
  tableState.db = db;
  tableState.table = table;
  tableState.open = true;
}
