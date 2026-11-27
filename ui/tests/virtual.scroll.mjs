/**
 * 真实浏览器里的虚拟滚动验收（不接 Tauri，用内置 mock）
 *
 * 1. `?tasks=1000` 造 1000 条历史任务
 * 2. 统计 DOM 里实际挂载的 .task-card 数量 —— 必须远小于总条数
 * 3. 滚到底，确认最后一条能出现（旧实现「首屏 60 条」做不到）
 * 4. 滚动过程中挂载量保持有界
 *
 * 用法：先 `npm run build` 并托管 dist，再
 *   node tests/virtual.scroll.mjs http://127.0.0.1:8099
 */
import { chromium } from "playwright-core";
import path from "node:path";
import os from "node:os";

const base = process.argv[2] ?? "http://127.0.0.1:8099";
const exe = path.join(
  os.homedir(),
  "AppData/Local/ms-playwright/chromium-1140/chrome-win/chrome.exe"
);

const browser = await chromium.launch({ executablePath: exe, headless: true });
const page = await browser.newPage({ viewport: { width: 420, height: 900 } });
await page.goto(`${base}/?tasks=1000`, { waitUntil: "networkidle" });
await page.waitForSelector(".task-card");

const total = 1000 + 4; // mock 固定 4 条 + 1000 条历史
const mounted = await page.locator(".task-card").count();

const info = await page.evaluate(() => {
  const sc = document.querySelector(".content");
  return { scrollHeight: sc.scrollHeight, clientHeight: sc.clientHeight };
});

// 滚到底
await page.evaluate(() => {
  const sc = document.querySelector(".content");
  sc.scrollTop = sc.scrollHeight;
});
await page.waitForTimeout(400);
await page.locator(".content").evaluate((el) => el.dispatchEvent(new Event("scroll")));
await page.waitForTimeout(300);

const mountedBottom = await page.locator(".task-card").count();
const lastVisible = await page
  .locator(".task-card", { hasText: "history-999.bin" })
  .count();

// 再滚回中间，确认没有越滚越多
await page.evaluate(() => {
  const sc = document.querySelector(".content");
  sc.scrollTop = sc.scrollHeight / 2;
});
await page.waitForTimeout(400);
const mountedMid = await page.locator(".task-card").count();

console.log(
  JSON.stringify(
    { total, mounted, mountedMid, mountedBottom, lastVisible, ...info },
    null,
    2
  )
);

const problems = [];
if (mounted > 80) problems.push(`首屏挂载过多：${mounted}`);
if (mountedMid > 80) problems.push(`滚动中挂载过多：${mountedMid}`);
if (mountedBottom > 80) problems.push(`底部挂载过多：${mountedBottom}`);
if (lastVisible !== 1) problems.push("滚到底看不到最后一条");
if (info.scrollHeight < 1000 * 50) problems.push("滚动高度没有覆盖全部记录");

await browser.close();
if (problems.length) {
  console.error("FAIL: " + problems.join("；"));
  process.exit(1);
}
console.log("PASS: 挂载量与 1000 条历史解耦，滚到底可见最后一条");
