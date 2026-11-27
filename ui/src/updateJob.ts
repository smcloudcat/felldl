/**
 * 应用内更新的全局 job。
 *
 * 以前「下载更新」的 phase/percent 都是 SettingsPage 的局部 state，
 * 离开设置页组件就卸载：下载其实还在后端跑，但进度、错误全丢，
 * 回来只看到一个空白的「立即更新」按钮，再点一次就会**并行下载同一个包**。
 *
 * 这里把更新状态放到模块级单例（不依赖任何组件生命周期）：
 * - 组件只是订阅者，离开/回来不影响 job；
 * - `start()` 自带单飞（inFlight），连点/两处入口都只会下载一次；
 * - `install.need_permission` 这类「授权后可原地重试」的错误保留 token，
 *   重试直接 apply，不再白下几十 MB。
 *
 * 纯状态机 + 注入 IO，可在 node 里直接测（见 tests/updateJob.test.ts）。
 */
import type { UpdateInfo } from "./types.ts";
import { toIpcError, type IpcError } from "./ipcError.ts";

export type UpdatePhase = "idle" | "download" | "apply";

export interface UpdateJobState {
  /** 最近一次检查更新的结果；App 顶部横幅与设置页共用 */
  info: UpdateInfo | null;
  checking: boolean;
  /** 检查更新的错误（和下载/安装错误分开展示，避免离线时误报成更新失败） */
  checkError: IpcError | null;

  phase: UpdatePhase;
  downloaded: number;
  total: number;
  percent: number;
  /** 下载或安装失败的结构化错误；成功后清空 */
  error: IpcError | null;
}

export interface UpdateProgressPayload {
  downloaded: number;
  total: number;
  percent: number;
}

export interface UpdateIO {
  checkUpdate(force?: boolean): Promise<UpdateInfo>;
  downloadUpdate(): Promise<string>;
  applyUpdate(token: string): Promise<string>;
  /** 安卓首次更新需要授权「安装未知应用」；其它平台不存在这个错误 */
  openInstallPermissionSettings(): Promise<void>;
}

export type UpdateOutcome =
  | { kind: "done"; mode: string }
  | { kind: "failed"; error: IpcError }
  | { kind: "busy" };

const INITIAL: UpdateJobState = {
  info: null,
  checking: false,
  checkError: null,
  phase: "idle",
  downloaded: 0,
  total: 0,
  percent: 0,
  error: null,
};

export class UpdateJob {
  private state: UpdateJobState = INITIAL;
  private listeners = new Set<() => void>();
  /** 已验签、待安装的更新 token。只在「授权后可原地重试」时保留 */
  private token: string | null = null;
  /** 单飞标记：同步置位，连点也只会进一次 */
  private inFlight = false;
  /** 检查更新也去重：启动横幅和设置页可能同时触发 */
  private checkInFlight: Promise<UpdateInfo> | null = null;

  private readonly io: UpdateIO;

  constructor(io: UpdateIO) {
    this.io = io;
  }

  getState = (): UpdateJobState => this.state;

  // 便捷只读字段：组件里直接用 job.phase / job.percent，不必层层展开 state
  get info(): UpdateInfo | null {
    return this.state.info;
  }
  get checking(): boolean {
    return this.state.checking;
  }
  get checkError(): IpcError | null {
    return this.state.checkError;
  }
  get phase(): UpdatePhase {
    return this.state.phase;
  }
  get downloaded(): number {
    return this.state.downloaded;
  }
  get total(): number {
    return this.state.total;
  }
  get percent(): number {
    return this.state.percent;
  }
  get error(): IpcError | null {
    return this.state.error;
  }

  subscribe = (cb: () => void): (() => void) => {
    this.listeners.add(cb);
    return () => {
      this.listeners.delete(cb);
    };
  };

  private emit(patch: Partial<UpdateJobState>) {
    this.state = { ...this.state, ...patch };
    for (const cb of this.listeners) cb();
  }

  /** 进度事件来自 `update://progress`（后端 200ms 节流） */
  applyProgress(p: UpdateProgressPayload) {
    if (this.state.phase !== "download") return;
    this.emit({
      downloaded: p.downloaded || 0,
      total: p.total || 0,
      percent: Math.max(0, Math.min(100, p.percent || 0)),
    });
  }

  /** 检查更新；并发调用复用同一轮请求 */
  check(force = false): Promise<UpdateInfo> {
    if (this.checkInFlight) return this.checkInFlight;
    this.emit({ checking: true, checkError: null });
    this.checkInFlight = (async () => {
      try {
        const info = await this.io.checkUpdate(force);
        this.emit({ info, checking: false, checkError: null });
        return info;
      } catch (e) {
        const err = toIpcError(e, "update.check");
        this.emit({ checking: false, checkError: err });
        throw err;
      } finally {
        this.checkInFlight = null;
      }
    })();
    return this.checkInFlight;
  }

  clearError() {
    this.emit({ error: null });
  }

  clearCheckError() {
    this.emit({ checkError: null });
  }

  /**
   * 下载并安装。返回结果给调用方做 toast；状态本身留在 job 里，
   * 调用方组件卸载也不影响后续进度与错误展示。
   */
  async start(): Promise<UpdateOutcome> {
    if (this.inFlight) return { kind: "busy" };
    this.inFlight = true;
    try {
      this.emit({ error: null, percent: 0, downloaded: 0, total: 0 });

      let token = this.token;
      if (!token) {
        this.emit({ phase: "download" });
        token = await this.io.downloadUpdate();
        this.token = token;
      }

      this.emit({ phase: "apply", percent: 100 });
      const mode = await this.io.applyUpdate(token);
      // apply 成功即 token 已被消费（Windows 会退出进程；安卓已拉起安装器）
      this.token = null;
      this.emit({ phase: "idle" });
      return { kind: "done", mode };
    } catch (e) {
      const err = toIpcError(e, "update.failed");
      this.emit({ phase: "idle", error: err });
      if (err.code === "install.need_permission") {
        // 权限检查发生在 take(token) 之前：包还是有效的，授权回来直接重试
        void this.io.openInstallPermissionSettings().catch(() => {});
      } else {
        // 其余失败（校验、token 失效、安装失败）包都不能再用，重下一遍
        this.token = null;
      }
      return { kind: "failed", error: err };
    } finally {
      this.inFlight = false;
    }
  }

  /** 仅测试用：确认单飞锁在异常路径也会释放 */
  isInFlight(): boolean {
    return this.inFlight;
  }

  hasToken(): boolean {
    return this.token !== null;
  }
}
