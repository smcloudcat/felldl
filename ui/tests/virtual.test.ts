/**
 * 虚拟滚动窗口的纯函数测试
 *
 * 重点是「挂载量不随历史规模增长」和「滚到底不截断」这两条——
 * 它们正是旧实现（首屏 60 条 + 显示更多）做不到的。
 */
import assert from "node:assert/strict";
import test from "node:test";
import { computeRange, ESTIMATED_ROW, moveFocus, OVERSCAN, scrollTopToReveal } from "../src/virtual.ts";

test("空的列表不产生占位", () => {
  assert.deepEqual(computeRange(0, 0, 800, 96), {
    start: 0,
    end: 0,
    padTop: 0,
    padBottom: 0,
  });
});

test("顶部时从 0 开始，且只挂可视区 + overscan", () => {
  const r = computeRange(1000, 0, 900, 100, 2);
  assert.equal(r.start, 0);
  // 900 高、行高 100 → 9 行可见，加 2 行 overscan
  assert.equal(r.end, 11);
  assert.equal(r.padTop, 0);
  assert.equal(r.padBottom, (1000 - 11) * 100);
});

test("挂载量只与视口有关，与历史规模无关", () => {
  const small = computeRange(100, 0, 900, 100, 2);
  const huge = computeRange(100_000, 0, 900, 100, 2);
  assert.equal(
    huge.end - huge.start,
    small.end - small.start,
    "十万条和一百条的挂载量必须一致"
  );
  assert.ok(huge.end - huge.start < 20, "挂载量应有上界");
});

test("滚动到中间时，上方/下方占位与区间互补", () => {
  const row = 100;
  const r = computeRange(1000, 5_000, 800, row, 2);
  assert.equal(r.start, 48); // 5000/100=50，减 overscan 2
  assert.equal(r.padTop, 48 * row);
  assert.equal(r.padBottom, (1000 - r.end) * row);
  // 区间内的高度 + 上下占位 = 全部内容高度（行高估算的意义就在于此）
  assert.equal(
    r.padTop + (r.end - r.start) * row + r.padBottom,
    1000 * row
  );
});

test("滚到底时最后一条一定在区间内", () => {
  const count = 1000;
  const row = 100;
  const viewport = 800;
  const maxScroll = count * row - viewport;
  const r = computeRange(count, maxScroll, viewport, row, OVERSCAN);
  assert.equal(r.end, count, "到底时不能留空");
  assert.ok(r.start < count, "至少挂最后几行");
  assert.equal(r.padBottom, 0);
});

test("非法入参按兜底处理，不产生负占位", () => {
  const r = computeRange(10, NaN, -5, 0, NaN);
  assert.equal(r.start, 0);
  assert.ok(r.end >= 0 && r.end <= 10);
  assert.ok(r.padTop >= 0 && r.padBottom >= 0);
  assert.equal(computeRange(-3, 0, 800, 96).end, 0);
});

test("默认行高常量是正数（防止被误改成 0 之后除零）", () => {
  assert.ok(ESTIMATED_ROW > 1);
});

test("键盘导航：首次按键落到头/尾，之后按方向逐条移动并夹在两端", () => {
  assert.equal(moveFocus(null, 1, 5), 0, "第一次按下键选第一张");
  assert.equal(moveFocus(null, -1, 5), 4, "第一次按上键选最后一张");
  assert.equal(moveFocus(0, -1, 5), 0, "已在顶部不越界");
  assert.equal(moveFocus(4, 1, 5), 4, "已在底部不越界");
  assert.equal(moveFocus(2, 1, 5), 3);
  assert.equal(moveFocus(2, -3, 5), 0);
  // PageDown 这类大步长同样要夹住
  assert.equal(moveFocus(1, 99, 5), 4);
  assert.equal(moveFocus(null, 1, 0), null, "空列表没有焦点");
  assert.equal(moveFocus(0, 1, 0), null);
});

test("键盘导航：只在焦点跑出视口时滚动，且留半行余量", () => {
  const row = 100;
  const vp = 400;
  // 已在视口内：不动
  assert.equal(scrollTopToReveal(2, row, vp, 0), null);
  assert.equal(scrollTopToReveal(1, row, vp, 100), null);
  // 向下出界：把目标底部露出来，再加半行
  assert.equal(scrollTopToReveal(5, row, vp, 0), 500 + 100 - 400 + 50);
  // 向上出界：滚到目标上方半行
  assert.equal(scrollTopToReveal(1, row, vp, 300), 50);
  // 顶部不产生负滚动
  assert.equal(scrollTopToReveal(0, row, vp, 300), 0);
  // 容器还没挂载（viewport 为 0）：不滚，交给下一帧
  assert.equal(scrollTopToReveal(3, row, 0, 0), null);
});
