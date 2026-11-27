/**
 * 虚拟滚动 Hook：把滚动容器 + 列表容器的位置翻译成「该挂载哪一段」。
 *
 * 为什么不自己监听 `onScroll` 属性：滚动容器是外层 `.content`（滚动条属于它，
 * 而不是列表），只有监听它才能同时处理「窗口尺寸变化」与「程序化滚动」。
 *
 * 两个必须处理的现实：
 * 1. 元素可能**晚于首次渲染才出现**（App 在 settings 未加载时提前返回，`.content`
 *    不在树上）。所以绑定逻辑放在每次渲染后比对「当前元素是不是上次那个」，
 *    而不是用空依赖数组只绑一次——否则永远绑不上。
 * 2. 行高不是常数（错误文案换行、元信息行开关）。每帧用已挂载卡片的实际平均
 *    高度回头修正估算值，误差不会随滚动距离累积。
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { computeRange, ESTIMATED_ROW, OVERSCAN, type Range } from "./virtual";

interface Options {
  /** 列表总条数 */
  count: number;
  /** 滚动容器（`.content`） */
  scrollRef: React.RefObject<HTMLElement | null>;
  /** 列表容器（`.task-list`），用来算「已经滚过列表顶部多少」 */
  listRef: React.RefObject<HTMLElement | null>;
  /** 卡片之间的间距，校准行高时补上 */
  gap?: number;
}

export interface VirtualRange extends Range {
  rowHeight: number;
}

export function useVirtualList({
  count,
  scrollRef,
  listRef,
  gap = 9,
}: Options): VirtualRange {
  const [scrollTop, setScrollTop] = useState(0);
  const [viewport, setViewport] = useState(0);
  const [rowHeight, setRowHeight] = useState(ESTIMATED_ROW);

  /** 当前真正绑定过监听的那个元素（用来判断是否换了容器） */
  const bound = useRef<HTMLElement | null>(null);
  /** 上次测量时的列表元素：列表可能比滚动容器晚一帧出现，变了要重新测 */
  const boundList = useRef<HTMLElement | null>(null);
  const observer = useRef<ResizeObserver | null>(null);

  // 已滚过列表顶部的距离 = 滚动容器顶 - 列表顶（列表顶随滚动上移，差值为正）
  const measure = useCallback(() => {
    const sc = scrollRef.current;
    const list = listRef.current;
    if (!sc || !list) return;
    const delta = sc.getBoundingClientRect().top - list.getBoundingClientRect().top;
    setScrollTop(Math.max(0, delta));
    setViewport(sc.clientHeight);
  }, [scrollRef, listRef]);

  // 每次渲染后比对容器：首次为 null、settings 到位后才有元素，切换视图时也可能重建。
  // 用「没有依赖数组」的 effect，跟着渲染一起发生；只有元素真的变了才重新绑定。
  useEffect(() => {
    const sc = scrollRef.current;
    const list = listRef.current;
    if (sc === bound.current && list === boundList.current) return;
    const prev = bound.current;
    observer.current?.disconnect();
    if (prev) prev.removeEventListener("scroll", measure);
    bound.current = sc;
    boundList.current = list;
    if (!sc) return;
    measure();
    sc.addEventListener("scroll", measure, { passive: true });
    const ro = new ResizeObserver(measure);
    ro.observe(sc);
    observer.current = ro;
  });

  // 卸载时只清理一次（measure 由 useCallback 固定，不会反复触发）
  useEffect(
    () => () => {
      observer.current?.disconnect();
      bound.current?.removeEventListener("scroll", measure);
      bound.current = null;
      boundList.current = null;
    },
    [measure]
  );

  // 用实际渲染出来的卡片高度校准估算值。差 2px 以内不动，避免抖动。
  useLayoutEffect(() => {
    const list = listRef.current;
    if (!list) return;
    const cards = list.querySelectorAll<HTMLElement>(".task-card");
    if (cards.length === 0) return;
    let sum = 0;
    cards.forEach((c) => {
      sum += c.getBoundingClientRect().height;
    });
    const avg = sum / cards.length + gap;
    if (Math.abs(avg - rowHeight) > 2) setRowHeight(avg);
  });

  const range = computeRange(count, scrollTop, viewport, rowHeight, OVERSCAN);
  return { ...range, rowHeight };
}
