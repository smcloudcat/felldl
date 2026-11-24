import { useEffect, useMemo, useRef, useState } from "react";
import type { AddTaskRequest, Settings } from "./types";
import { api } from "./bridge";
import { IconClock, IconFolder, IconLayers, IconLink, IconX, IconZap } from "./icons";
import { cx } from "./format";
import { useI18n } from "./i18n";

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
  const { t } = useI18n();
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
  const [cookie, setCookie] = useState("");
  const [mirrorsText, setMirrorsText] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [interval, setInterval] = useState(0);
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

  const limitLabel = (v: number) => (v === 0 ? t("dlg.unlimited") : `${v} KB/s`);

  const urls = useMemo(
    () =>
      text
        .split(/[\n\s]+/)
        .map((s) => s.trim())
        .filter((s) => s.length > 0 && (s.startsWith("http://") || s.startsWith("https://") || s.startsWith("magnet:"))),
    [text]
  );

  const hasMagnet = urls.some((u) => u.startsWith("magnet:"));

  const invalidCount = useMemo(() => {
    const all = text.split(/[\n\s]+/).map((s) => s.trim()).filter(Boolean);
    return all.length - urls.length;
  }, [text, urls.length]);

  const mirrorUrls = useMemo(
    () =>
      mirrorsText
        .split(/[\n\s]+/)
        .map((s) => s.trim())
        .filter((s) => s.startsWith("http://") || s.startsWith("https://")),
    [mirrorsText]
  );

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
      setErr(t("dlg.errEmpty"));
      return;
    }
    setBusy(true);
    setErr(null);
    try {
      const scheduledAt =
        schedule === "later" && scheduleAt ? new Date(scheduleAt).getTime() : null;
      if (schedule === "later" && (!scheduledAt || scheduledAt <= Date.now())) {
        setErr(t("dlg.errSchedule"));
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
        cookie: cookie.trim() || null,
        mirrors: mirrorUrls,
        username: username.trim() || null,
        password: password || null,
        interval_secs: interval,
      }));

      if (reqs.length === 1) {
        await api.addTask(reqs[0]);
      } else {
        await api.addTasks(reqs);
      }
      onDone(
        urls.length === 1
          ? schedule === "later"
            ? t("dlg.queuedScheduled")
            : t("dlg.started")
          : t("dlg.addedN", { n: urls.length })
      );
      onClose();
    } catch (e: any) {
      setErr(typeof e === "string" ? e : e?.message ?? t("dlg.addFail"));
      setBusy(false);
    }
  }


  return (
    <div className="overlay" onMouseDown={(e) => e.target === e.currentTarget && !busy && onClose()}>
      <div className="dialog wide" role="dialog" aria-modal="true">
        <div className="dialog-head">
          <div style={{ flex: 1 }}>
            <div className="dialog-title">{t("dlg.title")}</div>
            <div className="dialog-desc">{t("dlg.desc")}</div>
          </div>
          <button className="icon-btn" onClick={onClose} disabled={busy} aria-label={t("dlg.close")}>
            <IconX />
          </button>
        </div>

        <div className="dialog-body">
          <div className="field">
            <label className="field-label">
              <IconLink size={14} />
              {t("dlg.url")}
              {urls.length > 1 && (
                <span style={{ color: "var(--brand-1)", fontWeight: 700 }}>
                  · {t("dlg.recognized", { n: urls.length })}
                </span>
              )}
            </label>
            <textarea
              ref={textRef}
              className="textarea"
              placeholder={t("dlg.urlPlaceholder")}
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
            {hasMagnet && (
              <div className="field-hint" style={{ color: "var(--brand-1)" }}>
                🧲 {t("dlg.btHint")}
              </div>
            )}
            {invalidCount > 0 && (
              <div className="field-hint" style={{ color: "var(--accent-warn)" }}>
                {t("dlg.ignored", { n: invalidCount })}
              </div>
            )}
          </div>

          <div className="field">
            <label className="field-label">
              <IconFolder size={14} />
              {t("dlg.saveDir")}
            </label>
            <div className="input-row">
              <input
                className="input"
                value={dir}
                onChange={(e) => setDir(e.target.value)}
                spellCheck={false}
              />
              <button className="btn btn-soft" onClick={pickDir} type="button">
                {t("dlg.browse")}
              </button>
            </div>
          </div>

          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12 }}>
            <div className="field">
              <label className="field-label">
                <IconLayers size={14} />
                {t("dlg.conns")}
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
                {t("dlg.limit")}
              </label>
              <select
                className="select"
                value={limit}
                onChange={(e) => setLimit(Number(e.target.value))}
              >
                {SPEED_OPTIONS.map((o) => (
                  <option key={o.value} value={o.value}>
                    {limitLabel(o.value)}
                  </option>
                ))}
              </select>
            </div>
          </div>

          <div className="field">
            <label className="field-label">
              <IconClock size={14} />
              {t("dlg.time")}
            </label>
            <div className="seg-control" style={{ alignSelf: "flex-start" }}>
              <button
                type="button"
                className={cx("seg-btn", schedule === "now" && "active")}
                onClick={() => setSchedule("now")}
              >
                {t("dlg.now")}
              </button>
              <button
                type="button"
                className={cx("seg-btn", schedule === "later" && "active")}
                onClick={() => setSchedule("later")}
              >
                {t("dlg.later")}
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
              {showAdvanced ? t("dlg.advancedHide") : t("dlg.advanced")}
            </button>
          </div>

          {showAdvanced && (
            <>
              <div className="field">
                <label className="field-label">{t("dlg.referer")}</label>
                <input
                  className="input"
                  placeholder="https://example.com/"
                  value={referer}
                  onChange={(e) => setReferer(e.target.value)}
                  spellCheck={false}
                />
                <div className="field-hint">{t("dlg.refererHint")}</div>
              </div>
              <div className="field">
                <label className="field-label">🍪 {t("dlg.cookie")}</label>
                <textarea
                  className="textarea"
                  style={{ minHeight: 48 }}
                  placeholder="sessionid=xxx; token=yyy"
                  value={cookie}
                  onChange={(e) => setCookie(e.target.value)}
                  spellCheck={false}
                />
                <div className="field-hint">{t("dlg.cookieHint")}</div>
              </div>
              <div className="field">
                <label className="field-label">🪞 {t("dlg.mirrors")}</label>
                <textarea
                  className="textarea"
                  style={{ minHeight: 56 }}
                  placeholder={"https://mirror1.example.com/file.zip\nhttps://mirror2.example.org/file.zip"}
                  value={mirrorsText}
                  onChange={(e) => setMirrorsText(e.target.value)}
                  spellCheck={false}
                />
                <div className="field-hint">{t("dlg.mirrorsHint")}</div>
              </div>
              <div className="field">
                <label className="field-label">🔑 {t("dlg.auth")}</label>
                <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
                  <input
                    className="input"
                    placeholder={t("dlg.username")}
                    value={username}
                    onChange={(e) => setUsername(e.target.value)}
                    autoComplete="off"
                    spellCheck={false}
                  />
                  <input
                    className="input"
                    type="password"
                    placeholder={t("dlg.password")}
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    autoComplete="new-password"
                  />
                </div>
                <div className="field-hint">{t("dlg.authHint")}</div>
              </div>
              <div className="field">
                <label className="field-label">🔄 {t("dlg.interval")}</label>
                <select
                  className="select"
                  value={interval}
                  onChange={(e) => setInterval(Number(e.target.value))}
                >
                  <option value={0}>{t("dlg.intervalOff")}</option>
                  <option value={300}>5 min</option>
                  <option value={1800}>30 min</option>
                  <option value={3600}>1 h</option>
                  <option value={21600}>6 h</option>
                  <option value={86400}>24 h</option>
                </select>
                <div className="field-hint">{t("dlg.intervalHint")}</div>
              </div>
            </>
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
            {t("dlg.threadsSummary", { n: conns })} ·{" "}
            {limit > 0 ? t("dlg.limitSummary", { n: limit }) : t("dlg.unlimited")}
          </span>
          <div className="spacer" />
          <button className="btn btn-ghost" onClick={onClose} disabled={busy}>
            {t("dlg.cancel")}
          </button>
          <button
            className="btn btn-primary"
            onClick={submit}
            disabled={busy || urls.length === 0}
          >
            {busy ? t("dlg.processing") : schedule === "later" ? t("dlg.submitScheduled") : t("dlg.submit")}
          </button>
        </div>
      </div>
    </div>
  );
}
