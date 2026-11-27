import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { FilterKey, Settings, Task, TaskStats } from "./types";
import { api, subscribe, subscribeResync } from "./bridge";
import { mergeSnapshot, revOf } from "./taskMerge";
import { useVirtualList } from "./useVirtualList";
import { TaskCard } from "./TaskCard";
import { NewTaskDialog } from "./NewTaskDialog";
import { SettingsPage } from "./SettingsPage";
import { GrabPage } from "./GrabPage";
import { cx, formatSpeed } from "./format";
import { applyAppearance } from "./appearance";
import { useI18n } from "./i18n";
import { updateJob, useUpdateJob, wireUpdateJob } from "./useUpdateJob";
import {
  IconBolt,
  IconCheck,
  IconDownload,
  IconFolder,
  IconGlobe,
  IconMenu,
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
function matchesView(t: Task, filter: FilterKey, category: string | null): boolean {
  if (category && t.category !== category) return false;
  switch (filter) {
    case "downloading":
      return t.state === "downloading" || t.state === "probing" || t.state === "queued";
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
    [t]
  );

  const [tasks, setTasks] = useState<Task[]>([]);
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
  // 更新状态是全局 job：这里和设置页看的是同一份，切页面不重置
  const job = useUpdateJob();
  const updateInfo = job.info;
  const toastId = useRef(0);
  // 取快照（list_tasks）期间收到的事件：用来判断「谁是更新的那一份」，
  // 避免旧快照把已经推进到新状态的任务盖回去（见 taskMerge.ts）
  const touchedRef = useRef(new Set<string>());
  const removedRef = useRef(new Set<string>());

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
    (async () => {
      try {
        const [p, st] = await Promise.all([
          api.listTasks({ filter, category, offset: 0, limit: PAGE_SIZE }),
          api.taskStats(),
        ]);
        if (!alive) return;
        // 取第一页期间收到的事件不能丢：mergeSnapshot 用 touched 判断谁更新；
        // 合并后再按当前视图过一遍，挡住筛选切换期间在途事件带来的不匹配任务
        setTasks((prev) =>
          mergeSnapshot(prev, p.items, {
            touched: touchedRef.current,
            removed: removedRef.current,
          }).filter((t) => matchesView(t, filter, category))
        );
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
      setTasks((prev) => {
        let next = prev.slice();
        let changed = false;
        for (const [id, d] of pdelta) {
          const i = next.findIndex((x) => x.id === id);
          if (i === -1) continue; // 等完整快照到达后再合并
          const cur = next[i];
          // 已终态的任务忽略迟到进度，避免「完成/错误」被旧进度覆盖
          if (cur.state === "completed" || cur.state === "error") continue;
          // 版本号更小的增量是旧数据（例如补发的上一轮进度）
          if (revOf(d) < revOf(cur)) continue;
          next[i] = { ...cur, ...d };
          changed = true;
        }
        for (const t of batch.values()) {
          const i = next.findIndex((x) => x.id === t.id);
          if (i === -1) {
            next = [t, ...next];
          } else {
            // 本地已经更新（进度 tick 的 rev 更高）时不要被旧快照回退
            if (revOf(next[i]) > revOf(t)) continue;
            next[i] = t;
          }
          changed = true;
        }
        return changed ? next : prev;
      });
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
          setTasks((prev) => prev.filter((x) => x.id !== e.payload));
          break;
        case "settingsUpdated":
          setSettings(e.payload);
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
        const want = Math.max(tasks.length, PAGE_SIZE);
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
        setTasks((prev) =>
          mergeSnapshot(prev, items, {
            touched: touchedRef.current,
            removed: removedRef.current,
          }).filter((t) => matchesView(t, filter, category))
        );
        setPage((prev) => ({ ...prev, total: Math.max(prev.total, items.length) }));
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
    return () => {
      disposed = true;
      un?.();
    };
    // filter/category 决定重拉哪一段；tasks.length 变化说明加载窗口变了
  }, [filter, category, tasks.length]);

  // ── 主题 / 皮肤 / 按钮风格 ──
  useEffect(() => {
    if (!settings) return;
    const apply = () => {
      const pref = settings.theme;
      const dark =
        pref === "dark" ||
        (pref === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
      document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
      document.documentElement.setAttribute("data-skin", settings.ui_skin || "blue");
      document.documentElement.setAttribute("data-btn", settings.btn_style || "rounded");
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

  // 列表已由引擎按当前筛选分页返回，本地 `tasks` 就是「当前页 + 增量事件」，
  // 不再二次过滤（否则会漏掉尚未加载的历史）。
  const visible = tasks;

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
  const { start, end, padTop, padBottom } = useVirtualList({
    count: visible.length,
    scrollRef: contentRef,
    listRef,
  });
  const shownTasks = visible.slice(start, end);

  // 滚到接近已加载尾部就续拉下一页（虚拟列表只会挂当前视口，但数据仍需分页取）
  useEffect(() => {
    if (view !== "downloads" || !page.hasMore || loadingRef.current) return;
    if (end < tasks.length - 5) return;
    loadingRef.current = true;
    setLoadingMore(true);
    api
      .listTasks({ filter, category, offset: tasks.length, limit: PAGE_SIZE })
      .then((p) => {
        setTasks((prev) => {
          const seen = new Set(prev.map((x) => x.id));
          const add = p.items.filter((x) => !seen.has(x.id));
          return add.length ? [...prev, ...add] : prev;
        });
        setPage({ total: p.total, hasMore: p.has_more });
      })
      .catch(() => {})
      .finally(() => {
        loadingRef.current = false;
        setLoadingMore(false);
      });
  }, [end, tasks.length, page.hasMore, filter, category, view, PAGE_SIZE]);

  // ── 操作 ──
  const handlePause = useCallback(
    (id: string) => {
      api.pause(id).catch((e) => toast(String(e), "error"));
    },
    [toast]
  );

  const handleResume = useCallback(
    (id: string) => {
      api.resume(id).catch((e) => toast(String(e), "error"));
    },
    [toast]
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
    [delTarget, toast, t]
  );

  const handleOpen = useCallback(
    async (task: Task) => {
      try {
        await api.openFile(task.save_path);
      } catch {
        toast(t("act.openFail"), "error");
      }
    },
    [toast, t]
  );

  const handleReveal = useCallback(
    async (task: Task) => {
      try {
        await api.showInFolder(task.save_path);
      } catch {
        toast(t("act.revealFail"), "error");
      }
    },
    [toast, t]
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

  const hasActive = stats.active > 0;
  const hasResumable = stats.counts.paused + stats.counts.error > 0;

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
          <div className="boot-title">{initError ? t("boot.failed") : t("boot.starting")}</div>
          {slowHint && !initError && <div className="boot-hint">{t("boot.slow")}</div>}
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
      {sidebarOpen && <div className="sidebar-scrim" onClick={() => setSidebarOpen(false)} />}

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
              className={cx("nav-item", view === "downloads" && filter === key && "active")}
              onClick={() => {
                setView("downloads");
                setFilter(key);
                setSidebarOpen(false);
              }}
            >
              <Icon />
              {label}
              {stats.counts[key] > 0 && <span className="nav-count">{stats.counts[key]}</span>}
            </button>
          ))}

          {(settings.categories?.length ?? 0) > 0 && (
            <>
              <div className="nav-section">{t("set.categories")}</div>
              <button
                className={cx(
                  "nav-item",
                  view === "downloads" && category === null && filter === "all" && "active"
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
                const count = tasks.filter((t) => t.category === c.name).length;
                return (
                  <button
                    key={c.name}
                    className={cx("nav-item", view === "downloads" && category === c.name && "active")}
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
          </button>
        </nav>

        <div className="sidebar-footer">
          <div className="speed-card">
            <div className="speed-card-label">{t("side.speed")}</div>
            <div className="speed-card-value">
              {stats.total_speed > 0 ? formatSpeed(stats.total_speed) : t("side.idle")}
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
            {view === "settings"
              ? t("nav.settings")
              : view === "grab"
                ? t("nav.grab")
                : category
                  ? category
                  : FILTERS.find((f) => f.key === filter)?.label ?? t("filter.all")}
            {view === "downloads" && page.total > 0 && (
              <span className="count-pill">{Math.max(page.total, visible.length)}</span>
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
            {view === "downloads" && (
              <>
                {hasActive && (
                  <button className="btn btn-ghost btn-sm" onClick={pauseAll} title={t("hdr.pauseAll")}>
                    <IconPause size={13} />
                    <span className="desktop-label">{t("hdr.pauseAll")}</span>
                  </button>
                )}
                {!hasActive && hasResumable && (
                  <button className="btn btn-ghost btn-sm" onClick={resumeAll} title={t("hdr.resumeAll")}>
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
              </>
            )}
          </div>
        </header>

        <div className="content" ref={contentRef}>
          {/* 更新提示横幅 */}
          {view === "downloads" && updateInfo?.has_update && !updateDismissed && (
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
                  api.openUrl(updateInfo.release_url).catch(() => toast(t("upd.openFail"), "error"))
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
            <SettingsPage settings={settings} onChange={setSettings} toast={toast} />
          ) : view === "grab" ? (
            <GrabPage settings={settings} toast={toast} />
          ) : visible.length === 0 ? (
            <EmptyState filter={filter} onNew={() => setShowNew(true)} t={t} />
          ) : (
            // 上下占位用 padding 而不是空 div：`.task-list` 是 flex + gap 布局，
            // 空 div 会被额外加上一行 gap，占位高度就不准了。
            <div
              className="task-list"
              ref={listRef}
              style={{ paddingTop: padTop, paddingBottom: padBottom }}
            >
              {shownTasks.map((t) => (
                <TaskCard
                  key={t.id}
                  task={t}
                  meta={settings.task_meta}
                  onPause={handlePause}
                  onResume={handleResume}
                  onRemove={handleRemove}
                  onOpen={handleOpen}
                  onReveal={handleReveal}
                />
              ))}
            </div>
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

      {showNew && (
        <NewTaskDialog
          settings={settings}
          onClose={closeNew}
          onDone={toast}
        />
      )}

      <div className="toast-wrap">
        {toasts.map((t) => (
          <div key={t.id} className={cx("toast", t.kind === "error" && "error")}>
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
  const done = task.state === "completed";
  return (
    <div className="overlay" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div className="dialog del-dialog" role="dialog" aria-modal="true">
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
          {!done && <div className="field-hint warn">{t("del.partialHint")}</div>}
        </div>
        <div className="dialog-foot">
          <button className="btn" type="button" onClick={onCancel}>
            {t("dlg.cancel")}
          </button>
          <div className="spacer" />
          <button className="btn" type="button" onClick={() => onRemove(false)}>
            {t("del.keepFile")}
          </button>
          <button className="btn btn-danger" type="button" onClick={() => onRemove(true)}>
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
    downloading: { title: t("empty.downloading.title"), desc: t("empty.downloading.desc") },
    completed: { title: t("empty.completed.title"), desc: t("empty.completed.desc") },
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
