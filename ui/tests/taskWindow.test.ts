import assert from "node:assert/strict";
import test from "node:test";
import { INITIAL_LIMIT, STEP, nextLimit, visibleSlice } from "../src/taskWindow.ts";

test("列表比窗口短：全部渲染，没有隐藏项", () => {
  const list = [1, 2, 3];
  const { items, hidden } = visibleSlice(list, INITIAL_LIMIT);
  assert.deepEqual(items, [1, 2, 3]);
  assert.equal(hidden, 0);
});

test("列表比窗口长：只渲染前 limit 条，剩余条数如实上报", () => {
  const list = Array.from({ length: 200 }, (_, i) => i);
  const { items, hidden } = visibleSlice(list, 60);
  assert.equal(items.length, 60);
  assert.equal(items[0], 0);
  assert.equal(items[59], 59);
  assert.equal(hidden, 140);
});

test("刚好等于窗口：不显示「更多」", () => {
  const list = Array.from({ length: 60 }, (_, i) => i);
  const { items, hidden } = visibleSlice(list, 60);
  assert.equal(items.length, 60);
  assert.equal(hidden, 0);
});

test("limit 为 0 时一条都不渲染（但不崩）", () => {
  const { items, hidden } = visibleSlice([1, 2], 0);
  assert.deepEqual(items, []);
  assert.equal(hidden, 2);
});

test("非法 limit 按 0 处理：负数 / NaN 不得变成 -1 或抛错", () => {
  for (const bad of [-5, NaN, -Infinity]) {
    const { items, hidden } = visibleSlice([1, 2, 3], bad);
    assert.deepEqual(items, []);
    assert.equal(hidden, 3);
  }
});

test("空列表：任何 limit 都返回空", () => {
  const { items, hidden } = visibleSlice([], 100);
  assert.deepEqual(items, []);
  assert.equal(hidden, 0);
});

test("展开：每次加 STEP，直到超过列表长度", () => {
  assert.equal(nextLimit(INITIAL_LIMIT), INITIAL_LIMIT + STEP);
  assert.equal(nextLimit(0, 10), 10);
  // 非法入参不产出 NaN / 负数（否则 slice 会静默返回空列表，用户以为任务没了）
  assert.equal(nextLimit(NaN), STEP);
  assert.equal(nextLimit(-100, 10), 10);
  assert.equal(nextLimit(10, 0), 11, "step 为 0 时至少前进 1，不能原地不动");
});

test("截取不修改原数组（列表来自 state，不能被就地裁剪）", () => {
  const list = [1, 2, 3];
  const { items } = visibleSlice(list, 2);
  assert.notEqual(items, list);
  assert.deepEqual(list, [1, 2, 3]);
});

test("keep 谓词：排在后面的活动任务不被窗口挡住", () => {
  // 60 条历史在前，一条正在下载的老任务排在第 100 位
  const list = Array.from({ length: 120 }, (_, i) => ({ id: i, active: i === 99 }));
  const { items, hidden } = visibleSlice(list, 10, (t) => t.active);
  const ids = items.map((t) => t.id);
  assert.ok(ids.includes(99), "运行中的任务必须渲染，哪怕排在第 100 位");
  assert.equal(hidden, 109, "120 条里渲染 10 条历史 + 1 条活动，其余 109 条被挡住");
  // 相对顺序不变（不能因为 keep 就把活动任务提到最前面）
  assert.deepEqual(
    ids.filter((id) => id < 10),
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
  );
});

test("keep 谓词命中很多时：条数可以超过 limit，但不会丢任务", () => {
  const list = Array.from({ length: 50 }, (_, i) => ({ id: i, active: true }));
  const { items, hidden } = visibleSlice(list, 5, (t) => t.active);
  assert.equal(items.length, 50, "全部活动任务都要渲染");
  assert.equal(hidden, 0);
});
