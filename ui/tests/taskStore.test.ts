/**
 * normalized task store 的单测：结构变化才通知列表，进度只通知对应 id。
 *
 * 跑法：`node --experimental-strip-types tests/taskStore.test.ts`
 */
import test from "node:test";
import assert from "node:assert/strict";
import { TaskStore } from "../src/taskStore.ts";
import type { Task } from "../src/types.ts";

function task(id: string, patch: Partial<Task> = {}): Task {
  return {
    id,
    url: "https://e.com/f.bin",
    kind: "http",
    mirrors: [],
    file_name: `${id}.bin`,
    save_dir: "/tmp",
    save_path: `/tmp/${id}.bin`,
    size: 100,
    downloaded: 0,
    state: "downloading",
    error: null,
    connections: 8,
    segments: [],
    supports_range: true,
    etag: null,
    last_modified: null,
    content_type: null,
    referer: null,
    cookie: null,
    user_agent: null,
    speed_limit: 0,
    scheduled_at: null,
    created_at: 1,
    completed_at: null,
    speed: 0,
    peers: 0,
    upload_speed: 0,
    bt_info_hash: null,
    category: null,
    username: null,
    password: null,
    interval_secs: 0,
    rev: 0,
    ...patch,
  };
}

test("setAll 同时产出顺序与分类计数", () => {
  const s = new TaskStore();
  s.setAll([task("a", { category: "电影" }), task("b"), task("c", { category: "电影" })]);
  assert.deepEqual(s.getIds(), ["a", "b", "c"]);
  assert.deepEqual(s.getSnapshot().counts, { 电影: 2 });
});

test("append 去重并保持分页顺序", () => {
  const s = new TaskStore();
  s.setAll([task("a"), task("b")]);
  s.append([task("c"), task("a"), task("d")]);
  assert.deepEqual(s.getIds(), ["a", "b", "c", "d"]);
  assert.equal(s.all().length, 4);
});

test("新任务插到最前；旧 rev 的事件不覆盖新 rev", () => {
  const s = new TaskStore();
  s.setAll([task("a", { rev: 5, downloaded: 50 })]);
  s.upsert(task("b", { created_at: 2 }));
  assert.deepEqual(s.getIds(), ["b", "a"]);

  s.upsert(task("a", { rev: 4, downloaded: 10, state: "paused" }));
  assert.equal(s.get("a")?.state, "downloading", "旧 rev 必须被丢弃");

  s.upsert(task("a", { rev: 9, downloaded: 90, state: "completed" }));
  assert.equal(s.get("a")?.state, "completed");
});

test("进度只通知对应 id，不触发列表重渲染", () => {
  const s = new TaskStore();
  s.setAll([task("a", { rev: 1 }), task("b", { rev: 1 })]);
  let listNotifies = 0;
  let aNotifies = 0;
  let bNotifies = 0;
  s.subscribe(() => listNotifies++);
  s.subscribeTask("a", () => aNotifies++);
  s.subscribeTask("b", () => bNotifies++);

  s.patch("a", { rev: 2, downloaded: 40, speed: 1024 });
  assert.equal(s.get("a")?.downloaded, 40);
  assert.equal(listNotifies, 0, "列表快照不应变化");
  assert.equal(aNotifies, 1);
  assert.equal(bNotifies, 0, "别的卡片不该被通知");
});

test("进度合并的三条拒绝规则：未知 id / 终态 / 旧 rev", () => {
  const s = new TaskStore();
  s.setAll([
    task("done", { rev: 3, state: "completed", downloaded: 100 }),
    task("live", { rev: 5, downloaded: 50 }),
  ]);

  s.patch("ghost", { rev: 1, downloaded: 1 });
  assert.equal(s.get("ghost"), undefined, "未知 id 等完整事件，不凭空造任务");

  s.patch("done", { rev: 9, downloaded: 10, state: "downloading" });
  assert.equal(s.get("done")?.state, "completed", "终态忽略迟到进度");
  assert.equal(s.get("done")?.downloaded, 100);

  s.patch("live", { rev: 4, downloaded: 10 });
  assert.equal(s.get("live")?.downloaded, 50, "旧 rev 不得回退");
});

test("筛选准入：不匹配视图的新任务不插入", () => {
  const s = new TaskStore();
  s.setFilter((t) => t.state === "downloading");
  s.setAll([]);
  s.upsert(task("x", { state: "completed" }));
  assert.deepEqual(s.getIds(), []);
  s.upsert(task("y", { state: "downloading" }));
  assert.deepEqual(s.getIds(), ["y"]);
});

test("删除同时通知卡片与列表，并从 all() 消失", () => {
  const s = new TaskStore();
  s.setAll([task("a"), task("b")]);
  let listNotifies = 0;
  let aNotifies = 0;
  s.subscribe(() => listNotifies++);
  s.subscribeTask("a", () => aNotifies++);

  s.remove("a");
  assert.deepEqual(s.getIds(), ["b"]);
  assert.equal(s.get("a"), undefined);
  assert.equal(aNotifies, 1);
  assert.equal(listNotifies, 1);
  assert.deepEqual(
    s.all().map((t) => t.id),
    ["b"]
  );
});

test("分类变化会更新角标并通知列表", () => {
  const s = new TaskStore();
  s.setAll([task("a", { rev: 1, category: null })]);
  let listNotifies = 0;
  s.subscribe(() => listNotifies++);

  s.upsert(task("a", { rev: 2, category: "音乐" }));
  assert.equal(s.getSnapshot().counts["音乐"], 1);
  assert.equal(listNotifies, 1);
});

test("退订后不再收到通知", () => {
  const s = new TaskStore();
  s.setAll([task("a", { rev: 1 })]);
  let n = 0;
  const un = s.subscribeTask("a", () => n++);
  un();
  s.patch("a", { rev: 2, downloaded: 20 });
  assert.equal(n, 0);
});

test("后台冻结：不可见时不通知，回前台一次性刷出", () => {
  const s = new TaskStore();
  s.setAll([task("a", { rev: 1 }), task("b", { rev: 1 })]);
  let a = 0;
  let b = 0;
  let list = 0;
  s.subscribeTask("a", () => a++);
  s.subscribeTask("b", () => b++);
  s.subscribe(() => list++);

  s.setHidden(true);
  s.patch("a", { rev: 2, downloaded: 10 });
  s.patch("a", { rev: 3, downloaded: 20 });
  s.patch("b", { rev: 2, downloaded: 5 });
  s.upsert(task("c", { rev: 1 }));
  assert.equal(a, 0, "不可见时不该触发卡片重渲染");
  assert.equal(b, 0);
  assert.equal(list, 0);

  // 数据本身必须已经更新：回前台要能立刻画对，而不是等下一次事件。
  // 注意 `getIds()` 读的是对外快照，冻结期间刻意不刷新；顺序看 `all()`。
  assert.equal(s.get("a")!.downloaded, 20);
  assert.deepEqual(
    s.all().map((t) => t.id),
    ["c", "a", "b"],
  );
  assert.deepEqual(s.getIds(), ["a", "b"], "冻结期间不刷新对外快照");

  s.setHidden(false);
  assert.equal(a, 1, "同一任务的多条更新合并成一次通知");
  assert.equal(b, 1);
  assert.equal(list, 1, "列表快照也只刷一次");
  assert.equal(s.getSnapshot().ids.length, 3);

  // 恢复后应回到正常即时通知
  s.patch("a", { rev: 4, downloaded: 30 });
  assert.equal(a, 2);
});

test("后台冻结：隐藏状态重复设置不重复 flush", () => {
  const s = new TaskStore();
  s.setAll([task("a", { rev: 1 })]);
  let a = 0;
  s.subscribeTask("a", () => a++);
  s.setHidden(true);
  s.setHidden(true);
  s.patch("a", { rev: 2 });
  s.setHidden(false);
  s.setHidden(false);
  assert.equal(a, 1);
});
