import { useSyncExternalStore } from "react";
import { grabStore } from "./grabStore";
import type { GrabStatus } from "./types";

/** 订阅引擎侧的抓取任务列表（开始/进度/收尾都会让引用变化） */
export function useGrabJobs(): GrabStatus[] {
  return useSyncExternalStore(grabStore.subscribe, grabStore.getSnapshot, grabStore.getSnapshot);
}
