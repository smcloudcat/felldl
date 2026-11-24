import { useEffect, useState } from "react";
import type { Settings, UpdateInfo } from "./types";
import { api } from "./bridge";
import {
  IconCopy,
  IconFolder,
  IconGauge,
  IconGlobe,
  IconLayers,
  IconMoon,
  IconShield,
  IconSliders,
  IconSun,
  IconMonitor,
  IconCheck,
} from "./icons";
import { cx } from "./format";

interface Props {
  settings: Settings;
  onChange: (s: Settings) => void;
  toast: (msg: string, kind?: "ok" | "error") => void;
}

function Switch({
  on,
  onChange,
}: {
  on: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <button
      className={cx("switch", on && "on")}
      onClick={() => onChange(!on)}
      role="switch"
      aria-checked={on}
      type="button"
    />
  );
}

export function SettingsPage({ settings, onChange, toast }: Props) {
  const [draft, setDraft] = useState<Settings>(settings);
  const [saved, setSaved] = useState(false);
  const [apiInfo, setApiInfo] = useState<{ port: number; token: string; running: boolean } | null>(
    null
  );
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateErr, setUpdateErr] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);

  // 进入设置页时静默检查一次，顺带把当前版本号填上
  useEffect(() => {
    let alive = true;
    api
      .checkUpdate()
      .then((info) => {
        if (!alive) return;
        setUpdateInfo(info);
        setUpdateErr(null);
      })
      .catch((e) => {
        if (alive) setUpdateErr(typeof e === "string" ? e : "检查更新失败");
      });
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => setDraft(settings), [settings]);

  useEffect(() => {
    api.getApiInfo().then(setApiInfo).catch(() => {});
  }, [settings.api_port, settings.api_enabled]);

  const dirty = JSON.stringify(draft) !== JSON.stringify(settings);

  function patch(p: Partial<Settings>) {
    setDraft((d) => ({ ...d, ...p }));
    setSaved(false);
  }

  async function save() {
    try {
      const next = await api.updateSettings(draft);
      onChange(next);
      setSaved(true);
      toast("设置已保存");
      setTimeout(() => setSaved(false), 2000);
    } catch (e: any) {
      toast(typeof e === "string" ? e : "保存失败", "error");
    }
  }

  async function pickDir() {
    try {
      const p = await api.pickFolder();
      if (p) patch({ download_dir: p });
    } catch {}
  }

  async function copyToken() {
    if (!apiInfo) return;
    try {
      await navigator.clipboard.writeText(apiInfo.token);
      toast("令牌已复制");
    } catch {
      toast("复制失败", "error");
    }
  }

  async function checkUpdate() {
    setChecking(true);
    setUpdateErr(null);
    try {
      const info = await api.checkUpdate();
      setUpdateInfo(info);
      if (info.has_update) {
        toast(`发现新版本 v${info.latest_version}`);
      } else {
        toast(`已是最新版本 v${info.current_version}`);
      }
    } catch (e: any) {
      setUpdateErr(typeof e === "string" ? e : "检查更新失败");
      toast(typeof e === "string" ? e : "检查更新失败", "error");
    } finally {
      setChecking(false);
    }
  }

  const limitMB = Math.round((draft.global_speed_limit / 1024 / 1024) * 10) / 10;

  return (
    <div className="settings">
      {/* 下载设置 */}
      <div className="card">
        <div className="card-head">
          <div className="card-icon">
            <IconDownloadLayers />
          </div>
          <div className="card-title">下载</div>
        </div>
        <div className="card-sub">默认保存位置与并发策略</div>

        <div className="field">
          <label className="field-label">
            <IconFolder size={14} />
            默认下载目录
          </label>
          <div className="input-row">
            <input
              className="input"
              value={draft.download_dir}
              onChange={(e) => patch({ download_dir: e.target.value })}
              spellCheck={false}
            />
            <button className="btn btn-soft" onClick={pickDir} type="button">
              浏览
            </button>
          </div>
        </div>

        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12, marginTop: 14 }}>
          <div className="field">
            <label className="field-label">
              <IconLayers size={14} />
              默认连接数
            </label>
            <select
              className="select"
              value={draft.default_connections}
              onChange={(e) => patch({ default_connections: Number(e.target.value) })}
            >
              {[1, 2, 4, 8, 16, 32, 64].map((n) => (
                <option key={n} value={n}>
                  {n} 线程
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label className="field-label">
              <IconGauge size={14} />
              同时下载任务数
            </label>
            <select
              className="select"
              value={draft.max_concurrent_tasks}
              onChange={(e) => patch({ max_concurrent_tasks: Number(e.target.value) })}
            >
              {[1, 2, 3, 4, 5, 6, 8].map((n) => (
                <option key={n} value={n}>
                  {n} 个
                </option>
              ))}
            </select>
          </div>
        </div>

        <div className="switch-row" style={{ marginTop: 6 }}>
          <div className="switch-info">
            <div className="switch-title">添加后自动开始</div>
            <div className="switch-desc">关闭后新任务会进入排队状态，需手动开始</div>
          </div>
          <Switch on={draft.auto_start} onChange={(v) => patch({ auto_start: v })} />
        </div>
        <div className="switch-row">
          <div className="switch-info">
            <div className="switch-title">完成后通知</div>
            <div className="switch-desc">下载结束后发送系统通知</div>
          </div>
          <Switch
            on={draft.notify_on_complete}
            onChange={(v) => patch({ notify_on_complete: v })}
          />
        </div>
      </div>

      {/* 速度限制 */}
      <div className="card">
        <div className="card-head">
          <div className="card-icon">
            <IconGauge />
          </div>
          <div className="card-title">全局限速</div>
        </div>
        <div className="card-sub">
          限制所有任务的总带宽，0 表示不限速。当前：
          <strong style={{ color: "var(--brand-1)" }}>
            {draft.global_speed_limit === 0 ? "不限速" : `${limitMB} MB/s`}
          </strong>
        </div>
        <input
          type="range"
          min={0}
          max={50}
          step={0.5}
          value={limitMB}
          onChange={(e) =>
            patch({ global_speed_limit: Math.round(Number(e.target.value) * 1024 * 1024) })
          }
          style={{ width: "100%", accentColor: "var(--brand-1)", cursor: "pointer" }}
        />
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            fontSize: 11,
            color: "var(--text-mute)",
            marginTop: 2,
          }}
        >
          <span>不限</span>
          <span>25 MB/s</span>
          <span>50 MB/s</span>
        </div>
      </div>

      {/* 代理 */}
      <div className="card">
        <div className="card-head">
          <div className="card-icon">
            <IconGlobe />
          </div>
          <div className="card-title">代理服务器</div>
        </div>
        <div className="card-sub">通过 HTTP / SOCKS5 代理下载</div>

        <div className="switch-row" style={{ paddingTop: 0 }}>
          <div className="switch-info">
            <div className="switch-title">启用代理</div>
            <div className="switch-desc">所有下载请求都将经过代理</div>
          </div>
          <Switch
            on={draft.proxy.enabled}
            onChange={(v) => patch({ proxy: { ...draft.proxy, enabled: v } })}
          />
        </div>

        {draft.proxy.enabled && (
          <div style={{ marginTop: 10 }}>
            <div style={{ display: "grid", gridTemplateColumns: "110px 1fr 90px", gap: 8 }}>
              <select
                className="select"
                value={draft.proxy.kind}
                onChange={(e) => patch({ proxy: { ...draft.proxy, kind: e.target.value } })}
              >
                <option value="http">HTTP</option>
                <option value="socks5">SOCKS5</option>
              </select>
              <input
                className="input"
                placeholder="127.0.0.1"
                value={draft.proxy.host}
                onChange={(e) => patch({ proxy: { ...draft.proxy, host: e.target.value } })}
                spellCheck={false}
              />
              <input
                className="input"
                type="number"
                placeholder="端口"
                value={draft.proxy.port || ""}
                onChange={(e) =>
                  patch({ proxy: { ...draft.proxy, port: Number(e.target.value) || 0 } })
                }
              />
            </div>
            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8, marginTop: 8 }}>
              <input
                className="input"
                placeholder="用户名（可选）"
                value={draft.proxy.username}
                onChange={(e) => patch({ proxy: { ...draft.proxy, username: e.target.value } })}
                spellCheck={false}
              />
              <input
                className="input"
                type="password"
                placeholder="密码（可选）"
                value={draft.proxy.password}
                onChange={(e) => patch({ proxy: { ...draft.proxy, password: e.target.value } })}
              />
            </div>
          </div>
        )}
      </div>

      {/* 浏览器集成 */}
      <div className="card">
        <div className="card-head">
          <div className="card-icon">
            <IconShield />
          </div>
          <div className="card-title">浏览器集成</div>
        </div>
        <div className="card-sub">
          开启本地 API，浏览器扩展即可把下载任务转交给 Felldl
        </div>

        <div className="switch-row" style={{ paddingTop: 0 }}>
          <div className="switch-info">
            <div className="switch-title">启用本地接口</div>
            <div className="switch-desc">仅监听 127.0.0.1，需令牌鉴权</div>
          </div>
          <Switch on={draft.api_enabled} onChange={(v) => patch({ api_enabled: v })} />
        </div>

        {draft.api_enabled && apiInfo && (
          <div
            style={{
              marginTop: 10,
              padding: "11px 13px",
              borderRadius: "var(--r-sm)",
              background: "var(--bg-sunken)",
              border: "1px solid var(--border)",
              fontSize: 12,
              fontFamily: "ui-monospace, monospace",
              display: "flex",
              flexDirection: "column",
              gap: 7,
            }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span style={{ color: "var(--text-mute)", minWidth: 52 }}>地址</span>
              <span style={{ color: "var(--brand-1)", fontWeight: 650 }}>
                http://127.0.0.1:{apiInfo.port}
              </span>
              <span style={{ marginLeft: "auto", display: "flex", alignItems: "center", gap: 4 }}>
                <span
                  style={{
                    width: 6,
                    height: 6,
                    borderRadius: "50%",
                    background: apiInfo.running ? "var(--accent-ok)" : "var(--text-mute)",
                  }}
                />
                <span style={{ fontSize: 11, color: "var(--text-mute)" }}>
                  {apiInfo.running ? "运行中" : "已停止"}
                </span>
              </span>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span style={{ color: "var(--text-mute)", minWidth: 52 }}>令牌</span>
              <span
                style={{
                  flex: 1,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                  color: "var(--text-soft)",
                }}
              >
                {apiInfo.token}
              </span>
              <button className="icon-btn" onClick={copyToken} title="复制令牌">
                <IconCopy size={14} />
              </button>
            </div>
          </div>
        )}
      </div>

      {/* 外观 */}
      <div className="card">
        <div className="card-head">
          <div className="card-icon">
            <IconSliders />
          </div>
          <div className="card-title">外观</div>
        </div>
        <div className="card-sub">主题会立即生效</div>
        <div className="seg-control">
          {(
            [
              { v: "light", label: "浅色", Icon: IconSun },
              { v: "dark", label: "深色", Icon: IconMoon },
              { v: "system", label: "跟随系统", Icon: IconMonitor },
            ] as const
          ).map(({ v, label, Icon }) => (
            <button
              key={v}
              type="button"
              className={cx("seg-btn", draft.theme === v && "active")}
              onClick={() => patch({ theme: v })}
              style={{ display: "inline-flex", alignItems: "center", gap: 6 }}
            >
              <Icon size={13} />
              {label}
            </button>
          ))}
        </div>
      </div>

      {/* 关于与更新 */}
      <div className="card">
        <div className="card-head">
          <div className="card-icon">
            <IconGlobe size={15} />
          </div>
          <div className="card-title">关于与更新</div>
        </div>
        <div className="card-sub">
          Felldl v{updateInfo?.current_version ?? "…"} · 通过 GitHub Releases 检查新版本
        </div>

        <div style={{ display: "flex", gap: 8, marginTop: 12, alignItems: "center", flexWrap: "wrap" }}>
          <button className="btn btn-soft" onClick={checkUpdate} disabled={checking} type="button">
            {checking ? "检查中…" : "检查更新"}
          </button>
          {updateInfo && (
            <span style={{ fontSize: 12, color: "var(--text-mute)" }}>
              {updateInfo.has_update
                ? `发现新版本 v${updateInfo.latest_version}`
                : `已是最新（远端 v${updateInfo.latest_version || "-"}）`}
            </span>
          )}
          {updateInfo?.has_update && (
            <button
              className="btn btn-primary"
              onClick={() =>
                api.openUrl(updateInfo.release_url).catch(() => toast("打开链接失败", "error"))
              }
              type="button"
            >
              前往发布页下载
            </button>
          )}
          {updateErr && (
            <div
              style={{
                marginTop: 10,
                fontSize: 12,
                lineHeight: 1.6,
                color: "var(--accent-danger, #e5484d)",
                wordBreak: "break-word",
              }}
            >
              ⚠ {updateErr}
            </div>
          )}
        </div>
        {updateInfo?.has_update && updateInfo.notes && (
          <div
            style={{
              marginTop: 12,
              fontSize: 12,
              lineHeight: 1.6,
              whiteSpace: "pre-wrap",
              color: "var(--text-mute)",
              background: "var(--bg-soft, rgba(127,127,127,0.08))",
              borderRadius: 10,
              padding: "10px 12px",
              maxHeight: 160,
              overflowY: "auto",
            }}
          >
            {updateInfo.notes.slice(0, 1200)}
          </div>
        )}
      </div>

      {/* 保存栏 */}
      {dirty && (
        <div
          style={{
            position: "sticky",
            bottom: 0,
            display: "flex",
            gap: 8,
            padding: "12px 0",
            background: "linear-gradient(to top, var(--bg) 55%, transparent)",
          }}
        >
          <div className="spacer" style={{ flex: 1 }} />
          <button className="btn btn-ghost" onClick={() => setDraft(settings)}>
            放弃
          </button>
          <button className="btn btn-primary" onClick={save}>
            {saved ? (
              <>
                <IconCheck size={14} /> 已保存
              </>
            ) : (
              "保存设置"
            )}
          </button>
        </div>
      )}
    </div>
  );
}

function IconDownloadLayers() {
  return <IconLayers size={15} />;
}
