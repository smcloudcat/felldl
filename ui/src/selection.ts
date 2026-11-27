/**
 * 多选状态（纯函数，可单测）。
 *
 * 为什么单独抽出来：批量操作最容易错的地方不是命令调用，而是「选中集合」与
 * 「当前列表」不一致 —— 用户在别的窗口删掉任务、切换筛选、分页追加之后，
 * 集合里留着看不见的 id，批量删除就成了「删了 3 条却说删了 5 条」。
 * 这里只做三件事：切换、全选（按当前可见顺序）、按存活集合裁剪。
 */

/** 切换单条：返回新集合，React 才能看到引用变化 */
export function toggleSelected(
  cur: ReadonlySet<string>,
  id: string,
): Set<string> {
  const next = new Set(cur);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  return next;
}

/** 全选 / 取消全选：已全选时再点一次清空（用户预期是「反选」，不是永远只会加） */
export function toggleAll(
  cur: ReadonlySet<string>,
  ids: readonly string[],
): Set<string> {
  if (ids.length === 0) return new Set();
  const all = ids.every((id) => cur.has(id));
  return all ? new Set() : new Set(ids);
}

/**
 * 按「当前仍然存在的 id」裁剪。
 *
 * 没有任何变化时**返回原集合**（同一引用）：调用方直接 setState 也不会触发重渲染，
 * 否则每条进度事件都会让列表外壳重渲染一次。
 */
export function pruneSelection(
  cur: ReadonlySet<string>,
  alive: Iterable<string>,
): ReadonlySet<string> {
  if (cur.size === 0) return cur;
  const keep = new Set<string>();
  for (const id of alive) if (cur.has(id)) keep.add(id);
  return keep.size === cur.size ? cur : keep;
}

/** 批量操作后要保留的选中项：失败项留给用户重试，成功项清掉 */
export function keepFailed(failed: readonly { id: string }[]): Set<string> {
  return new Set(failed.map((f) => f.id));
}

export type BatchAction = "pause" | "resume" | "remove";

/** 与引擎 `TaskState::is_active` + 排队/定时保持一致的「可暂停」集合 */
const PAUSABLE = new Set(["downloading", "probing", "queued", "scheduled"]);
/** 「继续」只对停着或失败的任务有意义；已完成/正在下的不能算「恢复了」 */
const RESUMABLE = new Set(["paused", "error", "queued", "scheduled"]);

/**
 * 过滤出这次动作真正能作用于的任务。
 *
 * 引擎对「已完成任务调 pause」是无操作并返回成功，如果原样把它算进
 * 「已暂停 N 个」，用户会看到一个不实的数字。这里按状态先筛一遍，
 * 被跳过的条目不计入成功数，也不报失败：它们本来就不需要这个动作。
 */
export function filterEligible(
  action: BatchAction,
  items: readonly { id: string; state: string }[],
): string[] {
  if (action === "remove") return items.map((t) => t.id);
  const set = action === "pause" ? PAUSABLE : RESUMABLE;
  return items.filter((t) => set.has(t.state)).map((t) => t.id);
}
