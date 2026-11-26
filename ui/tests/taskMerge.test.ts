/**
 * 快照合并的水位语义单测。
 *
 * 跑法：`node --experimental-strip-types tests/taskMerge.test.ts`
 * （ui 没有引入测试框架，靠 Node 自带的类型擦除 + node:test，
 *  避免为一个纯函数拉一整套 vitest 依赖。）
 */
import test from "node:test";
import assert from "node:assert/strict";
import { mergeSnapshot } from "../src/taskMerge.ts";
import type { Task } from "../src/types.ts";

function task(id: string, patch: Partial<Task> = {}): Task {
  return {
    id,
    url: "https://e.com/f.bin",
    kind: "http",
    mirrors: [],
    file_name: "f.bin",
    save_dir: "/tmp",
    save_path: "/tmp/f.bin",
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

const empty = () => ({ removed: new Set<string>(), touched: new Set<string>() });

test("旧快照不得把已完成的任务打回下载中", () => {
  // 事件先到：已完成，rev=12；随后启动时的快照到达，里面还是下载中 rev=5
  const prev = [task("a", { state: "completed", downloaded: 100, rev: 12 })];
  const snapshot = [task("a", { state: "downloading", downloaded: 40, rev: 5 })];
  const out = mergeSnapshot(prev, snapshot, empty());
  assert.equal(out.length, 1);
  assert.equal(out[0].state, "completed", "较新的本地状态必须保留");
  assert.equal(out[0].downloaded, 100);
});

test("快照更新时以快照为准", () => {
  const prev = [task("a", { state: "downloading", downloaded: 10, rev: 3 })];
  const snapshot = [task("a", { state: "paused", downloaded: 80, rev: 9 })];
  const out = mergeSnapshot(prev, snapshot, empty());
  assert.equal(out[0].state, "paused");
  assert.equal(out[0].downloaded, 80);
});

test("取快照期间被删除的任务不会被快照复活", () => {
  // 快照是删除之前生成的，里面还有它；但期间已经收到了 taskRemoved
  const prev: Task[] = [];
  const snapshot = [task("gone", { rev: 4 }), task("keep", { rev: 2 })];
  const ctx = { removed: new Set(["gone"]), touched: new Set(["gone"]) };
  const out = mergeSnapshot(prev, snapshot, ctx);
  assert.deepEqual(
    out.map((t) => t.id),
    ["keep"],
    "已删除的任务绝不能因为旧快照而重新出现"
  );
});

test("快照生成之后新建的任务不会被丢掉", () => {
  // prev 里是刚通过事件新增的任务，快照是新增之前拍的，自然没有它
  const prev = [task("new", { rev: 1, created_at: 20 })];
  const snapshot = [task("old", { rev: 7, created_at: 1 })];
  const out = mergeSnapshot(prev, snapshot, {
    removed: new Set(),
    touched: new Set(["new"]),
  });
  assert.deepEqual(out.map((t) => t.id), ["new", "old"]);
});

test("快照里消失、且期间没有事件的任务视为已删除", () => {
  const prev = [task("stale", { rev: 3, created_at: 5 })];
  const out = mergeSnapshot(prev, [], empty());
  assert.deepEqual(out, [], "别的客户端删掉的任务不该残留");
});

test("rev 缺省（旧数据 / 预览模式）按 0 处理，不会互相挤掉", () => {
  const prev = [{ ...task("a"), rev: undefined }];
  const snapshot = [{ ...task("a", { state: "paused" }), rev: undefined }];
  const out = mergeSnapshot(prev as Task[], snapshot as Task[], empty());
  assert.equal(out.length, 1);
  assert.equal(out[0].state, "paused");
});
