import { useEffect, useMemo, useRef, useState } from "react";
import type { AddTaskRequest, Settings } from "./types";
import { api } from "./bridge";
import { IconClock, IconFolder, IconLayers, IconLink, IconX, IconZap } from "./icons";
import { cx } from "./format";

interface Props {
  settings: Settings;
  initialUrls?: string[];
  onClose: () => void;
  onDone: (msg: string) => void;
}

/** 连接数预设 */
const CONN_OPTIONS = [1, 4, 8, 16, 32];

/** 限速预设（KB/s，0 = 不限） */
const SPEED_OPTIONS = [
  { label: "不限速", value: 0 },
  { label: "512 KB/s", value: 512 },
  { label: "2 MB/s", value: 2048 },
  { label: "5 MB/s", value: 5120 },
  { label: "10 MB/s", value: 10240 },
];

export function NewTaskDialog({ settings, initialUrls, onClose, onDone }: Props) {
  const [text, setText] = useState((initialUrls ?? []).join("\n"));
  const [dir, setDir] = useState(settings.download_dir);
  const [conns, setConns] = useState(settings.default_connections);
  const [limit, setLimit] = useState(0);
  const [schedule, setSchedule] = useState<"now" | "later">("now");
  const [scheduleAt, setScheduleAt] = useState(() => {
    // 默认 10 分钟后
    const d = new Date(Date.now() + 10 * 60_000);
    d.setSeconds(0, 0);
    const pad = (v: number) => String(v).padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
  });
  const [referer, setReferer] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const textRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    textRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, busy]);

  const urls = useMemo(
    () =>
      text
        .split(/[\n\s]+/)
        .map((s) => s.trim())
        .filter((s) => s.length > 0 && (s.startsWith("http://") || s.startsWith("https://"))),
    [text]
  );

  const invalidCount = useMemo(() => {
    const all = text.split(/[\n\s]+/).map((s) => s.trim()).filter(Boolean);
    return all.length - urls.length;
  }, [text, urls.length]);

  async function pickDir() {
    try {
      const picked = await api.pickFolder();
      if (picked) setDir(picked);
    } catch {
      /* 浏览器预览模式忽略 */
    }
  }

  async function submit() {
    if (urls.length === 0) {
      setErr("请至少输入一个有效的 http/https 链接");
      return;
    }
    setBusy(true);
    setErr(null);
    try {
      const scheduledAt =
        schedule === "later" && scheduleAt ? new Date(scheduleAt).getTime() : null;
      if (schedule === "later" && (!scheduledAt || scheduledAt <= Date.now())) {
        setErr("定时时间必须晚于当前时间");
        setBusy(false);
        return;
      }

      const reqs: AddTaskRequest[] = urls.map((u) => ({
        url: u,
        save_dir: dir || null,
        connections: conns,
        speed_limit: limit > 0 ? limit * 1024 : null,
        scheduled_at: scheduledAt,
        referer: referer.trim() || null,
      }));

      if (reqs.length === 1) {
        await api.addTask(reqs[0]);
      } else {
        await api.addTasks(reqs);
      }
      onDone(
        urls.length === 1
          ? schedule === "later"
            ? "已加入定时队列"
            : "已开始下载"
          : `已添加 ${urls.length} 个任务`
      );
      onClose();
    } catch (e: any) {
      setErr(typeof e === "string" ? e : e?.message ?? "添加失败");
      setBusy(false);
    }
  }

  return (
    <div className="overlay" onMouseDown={(e) => e.target === e.currentTarget && !busy && onClose()}>
      <div className="dialog wide" role="dialog" aria-modal="true">
        <div className="dialog-head">
          <div style={{ flex: 1 }}>
            <div className="dialog-title">新建下载</div>
            <div className="dialog-desc">
              支持多线程分段加速，可一次粘贴多个链接
            </div>
          </div>
          <button className="icon-btn" onClick={onClose} disabled={busy} aria-label="关闭">
            <IconX />
          </button>
        </div>

        <div className="dialog-body">
          <div className="field">
            <label className="field-label">
              <IconLink size={14} />
              下载链接
              {urls.length > 1 && (
                <span style={{ color: "var(--brand-1)", fontWeight: 700 }}>
                  · 已识别 {urls.length} 个
                </span>
              )}
            </label>
            <textarea
              ref={textRef}
              className="textarea"
              placeholder={"https://example.com/file.zip\n每行一个链接，支持批量添加"}
              value={text}
              onChange={(e) => {
                setText(e.target.value);
                setErr(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) submit();
              }}
              spellCheck={false}
            />
            {invalidCount > 0 && (
              <div className="field-hint" style={{ color: "var(--accent-warn)" }}>
                已忽略 {invalidCount} 个非 http(s) 内容
              </div>
            )}
          </div>

          <div className="field">
            <label className="field-label">
              <IconFolder size={14} />
              保存位置
            </label>
            <div className="input-row">
              <input
                className="input"
                value={dir}
                onChange={(e) => setDir(e.target.value)}
                spellCheck={false}
              />
              <button className="btn btn-soft" onClick={pickDir} type="button">
                浏览
              </button>
            </div>
          </div>

          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12 }}>
            <div className="field">
              <label className="field-label">
                <IconLayers size={14} />
                连接数
              </label>
              <div className="seg-control numeric">
                {CONN_OPTIONS.map((n) => (
                  <button
                    key={n}
                    type="button"
                    className={cx("seg-btn", conns === n && "active")}
                    onClick={() => setConns(n)}
                  >
                    {n}
                  </button>
                ))}
              </div>
            </div>

            <div className="field">
              <label className="field-label">
                <IconZap size={14} />
                限速
              </label>
              <select
                className="select"
                value={limit}
                onChange={(e) => setLimit(Number(e.target.value))}
              >
                {SPEED_OPTIONS.map((o) => (
                  <option key={o.value} value={o.value}>
                    {o.label}
                  </option>
                ))}
              </select>
            </div>
          </div>

          <div className="field">
            <label className="field-label">
              <IconClock size={14} />
              开始时间
            </label>
            <div className="seg-control" style={{ alignSelf: "flex-start" }}>
              <button
                type="button"
                className={cx("seg-btn", schedule === "now" && "active")}
                onClick={() => setSchedule("now")}
              >
                立即开始
              </button>
              <button
                type="button"
                className={cx("seg-btn", schedule === "later" && "active")}
                onClick={() => setSchedule("later")}
              >
                定时下载
              </button>
            </div>
            {schedule === "later" && (
              <input
                className="input"
                type="datetime-local"
                value={scheduleAt}
                onChange={(e) => setScheduleAt(e.target.value)}
                style={{ marginTop: 8 }}
              />
            )}
          </div>

          <div>
            <button
              type="button"
              className="btn btn-soft btn-sm"
              onClick={() => setShowAdvanced((v) => !v)}
            >
              {showAdvanced ? "收起高级选项" : "高级选项"}
            </button>
          </div>

          {showAdvanced && (
            <div className="field">
              <label className="field-label">Referer（防盗链站点需要）</label>
              <input
                className="input"
                placeholder="https://example.com/"
                value={referer}
                onChange={(e) => setReferer(e.target.value)}
                spellCheck={false}
              />
              <div className="field-hint">
                部分网站会校验来源页。留空则由服务器自行判断。
              </div>
            </div>
          )}

          {err && (
            <div
              style={{
                padding: "9px 12px",
                borderRadius: "var(--r-sm)",
                background: "rgba(239,68,68,0.1)",
                border: "1px solid rgba(239,68,68,0.24)",
                color: "var(--accent-danger)",
                fontSize: 12.4,
              }}
            >
              {err}
            </div>
          )}
        </div>

        <div className="dialog-foot">
          <span style={{ fontSize: 11.6, color: "var(--text-mute)" }}>
            {conns} 线程 · {limit > 0 ? `限速 ${limit} KB/s` : "不限速"}
          </span>
          <div className="spacer" />
          <button className="btn btn-ghost" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button className="btn btn-primary" onClick={submit} disabled={busy || urls.length === 0}>
            {busy ? "处理中…" : schedule === "later" ? "加入定时" : "开始下载"}
          </button>
        </div>
      </div>
    </div>
  );
}
