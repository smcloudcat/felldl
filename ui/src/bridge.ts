/**
 * Tauri 桥接层
 *
 * 所有与 Rust 通信的地方都集中在这里，方便：
 *   1. 开发时在没有 Tauri 环境（纯浏览器）下也能跑，用假数据预览界面
 *   2. 换实现时只改这一处
 */
import type { AddTaskRequest, Settings, Task, UpdateInfo, PageAnalysis } from "./types";

type UnlistenFn = () => void;

interface EngineEvent {
  type: "taskUpdated" | "taskProgress" | "taskRemoved" | "settingsUpdated" | "notice";
  payload: any;
}

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

let invokeImpl: (cmd: string, args?: any) => Promise<any>;
let listenImpl: <T>(evt: string, cb: (e: { payload: T }) => void) => Promise<UnlistenFn>;

if (isTauri) {
  const mod = await import("@tauri-apps/api/core");
  const ev = await import("@tauri-apps/api/event");
  invokeImpl = mod.invoke;
  listenImpl = ev.listen as any;
} else {
  // ── 浏览器预览模式：内存假数据 ──
  const { mockEngine } = await import("./mock");
  invokeImpl = mockEngine.invoke;
  listenImpl = mockEngine.listen as any;
}

export const invoke = invokeImpl;
export const listen = listenImpl;
export const runningInTauri = isTauri;

// ───────────────────────── 命令封装 ─────────────────────────

export const api = {
  listTasks: () => invoke("list_tasks") as Promise<Task[]>,
  getSettings: () => invoke("get_settings") as Promise<Settings>,

  addTask: (req: AddTaskRequest) => invoke("add_task", { req }) as Promise<string>,

  /** 批量添加（多行 URL） */
  addTasks: (reqs: AddTaskRequest[]) =>
    invoke("add_tasks", { reqs }) as Promise<{ id: string; ok: boolean; error?: string }[]>,

  pause: (id: string) => invoke("pause_task", { id }) as Promise<void>,
  resume: (id: string) => invoke("resume_task", { id }) as Promise<void>,
  remove: (id: string, deleteFile = false) =>
    invoke("remove_task", { id, deleteFile }) as Promise<void>,
  clearCompleted: () => invoke("clear_completed") as Promise<void>,
  pauseAll: () => invoke("pause_all") as Promise<void>,
  resumeAll: () => invoke("resume_all") as Promise<void>,

  updateSettings: (s: Settings) => invoke("update_settings", { settings: s }) as Promise<Settings>,

  openFile: (path: string) => invoke("open_file", { path }) as Promise<void>,
  showInFolder: (path: string) => invoke("show_in_folder", { path }) as Promise<void>,
  pickFolder: () => invoke("pick_folder") as Promise<string | null>,

  getApiInfo: () =>
    invoke("get_api_info") as Promise<{ port: number; token: string; running: boolean }>,
  // force=true 时忽略本地缓存直接问 GitHub（带 ETag，内容没变不消耗配额）
  checkUpdate: (force?: boolean) =>
    invoke("check_update", { force: force ?? false }) as Promise<UpdateInfo>,
  openUrl: (url: string) => invoke("open_url", { url }) as Promise<void>,
  setTaskSpeedLimit: (id: string, limit: number) =>
    invoke("set_task_speed_limit", { id, limit }) as Promise<void>,

  // 网页抓取 / 嗅探 / HLS
  analyzeUrl: (url: string, referer?: string) =>
    invoke("analyze_url", { url, referer: referer || null }) as Promise<PageAnalysis>,
  grabSite: (opts: {
    url: string;
    save_dir: string;
    max_depth: number;
    max_pages: number;
    include_assets: boolean;
  }) => invoke("grab_site", opts) as Promise<string>,
  cancelGrab: (jobId: string) => invoke("cancel_grab", { jobId }) as Promise<boolean>,
  downloadHls: (url: string, saveDir?: string, fileName?: string) =>
    invoke("download_hls", {
      url,
      save_dir: saveDir || null,
      file_name: fileName || null,
    }) as Promise<string>,

  // 本机网络接口 IP 列表（绑定接口用）
  listInterfaces: () => invoke("list_interfaces") as Promise<string[]>,

  // 用指定 DNS 服务器解析域名（设置页测试用）
  testDns: (servers: string, host: string) =>
    invoke("test_dns", { servers, host }) as Promise<string[]>,

  // 自定义背景图：图片以 base64 传后端落盘，读取时返回 data URL
  saveBackground: (data: string, ext: string) =>
    invoke("save_background", { data, ext }) as Promise<string>,
  readBackground: (path: string) => invoke("read_background", { path }) as Promise<string>,
  clearBackground: () => invoke("clear_background") as Promise<void>,
};

/** 订阅引擎事件 */
export function subscribe(cb: (e: EngineEvent) => void): Promise<UnlistenFn> {
  return listen<EngineEvent>("engine://event", (evt) => cb(evt.payload));
}
