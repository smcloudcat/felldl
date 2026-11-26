/**
 * 命令错误归一化单测（`npm test` 会一起跑）。
 *
 * 重点：**只有真正的 `code: message` 才拆**。之前前端用 `startsWith("install.")`
 * 判断类型，文案一改就崩；反过来，如果把 `https://…` 或中文句子误拆成 code，
 * 前端会拿一个假 code 去做动作判断，同样错。
 */
import test from "node:test";
import assert from "node:assert/strict";
import { toIpcError } from "../src/ipcError.ts";

test("新后端：对象原样取字段", () => {
  const e = toIpcError({ code: "install.need_permission", message: "需要授权", retryable: true });
  assert.deepEqual(e, {
    code: "install.need_permission",
    message: "需要授权",
    retryable: true,
  });
});

test("对象缺 message / retryable 时给保守默认值", () => {
  const e = toIpcError({ code: "update.token" });
  assert.equal(e.code, "update.token");
  assert.equal(e.message, "update.token");
  assert.equal(e.retryable, false);

  const e2 = toIpcError({ code: "update.network", message: "断网", retryable: "yes" });
  assert.equal(e2.retryable, false, "非布尔值不能被当成 true");
});

test("旧约定：字符串 code: message 会被拆开", () => {
  const e = toIpcError("install.no_handler: 系统没有可用的安装器");
  assert.equal(e.code, "install.no_handler");
  assert.equal(e.message, "系统没有可用的安装器");
  assert.equal(e.retryable, false);
});

test("普通文案不会被误拆", () => {
  const cases = [
    "读取已安装版本失败：boom",
    "https://example.com/a:b",
    "失败: 原因是 X",
  ];
  for (const raw of cases) {
    const e = toIpcError(raw, "update.failed");
    assert.equal(e.code, "update.failed", `${raw} 不该被当成机器码`);
    assert.equal(e.message, raw);
  }
});

test("Error 实例取 message", () => {
  const e = toIpcError(new Error("network down"), "update.network");
  assert.equal(e.code, "update.network");
  assert.equal(e.message, "network down");
});

test("空值也有可展示文案", () => {
  for (const v of [null, undefined, ""]) {
    const e = toIpcError(v, "unknown");
    assert.equal(e.message, "unknown");
    assert.equal(e.code, "unknown");
  }
});
