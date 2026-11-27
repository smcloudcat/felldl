/**
 * 全局更新 job 的 React 绑定。
 *
 * 单例放在模块级：SettingsPage / App 顶部横幅 / 将来的更新入口都订阅同一份状态。
 * 进度事件只订阅一次（`wireUpdateJob` 幂等），组件卸载不会丢进度。
 */
import { useSyncExternalStore } from "react";
import { api, subscribeUpdateProgress } from "./bridge";
import { UpdateJob } from "./updateJob";

export const updateJob = new UpdateJob({
  checkUpdate: (force) => api.checkUpdate(force),
  downloadUpdate: () => api.downloadUpdate(),
  applyUpdate: (token) => api.applyUpdate(token),
  openInstallPermissionSettings: () => api.openInstallPermissionSettings(),
});

let wired = false;

/** 幂等接线：`update://progress` → job */
export function wireUpdateJob() {
  if (wired) return;
  wired = true;
  subscribeUpdateProgress((p) => updateJob.applyProgress(p)).catch(() => {
    /* 浏览器预览 / 旧构建没有这个事件：进度会停在 0，但下载本身仍能完成 */
  });
}

/** 订阅全局 job：返回值就是 job 本身（含 start/check 动作与当前状态字段） */
export function useUpdateJob(): UpdateJob {
  useSyncExternalStore(updateJob.subscribe, updateJob.getState, updateJob.getState);
  return updateJob;
}
