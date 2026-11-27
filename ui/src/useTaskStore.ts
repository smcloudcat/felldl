/**
 * taskStore 的 React 绑定。
 *
 * - `useTaskList()`：只订阅「有哪些任务 / 顺序 / 分类计数」这一层，进度事件不会触发；
 * - `useTask(id)`：单张卡片订阅自己的任务，一条进度只重渲染对应卡片。
 */
import { useCallback, useSyncExternalStore } from "react";
import type { Task } from "./types";
import { taskStore, type TaskListSnapshot } from "./taskStore.ts";

export function useTaskList(): TaskListSnapshot {
  return useSyncExternalStore(
    taskStore.subscribe,
    taskStore.getSnapshot,
    taskStore.getSnapshot
  );
}

export function useTask(id: string): Task | undefined {
  const subscribe = useCallback((cb: () => void) => taskStore.subscribeTask(id, cb), [id]);
  const get = useCallback(() => taskStore.get(id), [id]);
  return useSyncExternalStore(subscribe, get, get);
}
