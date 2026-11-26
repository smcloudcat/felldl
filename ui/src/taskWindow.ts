/**
 * 任务列表的渲染窗口
 *
 * 历史任务可能有上千条（每次更新 / 定时同步都会新增记录），而列表是一次性
 * `visible.map(<TaskCard/>)` 全量挂载的：首屏要创建上千个卡片 DOM，之后每条
 * 进度事件触发的重渲染也要走一遍长数组。即使 `TaskCard` 已经 memo，创建与
 * 布局上千个节点本身就足够让移动端着色卡顿。
 *
 * 这里不做真正的虚拟滚动（需要测量行高、处理动态展开，风险大），只做
 * 「首屏渲染前 N 条 + 按需展开」：排序本来就是「新建在前」，用户关心的
 * 进行中任务总在最上面，历史只是需要时往下翻。
 *
 * `keep` 谓词给「不能被窗口挡住的」任务开了口子：正在下载 / 排队 / 定时的
 * 任务无论排在多后面都照常渲染 —— 否则一个跑了三天的老任务会被几十条新完成
 * 的记录挤出首屏，用户会以为它消失了。
 *
 * 约束：`limit` 非法（负数 / NaN）时按 0 处理；`limit` 超过列表长度时全量返回。
 */

/** 首屏最多渲染的任务卡数量 */
export const INITIAL_LIMIT = 60;
/** 每次「显示更多」新增的数量 */
export const STEP = 60;

export interface TaskWindow<T> {
  items: T[];
  /** 被窗口挡住的条数（0 = 全部已渲染） */
  hidden: number;
}

/**
 * 取渲染窗口。
 *
 * @param keep 返回 true 的任务总是渲染（不占「被挡住」的名额，但仍然计入 limit）
 */
export function visibleSlice<T>(
  list: readonly T[],
  limit: number,
  keep?: (item: T) => boolean
): TaskWindow<T> {
  const max = Number.isFinite(limit) ? Math.max(0, Math.floor(limit)) : 0;
  const items: T[] = [];
  let hidden = 0;
  for (const item of list) {
    if ((keep && keep(item)) || items.length < max) items.push(item);
    else hidden += 1;
  }
  return { items, hidden };
}

/** 展开后新的窗口大小（只增不减；列表变短由 visibleSlice 自己夹住） */
export function nextLimit(limit: number, step: number = STEP): number {
  const base = Number.isFinite(limit) ? Math.max(0, Math.floor(limit)) : 0;
  const inc = Number.isFinite(step) ? Math.max(1, Math.floor(step)) : STEP;
  return base + inc;
}
