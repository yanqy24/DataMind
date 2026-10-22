/* ============================================================
 * ui.ts — 通用 UI 基建：Toast / 右键菜单 / 图标
 * ============================================================ */
import { reactive } from "vue";

/* ---------- 图标（引用 index.html 中的 SVG 精灵） ---------- */
export function icon(name: string, size = 14): string {
  return `<svg viewBox="0 0 24 24" width="${size}" height="${size}"><use href="#i-${name}"/></svg>`;
}

/* ---------- Toast ---------- */
export interface ToastItem {
  id: number;
  msg: string;
  type: "ok" | "err" | "info";
}
let toastSeed = 0;
export const toasts = reactive<ToastItem[]>([]);
export function toast(msg: string, type: ToastItem["type"] = "info") {
  const id = ++toastSeed;
  toasts.push({ id, msg, type });
  setTimeout(() => {
    const i = toasts.findIndex((t) => t.id === id);
    if (i >= 0) toasts.splice(i, 1);
  }, 2600);
}

/* ---------- 右键菜单 ---------- */
export type CtxItem =
  | "sep"
  | {
      header?: boolean;
      label: string;
      icon?: string;
      danger?: boolean;
      disabled?: boolean;
      short?: string;
      onClick?: () => void;
    };

interface CtxState {
  x: number;
  y: number;
  items: CtxItem[];
  open: boolean;
}
export const ctxMenuState = reactive<CtxState>({ x: 0, y: 0, items: [], open: false });

export function openCtxMenu(x: number, y: number, items: CtxItem[]) {
  ctxMenuState.x = x;
  ctxMenuState.y = y;
  ctxMenuState.items = items;
  ctxMenuState.open = true;
}
export function closeCtxMenu() {
  ctxMenuState.open = false;
}

/* ---------- 确认对话框 ---------- */
export interface ConfirmState {
  open: boolean;
  title: string;
  msg: string;
  okText: string;
  danger?: boolean;
  onOk: (() => void) | null;
}
export const confirmState = reactive<ConfirmState>({
  open: false,
  title: "",
  msg: "",
  okText: "确定",
  onOk: null,
});
export function confirmDialog(
  title: string,
  msg: string,
  onOk: () => void,
  okText = "确定",
  danger = true,
) {
  confirmState.title = title;
  confirmState.msg = msg;
  confirmState.okText = okText;
  confirmState.danger = danger;
  confirmState.onOk = onOk;
  confirmState.open = true;
}

/* ---------- 剪贴板（WebView2 优先 navigator，失败走 Tauri 插件） ---------- */
export async function copyText(text: string, tip?: string) {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
    } else {
      throw new Error("fallback");
    }
  } catch {
    try {
      const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
      await writeText(text);
    } catch {
      toast("复制失败", "err");
      return;
    }
  }
  toast(tip || "已复制", "ok");
}
