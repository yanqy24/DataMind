import { createApp } from "vue";
import App from "./App.vue";
import "./styles/base.css";
import "./styles/layout.css";
import "./styles/components.css";

// 启动期全局异常可见化（桌面端无控制台时排查用）
function paintError(msg: string) {
  const el = document.getElementById("bootErr") ?? (() => {
    const d = document.createElement("div");
    d.id = "bootErr";
    d.style.cssText =
      "position:fixed;left:12px;bottom:12px;z-index:99999;max-width:70vw;padding:10px 12px;background:#3b1214;color:#f5c0bf;border:1px solid #F0655F;border-radius:8px;font:12px/1.6 Consolas,monospace;white-space:pre-wrap;";
    document.body.appendChild(d);
    return d;
  })();
  el.textContent = msg;
}

window.addEventListener("error", (e) => paintError(`JS 错误: ${e.message}\n${e.filename}:${e.lineno}`));
window.addEventListener("unhandledrejection", (e) => paintError(`Promise 未处理: ${String(e.reason)}`));

try {
  createApp(App).mount("#app");
} catch (e) {
  paintError(`Vue 挂载失败: ${e instanceof Error ? e.stack ?? e.message : String(e)}`);
}
