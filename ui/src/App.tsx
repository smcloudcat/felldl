import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { FilterKey, Settings, Task, UpdateInfo } from "./types";
import { api, subscribe } from "./bridge";
import { TaskCard } from "./TaskCard";
import { NewTaskDialog } from "./NewTaskDialog";
import { SettingsPage } from "./SettingsPage";
import { GrabPage } from "./GrabPage";
import { cx, formatSpeed, isActive } from "./format";
import { useI18n } from "./i18n";
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
  // 稳定回调：进度更新重渲染不会让 NewTaskDialog 重挂 / 重新抢焦点
  const closeNew = useCallback(() => setShowNew(false), []);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [initError, setInitError] = useState<string | null>(null);
  const [slowHint, setSlowHint] = useState(false);
  const [reloadKey, setReloadKey] = useState(0);
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateDismissed, setUpdateDismissed] = useState(false);
  const toastId = useRef(0);

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
        const [ts, s] = await Promise.all([api.listTasks(), api.getSettings()]);
        if (!alive) return;
        clearTimeout(timer);
        setTasks(ts);
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

  // ── 启动后自动检查更新（静默：失败不打扰） ──
  useEffect(() => {
    if (!settings) return;
    let alive = true;
    api
      .checkUpdate()
      .then((info) => {
        if (alive && info?.has_update) setUpdateInfo(info);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
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
          next[i] = { ...cur, ...d };
          changed = true;
        }
        for (const t of batch.values()) {
          const i = next.findIndex((x) => x.id === t.id);
          if (i === -1) {
            next = [t, ...next];
          } else {
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
          progress.set(d.id, d);
          scheduleFlush(false);
          break;
        }
        case "taskUpdated": {
          const t = e.payload as Task;
          // 完整快照优先于增量进度
          progress.delete(t.id);
          // 合并同一任务的连续进度；非下载态（完成/暂停/错误/排队）立即提交
          pending.set(t.id, t);
          scheduleFlush(t.state !== "downloading" && t.state !== "probing");
          break;
        }
        case "taskRemoved":
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
    };
    apply();
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onMq = () => settings.theme === "system" && apply();
    mq.addEventListener("change", onMq);
    return () => mq.removeEventListener("change", onMq);
  }, [settings?.theme, settings?.ui_skin, settings?.btn_style]);

  // ── 统计 ──
  const stats = useMemo(() => {
    const downloading = tasks.filter((t) => isActive(t.state));
    const completed = tasks.filter((t) => t.state === "completed");
    const paused = tasks.filter((t) => t.state === "paused");
    const error = tasks.filter((t) => t.state === "error");
    const totalSpeed = downloading.reduce((a, t) => a + (t.speed || 0), 0);
    const scheduled = tasks.filter((t) => t.state === "scheduled");
    return {
      downloading,
      completed,
      paused,
      error,
      scheduled,
      totalSpeed,
      counts: {
        all: tasks.length,
        downloading: downloading.length + tasks.filter((t) => t.state === "queued").length,
        completed: completed.length,
        paused: paused.length,
        error: error.length,
      } as Record<FilterKey, number>,
    };
  }, [tasks]);

  const visible = useMemo(() => {
    let list = tasks;
    if (category) list = list.filter((t) => t.category === category);
    switch (filter) {
      case "downloading":
        return list.filter(
          (t) => t.state === "downloading" || t.state === "probing" || t.state === "queued"
        );
      case "completed":
        return list.filter((t) => t.state === "completed");
      case "paused":
        return list.filter((t) => t.state === "paused" || t.state === "scheduled");
      case "error":
        return list.filter((t) => t.state === "error");
      default:
        return list;
    }
  }, [tasks, filter, category]);

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

  const handleRemove = useCallback(
    async (id: string) => {
      try {
        await api.remove(id, false);
        toast(t("act.taskRemoved"));
      } catch (e) {
        toast(String(e), "error");
      }
    },
    [toast, t]
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

  const hasActive = stats.downloading.length > 0;
  const hasResumable = stats.paused.length + stats.error.length > 0;

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
              {stats.totalSpeed > 0 ? formatSpeed(stats.totalSpeed) : t("side.idle")}
            </div>
            <div className="speed-card-sub">
              {stats.downloading.length > 0
                ? t("side.activeTasks", { n: stats.downloading.length })
                : t("side.doneTasks", { n: stats.completed.length })}
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
            {view === "downloads" && visible.length > 0 && (
              <span className="count-pill">{visible.length}</span>
            )}
          </div>

          <div className="header-actions">
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
                {stats.completed.length > 0 && (
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

        <div className="content">
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
            <div className="task-list">
              {visible.map((t) => (
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
