/**
 * 快照 / 事件的乱序合并
 *
 * 引擎每次改动任务都会把 `Task.rev` 加一，事件里带的就是那一刻的版本号。
 * 界面上只有一处会「整表替换」——启动时（以及初始化失败重试时）的 `list_tasks`。
 * 那份快照是某个时刻的拷贝，等它到达时事件可能已经把任务推进得更远；
 * 直接 `setTasks(snapshot)` 就会把界面打回旧状态：进度倒退、
 * 「已完成」变回「下载中」，而且在下一条事件到达前一直错着。
 *
 * 这里的规则（水位语义）：
 *   1. 已收到删除事件的任务，任何快照都不许把它带回来（id 是 UUID，永不复用）；
 *   2. 快照里 rev 比本地更小的，保留本地（本地来自更晚的事件）；
 *   3. 本地有、快照里没有的任务：只有「取快照期间收到过事件」的才保留
 *      （说明它是快照生成之后才出现的），其余视为快照已被权威删除；
 *   4. 其余一律以快照为准。
 */
import type { Task } from "./types";

export interface SnapshotMergeContext {
  /** 取快照期间收到过 taskRemoved 的任务 id */
  removed: ReadonlySet<string>;
  /** 取快照期间收到过任何事件（更新 / 进度）的任务 id */
  touched: ReadonlySet<string>;
}

/** 缺省 0：旧快照 / 浏览器预览模式的假数据可能没有这个字段 */
export function revOf(t: Pick<Task, "rev"> | Partial<Task>): number {
  return typeof t.rev === "number" ? t.rev : 0;
}

export function mergeSnapshot(
  prev: readonly Task[],
  snapshot: readonly Task[],
  ctx: SnapshotMergeContext
): Task[] {
  const prevById = new Map(prev.map((t) => [t.id, t]));
  const snapIds = new Set(snapshot.map((t) => t.id));
  const out: Task[] = [];

  for (const s of snapshot) {
    if (ctx.removed.has(s.id)) continue;
    const cur = prevById.get(s.id);
    out.push(cur && revOf(cur) > revOf(s) ? cur : s);
  }
  for (const cur of prev) {
    if (snapIds.has(cur.id) || ctx.removed.has(cur.id)) continue;
    // 快照生成之后才出现（或期间被推进过）的任务：不能因为「快照里没有」就丢掉
    if (ctx.touched.has(cur.id)) out.push(cur);
  }

  // 与 snapshot() 的顺序保持一致：新建的在前
  out.sort((a, b) => (b.created_at ?? 0) - (a.created_at ?? 0));
  return out;
}
