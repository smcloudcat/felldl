/**
 * 外观注入：把「毛玻璃 / 背景图」设置写进 CSS 变量。
 *
 * 背景图不走 settings.json（几 MB 的 base64 会让每次读写设置都变慢），
 * 而是由后端落盘、这里按需读成 data URL 并缓存一份到内存。
 */
import { api } from "./bridge";
import type { Settings } from "./types";

export type AppearanceFields = Pick<
  Settings,
  "ui_background" | "ui_blur" | "ui_dim" | "ui_glass"
>;

/** 图片 data URL 缓存：path -> data URL（换图后按新 path 存，旧的自然失效） */
const cache = new Map<string, string>();
/** 令牌：快速拖动/连点多张图时，只让最后一次读取的结果落地 */
let token = 0;

function clamp(n: number | undefined, lo: number, hi: number, fallback: number) {
  const v = Number(n);
  if (!Number.isFinite(v)) return fallback;
  return Math.min(hi, Math.max(lo, v));
}

function el() {
  return document.documentElement;
}

/** 直接把图片数据塞进 CSS 变量（选图后即时预览用） */
export function primeBackground(path: string, dataUrl: string) {
  if (path) cache.set(path, dataUrl);
}

/** 清掉某张图的缓存（删除背景时） */
export function forgetBackground(path?: string) {
  if (path) cache.delete(path);
  else cache.clear();
}

function paint(url: string | null) {
  const root = el();
  if (!url) {
    root.style.setProperty("--bg-image", "none");
    root.classList.remove("has-bg-image");
    return;
  }
  root.style.setProperty("--bg-image", `url("${url}")`);
  root.classList.add("has-bg-image");
}

/**
 * 应用外观变量。主题（data-theme）需先由调用方写好，
 * 因为遮罩颜色深浅要跟随当前主题。
 */
export function applyAppearance(s: AppearanceFields | null | undefined) {
  const root = el();
  if (!s) return;
  const blur = clamp(s.ui_blur, 0, 40, 18);
  const dim = clamp(s.ui_dim, 0, 90, 32);
  const glass = clamp(s.ui_glass, 40, 100, 78);
  const dark = root.getAttribute("data-theme") === "dark";
  const path = (s.ui_background || "").trim();

  root.style.setProperty("--glass-blur", `${blur}px`);
  root.style.setProperty("--panel-a", `${glass}%`);
  // 模糊越大，图片要略微放大，避免四周出现半透明的软边
  root.style.setProperty("--bg-scale", String(1 + blur / 380));
  // 遮罩一律压深色：浅色主题下压白罩会把图片洗成一片白，反而看不见背景
  root.style.setProperty("--bg-dim-rgb", "2, 6, 23");
  // 浅色主题的白色面板本身就够亮，罩子只给一半浓度，保住图片的色彩
  root.style.setProperty("--bg-dim-a", path ? String((dark ? dim : dim * 0.5) / 100) : "0");

  const my = ++token;
  if (!path) {
    paint(null);
    return;
  }
  const hit = cache.get(path);
  if (hit) {
    paint(hit);
    return;
  }
  // 读盘期间先保持现状，避免闪一下空白
  api
    .readBackground(path)
    .then((url) => {
      cache.set(path, url);
      if (my === token) paint(url);
    })
    .catch(() => {
      if (my === token) paint(null);
    });
}

/** 主题切换后重算遮罩色（只改颜色，不重复读图） */
export function refreshAppearanceForTheme(s: AppearanceFields | null | undefined) {
  applyAppearance(s);
}
