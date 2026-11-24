/**
 * 浏览器预览用的假引擎
 *
 * 目的：不启动 Tauri 也能 `npm run dev` 看到完整界面和动效，方便调 UI。
 * 逻辑上模拟真实引擎的行为（下载进度、速度、暂停、完成）。
 */
import type { AddTaskRequest, Settings, Task } from "./types";

const listeners = new Set<(e: { payload: any }) => void>();

function emit(payload: any) {
  listeners.forEach((cb) => cb({ payload }));
}

let counter = 0;
const now = () => Date.now();

function makeTask(url: string, name: string, size: number): Task {
  return {
    id: `mock-${++counter}`,
    url,
    kind: "http",
    mirrors: [],
    file_name: name,
    save_dir: "/Users/you/Downloads",
    save_path: `/Users/you/Downloads/${name}`,
    size,
    downloaded: 0,
    state: "downloading",
    error: null,
    connections: 16,
    segments: Array.from({ length: 8 }, (_, i) => ({
      index: i,
      start: Math.floor((size / 8) * i),
      end: Math.floor((size / 8) * (i + 1)) - 1,
      done: 0,
    })),
    supports_range: true,
    etag: "mock-etag",
    last_modified: null,
    content_type: "application/octet-stream",
    referer: null,
    cookie: null,
    user_agent: null,
    speed_limit: 0,
    scheduled_at: null,
    created_at: now(),
    completed_at: null,
    speed: 0,
    peers: 0,
    upload_speed: 0,
    bt_info_hash: null,
  };
}

const tasks: Task[] = [
  makeTask(
    "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-releases/24.04/ubuntu-24.04.3-live-server-amd64.iso",
    "ubuntu-24.04.3-live-server-amd64.iso",
    3_303_444_480
  ),
];

{
  const t2 = makeTask("https://example.com/Blender-4.2.3.dmg", "Blender-4.2.3.dmg", 312_000_000);
  t2.downloaded = 312_000_000;
  t2.state = "completed";
  t2.completed_at = now() - 3600_000;
  t2.created_at = now() - 7200_000;
  t2.segments = t2.segments.map((s) => ({ ...s, done: s.end - s.start + 1 }));
  tasks.push(t2);

  const t3 = makeTask("https://example.com/项目资料.zip", "项目资料.zip", 88_000_000);
  t3.downloaded = 27_500_000;
  t3.state = "paused";
  t3.segments = t3.segments.map((s, i) => ({
    ...s,
    done: i < 2 ? s.end - s.start + 1 : 0,
  }));
  tasks.push(t3);

  const t4 = makeTask("https://example.com/broken-file.tar.gz", "broken-file.tar.gz", 5_000_000);
  t4.state = "error";
  t4.error = "连接提前结束（1048576/5000000）";
  tasks.push(t4);
}

let settings: Settings = {
  download_dir: "/Users/you/Downloads",
  max_concurrent_tasks: 3,
  default_connections: 16,
  global_speed_limit: 0,
  proxy: {
    enabled: false,
    kind: "http",
    host: "",
    port: 7890,
    username: "",
    password: "",
    no_proxy: "",
  },
  theme: "system",
  api_enabled: true,
  api_port: 8899,
  api_token: "mock-token-abcdef123456",
  notify_on_complete: true,
  user_agent:
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
  auto_start: true,
};

// 每 400ms 推进下载进度
setInterval(() => {
  let changed = false;
  for (const t of tasks) {
    if (t.state !== "downloading" || !t.size) continue;
    // 模拟 6~13 MB/s 抖动
    const speed = (6 + Math.random() * 7) * 1024 * 1024;
    t.speed = speed;
    const delta = Math.floor(speed * 0.4);
    t.downloaded = Math.min(t.size, t.downloaded + delta);

    // 同步分片进度
    let remaining = t.downloaded;
    for (const s of t.segments) {
      const len = s.end - s.start + 1;
      s.done = Math.max(0, Math.min(len, remaining));
      remaining -= s.done;
    }

    if (t.downloaded >= t.size) {
      t.state = "completed";
      t.completed_at = now();
      t.speed = 0;
      t.segments = t.segments.map((s) => ({ ...s, done: s.end - s.start + 1 }));
    }
    changed = true;
    emit({ type: "taskUpdated", payload: { ...t } });
  }
  if (!changed) return;
}, 400);

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export const mockEngine = {
  async invoke(cmd: string, args?: any): Promise<any> {
    await sleep(60);
    switch (cmd) {
      case "list_tasks":
        return tasks.map((t) => ({ ...t }));
      case "get_settings":
        return { ...settings };
      case "add_task": {
        const req: AddTaskRequest = args.req;
        const name = req.file_name || req.url.split("/").pop() || "download.bin";
        const t = makeTask(req.url, name, 240_000_000);
        t.state = req.scheduled_at ? "scheduled" : "downloading";
        t.scheduled_at = req.scheduled_at ?? null;
        tasks.unshift(t);
        emit({ type: "taskUpdated", payload: { ...t } });
        return t.id;
      }
      case "add_tasks":
        return [
          {
            id: `mock-${++counter}`,
            ok: true,
          },
        ];
      case "pause_task": {
        const t = tasks.find((x) => x.id === args.id);
        if (t) {
          t.state = "paused";
          t.speed = 0;
          emit({ type: "taskUpdated", payload: { ...t } });
        }
        return null;
      }
      case "resume_task": {
        const t = tasks.find((x) => x.id === args.id);
        if (t) {
          t.state = "downloading";
          emit({ type: "taskUpdated", payload: { ...t } });
        }
        return null;
      }
      case "remove_task": {
        const i = tasks.findIndex((x) => x.id === args.id);
        if (i >= 0) {
          tasks.splice(i, 1);
          emit({ type: "taskRemoved", payload: args.id });
        }
        return null;
      }
      case "pause_all":
        tasks.forEach((t) => {
          if (t.state === "downloading") {
            t.state = "paused";
            t.speed = 0;
            emit({ type: "taskUpdated", payload: { ...t } });
          }
        });
        return null;
      case "resume_all":
        tasks.forEach((t) => {
          if (t.state === "paused" || t.state === "error") {
            t.state = "downloading";
            emit({ type: "taskUpdated", payload: { ...t } });
          }
        });
        return null;
      case "clear_completed":
        for (let i = tasks.length - 1; i >= 0; i--) {
          if (tasks[i].state === "completed") {
            emit({ type: "taskRemoved", payload: tasks[i].id });
            tasks.splice(i, 1);
          }
        }
        return null;
      case "update_settings":
        settings = { ...args.settings };
        emit({ type: "settingsUpdated", payload: { ...settings } });
        return { ...settings };
      case "pick_folder":
        return "/Users/you/Downloads";
      case "get_api_info":
        return { port: settings.api_port, token: settings.api_token, running: true };
      case "open_file":
      case "show_in_folder":
      case "set_task_speed_limit":
        return null;
      default:
        return null;
    }
  },
  async listen(_evt: string, cb: (e: { payload: any }) => void) {
    listeners.add(cb);
    return () => listeners.delete(cb);
  },
};
