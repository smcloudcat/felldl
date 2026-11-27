/**
 * 抓取任务 store 的单测：进度合并、收尾移除、快照引用稳定。
 *
 * 跑法：`node --experimental-strip-types --test tests/grabStore.test.ts`
 */
import test from "node:test";
import assert from "node:assert/strict";
import { grabStore } from "../src/grabStore.ts";
import type { GrabStatus } from "../src/types.ts";

function st(id: string, done: number, extra: Partial<GrabStatus> = {}): GrabStatus {
  return { id, url: "https://e.com/", done, total: 10, finished: false, ...extra };
}

test("apply 新增与推进进度", () => {
  grabStore.setAll([]);
  const seen: number[] = [];
  const un = grabStore.subscribe(() => seen.push(grabStore.getSnapshot()[0]?.done ?? -1));
  grabStore.apply(st("a", 0));
  grabStore.apply(st("a", 1));
  grabStore.apply(st("a", 2));
  un();
  assert.equal(grabStore.getSnapshot().length, 1);
  assert.equal(grabStore.getSnapshot()[0].done, 2);
  assert.deepEqual(seen, [0, 1, 2]);
});

test("迟到的旧进度不覆盖新进度", () => {
  grabStore.setAll([st("a", 5)]);
  let notified = 0;
  const un = grabStore.subscribe(() => notified++);
  grabStore.apply(st("a", 3));
  un();
  assert.equal(grabStore.getSnapshot()[0].done, 5);
  assert.equal(notified, 0, "被丢弃的旧事件不该触发通知");
});

test("finished 移除任务；未知 id 的收尾是无操作", () => {
  grabStore.setAll([st("a", 1), st("b", 2)]);
  grabStore.apply(st("a", 2, { finished: true }));
  assert.deepEqual(
    grabStore.getSnapshot().map((j) => j.id),
    ["b"]
  );
  let notified = 0;
  const un = grabStore.subscribe(() => notified++);
  grabStore.apply(st("ghost", 1, { finished: true }));
  un();
  assert.equal(notified, 0);
});

test("setAll 重建时丢掉已结束的条目", () => {
  grabStore.setAll([st("a", 1), st("b", 2, { finished: true })]);
  assert.deepEqual(
    grabStore.getSnapshot().map((j) => j.id),
    ["a"]
  );
});

test("重复事件保持同一快照引用", () => {
  grabStore.setAll([st("a", 1)]);
  const before = grabStore.getSnapshot();
  grabStore.apply(st("a", 1));
  assert.equal(grabStore.getSnapshot(), before, "内容没变就不该换引用");
});

test("drop 立即移除（取消后不等收尾事件）", () => {
  grabStore.setAll([st("a", 1)]);
  grabStore.drop("a");
  assert.equal(grabStore.getSnapshot().length, 0);
  grabStore.drop("a"); // 再来一次也不报错
});
