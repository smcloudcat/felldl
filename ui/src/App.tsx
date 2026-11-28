import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import type {
  BatchReport,
  FilterKey,
  GrabStatus,
  Settings,
  Task,
  TaskStats,
} from "./types";
import { api, subscribe, subscribeResync } from "./bridge";
import { mergeSnapshot } from "./taskMerge";
import { taskStore } from "./taskStore";
import { useTaskList } from "./useTaskStore";
import { useVirtualList } from "./useVirtualList";
import { moveFocus, scrollTopToReveal } from "./virtual";
import { TaskCard } from "./TaskCard";
import { NewTaskDialog } from "./NewTaskDialog";
import { SettingsPage } from "./SettingsPage";
import { GrabPage } from "./GrabPage";
import { cx, formatSpeed } from "./format";
import { applyAppearance } from "./appearance";
import { useI18n } from "./i18n";
import { updateJob, useUpdateJob, wireUpdateJob } from "./useUpdateJob";
import { grabStore } from "./grabStore";
import { useGrabJobs } from "./useGrabJobs";
import { settingsDraft } from "./settingsDraft";
import { useDialogA11y } from "./useDialogA11y";
import { useAndroidBack } from "./useAndroidBack";
import {
  filterEligible,
  keepFailed,
  pruneSelection,
  toggleAll,
  toggleSelected,
  type BatchAction,
} from "./selection";
import {
  IconBolt,
  IconCheck,
  IconCheckSquare,
  IconDownload,
  IconFolder,
  IconGlobe,
  IconMenu,
  IconMore,
  IconPause,
  IconPlay,
  IconPlus,
  IconSettings,
  IconTrash,
  IconX,
} from "./icons";

type View = "downloads" | "grab" | "settings";

/// 与引擎 `Engine::matches_filter` 保持一致。
/// 列表本身已由引擎按筛选分页返回，这里只用来挡住「切换筛选时在途事件」带进来的
/// 不匹配行（否则在「已完成」里会突然冒出一条正在下载的任务）。
function matchesView(
  t: Task,
  filter: FilterKey,
  category: string | null,
): boolean {
  if (category && t.category !== category) return false;
  switch (filter) {
    case "downloading":
      return (
        t.state === "downloading" ||
        t.state === "probing" ||
        t.state === "queued"
      );
    case "completed":
      return t.state === "completed";
    case "paused":
      return t.state === "paused" || t.state === "scheduled";
    case "error":
      return t.state === "error";
    default:
      return true;
  }
}

export default function App() {
  const { t } = useI18n();
  const FILTERS = useMemo(
    () =>
      [
        { key: "all", label: t("filter.all"), icon: IconDownload },
        { key: "downloading", label: t("filter.downloading"), icon: IconBolt },
        { key: "completed", label: t("filter.completed"), icon: IconCheck },
        { key: "paused", label: t("filter.paused"), icon: IconPause },
        { key: "error", label: t("filter.error"), icon: IconX },
      ] as { key: FilterKey; label: string; icon: any }[],
    [t],
  );

  const [settings, setSettings] = useState<Settings | null>(null);
  const [view, setView] = useState<View>("downloads");
  const [filter, setFilter] = useState<FilterKey>("all");
  const [category, setCategory] = useState<string | null>(null);
  const [showNew, setShowNew] = useState(false);
  // 删除确认：仅移除任务 / 连文件一起删
  const [delTarget, setDelTarget] = useState<Task | null>(null);
  // 稳定回调：进度更新重渲染不会让 NewTaskDialog 重挂 / 重新抢焦点
  const closeNew = useCallback(() => setShowNew(false), []);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [initError, setInitError] = useState<string | null>(null);
  const [slowHint, setSlowHint] = useState(false);
  const [reloadKey, setReloadKey] = useState(0);
  // 分页：一页 80 条，滚到底自动续拉。历史任务上万条时，一次性传全量实测 7.2MB，
  // 分页后首屏只传一页；侧边栏计数改由 task_stats 提供（本地只有当前页，算不出全量）。
  const PAGE_SIZE = 80;
  const [page, setPage] = useState<{ total: number; hasMore: boolean }>({
    total: 0,
    hasMore: false,
  });
  const [loadingMore, setLoadingMore] = useState(false);
  const loadingRef = useRef(false);
  const [stats, setStats] = useState<TaskStats>({
    counts: { all: 0, downloading: 0, completed: 0, paused: 0, error: 0 },
    active: 0,
    total_speed: 0,
  });
  const [updateDismissed, setUpdateDismissed] = useState(false);
  // ── 多选与批量操作 ──
  // 选中集合只存 id：任务本体跟着 store 走，进度更新不会把选中状态冲掉。
  // 批量删除确认与结果明细分开：前者决定「要不要删文件」，后者是「哪几条没成功」。
  const [selectMode, setSelectMode] = useState(false);
  const [selected, setSelected] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const [batchBusy, setBatchBusy] = useState(false);
  const [batchDelOpen, setBatchDelOpen] = useState(false);
  const [batchResult, setBatchResult] = useState<{
    report: BatchReport;
    action: BatchAction;
    deleteFile: boolean;
  } | null>(null);
  // 列表只订阅「有哪些任务」这一层：进度事件不会让 App 重渲染
  const { ids: visibleIds, counts: categoryCounts } = useTaskList();
  // 更新状态是全局 job：这里和设置页看的是同一份，切页面不重置
  const job = useUpdateJob();
  const updateInfo = job.info;
  // 抓取任务在引擎侧后台跑：这里只订阅「有哪些在跑」，用于非抓取页的角标
  const grabJobs = useGrabJobs();

  // 安卓返回键：按优先级关最上层，而不是直接退出应用。
  // 顺序 = 从最「靠近用户」的层往外：对话框 → 多选 → 非下载页 → 分类/筛选。
  useAndroidBack([
    { active: showNew, close: closeNew },
    { active: !!delTarget, close: () => setDelTarget(null) },
    { active: batchDelOpen, close: () => setBatchDelOpen(false) },
    { active: !!batchResult, close: () => setBatchResult(null) },
    {
      active: selectMode,
      close: () => {
        setSelectMode(false);
        setSelected(new Set());
      },
    },
    { active: view !== "downloads", close: () => setView("downloads") },
    { active: category !== null, close: () => setCategory(null) },
    { active: filter !== "all", close: () => setFilter("all") },
  ]);

  // ── 移动端「更多」：窄屏下 header 的次要按钮被隐藏，收进这个菜单 ──
  const [moreOpen, setMoreOpen] = useState(false);
  const moreRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!moreOpen) return;
    // 点外部 / Esc 关闭：菜单不能只靠再点一次按钮才能关
    const onDown = (e: MouseEvent) => {
      if (moreRef.current && !moreRef.current.contains(e.target as Node)) {
        setMoreOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMoreOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [moreOpen]);
  // 切换视图/进入多选时菜单没有意义了，顺手关掉
  useEffect(() => setMoreOpen(false), [view, selectMode]);
  // 设置草稿未保存时在侧栏留个记号：切走不丢草稿，但用户得知道它还在
  const draftState = useSyncExternalStore(
    settingsDraft.subscribe,
    settingsDraft.getState,
    settingsDraft.getState,
  );
  const toastId = useRef(0);
  // 取快照（list_tasks）期间收到的事件：用来判断「谁是更新的那一份」，
  // 避免旧快照把已经推进到新状态的任务盖回去（见 taskMerge.ts）
  const touchedRef = useRef(new Set<string>());
  const removedRef = useRef(new Set<string>());
  /** 回前台时复用「断流重同步」那套逻辑；在下面的 resync effect 里赋值 */
  const resyncRef = useRef<null | (() => Promise<void>)>(null);

  interface Toast {
    id: number;
    msg: string;
    kind: "ok" | "error";
  }

  const toast = useCallback((msg: string, kind: "ok" | "error" = "ok") => {
    const id = ++toastId.current;
    setToasts((t) => [...t, { id, msg, kind }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 2600);
  }, []);

  // ── 初始化 ──
  useEffect(() => {
    let alive = true;
    const timer = setTimeout(() => {
      if (alive) setSlowHint(true);
    }, 20000);
    (async () => {
      try {
        // 任务列表走分页 effect（要先知道 settings 才能确定分类筛选的默认值）
        const s = await api.getSettings();
        if (!alive) return;
        clearTimeout(timer);
        setSettings(s);
        setInitError(null);
        setSlowHint(false);
      } catch (e) {
        if (!alive) return;
        clearTimeout(timer);
        console.error("初始化失败", e);
        setInitError(String(e));
        setSlowHint(false);
        toast(t("act.initFail"), "error");
      }
    })();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [toast, reloadKey, t]);

  // ── 任务列表：按当前筛选分页加载 ──
  // 筛选/分类/reloadKey 变化时回到第一页；切筛选必须重新向引擎要，因为本地只有上一页，
  // 客户端过滤会漏掉还没加载的历史（比如「已完成」里第 300 条）。
  useEffect(() => {
    if (!settings) return;
    let alive = true;
    touchedRef.current.clear();
    removedRef.current.clear();
    // 先设准入：首屏请求期间到达的新任务，只有匹配当前视图才插入
    taskStore.setFilter((t) => matchesView(t, filter, category));
    (async () => {
      try {
        const [p, st] = await Promise.all([
          api.listTasks({ filter, category, offset: 0, limit: PAGE_SIZE }),
          api.taskStats(),
        ]);
        if (!alive) return;
        // 取第一页期间收到的事件不能丢：mergeSnapshot 用 touched 判断谁更新；
        // 合并后再按当前视图过一遍，挡住筛选切换期间在途事件带来的不匹配任务
        const merged = mergeSnapshot(taskStore.all(), p.items, {
          touched: touchedRef.current,
          removed: removedRef.current,
        }).filter((t) => matchesView(t, filter, category));
        taskStore.setAll(merged);
        setPage({ total: p.total, hasMore: p.has_more });
        setStats(st);
      } catch (e) {
        if (alive) console.error("加载任务失败", e);
      }
    })();
    return () => {
      alive = false;
    };
  }, [settings, filter, category, reloadKey]);

  // 统计轮询：进度事件不携带「全库计数」，2s 拉一次只有几十字节
  useEffect(() => {
    if (!settings || view !== "downloads") return;
    let alive = true;
    const tick = () => {
      api
        .taskStats()
        .then((s) => {
          if (alive) setStats(s);
        })
        .catch(() => {});
    };
    const id = setInterval(tick, 2000);
    tick();
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [settings, view]);

  // ── 启动后自动检查更新（静默：失败不打扰） ──
  useEffect(() => {
    if (!settings) return;
    wireUpdateJob();
    // 启动时对齐一次抓取任务：UI 还没订阅到的事件不会补发
    api
      .grabJobs()
      .then((list) => grabStore.setAll(list))
      .catch(() => {});
    // job 里已有结果（例如刚在设置页检查过）就不再重复请求；
    // 后端本身也有 10 分钟缓存与 ETag
    if (updateJob.getState().info) return;
    updateJob.check(false).catch(() => {
      /* 离线/源站不可用时静默；用户主动检查时才提示 */
    });
  }, [settings, reloadKey]);

  // ── 订阅引擎事件 ──
  useEffect(() => {
    let un: (() => void) | undefined;
    let disposed = false;
    // 进度事件批量合并：250ms 内的更新只触发一次 setState，
    // 避免高频进度把整个列表反复重渲染；终态/删除立即提交
    let pending = new Map<string, Task>();
    // 轻量进度增量（只含变化字段），落盘前按 id 合并进现有任务
    let progress = new Map<string, Partial<Task>>();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const flush = () => {
      timer = undefined;
      if (pending.size === 0 && progress.size === 0) return;
      const batch = pending;
      pending = new Map();
      const pdelta = progress;
      progress = new Map();
      // store 内部按 rev 水位 / 终态规则过滤，这里只管按到达顺序提交
      for (const [id, d] of pdelta) taskStore.patch(id, d);
      for (const t of batch.values()) taskStore.upsert(t);
    };
    const scheduleFlush = (immediate: boolean) => {
      if (immediate) {
        if (timer !== undefined) {
          clearTimeout(timer);
        }
        flush();
      } else if (timer === undefined) {
        timer = setTimeout(flush, 250);
      }
    };
    subscribe((e) => {
      switch (e?.type) {
        case "taskProgress": {
          const d = e.payload as Partial<Task> & { id: string };
          if (!d?.id) break;
          touchedRef.current.add(d.id);
          progress.set(d.id, d);
          scheduleFlush(false);
          break;
        }
        case "taskUpdated": {
          const t = e.payload as Task;
          touchedRef.current.add(t.id);
          // 完整快照优先于增量进度
          progress.delete(t.id);
          // 合并同一任务的连续进度；非下载态（完成/暂停/错误/排队）立即提交
          pending.set(t.id, t);
          scheduleFlush(t.state !== "downloading" && t.state !== "probing");
          break;
        }
        case "taskRemoved":
          touchedRef.current.add(e.payload as string);
          removedRef.current.add(e.payload as string);
          pending.delete(e.payload as string);
          progress.delete(e.payload as string);
          taskStore.remove(e.payload as string);
          break;
        case "settingsUpdated":
          setSettings(e.payload);
          break;
        case "grabStatus":
          grabStore.apply(e.payload as GrabStatus);
          break;
        case "notice":
          toast(String(e.payload));
          break;
      }
    })
      .then((f) => {
        // 订阅完成前组件已卸载（开发 StrictMode 双挂载常见）：立刻退订
        if (disposed) f();
        else un = f;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      if (timer !== undefined) clearTimeout(timer);
      un?.();
    };
  }, [toast]);

  // ── 事件断流后的重同步 ──
  // 后端广播通道有容量上限，前端处理不过来时会丢事件（Rust 侧会发 resync 信号）；
  // 丢掉的事件不会补发 —— 例如「最后一条是已完成」的任务会永远停在旧状态。
  // 收到信号就重新拉一次权威快照，仍按 rev 水位合并，避免把更新的事件盖回去。
  useEffect(() => {
    let un: (() => void) | undefined;
    let disposed = false;
    let inflight = false;
    const resync = async () => {
      if (inflight) return;
      inflight = true;
      touchedRef.current.clear();
      try {
        // 分页之后「权威快照」只覆盖已加载的那一段：重新拉 0..已加载条数，
        // 不能只拉第一页，否则窗口外的任务会被 mergeSnapshot 当成已删除。
        const want = Math.max(visibleIds.length, PAGE_SIZE);
        const items: Task[] = [];
        let offset = 0;
        let more = true;
        while (more && offset < want) {
          const limit = Math.min(PAGE_SIZE, want - offset);
          const p = await api.listTasks({ filter, category, offset, limit });
          items.push(...p.items);
          offset += p.items.length;
          more = p.has_more && p.items.length > 0;
        }
        if (disposed) return;
        const merged = mergeSnapshot(taskStore.all(), items, {
          touched: touchedRef.current,
          removed: removedRef.current,
        }).filter((t) => matchesView(t, filter, category));
        taskStore.setAll(merged);
        setPage((prev) => ({
          ...prev,
          total: Math.max(prev.total, items.length),
        }));
      } catch {
        /* 拉不到就等下一次信号；不要用 toast 打扰用户 */
      } finally {
        inflight = false;
      }
    };
    subscribeResync(() => void resync())
      .then((f) => {
        if (disposed) f();
        else un = f;
      })
      .catch(() => {});
    // 回前台时可能错过广播（Android WebView 在后台被冻结，JS 也不一定跑），
    // 这里顺手记下当前的重同步函数，由 visibilitychange 调用
    resyncRef.current = resync;
    return () => {
      disposed = true;
      resyncRef.current = null;
      un?.();
    };
    // filter/category 决定重拉哪一段；已加载窗口长度变化说明要重新对齐
  }, [filter, category, visibleIds.length]);

  // ── 后台冻结 ──
  // 页面不可见时 store 只记脏不通知（见 taskStore.setHidden）：后台不跑 React 提交，
  // 回前台一次性刷出来。广播事件在后台可能整段丢失，所以回前台再拉一次权威快照。
  useEffect(() => {
    const onVisible = () => {
      const hidden = document.hidden;
      taskStore.setHidden(hidden);
      // 首次挂载时已经是 hidden 的话不必重拉（还没有任何事件漏掉）
      if (!hidden) void resyncRef.current?.();
    };
    taskStore.setHidden(document.hidden);
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, []);

  // ── 主题 / 皮肤 / 按钮风格 ──
  useEffect(() => {
    if (!settings) return;
    const apply = () => {
      const pref = settings.theme;
      const dark =
        pref === "dark" ||
        (pref === "system" &&
          window.matchMedia("(prefers-color-scheme: dark)").matches);
      document.documentElement.setAttribute(
        "data-theme",
        dark ? "dark" : "light",
      );
      document.documentElement.setAttribute(
        "data-skin",
        settings.ui_skin || "blue",
      );
      document.documentElement.setAttribute(
        "data-btn",
        settings.btn_style || "rounded",
      );
      // 毛玻璃 / 背景图：必须在 data-theme 之后应用（遮罩色跟随主题）
      applyAppearance(settings);
    };
    apply();
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onMq = () => settings.theme === "system" && apply();
    mq.addEventListener("change", onMq);
    return () => mq.removeEventListener("change", onMq);
  }, [
    settings?.theme,
    settings?.ui_skin,
    settings?.btn_style,
    settings?.ui_background,
    settings?.ui_blur,
    settings?.ui_dim,
    settings?.ui_glass,
  ]);

  // 列表只订阅「有哪些任务」这一层：本地 ids 就是「当前页 + 增量事件」，
  // 不再二次过滤（否则会漏掉尚未加载的历史）。
  const visible = visibleIds;

  // 选中项必须跟着列表走：别的窗口删了、切了筛选、分页替换之后，
  // 集合里留着看不见的 id，批量删除就会「删了 3 条却说删了 5 条」。
  useEffect(() => {
    setSelected((prev) => pruneSelection(prev, visible));
  }, [visible]);

  // 换视图 / 筛选 / 分类：选中集合已经不可见，直接退出多选，不留悬空状态
  useEffect(() => {
    setSelectMode(false);
    setSelected(new Set());
    setBatchDelOpen(false);
  }, [view, filter, category]);

  // ── 列表渲染窗口 ──
  // 历史任务可能上千条，全量挂载 TaskCard 会让首屏和每次进度事件都变卡；
  // 切换视图 / 筛选 / 分类时回到顶部：否则会停在上一个列表的深位置，
  // 看起来像「点了筛选但列表是空的」。
  const contentRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    contentRef.current?.scrollTo({ top: 0 });
  }, [view, filter, category]);
  // 只挂载可视区域内的卡片：挂载量与历史条数无关
  const { start, end, padTop, padBottom, rowHeight } = useVirtualList({
    count: visible.length,
    scrollRef: contentRef,
    listRef,
  });
  const shownTasks = visible.slice(start, end);

  // ── 键盘导航：方向键在列表里移动焦点 ──
  // 与虚拟滚动配合：目标可能不在已挂载区间内，于是分两步——先滚动（下面第二个
  // effect），等 start/end 更新、卡片挂上来之后，再把 DOM 焦点交给它（第三个 effect）。
  const [focusIdx, setFocusIdx] = useState<number | null>(null);
  // 换视图/筛选/分类时焦点作废：否则会指到新列表里完全不相干的卡片
  useEffect(() => {
    setFocusIdx(null);
  }, [view, filter, category]);

  useEffect(() => {
    if (view !== "downloads") return;
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      // 对话框打开时方向键归它（overlay 由 useDialogA11y 负责焦点循环）
      if (document.querySelector(".overlay")) return;
      const n = visible.length;
      if (n === 0) return;
      const vp = contentRef.current?.clientHeight ?? 0;
      const page = Math.max(1, Math.floor(vp / rowHeight) - 1);
      let delta: number;
      switch (e.key) {
        case "ArrowDown":
          delta = 1;
          break;
        case "ArrowUp":
          delta = -1;
          break;
        case "PageDown":
          delta = page;
          break;
        case "PageUp":
          delta = -page;
          break;
        case "Home":
          e.preventDefault();
          setFocusIdx(0);
          return;
        case "End":
          e.preventDefault();
          setFocusIdx(n - 1);
          return;
        default:
          return;
      }
      e.preventDefault();
      setFocusIdx((cur) => moveFocus(cur, delta, n));
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [view, visible, rowHeight]);

  // 焦点出视野才滚。
  //
  // 优先用**已挂载卡片的真实坐标**：行高是估算值（卡片 64~115px 不等，还带 gap），
  // 拿估算值判断「要不要滚」会出现「明明在视口外却说不用滚」，焦点就卡在屏幕下方。
  // 只有目标还没挂载（End 跨页跳转）时才退回行高估算，等 start 更新、卡片挂上来，
  // 这个 effect 会因 start 变化再跑一次，把误差精确修掉。
  //
  // 必须用 `instant`：`.content` 上有 `scroll-behavior: smooth`，传 `auto` 也会被
  // 平滑化，连按方向键时动画互相打断，焦点会短暂跑到视口外。
  useEffect(() => {
    if (focusIdx == null) return;
    const el = contentRef.current;
    const list = listRef.current;
    if (!el || !list) return;

    const card = list.querySelectorAll<HTMLElement>(".task-card")[focusIdx - start];
    if (card) {
      const r = card.getBoundingClientRect();
      const s = el.getBoundingClientRect();
      const pad = 8;
      if (r.top < s.top + pad) {
        el.scrollBy({ top: r.top - s.top - pad, behavior: "instant" as ScrollBehavior });
      } else if (r.bottom > s.bottom - pad) {
        el.scrollBy({ top: r.bottom - s.bottom + pad, behavior: "instant" as ScrollBehavior });
      }
      return;
    }

    const top = scrollTopToReveal(
      focusIdx,
      rowHeight,
      el.clientHeight,
      el.scrollTop,
    );
    if (top != null) el.scrollTo({ top, behavior: "instant" as ScrollBehavior });
  }, [focusIdx, start, rowHeight]);

  // 卡片挂上来之后再把 DOM 焦点交过去；目标还没挂载（在滚动途中）就等下一轮
  useEffect(() => {
    if (focusIdx == null) return;
    const cards = listRef.current?.querySelectorAll<HTMLElement>(".task-card");
    cards?.[focusIdx - start]?.focus({ preventScroll: true });
  }, [focusIdx, start]);

  // 滚到接近已加载尾部就续拉下一页（虚拟列表只会挂当前视口，但数据仍需分页取）
  useEffect(() => {
    if (view !== "downloads" || !page.hasMore || loadingRef.current) return;
    if (end < visible.length - 5) return;
    loadingRef.current = true;
    setLoadingMore(true);
    api
      .listTasks({ filter, category, offset: visible.length, limit: PAGE_SIZE })
      .then((p) => {
        taskStore.append(p.items);
        setPage({ total: p.total, hasMore: p.has_more });
      })
      .catch(() => {})
      .finally(() => {
        loadingRef.current = false;
        setLoadingMore(false);
      });
  }, [end, visible.length, page.hasMore, filter, category, view, PAGE_SIZE]);

  // ── 操作 ──
  const handlePause = useCallback(
    (id: string) => {
      api.pause(id).catch((e) => toast(String(e), "error"));
    },
    [toast],
  );

  const handleResume = useCallback(
    (id: string) => {
      api.resume(id).catch((e) => toast(String(e), "error"));
    },
    [toast],
  );

  // 删除默认保留文件；用户点「连同文件」时才真正删盘
  const handleRemove = useCallback((task: Task) => setDelTarget(task), []);

  const confirmRemove = useCallback(
    async (deleteFile: boolean) => {
      const task = delTarget;
      if (!task) return;
      setDelTarget(null);
      try {
        await api.remove(task.id, deleteFile);
        toast(deleteFile ? t("act.taskAndFileRemoved") : t("act.taskRemoved"));
      } catch (e) {
        toast(String(e), "error");
      }
    },
    [delTarget, toast, t],
  );

  const handleOpen = useCallback(
    async (task: Task) => {
      try {
        await api.openFile(task.save_path);
      } catch {
        toast(t("act.openFail"), "error");
      }
    },
    [toast, t],
  );

  const handleReveal = useCallback(
    async (task: Task) => {
      try {
        await api.showInFolder(task.save_path);
      } catch {
        toast(t("act.revealFail"), "error");
      }
    },
    [toast, t],
  );

  const pauseAll = useCallback(async () => {
    try {
      await api.pauseAll();
      toast(t("act.pausedAll"));
    } catch (e) {
      toast(String(e), "error");
    }
  }, [toast, t]);

  const resumeAll = useCallback(async () => {
    try {
      await api.resumeAll();
      toast(t("act.resumedAll"));
    } catch (e) {
      toast(String(e), "error");
    }
  }, [toast, t]);

  const clearCompleted = useCallback(async () => {
    try {
      await api.clearCompleted();
      toast(t("act.cleared"));
    } catch (e) {
      toast(String(e), "error");
    }
  }, [toast, t]);

  // ── 批量操作 ──
  const toggleOne = useCallback(
    (id: string) => setSelected((prev) => toggleSelected(prev, id)),
    [],
  );

  // 长按一张卡 = 进入多选并选中它。和「批量选择」按钮的区别只是入口：
  // 手机上按钮在 header，单手握持时够不到，长按是更自然的路径。
  const enterSelectFromLongPress = useCallback((id: string) => {
    setSelectMode(true);
    setSelected(new Set([id]));
  }, []);

  const runBatch = useCallback(
    async (action: BatchAction, ids: string[], deleteFile = false) => {
      if (ids.length === 0 || batchBusy) return;
      // 已完成的任务调 pause 是「无操作但返回成功」：先按状态筛掉，
      // 否则 toast 会报出一个用户看不到效果的条数
      const targets = filterEligible(
        action,
        ids.map((id) => ({ id, state: taskStore.get(id)?.state ?? "" })),
      );
      if (targets.length === 0) {
        toast(t("batch.nothing"));
        return;
      }
      setBatchBusy(true);
      try {
        const report = await api.batch(action, targets, deleteFile);
        // 失败的留着选中：用户可以直接点「重试失败项」，不用重新勾一遍
        setSelected(keepFailed(report.failed));
        if (report.failed.length === 0) {
          setSelectMode(false);
          toast(
            t(
              action === "pause"
                ? "batch.paused"
                : action === "resume"
                  ? "batch.resumed"
                  : "batch.removed",
              {
                n: report.ok.length,
              },
            ),
          );
        } else {
          // 有成功也有失败：仍在多选里，方便继续处理剩下的
          setSelectMode(true);
          setBatchResult({ report, action, deleteFile });
        }
      } catch (e) {
        toast(String(e), "error");
      } finally {
        setBatchBusy(false);
      }
    },
    [batchBusy, toast, t],
  );

  const hasActive = stats.active > 0;
  const hasResumable = stats.counts.paused + stats.counts.error > 0;
  // 「全选」只覆盖已经加载的条目：批量命令只认真实存在的 id，
  // 没加载到的历史用户也看不见，不该被默默算进来
  const allSelected =
    visible.length > 0 && visible.every((id) => selected.has(id));

  // 键盘快捷键
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "n") {
        e.preventDefault();
        setShowNew(true);
      }
      if (e.key === "Escape") setSidebarOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (!settings) {
    return (
      <div className="app">
        <div className="boot-screen">
          <div className="boot-title">
            {initError ? t("boot.failed") : t("boot.starting")}
          </div>
          {slowHint && !initError && (
            <div className="boot-hint">{t("boot.slow")}</div>
          )}
          {initError && (
            <>
              <div className="boot-error">{initError}</div>
              <button
                className="btn btn-primary"
                onClick={() => {
                  setInitError(null);
                  setSlowHint(false);
                  setReloadKey((k) => k + 1);
                }}
              >
                {t("boot.retry")}
              </button>
            </>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="app">
      {sidebarOpen && (
        <div className="sidebar-scrim" onClick={() => setSidebarOpen(false)} />
      )}

      {/* 侧边栏 */}
      <aside className={cx("sidebar", sidebarOpen && "open")}>
        <div className="brand">
          <div className="brand-logo">
            <IconBolt size={19} />
          </div>
          <div className="brand-text">
            <div className="brand-name">Felldl</div>
            <div className="brand-sub">Fast Downloader</div>
          </div>
        </div>

        {/* 导航区可滚动，底部速度卡始终固定可见 */}
        <nav className="sidebar-nav">
          <div className="nav-section">{t("nav.downloads")}</div>
          {FILTERS.map(({ key, label, icon: Icon }) => (
            <button
              key={key}
              className={cx(
                "nav-item",
                view === "downloads" && filter === key && "active",
              )}
              onClick={() => {
                setView("downloads");
                setFilter(key);
                setSidebarOpen(false);
              }}
            >
              <Icon />
              {label}
              {stats.counts[key] > 0 && (
                <span className="nav-count">{stats.counts[key]}</span>
              )}
            </button>
          ))}

          {(settings.categories?.length ?? 0) > 0 && (
            <>
              <div className="nav-section">{t("set.categories")}</div>
              <button
                className={cx(
                  "nav-item",
                  view === "downloads" &&
                    category === null &&
                    filter === "all" &&
                    "active",
                )}
                onClick={() => {
                  setView("downloads");
                  setCategory(null);
                  setFilter("all");
                  setSidebarOpen(false);
                }}
              >
                <IconDownload />
                {t("filter.all")}
              </button>
              {settings.categories.map((c) => {
                const count = categoryCounts[c.name] ?? 0;
                return (
                  <button
                    key={c.name}
                    className={cx(
                      "nav-item",
                      view === "downloads" && category === c.name && "active",
                    )}
                    onClick={() => {
                      setView("downloads");
                      setCategory(c.name);
                      setFilter("all");
                      setSidebarOpen(false);
                    }}
                  >
                    <IconFolder />
                    {c.name}
                    {count > 0 && <span className="nav-count">{count}</span>}
                  </button>
                );
              })}
            </>
          )}

          <div className="nav-section">{t("nav.tools")}</div>
          <button
            className={cx("nav-item", view === "grab" && "active")}
            onClick={() => {
              setView("grab");
              setSidebarOpen(false);
            }}
          >
            <IconGlobe />
            {t("nav.grab")}
          </button>
          <button
            className={cx("nav-item", view === "settings" && "active")}
            onClick={() => {
              setView("settings");
              setSidebarOpen(false);
            }}
          >
            <IconSettings />
            {t("nav.settings")}
            {draftState.dirty && (
              <span
                className="nav-dot"
                title={t("set.unsavedDot")}
                aria-label={t("set.unsavedDot")}
              />
            )}
          </button>
        </nav>

        <div className="sidebar-footer">
          <div className="speed-card">
            <div className="speed-card-label">{t("side.speed")}</div>
            <div className="speed-card-value">
              {stats.total_speed > 0
                ? formatSpeed(stats.total_speed)
                : t("side.idle")}
            </div>
            <div className="speed-card-sub">
              {stats.active > 0
                ? t("side.activeTasks", { n: stats.active })
                : t("side.doneTasks", { n: stats.counts.completed })}
            </div>
          </div>
        </div>
      </aside>

      {/* 主区 */}
      <main className="main">
        <header className="header">
          <button
            className="icon-btn mobile-only"
            onClick={() => setSidebarOpen((v) => !v)}
            aria-label={t("hdr.menu")}
          >
            <IconMenu />
          </button>

          <div className="header-title">
            {selectMode && view === "downloads"
              ? t("batch.title", { n: selected.size })
              : view === "settings"
                ? t("nav.settings")
                : view === "grab"
                  ? t("nav.grab")
                  : category
                    ? category
                    : (FILTERS.find((f) => f.key === filter)?.label ??
                      t("filter.all"))}
            {view === "downloads" && !selectMode && page.total > 0 && (
              <span className="count-pill">
                {Math.max(page.total, visible.length)}
              </span>
            )}
          </div>

          <div className="header-actions">
            {/* 更新在后台下载时，切到别的页面也能看到并一键回到设置页 */}
            {job.phase !== "idle" && view !== "settings" && (
              <button
                className="btn btn-soft btn-sm"
                onClick={() => setView("settings")}
                title={t("upd.inProgress")}
              >
                <IconBolt size={13} />
                {job.phase === "apply"
                  ? t("set.restarting")
                  : `${Math.round(job.percent)}%`}
              </button>
            )}
            {/* 抓取在后台跑时，切到别的页面也能看到进度并一键回到抓取页 */}
            {grabJobs.length > 0 && view !== "grab" && (
              <button
                className="btn btn-soft btn-sm"
                onClick={() => setView("grab")}
                title={t("grab.jobsHint")}
              >
                <IconGlobe size={13} />
                {t("grab.jobBadge", {
                  done: grabJobs.reduce((n, j) => n + j.done, 0),
                  total: grabJobs.reduce((n, j) => n + j.total, 0),
                })}
              </button>
            )}
            {view === "downloads" && selectMode && (
              <>
                <button
                  className="btn btn-soft btn-sm"
                  onClick={() => setSelected(toggleAll(selected, visible))}
                >
                  {allSelected ? t("batch.unselectAll") : t("batch.selectAll")}
                </button>
                <button
                  className="btn btn-soft btn-sm"
                  onClick={() => {
                    setSelectMode(false);
                    setSelected(new Set());
                  }}
                >
                  <IconX size={13} />
                  {t("dlg.cancel")}
                </button>
              </>
            )}
            {view === "downloads" && !selectMode && (
              <>
                {visible.length > 0 && (
                  <button
                    className="btn btn-soft btn-sm select-entry"
                    onClick={() => setSelectMode(true)}
                    title={t("batch.enter")}
                  >
                    <IconCheckSquare size={14} />
                    <span className="desktop-label">{t("batch.enter")}</span>
                  </button>
                )}
                {hasActive && (
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={pauseAll}
                    title={t("hdr.pauseAll")}
                  >
                    <IconPause size={13} />
                    <span className="desktop-label">{t("hdr.pauseAll")}</span>
                  </button>
                )}
                {!hasActive && hasResumable && (
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={resumeAll}
                    title={t("hdr.resumeAll")}
                  >
                    <IconPlay size={13} />
                    <span className="desktop-label">{t("hdr.resumeAll")}</span>
                  </button>
                )}
                {stats.counts.completed > 0 && (
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={clearCompleted}
                    title={t("hdr.clearDone")}
                  >
                    <IconTrash size={13} />
                    <span className="desktop-label">{t("hdr.clearDone")}</span>
                  </button>
                )}
                <button
                  className="btn btn-primary header-new-btn"
                  onClick={() => setShowNew(true)}
                  title={t("hdr.newTask") + " (Ctrl+N)"}
                >
                  <IconPlus />
                  <span>{t("hdr.newTask")}</span>
                </button>
                {(hasActive || hasResumable || stats.counts.completed > 0) && (
                  <div className="header-more" ref={moreRef}>
                    <button
                      className="icon-btn"
                      onClick={() => setMoreOpen((v) => !v)}
                      aria-haspopup="menu"
                      aria-expanded={moreOpen}
                      aria-label={t("hdr.more")}
                      title={t("hdr.more")}
                    >
                      <IconMore />
                    </button>
                    {moreOpen && (
                      <div className="more-menu" role="menu">
                        {hasActive && (
                          <button
                            className="more-item"
                            role="menuitem"
                            onClick={() => {
                              setMoreOpen(false);
                              pauseAll();
                            }}
                          >
                            <IconPause size={14} />
                            {t("hdr.pauseAll")}
                          </button>
                        )}
                        {!hasActive && hasResumable && (
                          <button
                            className="more-item"
                            role="menuitem"
                            onClick={() => {
                              setMoreOpen(false);
                              resumeAll();
                            }}
                          >
                            <IconPlay size={14} />
                            {t("hdr.resumeAll")}
                          </button>
                        )}
                        {stats.counts.completed > 0 && (
                          <button
                            className="more-item"
                            role="menuitem"
                            onClick={() => {
                              setMoreOpen(false);
                              clearCompleted();
                            }}
                          >
                            <IconTrash size={14} />
                            {t("hdr.clearDone")}
                          </button>
                        )}
                      </div>
                    )}
                  </div>
                )}
              </>
            )}
          </div>
        </header>

        <div className="content" ref={contentRef}>
          {/* 批量操作条：只在多选模式出现，吸顶显示，滚到列表深处也能一键处理 */}
          {view === "downloads" && selectMode && (
            <div className="batch-bar">
              <div className="batch-count">
                {t("batch.title", { n: selected.size })}
              </div>
              <div className="batch-actions">
                <button
                  className="btn btn-soft btn-sm"
                  disabled={selected.size === 0 || batchBusy}
                  onClick={() => void runBatch("pause", [...selected])}
                >
                  <IconPause size={13} />
                  {t("batch.pause")}
                </button>
                <button
                  className="btn btn-soft btn-sm"
                  disabled={selected.size === 0 || batchBusy}
                  onClick={() => void runBatch("resume", [...selected])}
                >
                  <IconPlay size={13} />
                  {t("batch.resume")}
                </button>
                <button
                  className="btn btn-danger btn-sm"
                  disabled={selected.size === 0 || batchBusy}
                  onClick={() => setBatchDelOpen(true)}
                >
                  <IconTrash size={13} />
                  {t("batch.remove")}
                </button>
              </div>
            </div>
          )}
          {/* 更新提示横幅 */}
          {view === "downloads" &&
            updateInfo?.has_update &&
            !updateDismissed && (
              <div className="update-banner">
                <span className="update-badge">NEW</span>
                <div className="update-text">
                  {t("upd.found")} <b>v{updateInfo.latest_version}</b>
                  <span className="update-muted">
                    （{t("upd.current")} v{updateInfo.current_version}）
                  </span>
                </div>
                <button
                  className="btn btn-primary update-btn"
                  onClick={() =>
                    api
                      .openUrl(updateInfo.release_url)
                      .catch(() => toast(t("upd.openFail"), "error"))
                  }
                >
                  {t("upd.go")}
                </button>
                <button
                  className="icon-btn update-close"
                  aria-label={t("dlg.close")}
                  onClick={() => setUpdateDismissed(true)}
                >
                  ✕
                </button>
              </div>
            )}
          {view === "settings" ? (
            <SettingsPage
              settings={settings}
              onChange={setSettings}
              toast={toast}
            />
          ) : (
            <>
              {/* 抓取页保持挂载、只切显隐：输入草稿、分析结果、勾选与滚动位置
                  切走再回来都还在（卸载重建会把它们全部丢掉） */}
              <div style={{ display: view === "grab" ? undefined : "none" }}>
                <GrabPage settings={settings} toast={toast} />
              </div>
              {view === "downloads" &&
                (visible.length === 0 ? (
                  <EmptyState
                    filter={filter}
                    onNew={() => setShowNew(true)}
                    t={t}
                  />
                ) : (
                  // 上下占位用 padding 而不是空 div：`.task-list` 是 flex + gap 布局，
                  // 空 div 会被额外加上一行 gap，占位高度就不准了。
                  <div
                    className="task-list"
                    ref={listRef}
                    style={{ paddingTop: padTop, paddingBottom: padBottom }}
                  >
                    {shownTasks.map((id, i) => (
                      <TaskCard
                        key={id}
                        taskId={id}
                        meta={settings.task_meta}
                        onPause={handlePause}
                        onResume={handleResume}
                        onRemove={handleRemove}
                        onOpen={handleOpen}
                        onReveal={handleReveal}
                        selectable={selectMode}
                        selected={selected.has(id)}
                        onToggle={toggleOne}
                        focused={focusIdx === start + i}
                        onLongPress={enterSelectFromLongPress}
                      />
                    ))}
                  </div>
                ))}
            </>
          )}
        </div>
      </main>

      {/* 移动端浮动新建按钮：必须放在 .header 之外，
          否则会被 header 的 backdrop-filter 变成包含块而错位 */}
      {view === "downloads" && (
        <button
          className="btn btn-primary mobile-fab"
          onClick={() => setShowNew(true)}
          aria-label={t("hdr.newTask")}
          title={t("hdr.newTask")}
        >
          <IconPlus />
        </button>
      )}

      {delTarget && (
        <DeleteDialog
          task={delTarget}
          onCancel={() => setDelTarget(null)}
          onRemove={confirmRemove}
        />
      )}

      {batchDelOpen && (
        <BatchDeleteDialog
          count={selected.size}
          hasDone={[...selected].some(
            (id) => taskStore.get(id)?.state === "completed",
          )}
          onCancel={() => setBatchDelOpen(false)}
          onRemove={(deleteFile) => {
            setBatchDelOpen(false);
            void runBatch("remove", [...selected], deleteFile);
          }}
        />
      )}

      {batchResult && (
        <BatchResultDialog
          report={batchResult.report}
          onClose={() => {
            setBatchResult(null);
            // 没有可重试项时直接退出多选：剩下的失败要么是任务已不存在，
            // 要么是重试也不会成功，留着选中只会让用户以为自己还能再点一次
            if (batchResult.report.failed.every((f) => !f.retryable)) {
              setSelectMode(false);
              setSelected(new Set());
            }
          }}
          onRetry={(ids) => {
            const { action, deleteFile } = batchResult;
            setBatchResult(null);
            void runBatch(action, ids, deleteFile);
          }}
        />
      )}

      {showNew && (
        <NewTaskDialog settings={settings} onClose={closeNew} onDone={toast} />
      )}

      <div className="toast-wrap">
        {toasts.map((t) => (
          <div
            key={t.id}
            className={cx("toast", t.kind === "error" && "error")}
          >
            {t.kind === "error" ? <IconX size={15} /> : <IconCheck size={15} />}
            {t.msg}
          </div>
        ))}
      </div>
    </div>
  );
}

/** 删除任务确认：只删记录，还是连本地文件（含未下完的半成品）一起删 */
function DeleteDialog({
  task,
  onCancel,
  onRemove,
}: {
  task: Task;
  onCancel: () => void;
  onRemove: (deleteFile: boolean) => void;
}) {
  const { t } = useI18n();
  const dlgRef = useDialogA11y(onCancel);
  const done = task.state === "completed";
  return (
    <div
      className="overlay"
      onMouseDown={(e) => e.target === e.currentTarget && onCancel()}
    >
      <div
        className="dialog del-dialog"
        role="dialog"
        aria-modal="true"
        tabIndex={-1}
        ref={dlgRef}
      >
        <div className="dialog-head">
          <div className="del-icon">
            <IconTrash size={17} />
          </div>
          <div style={{ flex: 1 }}>
            <div className="dialog-title">{t("del.title")}</div>
            <div className="dialog-desc">
              {done ? t("del.descDone") : t("del.descPartial")}
            </div>
          </div>
        </div>
        <div className="dialog-body">
          <div className="del-file">
            <div className="del-file-name" title={task.file_name}>
              {task.file_name}
            </div>
            <div className="del-file-path" title={task.save_path}>
              {task.save_path}
            </div>
          </div>
          {!done && (
            <div className="field-hint warn">{t("del.partialHint")}</div>
          )}
        </div>
        <div className="dialog-foot">
          <button className="btn" type="button" onClick={onCancel}>
            {t("dlg.cancel")}
          </button>
          <div className="spacer" />
          <button className="btn" type="button" onClick={() => onRemove(false)}>
            {t("del.keepFile")}
          </button>
          <button
            className="btn btn-danger"
            type="button"
            onClick={() => onRemove(true)}
          >
            {t("del.withFile")}
          </button>
        </div>
      </div>
    </div>
  );
}

function EmptyState({
  filter,
  onNew,
  t,
}: {
  filter: FilterKey;
  onNew: () => void;
  t: (k: string) => string;
}) {
  const content: Record<FilterKey, { title: string; desc: string }> = {
    all: { title: t("empty.all.title"), desc: t("empty.all.desc") },
    downloading: {
      title: t("empty.downloading.title"),
      desc: t("empty.downloading.desc"),
    },
    completed: {
      title: t("empty.completed.title"),
      desc: t("empty.completed.desc"),
    },
    paused: { title: t("empty.paused.title"), desc: t("empty.paused.desc") },
    error: { title: t("empty.error.title"), desc: t("empty.error.desc") },
  };
  const c = content[filter];
  return (
    <div className="empty">
      <div className="empty-icon">
        <IconDownload size={33} />
      </div>
      <div className="empty-title">{c.title}</div>
      <div className="empty-desc">{c.desc}</div>
      {filter === "all" && (
        <div className="empty-actions">
          <button className="btn btn-primary" onClick={onNew}>
            <IconPlus />
            {t("hdr.newTask")}
          </button>
        </div>
      )}
    </div>
  );
}

/** 批量删除确认：条数与「是否全都不算已完成」决定提示语气，删文件仍要单独确认一次 */
function BatchDeleteDialog({
  count,
  hasDone,
  onCancel,
  onRemove,
}: {
  count: number;
  hasDone: boolean;
  onCancel: () => void;
  onRemove: (deleteFile: boolean) => void;
}) {
  const { t } = useI18n();
  const dlgRef = useDialogA11y(onCancel);
  return (
    <div
      className="overlay"
      onMouseDown={(e) => e.target === e.currentTarget && onCancel()}
    >
      <div
        className="dialog del-dialog"
        role="dialog"
        aria-modal="true"
        tabIndex={-1}
        ref={dlgRef}
      >
        <div className="dialog-head">
          <div className="del-icon">
            <IconTrash size={17} />
          </div>
          <div style={{ flex: 1 }}>
            <div className="dialog-title">
              {t("batch.delTitle", { n: count })}
            </div>
            <div className="dialog-desc">
              {hasDone ? t("batch.delDescDone") : t("batch.delDescPartial")}
            </div>
          </div>
        </div>
        <div className="dialog-body">
          <div className="field-hint warn">{t("batch.delHint")}</div>
        </div>
        <div className="dialog-foot">
          <button className="btn" type="button" onClick={onCancel}>
            {t("dlg.cancel")}
          </button>
          <div className="spacer" />
          <button className="btn" type="button" onClick={() => onRemove(false)}>
            {t("del.keepFile")}
          </button>
          <button
            className="btn btn-danger"
            type="button"
            onClick={() => onRemove(true)}
          >
            {t("del.withFile")}
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * 批量结果明细：成功几条、失败几条、每条失败的原因。
 *
 * 不只给一句「部分失败」——用户需要知道是哪几条、要不要重试。失败项里
 * `retryable` 为真的才提供「重试失败项」，否则按钮点了也只会再失败一次。
 */
function BatchResultDialog({
  report,
  onClose,
  onRetry,
}: {
  report: BatchReport;
  onClose: () => void;
  onRetry: (ids: string[]) => void;
}) {
  const { t } = useI18n();
  const dlgRef = useDialogA11y(onClose);
  const retryable = report.failed.filter((f) => f.retryable).map((f) => f.id);
  return (
    <div
      className="overlay"
      onMouseDown={(e) => e.target === e.currentTarget && onClose()}
    >
      <div
        className="dialog batch-result"
        role="dialog"
        aria-modal="true"
        tabIndex={-1}
        ref={dlgRef}
      >
        <div className="dialog-head">
          <div style={{ flex: 1 }}>
            <div className="dialog-title">{t("batch.resultTitle")}</div>
            <div className="dialog-desc">
              {t("batch.resultSummary", {
                ok: report.ok.length,
                fail: report.failed.length,
              })}
            </div>
          </div>
        </div>
        <div className="dialog-body">
          <ul className="batch-fails">
            {report.failed.map((f) => {
              const name = taskStore.get(f.id)?.file_name ?? f.id;
              return (
                <li key={f.id} className="batch-fail">
                  <div className="batch-fail-name" title={name}>
                    {name}
                  </div>
                  <div className="batch-fail-reason">
                    {f.message}
                    {/* 机器码放小字：反馈问题时能直接抄，不用猜是哪类错误 */}
                    <span className="batch-fail-code">{f.code}</span>
                  </div>
                </li>
              );
            })}
          </ul>
        </div>
        <div className="dialog-foot">
          <button className="btn" type="button" onClick={onClose}>
            {t("dlg.close")}
          </button>
          <div className="spacer" />
          <button
            className="btn btn-primary"
            type="button"
            disabled={retryable.length === 0}
            onClick={() => onRetry(retryable)}
          >
            {t("batch.retryFailed", { n: retryable.length })}
          </button>
        </div>
      </div>
    </div>
  );
}
