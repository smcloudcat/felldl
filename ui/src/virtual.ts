/**
 * 任务列表的虚拟滚动
 *
 * 历史任务可能有上千条（每次更新 / 定时同步都会新增记录）。以前的做法是
 * 「首屏渲染前 60 条 + 显示更多按钮」，它把挂载量压到了 60，但代价是：
 * 第 61 条之后的记录在界面上根本不存在，用户必须一路点「显示更多」；
 * 而被 `keep` 拉进来的老任务会让列表看起来「跳过了一段」。
 *
 * 现在按可视区域挂载：只渲染 [start, end) 这一段，上下用等高占位撑住滚动条。
 * 挂载量只与「可视高度 / 行高」有关，与历史规模无关。
 *
 * 行高是估算的（卡片高度随文件大小、错误文案、元信息行变化），所以挂载后
 * 用实际高度做一次校准（见 hook），否则误差累积会让「滚到底」被截断或留白。
 */

/** 可视区上下各多挂几行，滚动时不至于露白 */
export const OVERSCAN = 6;

/** 初始行高估算：卡片约 78px + 列表 gap 9px */
export const ESTIMATED_ROW = 96;

export interface Range {
  /** 起始下标（含） */
  start: number;
  /** 结束下标（不含） */
  end: number;
  /** 上方占位高度 */
  padTop: number;
  /** 下方占位高度 */
  padBottom: number;
}

/**
 * 计算要挂载的下标区间。纯函数，方便单测与在别处复用。
 *
 * 所有入参都做兜底：滚动容器未挂载时 viewport 可能是 0，列表为空时 count 是 0，
 * 这些情况都不应该抛错或产生负数占位。
 */
export function computeRange(
  count: number,
  scrollTop: number,
  viewport: number,
  rowHeight: number,
  overscan: number = OVERSCAN
): Range {
  const n = Number.isFinite(count) ? Math.max(0, Math.floor(count)) : 0;
  if (n === 0) return { start: 0, end: 0, padTop: 0, padBottom: 0 };

  const row =
    Number.isFinite(rowHeight) && rowHeight > 1 ? rowHeight : ESTIMATED_ROW;
  const vp = Number.isFinite(viewport) && viewport > 0 ? viewport : 0;
  const top = Number.isFinite(scrollTop) && scrollTop > 0 ? scrollTop : 0;
  const over =
    Number.isFinite(overscan) && overscan >= 0 ? Math.floor(overscan) : OVERSCAN;

  const first = Math.floor(top / row) - over;
  const last = Math.ceil((top + vp) / row) + over;
  const start = Math.min(Math.max(first, 0), n);
  const end = Math.min(Math.max(last, start), n);
  return {
    start,
    end,
    padTop: start * row,
    padBottom: (n - end) * row,
  };
}
