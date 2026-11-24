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

export type TaskKind = "http" | "bt" | "hls";

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
  connections: number;
  segments: Segment[];
  supports_range: boolean;
  etag: string | null;
  last_modified: string | null;
  content_type: string | null;
  referer: string | null;
  cookie: string | null;
  user_agent: string | null;
  speed_limit: number;
  scheduled_at: number | null;
  created_at: number;
  completed_at: number | null;
  speed: number;
  peers: number;
  upload_speed: number;
  bt_info_hash: string | null;
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
}
