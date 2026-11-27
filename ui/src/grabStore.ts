/**
 * 整站抓取任务的全局视图。
 *
 * 抓取任务真正跑在引擎里（`Engine::grab_jobs`），UI 只是一个观察者：
 * - 事件 `grabStatus` 推增量（开始 / 每存一页 / 收尾）；
 * - 事件会因通道溢出而丢，且离开页面期间到达的事件前端可能没人接，
 *   所以重新进入抓取页时还要用 `grab_jobs` 查一次当前活动任务。
 *
 * 这个 store 不持有任何「分析结果 / 表单草稿」——那些留在页面组件里，
 * 页面切走时靠「保持挂载」保住，避免把两种生命周期混在一起。
 */
import type { GrabStatus } from "./types";

class GrabStore {
  private jobs = new Map<string, GrabStatus>();
  private snapshot: GrabStatus[] = [];
  private listeners = new Set<() => void>();

  subscribe = (cb: () => void): (() => void) => {
    this.listeners.add(cb);
    return () => {
      this.listeners.delete(cb);
    };
  };

  getSnapshot = (): GrabStatus[] => this.snapshot;

  /** 应用一条进度事件；`finished` 表示收尾，直接从活动表移除 */
  apply = (s: GrabStatus): void => {
    if (!s || !s.id) return;
    if (s.finished) {
      if (!this.jobs.delete(s.id)) return;
    } else {
      const prev = this.jobs.get(s.id);
      // 迟到/乱序的旧进度不覆盖新进度（同名事件可能攒在队列里）
      if (prev && prev.done > s.done) return;
      this.jobs.set(s.id, s);
    }
    this.commit();
  };

  /** 用一次性查询的结果重建：只保留未结束的任务 */
  setAll = (list: GrabStatus[]): void => {
    this.jobs.clear();
    for (const s of list) if (s && s.id && !s.finished) this.jobs.set(s.id, s);
    this.commit();
  };

  /** 本地立即移除（取消成功后不等收尾事件，UI 不必等一个来回） */
  drop = (id: string): void => {
    if (this.jobs.delete(id)) this.commit();
  };

  private commit(): void {
    const next = [...this.jobs.values()];
    if (sameJobs(this.snapshot, next)) return;
    this.snapshot = next;
    for (const cb of this.listeners) cb();
  }
}

/** 快照相同就保持同一引用：useSyncExternalStore 靠它跳过无意义的重渲染 */
function sameJobs(a: GrabStatus[], b: GrabStatus[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (
      a[i].id !== b[i].id ||
      a[i].done !== b[i].done ||
      a[i].total !== b[i].total
    )
      return false;
  }
  return true;
}

export const grabStore = new GrabStore();
