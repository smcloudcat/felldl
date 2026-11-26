/**
 * Tauri 命令错误 → 结构化对象
 *
 * 后端命令现在返回 `{ code, message, retryable }`（见 `src-tauri/src/ipc.rs`），
 * 但开发时可能连着旧构建、或者别的命令仍然返回裸字符串，所以这里做一次归一化：
 *
 * * 对象 → 直接取字段（缺字段用保守默认值）；
 * * 字符串 `code: message` → 拆开，前端不必再写 `startsWith("install.")`；
 * * 其它 → 整串当文案，code 用调用方给的兜底值。
 *
 * 这样 UI 只用判断 `code`，文案改多少次都不会影响逻辑。
 */

export interface IpcError {
  /** 稳定机器码，如 `install.need_permission` / `update.network` */
  code: string;
  /** 给人看的完整说明 */
  message: string;
  /** 什么都不改、再点一次有没有可能成功 */
  retryable: boolean;
}

/** 点分小写标识 + 冒号，避免把 `https://…` 或中文句子误拆 */
const CODED = /^([a-z][a-z0-9_]*(?:\.[a-z0-9_]+)+):\s*([\s\S]*)$/;

export function toIpcError(e: unknown, fallbackCode = "unknown"): IpcError {
  if (e && typeof e === "object") {
    const o = e as Record<string, unknown>;
    if (typeof o.code === "string" && o.code) {
      const message =
        typeof o.message === "string" && o.message ? o.message : String(o.code);
      return { code: o.code, message, retryable: o.retryable === true };
    }
  }

  const raw =
    typeof e === "string"
      ? e
      : e instanceof Error
        ? e.message
        : e === null || e === undefined
          ? ""
          : String(e);

  const m = CODED.exec(raw.trim());
  if (m) return { code: m[1], message: m[2] || m[1], retryable: false };
  return { code: fallbackCode, message: raw || fallbackCode, retryable: false };
}
