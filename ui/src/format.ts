/** 格式化与文件类型工具 */

export function formatBytes(n: number | null | undefined): string {
  if (n == null || !isFinite(n)) return "—";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB", "PB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

export function formatSpeed(n: number): string {
  if (!n) return "—";
  return `${formatBytes(n)}/s`;
}

export function formatPercent(p: number): string {
  if (p <= 0) return "0%";
  if (p >= 1) return "100%";
  if (p < 0.001) return "<0.1%";
  return `${(p * 100).toFixed(1)}%`;
}

/** 剩余时间估算 */
export function formatEta(downloaded: number, size: number | null, speed: number): string {
  if (!size || !speed || speed <= 0) return "—";
  const remain = size - downloaded;
  if (remain <= 0) return "—";
  const sec = remain / speed;
  if (sec < 1) return "即将完成";
  if (sec < 60) return `${Math.ceil(sec)} 秒`;
  if (sec < 3600) return `${Math.floor(sec / 60)} 分 ${Math.ceil(sec % 60)} 秒`;
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  return `${h} 小时 ${m} 分`;
}

export function formatDate(ms: number): string {
  const d = new Date(ms);
  const pad = (v: number) => String(v).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function formatRelative(ms: number): string {
  const diff = Date.now() - ms;
  if (diff < 60_000) return "刚刚";
  if (diff < 3600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86400_000) return `${Math.floor(diff / 3600_000)} 小时前`;
  if (diff < 7 * 86400_000) return `${Math.floor(diff / 86400_000)} 天前`;
  return formatDate(ms);
}

export type FileKind = "video" | "audio" | "image" | "archive" | "doc" | "app" | "code" | "other";

const EXT_MAP: Record<string, FileKind> = {};
const add = (kind: FileKind, exts: string[]) => exts.forEach((e) => (EXT_MAP[e] = kind));

add("video", ["mp4", "mkv", "avi", "mov", "webm", "flv", "wmv", "m4v", "ts", "rmvb", "mpg", "mpeg"]);
add("audio", ["mp3", "flac", "wav", "aac", "ogg", "m4a", "wma", "opus", "ape"]);
add("image", ["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "ico", "tiff", "avif", "heic"]);
add("archive", ["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "zst", "iso", "tgz", "cab"]);
add("doc", ["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "csv", "epub", "mobi"]);
add("app", ["exe", "msi", "apk", "dmg", "pkg", "deb", "rpm", "appimage", "ipa", "aab"]);
add("code", ["js", "ts", "py", "rs", "go", "java", "c", "cpp", "h", "json", "xml", "html", "css", "sh", "yml", "yaml", "toml"]);

export function fileKind(name: string): FileKind {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return EXT_MAP[ext] ?? "other";
}

export function fileExt(name: string): string {
  const parts = name.split(".");
  if (parts.length < 2) return "FILE";
  const ext = parts.pop()!;
  return ext.slice(0, 4).toUpperCase();
}

/** 简易 className 拼接 */
export function cx(...args: (string | false | null | undefined)[]): string {
  return args.filter(Boolean).join(" ");
}

/** 状态中文名 */
export const STATE_LABEL: Record<string, string> = {
  queued: "排队中",
  probing: "连接中",
  downloading: "下载中",
  paused: "已暂停",
  completed: "已完成",
  error: "出错",
  scheduled: "定时等待",
};

/** 是否处于活跃下载状态 */
export function isActive(state: string): boolean {
  return state === "downloading" || state === "probing";
}
