import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles.css";

// 标记运行平台：安卓 15+ 强制 edge-to-edge，状态栏会盖住页面顶部，
// 需要在 CSS 里给状态栏留兜底高度（见 styles.css 里的 --safe-top）。
const ua = navigator.userAgent;
const platform = /Android/i.test(ua)
  ? "android"
  : /iPhone|iPad|iPod/i.test(ua)
    ? "ios"
    : "desktop";
document.documentElement.dataset.platform = platform;

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>
);
