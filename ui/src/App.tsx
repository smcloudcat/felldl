import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { FilterKey, Settings, Task } from "./types";
import { api, subscribe } from "./bridge";
import { TaskCard } from "./TaskCard";
import { NewTaskDialog } from "./NewTaskDialog";
import { SettingsPage } from "./SettingsPage";
import { cx, formatSpeed, isActive } from "./format";
import {
  IconBolt,
  IconCheck,
  IconClock,
  IconDownload,
  IconFolder,
  IconMenu,
  IconPause,
  IconPlay,
  IconPlus,
  IconSettings,
  IconTrash,
  IconX,
} from "./icons";

type View = "downloads" | "settings";

const FILTERS: { key: FilterKey; label: string; icon: any }[] = [
  { key: "all", label: "全部任务", icon: IconDownload },
  { key: "downloading", label: "进行中", icon: IconBolt },
  { key: "completed", label: "已完成", icon: IconCheck },
  { key: "paused", label: "已暂停", icon: IconPause },
  { key: "error", label: "失败", icon: IconX },
];

interface Toast {
  id: number;
  msg: string;
  kind: "ok" | "error";
}

export default function App() {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [view, setView] = useState<View>("downloads");
  const [filter, setFilter] = useState<FilterKey>("all");
  const [showNew, setShowNew] = useState(false);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastId = useRef(0);

  const toast = useCallback((msg: string, kind: "ok" | "error" = "ok") => {
    const id = ++toastId.current;
    setToasts((t) => [...t, { id, msg, kind }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 2600);
  }, []);

  // ── 初始化 ──
  useEffect(() => {
    (async () => {
      try {
        const [ts, s] = await Promise.all([api.listTasks(), api.getSettings()]);
        setTasks(ts);
        setSettings(s);
      } catch (e) {
        console.error("初始化失败", e);
        toast("初始化失败，请检查后端服务", "error");
      }
    })();
  }, [toast]);

  // ── 订阅引擎事件 ──
  useEffect(() => {
    let un: (() => void) | undefined;
    subscribe((e) => {
      // bridge 已解包，e 就是 EngineEvent 本身
      switch (e?.type) {
        case "taskUpdated": {
          const t = e.payload as Task;
          setTasks((prev) => {
            const i = prev.findIndex((x) => x.id === t.id);
            if (i === -1) return [t, ...prev];
            const next = prev.slice();
            next[i] = t;
            return next;
          });
          break;
        }
        case "taskRemoved":
          setTasks((prev) => prev.filter((x) => x.id !== e.payload));
          break;
        case "settingsUpdated":
          setSettings(e.payload);
          break;
        case "notice":
          toast(String(e.payload));
          break;
      }
    }).then((f) => (un = f));
    return () => un?.();
  }, [toast]);

  // ── 主题 ──
  useEffect(() => {
    if (!settings) return;
    const apply = () => {
      const pref = settings.theme;
      const dark =
        pref === "dark" ||
        (pref === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
      document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
    };
    apply();
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onMq = () => settings.theme === "system" && apply();
    mq.addEventListener("change", onMq);
    return () => mq.removeEventListener("change", onMq);
  }, [settings?.theme]);

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
    switch (filter) {
      case "downloading":
        return tasks.filter(
          (t) => t.state === "downloading" || t.state === "probing" || t.state === "queued"
        );
      case "completed":
        return tasks.filter((t) => t.state === "completed");
      case "paused":
        return tasks.filter((t) => t.state === "paused" || t.state === "scheduled");
      case "error":
        return tasks.filter((t) => t.state === "error");
      default:
        return tasks;
    }
  }, [tasks, filter]);

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
        toast("已删除任务");
      } catch (e) {
        toast(String(e), "error");
      }
    },
    [toast]
  );

  const handleOpen = useCallback(
    async (t: Task) => {
      try {
        await api.openFile(t.save_path);
      } catch (e) {
        toast("打开失败", "error");
      }
    },
    [toast]
  );

  const handleReveal = useCallback(
    async (t: Task) => {
      try {
        await api.showInFolder(t.save_path);
      } catch (e) {
        toast("打开文件夹失败", "error");
      }
    },
    [toast]
  );

  const pauseAll = useCallback(async () => {
    try {
      await api.pauseAll();
      toast("已暂停全部任务");
    } catch (e) {
      toast(String(e), "error");
    }
  }, [toast]);

  const resumeAll = useCallback(async () => {
    try {
      await api.resumeAll();
      toast("已恢复全部任务");
    } catch (e) {
      toast(String(e), "error");
    }
  }, [toast]);

  const clearCompleted = useCallback(async () => {
    try {
      await api.clearCompleted();
      toast("已清空完成记录");
    } catch (e) {
      toast(String(e), "error");
    }
  }, [toast]);

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
        <div style={{ margin: "auto", color: "var(--text-mute)", fontSize: 13 }}>
          正在启动引擎…
        </div>
      </div>
    );
  }

  return (
    <div className="app">
      {/* 移动端遮罩 */}
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

        <div className="nav-section">下载</div>
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

        <div className="nav-section">工具</div>
        <button
          className={cx("nav-item", view === "settings" && "active")}
          onClick={() => {
            setView("settings");
            setSidebarOpen(false);
          }}
        >
          <IconSettings />
          设置
        </button>

        <div className="sidebar-footer">
          <div className="speed-card">
            <div className="speed-card-label">当前速度</div>
            <div className="speed-card-value">
              {stats.totalSpeed > 0 ? formatSpeed(stats.totalSpeed) : "空闲"}
            </div>
            <div className="speed-card-sub">
              {stats.downloading.length > 0
                ? `${stats.downloading.length} 个任务下载中`
                : `${stats.completed.length} 个已完成`}
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
            aria-label="菜单"
          >
            <IconMenu />
          </button>

          <div className="header-title">
            {view === "settings"
              ? "设置"
              : FILTERS.find((f) => f.key === filter)?.label ?? "全部任务"}
            {view === "downloads" && visible.length > 0 && (
              <span className="count-pill">{visible.length}</span>
            )}
          </div>

          <div className="header-actions">
            {view === "downloads" && (
              <>
                {hasActive && (
                  <button className="btn btn-ghost btn-sm" onClick={pauseAll} title="全部暂停">
                    <IconPause size={13} />
                    <span className="desktop-label">全部暂停</span>
                  </button>
                )}
                {!hasActive && hasResumable && (
                  <button className="btn btn-ghost btn-sm" onClick={resumeAll} title="全部开始">
                    <IconPlay size={13} />
                    <span className="desktop-label">全部开始</span>
                  </button>
                )}
                {stats.completed.length > 0 && (
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={clearCompleted}
                    title="清空已完成"
                  >
                    <IconTrash size={13} />
                    <span className="desktop-label">清空完成</span>
                  </button>
                )}
                <button
                  className="btn btn-primary mobile-fab"
                  onClick={() => setShowNew(true)}
                  title="新建下载 (Ctrl+N)"
                >
                  <IconPlus />
                  <span>新建下载</span>
                </button>
              </>
            )}
          </div>
        </header>

        <div className="content">
          {view === "settings" ? (
            <SettingsPage
              settings={settings}
              onChange={setSettings}
              toast={toast}
            />
          ) : visible.length === 0 ? (
            <EmptyState filter={filter} onNew={() => setShowNew(true)} />
          ) : (
            <div className="task-list">
              {visible.map((t) => (
                <TaskCard
                  key={t.id}
                  task={t}
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

      {showNew && (
        <NewTaskDialog
          settings={settings}
          onClose={() => setShowNew(false)}
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

function EmptyState({ filter, onNew }: { filter: FilterKey; onNew: () => void }) {
  const content: Record<FilterKey, { title: string; desc: string }> = {
    all: {
      title: "还没有下载任务",
      desc: "粘贴一个链接开始吧，Felldl 会用多线程分段加速帮你抢满带宽。",
    },
    downloading: { title: "暂无进行中的任务", desc: "当前没有任务在下载。" },
    completed: { title: "还没有完成的任务", desc: "下载完成的文件会出现在这里。" },
    paused: { title: "没有暂停的任务", desc: "被暂停或定时等待的任务会显示在这里。" },
    error: { title: "没有失败的任务", desc: "一切顺利。" },
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
            新建下载
          </button>
        </div>
      )}
    </div>
  );
}
