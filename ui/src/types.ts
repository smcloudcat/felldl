/** 与 Rust 端 model.rs 保持一致的镜像类型 */

export type TaskState =
  | "queued"
  | "probing"
  | "downloading"
  | "paused"
  | "completed"
  | "error"
  | "scheduled";

export interface Segment {
  index: number;
  start: number;
  end: number;
  done: number;
}

export type TaskKind = "http" | "bt" | "hls" | "ftp" | "sftp";

export interface Category {
  name: string;
  extensions: string[];
  site_pattern?: string | null;
  save_dir?: string | null;
}

export interface Task {
  id: string;
  url: string;
  kind: TaskKind;
  mirrors: string[];
  file_name: string;
  save_dir: string;
  save_path: string;
  size: number | null;
  downloaded: number;
  state: TaskState;
  error: string | null;
  /** 稳定机器码：http.not_found / net.timeout / disk.full … 文案可以变，它不变 */
  error_code?: string | null;
  /** 自动重试是否有意义；null/缺省表示当前没有错误 */
  retryable?: boolean | null;
  /**
   * 任务版本号：后端每次改动任务就 +1。
   * 用来丢弃倒序到达的旧快照 / 旧进度（见 taskMerge.ts）。
   */
  rev?: number;
  connections: number;
  segments: Segment[];
  supports_range: boolean;
  etag: string | null;
  last_modified: string | null;
  content_type: string | null;
  referer: string | null;
  cookie: string | null;
  /** 凭据归属 origin（scheme://host:port）：镜像/跨域跳转不会带认证信息 */
  auth_origin?: string;
  user_agent: string | null;
  speed_limit: number;
  scheduled_at: number | null;
  created_at: number;
  completed_at: number | null;
  speed: number;
  peers: number;
  upload_speed: number;
  bt_info_hash: string | null;
  category: string | null;
  username: string | null;
  password: string | null;
  interval_secs: number;
}

export interface ProxyConfig {
  enabled: boolean;
  kind: "http" | "socks5" | string;
  host: string;
  port: number;
  username: string;
  password: string;
  no_proxy: string;
}

export interface Settings {
  download_dir: string;
  max_concurrent_tasks: number;
  default_connections: number;
  global_speed_limit: number;
  proxy: ProxyConfig;
  theme: "light" | "dark" | "system";
  api_enabled: boolean;
  api_port: number;
  api_token: string;
  notify_on_complete: boolean;
  user_agent: string;
  auto_start: boolean;
  categories: Category[];
  after_complete: "none" | "shutdown" | "sleep" | string;
  bind_ip: string;
  browser_panel: "notify" | "minimize" | "none" | string;
  ui_skin: "blue" | "purple" | "green" | "orange" | string;
  btn_style: "rounded" | "flat" | "outline" | string;
  task_meta: string[];
  dns_mode: "system" | "custom" | string;
  dns_servers: string;
  /** 自定义背景图本地路径（空 = 不使用） */
  ui_background: string;
  /** 背景模糊半径 px（0~40） */
  ui_blur: number;
  /** 背景遮罩浓度 %（0~90） */
  ui_dim: number;
  /** 面板不透明度 %（40~100） */
  ui_glass: number;
  mirror_enabled: boolean;
  mirror_mode: "auto" | "ghfast" | "ghproxy" | "hf" | "custom" | string;
  mirror_custom: string;
}

export interface AddTaskRequest {
  url: string;
  save_dir?: string | null;
  file_name?: string | null;
  connections?: number | null;
  speed_limit?: number | null;
  scheduled_at?: number | null;
  referer?: string | null;
  cookie?: string | null;
  headers?: Record<string, string>;
  mirrors?: string[];
  username?: string | null;
  password?: string | null;
  interval_secs?: number;
  category?: string | null;
}

// ── 网页抓取 / 嗅探 ──
export interface GrabResource {
  url: string;
  kind: string;
  name: string;
}

export interface PageAnalysis {
  url: string;
  title: string;
  resources: GrabResource[];
  links: string[];
}

export type FilterKey =
  | "all"
  | "downloading"
  | "completed"
  | "paused"
  | "error";

export function isActive(s: TaskState): boolean {
  return s === "downloading" || s === "probing";
}

// ── 检查更新 ──
export interface UpdateAsset {
  name: string;
  download_url: string;
  size: number;
}

export interface UpdateInfo {
  current_version: string;
  latest_version: string;
  has_update: boolean;
  release_url: string;
  notes: string;
  published_at: string;
  assets: UpdateAsset[];
  /** 结果来自缓存或降级来源时的说明（正常联网查询时不返回） */
  note?: string;
}

// ── 分页与统计 ──
// 历史任务可能上万条：list_tasks 只回当前筛选下的一页，侧边栏计数由 task_stats 给。
export interface TaskPage {
  items: Task[];
  /** 当前筛选条件下的总条数（不是全库） */
  total: number;
  has_more: boolean;
}

export interface TaskCounts {
  all: number;
  downloading: number;
  completed: number;
  paused: number;
  error: number;
}

export interface TaskStats {
  counts: TaskCounts;
  /** 真正在传输（downloading/probing）的条数，不含排队 */
  active: number;
  total_speed: number;
}
