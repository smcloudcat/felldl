import test from "node:test";
import assert from "node:assert/strict";
// @ts-ignore -- Node 直接跑 .ts（类型擦除），这里只测纯逻辑
import { createSettingsDraftStore } from "../src/settingsDraft.ts";
import type { Settings } from "../src/types.ts";

function settings(over: Partial<Settings> = {}): Settings {
  return {
    download_dir: "/downloads",
    max_concurrent_tasks: 3,
    default_connections: 8,
    global_speed_limit: 0,
    proxy: { enabled: false, kind: "http", host: "", port: 0, username: "", password: "" },
    theme: "system",
    api_enabled: true,
    api_port: 6800,
    api_token: "tok",
    notify_on_complete: true,
    user_agent: "",
    auto_start: false,
    categories: [],
    after_complete: "none",
    bind_ip: "0.0.0.0",
    browser_panel: "notify",
    ui_skin: "blue",
    btn_style: "rounded",
    task_meta: [],
    dns_mode: "system",
    dns_servers: "",
    ui_background: "",
    ui_blur: 18,
    ui_dim: 32,
    ui_glass: 78,
    mirror_enabled: false,
    mirror_mode: "auto",
    mirror_custom: "",
    ...over,
  } as Settings;
}

test("首次 sync 建立基线，dirty 为 false", () => {
  const st = createSettingsDraftStore();
  st.sync(settings());
  const s = st.getState();
  assert.equal(s.dirty, false);
  assert.equal(s.draft?.api_port, 6800);
});

test("patch 只改草稿，基线保持不变", () => {
  const st = createSettingsDraftStore();
  st.sync(settings());
  st.patch({ api_port: 7000 });
  const s = st.getState();
  assert.equal(s.draft?.api_port, 7000);
  assert.equal(s.base?.api_port, 6800);
  assert.equal(s.dirty, true);
});

test("discard 回到基线", () => {
  const st = createSettingsDraftStore();
  st.sync(settings());
  st.patch({ api_port: 7000 });
  st.discard();
  const s = st.getState();
  assert.equal(s.draft?.api_port, 6800);
  assert.equal(s.dirty, false);
});

test("脏草稿不会被外部 settings 更新冲掉", () => {
  const st = createSettingsDraftStore();
  st.sync(settings());
  st.patch({ api_port: 7000 });
  // 另一个窗口改了语言，后端推来新设置
  st.sync(settings({ theme: "dark" }));
  const s = st.getState();
  assert.equal(s.draft?.api_port, 7000, "用户正在编辑的端口必须保留");
  assert.equal(s.base?.theme, "dark", "基线要跟上后端");
  assert.equal(s.dirty, true);
});

test("外部更新恰好与草稿一致时 dirty 归零", () => {
  const st = createSettingsDraftStore();
  st.sync(settings());
  st.patch({ api_port: 7000 });
  st.sync(settings({ api_port: 7000 }));
  assert.equal(st.getState().dirty, false);
});

test("commit 后基线变为服务端返回值，后续 sync 正常对齐", () => {
  const st = createSettingsDraftStore();
  st.sync(settings());
  st.patch({ api_port: 7000 });
  st.commit(settings({ api_port: 7000, theme: "dark" }));
  let s = st.getState();
  assert.equal(s.dirty, false);
  assert.equal(s.base?.theme, "dark");
  st.sync(settings({ api_port: 7000, theme: "dark", max_concurrent_tasks: 5 }));
  s = st.getState();
  assert.equal(s.draft?.max_concurrent_tasks, 5, "无脏改动时应跟随后端");
});

test("值没变化时 sync 不通知订阅者", () => {
  const st = createSettingsDraftStore();
  st.sync(settings());
  let calls = 0;
  const off = st.subscribe(() => calls++);
  st.sync(settings()); // 同值
  st.sync(settings({ api_port: 9999 }));
  off();
  assert.equal(calls, 1);
});

test("订阅者在 patch/discard 时被通知，退订后不再收到", () => {
  const st = createSettingsDraftStore();
  st.sync(settings());
  let calls = 0;
  const off = st.subscribe(() => calls++);
  st.patch({ api_port: 7001 });
  st.discard();
  assert.equal(calls, 2);
  off();
  st.patch({ api_port: 7002 });
  assert.equal(calls, 2);
});

test("未 sync 时 patch 不炸（设置页尚未加载）", () => {
  const st = createSettingsDraftStore();
  st.patch({ api_port: 1 });
  assert.equal(st.getState().draft, null);
  assert.equal(st.getState().dirty, false);
});
