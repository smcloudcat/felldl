/**
 * 任务列表的 normalized store（按 id 存放 + 独立的顺序表）。
 *
 * 之前 `App` 用 `Task[]` 作为唯一状态：每条进度事件都要 `slice()` 整个数组、
 * `findIndex` 找任务、再 `setTasks`。历史任务加载到几千条后，250ms 一次的事件
 * 会让整个页面重渲染，TaskCard 的 `memo` 只能省掉子组件，省不掉父组件的 diff。
 *
 * 现在拆成两层：
 * - `Map<id, Task>`：进度/状态更新是 O(1)，且只通知**订阅了这个 id** 的组件；
 * - `ids` + `counts`：只有新增/删除/换筛选时才变化，App 只订阅这一层。
 *
 * 事件乱序仍按 `rev` 水位处理（与 taskMerge 的规则一致）：
 * 旧 rev 不覆盖新 rev；已终态的任务忽略迟到进度。
 */
import type { Task } from "./types.ts";
import { revOf } from "./taskMerge.ts";

export interface TaskListSnapshot {
  /** 当前窗口内的任务 id，顺序与列表渲染一致 */
  ids: string[];
  /** 当前窗口内每个分类的任务数（侧边栏角标） */
  counts: Record<string, number>;
}

export class TaskStore {
  private map = new Map<string, Task>();
  private ids: string[] = [];
  private snapshot: TaskListSnapshot = { ids: [], counts: {} };
  private listListeners = new Set<() => void>();
  private idListeners = new Map<string, Set<() => void>>();
  /** 当前筛选的准入判断：事件带来的新任务不匹配视图时不插入 */
  private accept: (t: Task) => boolean = () => true;

  /**
   * 页面不可见（切后台、锁屏、切标签页）时只记脏标记，不通知订阅者。
   *
   * 冻结的只是「通知 → React 重渲染」这一段，数据本身照常更新：
   * - 直接丢弃事件会让回前台后的界面停在旧状态，还得靠 resync 补齐；
   * - 逐个通知则在后台白跑几百次 setState，安卓 WebView 里这很费电。
   * 回前台调 `setHidden(false)`，把累积的脏标记一次性刷出去。
   */
  private hidden = false;
  private dirtyIds = new Set<string>();
  private listDirty = false;

  setHidden(h: boolean) {
    if (this.hidden === h) return;
    this.hidden = h;
    if (!h) this.flush();
  }

  /** 回前台：冻结期间累积的变化一次性通知（顺序无关，订阅者只读最新值） */
  flush() {
    const ids = this.dirtyIds;
    this.dirtyIds = new Set();
    for (const id of ids) this.notifyTaskAlways(id);
    if (this.listDirty) {
      this.listDirty = false;
      this.refreshListAlways();
    }
  }

  getSnapshot = (): TaskListSnapshot => this.snapshot;

  getIds = (): string[] => this.snapshot.ids;

  subscribe = (cb: () => void): (() => void) => {
    this.listListeners.add(cb);
    return () => {
      this.listListeners.delete(cb);
    };
  };

  subscribeTask(id: string, cb: () => void): () => void {
    let set = this.idListeners.get(id);
    if (!set) {
      set = new Set();
      this.idListeners.set(id, set);
    }
    set.add(cb);
    return () => {
      const cur = this.idListeners.get(id);
      if (!cur) return;
      cur.delete(cb);
      if (cur.size === 0) this.idListeners.delete(id);
    };
  }

  get(id: string): Task | undefined {
    return this.map.get(id);
  }

  /** 当前窗口的任务（按 ids 顺序）；仅在整表合并/重同步时使用 */
  all(): Task[] {
    const out: Task[] = [];
    for (const id of this.ids) {
      const t = this.map.get(id);
      if (t) out.push(t);
    }
    return out;
  }

  setFilter(fn: (t: Task) => boolean) {
    this.accept = fn;
  }

  /** 整表替换（首屏 / 换筛选 / 重同步合并后的结果） */
  setAll(items: readonly Task[]) {
    this.map = new Map(items.map((t) => [t.id, t]));
    this.ids = items.map((t) => t.id);
    this.refreshList();
  }

  /** 追加下一页：去重并保持引擎给的分页顺序 */
  append(items: readonly Task[]) {
    let added = false;
    for (const t of items) {
      if (this.map.has(t.id)) continue;
      this.map.set(t.id, t);
      this.ids.push(t.id);
      added = true;
    }
    if (added) this.refreshList();
  }

  /** 完整任务事件（taskUpdated），按 rev 水位决定是否覆盖 */
  upsert(t: Task) {
    const cur = this.map.get(t.id);
    if (cur) {
      if (revOf(cur) > revOf(t)) return;
      this.map.set(t.id, t);
      this.notifyTask(t.id);
      // 分类变化会影响侧边栏角标
      if (cur.category !== t.category) this.refreshList();
      return;
    }
    if (!this.accept(t)) return;
    this.map.set(t.id, t);
    // 新建任务在前，与引擎 snapshot 的顺序一致
    this.ids.unshift(t.id);
    this.notifyTask(t.id);
    this.refreshList();
  }

  /** 轻量进度增量：只改字段，不改变列表结构 */
  patch(id: string, d: Partial<Task>) {
    const cur = this.map.get(id);
    if (!cur) return; // 等完整快照/事件到达
    if (cur.state === "completed" || cur.state === "error") return;
    if (revOf(d) < revOf(cur)) return;
    this.map.set(id, { ...cur, ...d });
    this.notifyTask(id);
  }

  remove(id: string) {
    if (!this.map.delete(id)) return;
    const i = this.ids.indexOf(id);
    if (i !== -1) this.ids.splice(i, 1);
    // 卡片可能还挂着：先通知它变成 undefined，再由列表快照触发卸载
    this.notifyTask(id);
    this.refreshList();
  }

  /** 通知单条任务的订阅者；页面不可见时只记脏 */
  private notifyTask(id: string) {
    if (this.hidden) {
      this.dirtyIds.add(id);
      return;
    }
    this.notifyTaskAlways(id);
  }

  private notifyTaskAlways(id: string) {
    const set = this.idListeners.get(id);
    if (!set) return;
    for (const cb of set) cb();
  }

  /** 重算列表快照；页面不可见时只记脏，回前台再算一次 */
  private refreshList() {
    if (this.hidden) {
      this.listDirty = true;
      return;
    }
    this.refreshListAlways();
  }

  private refreshListAlways() {
    const counts: Record<string, number> = {};
    for (const id of this.ids) {
      const t = this.map.get(id);
      if (t?.category) counts[t.category] = (counts[t.category] ?? 0) + 1;
    }
    // ids 与 counts 都换新引用，useSyncExternalStore 才能看到变化
    this.snapshot = { ids: this.ids.slice(), counts };
    for (const cb of this.listListeners) cb();
  }
}

export const taskStore = new TaskStore();
