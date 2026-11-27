/**
 * 多选状态的单测。
 *
 * 这里的规则都很小，但错一条就会变成「删了看不见的任务」或者
 * 「每条进度事件都让整个列表重渲染」，所以单独钉住。
 *
 * 跑法：`node --experimental-strip-types --test tests/selection.test.ts`
 */
import test from "node:test";
import assert from "node:assert/strict";
import { keepFailed, pruneSelection, toggleAll, toggleSelected } from "../src/selection.ts";

test("toggleSelected 增删并返回新集合", () => {
  const a = new Set<string>();
  const b = toggleSelected(a, "x");
  assert.deepEqual([...b], ["x"]);
  assert.deepEqual([...a], [], "不能就地修改原集合");
  const c = toggleSelected(b, "x");
  assert.equal(c.size, 0);
  assert.equal(b.size, 1, "原集合保持不变");
});

test("toggleAll 在已全选时清空", () => {
  const ids = ["a", "b", "c"];
  const all = toggleAll(new Set(["a"]), ids);
  assert.deepEqual([...all].sort(), ["a", "b", "c"]);
  const none = toggleAll(all, ids);
  assert.equal(none.size, 0, "全选状态下再点一次应是取消全选");
});

test("toggleAll 空列表返回空集合", () => {
  assert.equal(toggleAll(new Set(["a"]), []).size, 0);
});

test("pruneSelection 去掉已不存在的 id", () => {
  const cur = new Set(["a", "b", "c"]);
  const next = pruneSelection(cur, ["a", "c", "d"]);
  assert.deepEqual([...next].sort(), ["a", "c"]);
});

test("pruneSelection 无变化时返回同一引用", () => {
  const cur = new Set(["a", "b"]);
  assert.equal(pruneSelection(cur, ["a", "b", "c"]), cur, "同引用才能让 setState 跳过重渲染");
  assert.equal(pruneSelection(new Set(), ["a"]).size, 0);
});

test("keepFailed 只保留失败项", () => {
  const next = keepFailed([{ id: "b" }, { id: "z" }]);
  assert.deepEqual([...next].sort(), ["b", "z"]);
  assert.equal(keepFailed([]).size, 0);
});

test("filterEligible 跳过不需要该动作的状态", async () => {
  const { filterEligible } = await import("../src/selection.ts");
  const items = [
    { id: "a", state: "downloading" },
    { id: "b", state: "completed" },
    { id: "c", state: "error" },
    { id: "d", state: "paused" },
    { id: "e", state: "queued" },
    { id: "f", state: "scheduled" },
  ];
  // 暂停：已完成的不能算「暂停成功」；错误/暂停态本来就没在跑
  assert.deepEqual(filterEligible("pause", items), ["a", "e", "f"]);
  // 继续：只对停着/失败/排队/定时有意义
  assert.deepEqual(filterEligible("resume", items), ["c", "d", "e", "f"]);
  // 删除：一律全部
  assert.deepEqual(filterEligible("remove", items), ["a", "b", "c", "d", "e", "f"]);
});
