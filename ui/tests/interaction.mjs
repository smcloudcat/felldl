/**
 * 真实浏览器里的交互验收（不接 Tauri，用内置 mock）
 *
 * 覆盖三件事：
 * 1. 键盘导航：方向键移动焦点、End/Home 跳两端、焦点卡必须在视口内（虚拟滚动要配合）
 * 2. 长按进入多选：按住 600ms 后出现批量条且只选中这一张；紧随的 click 不能把它取消
 * 3. 切到后台再回来：列表还在、不报错（store 冻结期间只记脏，回前台 flush）
 *
 * 用法：先 `npm run build` 并托管 dist，再
 *   node tests/interaction.mjs http://127.0.0.1:8099
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
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));

await page.goto(`${base}/?tasks=1000`, { waitUntil: "networkidle" });
await page.waitForSelector(".task-card");
await page.waitForTimeout(700);

const results = [];
const check = (name, ok, detail = "") => {
  results.push({ name, ok, detail });
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}`);
};

// ── 1. 键盘导航 ──
await page.keyboard.press("ArrowDown");
await page.waitForTimeout(120);
const firstFocus = await page.evaluate(() => {
  const el = document.querySelector(".task-card.kbd-focus");
  if (!el) return null;
  const r = el.getBoundingClientRect();
  return { id: el.querySelector(".task-name")?.textContent ?? "", top: r.top, bottom: r.bottom };
});
check("第一次按下键选中第一张", !!firstFocus, firstFocus?.id);

for (let i = 0; i < 12; i++) await page.keyboard.press("ArrowDown");
await page.waitForTimeout(400);
const afterDown = await page.evaluate(() => {
  const el = document.querySelector(".task-card.kbd-focus");
  const sc = document.querySelector(".content");
  const s = sc.getBoundingClientRect();
  const r = el?.getBoundingClientRect();
  return {
    id: el?.querySelector(".task-name")?.textContent ?? "",
    inView: !!r && r.top >= s.top - 1 && r.bottom <= s.bottom + 1,
    mounted: document.querySelectorAll(".task-card").length,
    geo: r
      ? {
          cardTop: Math.round(r.top),
          cardBottom: Math.round(r.bottom),
          scTop: Math.round(s.top),
          scBottom: Math.round(s.bottom),
          scrollTop: Math.round(sc.scrollTop),
          clientH: sc.clientHeight,
          padTop: parseFloat(getComputedStyle(sc).paddingTop) || 0,
        }
      : null,
  };
});
console.log("  诊断:", JSON.stringify(afterDown.geo));
check("连按 12 次下键后焦点仍在视口内", !!afterDown?.inView, afterDown?.id);
check("移动焦点不会把挂载量撑大", (afterDown?.mounted ?? 999) < 60, `mounted=${afterDown?.mounted}`);
check("焦点只有一个", (await page.locator(".task-card.kbd-focus").count()) === 1);

await page.keyboard.press("End");
await page.waitForTimeout(600);
const atEnd = await page.evaluate(() => {
  const el = document.querySelector(".task-card.kbd-focus");
  const sc = document.querySelector(".content");
  return {
    id: el?.querySelector(".task-name")?.textContent ?? null,
    scrollTop: Math.round(sc.scrollTop),
    maxScroll: Math.round(sc.scrollHeight - sc.clientHeight),
  };
});
// 分页下「End」到的是**当时已加载窗口**的最后一条；滚到这里会触发自动续拉，
// 下一秒 scrollHeight 就变大了，所以不能拿 maxScroll 比。用「已越过前 30 条」验证。
check(
  "End 把列表滚到已加载窗口底部",
  atEnd.scrollTop > 3000 && !!atEnd.id,
  `scrollTop=${atEnd.scrollTop} ${atEnd.id ?? "(无焦点)"}`,
);

await page.keyboard.press("Home");
await page.waitForTimeout(600);
const atHome = await page.locator(".task-card.kbd-focus").count();
check("Home 回到顶部且有焦点", atHome === 1);

// ── 2. 长按进入多选 ──
const card = page.locator(".task-card").first();
const box = await card.boundingBox();
await page.mouse.move(box.x + box.width / 2, box.y + 20);
await page.mouse.down();
await page.waitForTimeout(650);
await page.mouse.up();
await page.waitForTimeout(250);

const batch = await page.evaluate(() => ({
  bar: !!document.querySelector(".batch-bar"),
  selected: document.querySelectorAll(".task-card.selected").length,
  selectable: document.querySelectorAll(".task-card.selectable").length > 0,
}));
check("长按 650ms 进入多选", batch.bar && batch.selectable);
check("长按后仅选中被按住的那一张", batch.selected === 1, `selected=${batch.selected}`);

// 长按之后浏览器会补发一次 click。这一次必须被吞掉，否则「刚选中又被自己取消」；
// 之后的点击要恢复正常切换（整卡点击 = 加选/减选）。
await card.click();
await page.waitForTimeout(200);
const afterClick = await page.evaluate(
  () => document.querySelectorAll(".task-card.selected").length,
);
check("长按后的第一次 click 被吞掉（不会立刻取消选中）", afterClick === 1, `selected=${afterClick}`);
await card.click();
await page.waitForTimeout(200);
const afterClick2 = await page.evaluate(
  () => document.querySelectorAll(".task-card.selected").length,
);
check("之后的点击恢复正常切换", afterClick2 === 0, `selected=${afterClick2}`);

// ── 3. 切后台再回来 ──
const other = await browser.newPage();
await other.goto("about:blank");
await other.bringToFront();
await page.waitForTimeout(900);
await page.bringToFront();
await page.waitForTimeout(600);
const afterBack = await page.evaluate(() => ({
  cards: document.querySelectorAll(".task-card").length,
  hidden: document.hidden,
}));
check("切回前台列表仍在", afterBack.cards > 0 && !afterBack.hidden, `cards=${afterBack.cards}`);

check("无未捕获异常", errors.length === 0, errors.slice(0, 2).join(" | "));

await browser.close();
const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} 通过`);
process.exit(failed.length ? 1 : 0);
