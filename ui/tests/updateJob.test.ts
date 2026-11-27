/**
 * 全局更新 job 的状态机单测。
 *
 * 跑法：`node --experimental-strip-types tests/updateJob.test.ts`
 */
import test from "node:test";
import assert from "node:assert/strict";
import { UpdateJob, type UpdateIO } from "../src/updateJob.ts";
import type { UpdateInfo } from "../src/types.ts";

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const INFO = {
  current_version: "1.0.0",
  latest_version: "1.1.0",
  has_update: true,
  release_url: "https://example.com/r",
  notes: "",
  published_at: "",
  assets: [],
} as UpdateInfo;

function fakeIO(over: Partial<UpdateIO> = {}) {
  const calls = { check: 0, download: 0, apply: 0, perms: 0 };
  const io: UpdateIO = {
    checkUpdate: async (force) => {
      calls.check++;
      return (over.checkUpdate ?? (async () => INFO))(force);
    },
    downloadUpdate: async () => {
      calls.download++;
      return (over.downloadUpdate ?? (async () => "tok-1"))();
    },
    applyUpdate: async (token) => {
      calls.apply++;
      return (over.applyUpdate ?? (async () => "installing"))(token);
    },
    openInstallPermissionSettings: async () => {
      calls.perms++;
      return (over.openInstallPermissionSettings ?? (async () => {}))();
    },
  };
  return { io, calls };
}

test("连点只下载一次：第二个 start 直接返回 busy", async () => {
  const gate = deferred<string>();
  const { io, calls } = fakeIO({ downloadUpdate: () => gate.promise });
  const job = new UpdateJob(io);

  const first = job.start();
  const second = await job.start();
  assert.equal(second.kind, "busy");
  assert.equal(calls.download, 1);

  gate.resolve("tok-1");
  const done = await first;
  assert.equal(done.kind, "done");
  assert.equal(calls.apply, 1);
  // 结束后释放单飞锁，下一次还能再触发
  assert.equal(job.isInFlight(), false);
});

test("进度只在下载阶段生效，离开页面不影响 job 状态", async () => {
  const gate = deferred<string>();
  const { io } = fakeIO({ downloadUpdate: () => gate.promise });
  const job = new UpdateJob(io);

  const seen: number[] = [];
  const un = job.subscribe(() => seen.push(job.getState().percent));
  const p = job.start();
  await Promise.resolve();
  job.applyProgress({ downloaded: 25, total: 100, percent: 25 });

  // 模拟「设置页卸载」：订阅者全部退订，job 本身仍然保留进度
  un();
  assert.equal(job.getState().percent, 25);
  assert.equal(job.getState().phase, "download");

  gate.resolve("tok-1");
  await p;
  assert.ok(seen.includes(25));
});

test("授权错误保留 token：重试直接安装，不重新下载", async () => {
  let applyCount = 0;
  const { io, calls } = fakeIO({
    applyUpdate: async () => {
      applyCount++;
      if (applyCount === 1) {
        throw { code: "install.need_permission", message: "need permission", retryable: true };
      }
      return "installing";
    },
  });
  const job = new UpdateJob(io);

  const first = await job.start();
  assert.equal(first.kind, "failed");
  assert.equal(job.hasToken(), true);
  assert.equal(calls.perms, 1, "应自动打开授权页");
  assert.equal(job.getState().error?.code, "install.need_permission");

  const second = await job.start();
  assert.equal(second.kind, "done");
  assert.equal(calls.download, 1, "授权后重试不应重新下载");
  assert.equal(calls.apply, 2);
  assert.equal(job.hasToken(), false);
});

test("不可重试的安装错误清掉 token，重试会重新下载", async () => {
  const { io, calls } = fakeIO({
    applyUpdate: async () => {
      throw { code: "install.failed", message: "boom", retryable: false };
    },
  });
  const job = new UpdateJob(io);

  const first = await job.start();
  assert.equal(first.kind, "failed");
  assert.equal(job.hasToken(), false);
  assert.equal(job.getState().phase, "idle");

  await job.start();
  assert.equal(calls.download, 2);
});

test("检查更新并发去重，成功后共享同一份 info", async () => {
  const gate = deferred<UpdateInfo>();
  const { io, calls } = fakeIO({ checkUpdate: () => gate.promise });
  const job = new UpdateJob(io);

  const a = job.check(false);
  const b = job.check(true);
  assert.equal(job.getState().checking, true);
  gate.resolve(INFO);

  const [ra, rb] = await Promise.all([a, b]);
  assert.equal(ra, rb);
  assert.equal(calls.check, 1);
  assert.equal(job.getState().info?.latest_version, "1.1.0");
});

test("检查失败进 checkError，而不是下载错误", async () => {
  const { io } = fakeIO({
    checkUpdate: async () => {
      throw { code: "update.manifest", message: "offline", retryable: true };
    },
  });
  const job = new UpdateJob(io);

  await assert.rejects(() => job.check(true));
  assert.equal(job.getState().checkError?.code, "update.manifest");
  assert.equal(job.getState().error, null);
  assert.equal(job.getState().checking, false);
});
