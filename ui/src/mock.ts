/**
 * 浏览器预览用的假引擎
 *
 * 目的：不启动 Tauri 也能 `npm run dev` 看到完整界面和动效，方便调 UI。
 * 逻辑上模拟真实引擎的行为（下载进度、速度、暂停、完成）。
 */
import type { AddTaskRequest, Settings, Task } from "./types";

type Listener = (e: { payload: any }) => void;
const listeners = new Map<string, Set<Listener>>();

function listenersOf(evt: string) {
  let set = listeners.get(evt);
  if (!set) {
    set = new Set();
    listeners.set(evt, set);
  }
  return set;
}

/** 引擎事件（App 订阅的是 engine://event） */
function emit(payload: any) {
  listenersOf("engine://event").forEach((cb) => cb({ payload }));
}

/** 按事件名发事件（更新包下载进度等） */
function emitEvent(evt: string, payload: any) {
  listenersOf(evt).forEach((cb) => cb({ payload }));
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
    category: null,
    username: null,
    password: null,
    interval_secs: 0,
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

// `?tasks=N` 造 N 条历史任务：用来在浏览器里验证「挂载量与历史规模解耦」，
// 不接真机、不跑真引擎也能复现「上千条记录」的列表压力。
{
  const q =
    typeof window !== "undefined" ? new URLSearchParams(window.location.search) : null;
  const extra = Number(q?.get("tasks") ?? 0);
  if (Number.isFinite(extra) && extra > 0) {
    for (let i = 0; i < extra; i++) {
      const t = makeTask(
        `https://example.com/archive/history-${i}.bin`,
        `history-${i}.bin`,
        10_000_000 + i
      );
      t.downloaded = t.size ?? 0;
      t.state = "completed";
      t.completed_at = now() - 60_000 * (i + 1);
      t.created_at = now() - 60_000 * (i + 10);
      tasks.push(t);
    }
  }
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
  categories: [
    { name: "视频", extensions: ["mp4", "mkv", "webm"], site_pattern: null, save_dir: null },
    { name: "音乐", extensions: ["mp3", "flac"], site_pattern: null, save_dir: null },
    { name: "文档", extensions: ["pdf", "docx"], site_pattern: null, save_dir: null },
    { name: "压缩包", extensions: ["zip", "7z"], site_pattern: null, save_dir: null },
    { name: "程序", extensions: ["exe", "apk"], site_pattern: null, save_dir: null },
  ],
  after_complete: "none",
  bind_ip: "",
  browser_panel: "notify",
  ui_skin: "blue",
  btn_style: "rounded",
  task_meta: ["size", "speed", "eta", "mirrors", "date"],
  dns_mode: "system",
  dns_servers: "",
  ui_background: "",
  ui_blur: 18,
  ui_dim: 32,
  ui_glass: 78,
  mirror_enabled: false,
  mirror_mode: "auto",
  mirror_custom: "",
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

/**
 * 浏览器验收脚本用：直接操作假任务数组，**不广播事件**。
 *
 * 这样可以让「UI 里仍看得见、引擎侧已不存在」的错位发生，
 * 用来验证批量操作的失败明细（真实场景是别的窗口删了同一条）。
 * 只存在于 mock，不参与 Tauri 构建路径。
 */
export const __devTasks = tasks;

/** 假抓取任务：让浏览器预览也能看到「进度 + 取消」，不必起真引擎 */
const grabJobs = new Map<string, { id: string; url: string; done: number; total: number; timer: any }>();

function emitGrab(job: { id: string; url: string; done: number; total: number }, finished: boolean) {
  emitEvent("engine://event", {
    type: "grabStatus",
    payload: { id: job.id, url: job.url, done: job.done, total: job.total, finished },
  });
}

export const mockEngine = {
  async invoke(cmd: string, args?: any): Promise<any> {
    await sleep(60);
    switch (cmd) {
      case "list_tasks": {
        // 与引擎一致的筛选语义：下载中含排队/探测，暂停含定时
        const filter: string = args?.filter ?? "all";
        const category: string | null = args?.category ?? null;
        const offset: number = args?.offset ?? 0;
        const limit: number = args?.limit ?? 80;
        const matched = tasks
          .filter((t) => (category ? t.category === category : true))
          .filter((t) => {
            switch (filter) {
              case "downloading":
                return t.state === "downloading" || t.state === "probing" || t.state === "queued";
              case "completed":
                return t.state === "completed";
              case "paused":
                return t.state === "paused" || t.state === "scheduled";
              case "error":
                return t.state === "error";
              default:
                return true;
            }
          })
          .sort((a, b) => b.created_at - a.created_at || (a.id < b.id ? 1 : -1));
        return {
          items: matched.slice(offset, offset + limit).map((t) => ({ ...t })),
          total: matched.length,
          has_more: offset + limit < matched.length,
        };
      }
      case "task_stats": {
        const counts = { all: tasks.length, downloading: 0, completed: 0, paused: 0, error: 0 };
        let active = 0;
        let total_speed = 0;
        for (const t of tasks) {
          if (t.state === "downloading" || t.state === "probing") {
            counts.downloading += 1;
            active += 1;
            total_speed += t.speed || 0;
          } else if (t.state === "queued") counts.downloading += 1;
          else if (t.state === "completed") counts.completed += 1;
          else if (t.state === "paused") counts.paused += 1;
          else if (t.state === "error") counts.error += 1;
        }
        return { counts, active, total_speed };
      }
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
      case "batch_tasks": {
        // 与真实引擎一致：逐条执行，不存在的 id 进 failed，不影响其它条
        const action: string = args.action;
        const ids: string[] = Array.isArray(args.ids) ? args.ids : [];
        const ok: string[] = [];
        const failed: { id: string; code: string; message: string; retryable: boolean }[] = [];
        const seen = new Set<string>();
        for (const id of ids) {
          if (seen.has(id)) continue;
          seen.add(id);
          const t = tasks.find((x) => x.id === id);
          if (!t) {
            failed.push({
              id,
              code: "task.not_found",
              message: "任务不存在（可能已被其它窗口删除）",
              retryable: false,
            });
            continue;
          }
          if (action === "remove") {
            tasks.splice(tasks.indexOf(t), 1);
            emit({ type: "taskRemoved", payload: id });
          } else if (action === "pause") {
            if (t.state === "downloading" || t.state === "probing" || t.state === "queued") {
              t.state = "paused";
              t.speed = 0;
              emit({ type: "taskUpdated", payload: { ...t } });
            }
          } else {
            if (t.state !== "completed") {
              t.state = "downloading";
              emit({ type: "taskUpdated", payload: { ...t } });
            }
          }
          ok.push(id);
        }
        return { ok, failed };
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
      case "app_version":
        return "0.3.8";
      case "all_files_access":
        return "granted";
      case "request_all_files_access":
        return "granted";
      case "dir_writable":
        return true;
      case "check_update":
        // 预览模式固定返回「有新版本」，方便看更新按钮与进度条
        return {
          current_version: "0.3.9",
          latest_version: "0.3.9",
          has_update: true,
          release_url: "https://github.com/smcloudcat/felldl/releases",
          notes: "预览模式：这是假的更新说明，真机里会显示 Release 正文。",
          published_at: "2026-11-25T00:00:00Z",
          assets: [
            {
              name: "Felldl-0.3.9-win-x64.exe",
              download_url: "https://example.invalid/Felldl-0.3.9-win-x64.exe",
              size: 11000000,
            },
          ],
        };
      case "download_update": {
        const total = 11000000;
        for (let i = 1; i <= 10; i++) {
          await new Promise((r) => setTimeout(r, 120));
          emitEvent("update://progress", {
            downloaded: (total / 10) * i,
            total,
            percent: i * 10,
          });
        }
        // 预览模式：返回一个假的「已校验」token（真实链路只接受后端登记过的 token）
        return "mock-verified-token";
      }
      case "apply_update":
        // 预览模式不重启，走「已下载，请手动安装」分支
        return "manual";
      case "save_background":
        // 浏览器预览模式不落盘，直接回一个假路径
        return "/mock/bg-" + Date.now() + "." + (args?.ext || "png");
      case "read_background":
        return "";
      case "clear_background":
        return null;
      case "get_api_info":
        return { port: settings.api_port, token: settings.api_token, running: true };
      case "list_interfaces":
        return ["192.168.123.137", "10.8.0.2", "fe80::1"];
      case "test_dns":
        return ["93.184.216.34", "2606:2800:220:1:248:1893:25c8:1946"];
      case "open_file":
      case "show_in_folder":
      case "analyze_url": {
        const url: string = args?.url ?? "https://example.com/";
        const host = url.replace(/^https?:\/\//, "").split("/")[0] || "example.com";
        return {
          url,
          title: `示例页面 · ${host}`,
          resources: [
            { url: `${url.replace(/\/$/, "")}/media/movie.mp4`, kind: "video", name: "movie.mp4" },
            { url: `${url.replace(/\/$/, "")}/media/poster.jpg`, kind: "image", name: "poster.jpg" },
            { url: `${url.replace(/\/$/, "")}/media/stream.m3u8`, kind: "hls", name: "stream.m3u8" },
          ],
          links: [`${url.replace(/\/$/, "")}/about.html`, `${url.replace(/\/$/, "")}/docs/index.html`],
        };
      }
      case "grab_site": {
        const id = `grab-${++counter}`;
        const url: string = args?.url ?? "https://example.com/";
        const total: number = Math.max(1, Math.min(Number(args?.max_pages) || 10, 8));
        const job = { id, url, done: 0, total, timer: 0 as any };
        grabJobs.set(id, job);
        emitGrab(job, false);
        // 每 700ms 存一页：慢到足以在界面上看到进度条，也不至于让验收脚本等太久
        job.timer = setInterval(() => {
          job.done += 1;
          if (job.done >= job.total) {
            clearInterval(job.timer);
            grabJobs.delete(id);
            emitGrab(job, true);
          } else {
            emitGrab(job, false);
          }
        }, 700);
        return id;
      }
      case "grab_jobs":
        return [...grabJobs.values()].map(({ id, url, done, total }) => ({
          id,
          url,
          done,
          total,
          finished: false,
        }));
      case "cancel_grab": {
        const job = grabJobs.get(args?.jobId);
        if (!job) return false;
        clearInterval(job.timer);
        grabJobs.delete(job.id);
        emitGrab(job, true);
        return true;
      }
      case "download_hls":
        return `mock-hls-${++counter}`;
      case "set_task_speed_limit":
        return null;
      default:
        return null;
    }
  },
  async listen(evt: string, cb: (e: { payload: any }) => void) {
    const set = listenersOf(evt);
    set.add(cb);
    return () => set.delete(cb);
  },
};
