/**
 * 设置页草稿（事务语义）。
 *
 * 之前草稿是 SettingsPage 的局部 state：切到下载页再切回来，组件重挂，
 * 改了半天的端口/代理/背景全部消失；而「放弃」只能回到上一次挂载时的值。
 *
 * 这里把草稿提到模块级：
 * - 离开页面 ≠ 丢弃草稿，回来接着改；
 * - 后端推来新的 settings 时，没有脏改动就整体对齐，有脏改动只更新「已保存基线」，
 *   绝不静默覆盖用户正在编辑的内容；
 * - 保存成功由 commit() 收口；放弃由 discard() 回到基线。
 *
 * 草稿只存在内存里：未提交的改动不应该跟着应用重启活过来（尤其是背景图路径，
 * 可能指向已被清理的文件）。需要跨重启保留就属于「保存」而不是「草稿」。
 */
import type { Settings } from "./types";

export interface SettingsDraftState {
  /** 已保存基线（后端最近一次确认的值） */
  base: Settings | null;
  /** 正在编辑的值 */
  draft: Settings | null;
  /** 与基线是否有差异 */
  dirty: boolean;
}

const EMPTY: SettingsDraftState = { base: null, draft: null, dirty: false };

/** 归一化后比较：只关心字段值，不关心对象引用 */
function same(a: Settings | null, b: Settings | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return JSON.stringify(a) === JSON.stringify(b);
}

class SettingsDraftStore {
  private state: SettingsDraftState = EMPTY;
  private listeners = new Set<() => void>();

  getState = (): SettingsDraftState => this.state;

  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  };

  private emit(next: SettingsDraftState) {
    if (next === this.state) return;
    this.state = next;
    for (const fn of this.listeners) fn();
  }

  /**
   * 用后端的最新值对齐。
   *
   * 关键分支：`dirty` 时只换 base，不动 draft —— 外部设置变化（例如另一个窗口
   * 改了语言）不该冲掉用户当前填了一半的表单。
   */
  sync(base: Settings) {
    const { base: curBase, draft } = this.state;
    if (!curBase || !draft) {
      this.emit({ base, draft: base, dirty: false });
      return;
    }
    if (!this.state.dirty) {
      // 无脏改动：直接用新基线重置（值相同就不发通知，避免多余渲染）
      if (same(curBase, base) && same(draft, base)) return;
      this.emit({ base, draft: base, dirty: false });
      return;
    }
    // 有脏改动：只更新基线，重新判定 dirty（外部可能刚好把某项改成与草稿一致）
    this.emit({ base, draft, dirty: !same(draft, base) });
  }

  patch(p: Partial<Settings>) {
    const { draft } = this.state;
    if (!draft) return;
    const next = { ...draft, ...p };
    this.emit({ ...this.state, draft: next, dirty: !same(next, this.state.base) });
  }

  /** 放弃改动：回到已保存基线 */
  discard() {
    const { base } = this.state;
    if (!base) return;
    this.emit({ base, draft: base, dirty: false });
  }

  /** 保存成功：以服务端返回值为新基线（服务端可能规范化了字段） */
  commit(saved: Settings) {
    this.emit({ base: saved, draft: saved, dirty: false });
  }

  /** 仅供测试：清空单例状态 */
  reset() {
    this.emit(EMPTY);
  }
}

export const settingsDraft = new SettingsDraftStore();

/** 便于测试构造独立实例（不污染全局单例） */
export function createSettingsDraftStore(): SettingsDraftStore {
  return new SettingsDraftStore();
}
