import { useEffect, useState } from "react";
import type { Settings, UpdateInfo } from "./types";
import { api } from "./bridge";
import { useI18n, setLang } from "./i18n";
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
  IconX,
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
  const { t, lang } = useI18n();
  const [draft, setDraft] = useState<Settings>(settings);
  const [saved, setSaved] = useState(false);
  const [apiInfo, setApiInfo] = useState<{ port: number; token: string; running: boolean } | null>(
    null
  );
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateErr, setUpdateErr] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [ifaces, setIfaces] = useState<string[]>([]);
  const [dnsTesting, setDnsTesting] = useState(false);
  const [dnsResult, setDnsResult] = useState<string | null>(null);

  async function testDns() {
    const servers = draft.dns_servers.trim();
    if (!servers) return;
    setDnsTesting(true);
    setDnsResult(null);
    try {
      const ips = await api.testDns(servers, "www.example.com");
      setDnsResult(`www.example.com → ${ips.join(", ")}`);
    } catch (e: any) {
      setDnsResult(typeof e === "string" ? e : String(e));
    } finally {
      setDnsTesting(false);
    }
  }

  useEffect(() => {
    api.listInterfaces().then(setIfaces).catch(() => {});
  }, []);

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
        if (alive) setUpdateErr(typeof e === "string" ? e : t("set.checkFail"));
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
      toast(t("set.saved"));
      setTimeout(() => setSaved(false), 2000);
    } catch (e: any) {
      toast(typeof e === "string" ? e : t("set.saveFail"), "error");
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
      toast(t("set.copied"));
    } catch {
      toast(t("set.copyFail"), "error");
    }
  }

  async function checkUpdate() {
    setChecking(true);
    setUpdateErr(null);
    try {
      const info = await api.checkUpdate();
      setUpdateInfo(info);
      if (info?.has_update) {
        toast(t("set.newFound", { v: info.latest_version }));
      } else {
        toast(t("set.latest", { v: info?.current_version ?? "…" }));
      }
    } catch (e: any) {
      setUpdateErr(typeof e === "string" ? e : t("set.checkFail"));
      toast(typeof e === "string" ? e : t("set.checkFail"), "error");
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
            <IconLayers size={15} />
          </div>
          <div className="card-title">{t("set.download")}</div>
        </div>
        <div className="card-sub">{t("set.downloadSub")}</div>

        <div className="field">
          <label className="field-label">
            <IconFolder size={14} />
            {t("set.downloadDir")}
          </label>
          <div className="input-row">
            <input
              className="input"
              value={draft.download_dir}
              onChange={(e) => patch({ download_dir: e.target.value })}
              spellCheck={false}
            />
            <button className="btn btn-soft" onClick={pickDir} type="button">
              {t("dlg.browse")}
            </button>
          </div>
        </div>

        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12, marginTop: 14 }}>
          <div className="field">
            <label className="field-label">
              <IconLayers size={14} />
              {t("set.defaultConns")}
            </label>
            <select
              className="select"
              value={draft.default_connections}
              onChange={(e) => patch({ default_connections: Number(e.target.value) })}
            >
              {[1, 2, 4, 8, 16, 32, 64].map((n) => (
                <option key={n} value={n}>
                  {t("set.threadsN", { n })}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label className="field-label">
              <IconGauge size={14} />
              {t("set.maxTasks")}
            </label>
            <select
              className="select"
              value={draft.max_concurrent_tasks}
              onChange={(e) => patch({ max_concurrent_tasks: Number(e.target.value) })}
            >
              {[1, 2, 3, 4, 5, 6, 8].map((n) => (
                <option key={n} value={n}>
                  {t("set.maxTasksN", { n })}
                </option>
              ))}
            </select>
          </div>
        </div>

        <div className="switch-row" style={{ marginTop: 6 }}>
          <div className="switch-info">
            <div className="switch-title">{t("set.autoStart")}</div>
            <div className="switch-desc">{t("set.autoStartDesc")}</div>
          </div>
          <Switch on={draft.auto_start} onChange={(v) => patch({ auto_start: v })} />
        </div>
        <div className="switch-row">
          <div className="switch-info">
            <div className="switch-title">{t("set.notify")}</div>
            <div className="switch-desc">{t("set.notifyDesc")}</div>
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
          <div className="card-title">{t("set.globalLimit")}</div>
        </div>
        <div className="card-sub">
          {t("set.globalLimitSub")}{" "}
          <strong style={{ color: "var(--brand-1)" }}>
            {draft.global_speed_limit === 0 ? t("set.unlimited") : `${limitMB} MB/s`}
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
          <span>{t("set.noLimit")}</span>
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
          <div className="card-title">{t("set.proxy")}</div>
        </div>
        <div className="card-sub">{t("set.proxySub")}</div>

        <div className="switch-row" style={{ paddingTop: 0 }}>
          <div className="switch-info">
            <div className="switch-title">{t("set.proxyEnable")}</div>
            <div className="switch-desc">{t("set.proxyEnableDesc")}</div>
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
                placeholder="Port"
                value={draft.proxy.port || ""}
                onChange={(e) =>
                  patch({ proxy: { ...draft.proxy, port: Number(e.target.value) || 0 } })
                }
              />
            </div>
            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8, marginTop: 8 }}>
              <input
                className="input"
                placeholder={t("set.proxyUser")}
                value={draft.proxy.username}
                onChange={(e) => patch({ proxy: { ...draft.proxy, username: e.target.value } })}
                spellCheck={false}
              />
              <input
                className="input"
                type="password"
                placeholder={t("set.proxyPass")}
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
          <div className="card-title">{t("set.browser")}</div>
        </div>
        <div className="card-sub">{t("set.browserSub")}</div>

        <div className="switch-row" style={{ paddingTop: 0 }}>
          <div className="switch-info">
            <div className="switch-title">{t("set.apiEnable")}</div>
            <div className="switch-desc">{t("set.apiEnableDesc")}</div>
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
              <span style={{ color: "var(--text-mute)", minWidth: 52 }}>{t("set.addr")}</span>
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
                  {apiInfo.running ? t("set.running") : t("set.stopped")}
                </span>
              </span>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span style={{ color: "var(--text-mute)", minWidth: 52 }}>{t("set.token")}</span>
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
              <button className="icon-btn" onClick={copyToken} title={t("set.copyToken")}>
                <IconCopy size={14} />
              </button>
            </div>
          </div>
        )}

        {draft.api_enabled && (
          <div style={{ marginTop: 14 }}>
            <label className="field-label">{t("set.browserPanel")}</label>
            <div className="seg-control" style={{ marginTop: 6 }}>
              {(
                [
                  { v: "notify", label: t("set.panelNotify") },
                  { v: "minimize", label: t("set.panelMinimize") },
                  { v: "none", label: t("set.panelNone") },
                ] as const
              ).map(({ v, label }) => (
                <button
                  key={v}
                  type="button"
                  className={cx("seg-btn", draft.browser_panel === v && "active")}
                  onClick={() => patch({ browser_panel: v })}
                >
                  {label}
                </button>
              ))}
            </div>
            <div style={{ fontSize: 11.5, color: "var(--text-mute)", marginTop: 8, lineHeight: 1.6 }}>
              {t("set.extHint")}
            </div>
          </div>
        )}
      </div>

      {/* 下载分类 */}
      <div className="card card-wide">
        <div className="card-head">
          <div className="card-icon">
            <IconFolder size={15} />
          </div>
          <div className="card-title">{t("set.categories")}</div>
        </div>
        <div className="card-sub">{t("set.categoriesSub")}</div>
        <div style={{ display: "flex", flexDirection: "column", gap: 8, marginTop: 12 }}>
          {draft.categories.map((c, i) => (
            <div
              key={i}
              style={{
                display: "grid",
                gridTemplateColumns: "90px 1.4fr 1fr 1.2fr auto",
                gap: 6,
                alignItems: "center",
              }}
            >
              <input
                className="input"
                value={c.name}
                placeholder={t("set.catName")}
                onChange={(e) => {
                  const cats = draft.categories.slice();
                  cats[i] = { ...c, name: e.target.value };
                  patch({ categories: cats });
                }}
              />
              <input
                className="input"
                value={c.extensions.join(",")}
                placeholder={t("set.catExt")}
                onChange={(e) => {
                  const cats = draft.categories.slice();
                  cats[i] = {
                    ...c,
                    extensions: e.target.value.split(/[,\s]+/).filter(Boolean),
                  };
                  patch({ categories: cats });
                }}
              />
              <input
                className="input"
                value={c.site_pattern || ""}
                placeholder={t("set.catSite")}
                onChange={(e) => {
                  const cats = draft.categories.slice();
                  cats[i] = { ...c, site_pattern: e.target.value || null };
                  patch({ categories: cats });
                }}
              />
              <input
                className="input"
                value={c.save_dir || ""}
                placeholder={t("set.catDir")}
                onChange={(e) => {
                  const cats = draft.categories.slice();
                  cats[i] = { ...c, save_dir: e.target.value || null };
                  patch({ categories: cats });
                }}
              />
              <button
                className="icon-btn"
                title={t("set.catDelete")}
                onClick={() =>
                  patch({ categories: draft.categories.filter((_, k) => k !== i) })
                }
              >
                <IconX size={14} />
              </button>
            </div>
          ))}
          {draft.categories.length === 0 && (
            <div style={{ fontSize: 12, color: "var(--text-mute)" }}>{t("set.catEmpty")}</div>
          )}
          <div>
            <button
              className="btn btn-soft btn-sm"
              type="button"
              onClick={() =>
                patch({
                  categories: [
                    ...draft.categories,
                    { name: "", extensions: [], site_pattern: null, save_dir: null },
                  ],
                })
              }
            >
              + {t("set.catAdd")}
            </button>
          </div>
        </div>
      </div>

      {/* 下载后动作 */}
      <div className="card">
        <div className="card-head">
          <div className="card-icon">
            <IconMoon size={15} />
          </div>
          <div className="card-title">{t("set.after")}</div>
        </div>
        <div className="card-sub">{t("set.afterSub")}</div>
        <div className="seg-control" style={{ marginTop: 12 }}>
          {(
            [
              { v: "none", label: t("set.afterNone") },
              { v: "shutdown", label: t("set.afterShutdown") },
              { v: "sleep", label: t("set.afterSleep") },
            ] as const
          ).map(({ v, label }) => (
            <button
              key={v}
              type="button"
              className={cx("seg-btn", draft.after_complete === v && "active")}
              onClick={() => patch({ after_complete: v })}
            >
              {label}
            </button>
          ))}
        </div>
        {draft.after_complete === "shutdown" && (
          <div style={{ fontSize: 11.5, color: "var(--text-mute)", marginTop: 8 }}>
            {t("set.afterShutdownHint")}
          </div>
        )}
      </div>

      {/* 网络接口绑定 */}
      <div className="card">
        <div className="card-head">
          <div className="card-icon">
            <IconGlobe size={15} />
          </div>
          <div className="card-title">{t("set.net")}</div>
        </div>
        <div className="card-sub">{t("set.netSub")}</div>
        <div className="field" style={{ marginTop: 12 }}>
          <label className="field-label">{t("set.bindIp")}</label>
          <select
            className="select"
            value={draft.bind_ip}
            onChange={(e) => patch({ bind_ip: e.target.value })}
          >
            <option value="">{t("set.bindAuto")}</option>
            {(ifaces.length ? ifaces : draft.bind_ip ? [draft.bind_ip] : []).map((ip) => (
              <option key={ip} value={ip}>
                {ip}
              </option>
            ))}
          </select>
          <div style={{ fontSize: 11.5, color: "var(--text-mute)", marginTop: 6 }}>
            {t("set.bindIpHint")}
          </div>
        </div>

        {/* DNS */}
        <div className="field" style={{ marginTop: 16 }}>
          <label className="field-label">{t("set.dns")}</label>
          <select
            className="select"
            value={draft.dns_mode || "system"}
            onChange={(e) => patch({ dns_mode: e.target.value })}
          >
            <option value="system">{t("set.dnsSystem")}</option>
            <option value="custom">{t("set.dnsCustom")}</option>
          </select>
          {draft.dns_mode === "custom" && (
            <>
              <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
                <input
                  className="input"
                  style={{ flex: 1 }}
                  placeholder="1.1.1.1, 8.8.8.8"
                  value={draft.dns_servers}
                  onChange={(e) => patch({ dns_servers: e.target.value })}
                  spellCheck={false}
                />
                <button
                  type="button"
                  className="btn"
                  disabled={dnsTesting || !draft.dns_servers.trim()}
                  onClick={testDns}
                >
                  {dnsTesting ? t("set.dnsTesting") : t("set.dnsTest")}
                </button>
              </div>
              <div style={{ fontSize: 11.5, color: "var(--text-mute)", marginTop: 6 }}>
                {dnsResult || t("set.dnsHint")}
              </div>
            </>
          )}
        </div>
      </div>

      {/* 外观 */}
      <div className="card">
        <div className="card-head">
          <div className="card-icon">
            <IconSliders />
          </div>
          <div className="card-title">{t("set.appearance")}</div>
        </div>
        <div className="card-sub">{t("set.appearanceSub")}</div>
        <div className="seg-control">
          {(
            [
              { v: "light", label: t("set.themeLight"), Icon: IconSun },
              { v: "dark", label: t("set.themeDark"), Icon: IconMoon },
              { v: "system", label: t("set.themeSystem"), Icon: IconMonitor },
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

        {/* 界面自定义 */}
        <div className="card-sub" style={{ marginTop: 16 }}>{t("set.customizeSub")}</div>
        <div className="field" style={{ marginTop: 8 }}>
          <label className="field-label">{t("set.skin")}</label>
          <div className="seg-control">
            {(
              [
                { v: "blue", label: t("set.skinBlue"), c: "#3b82f6" },
                { v: "purple", label: t("set.skinPurple"), c: "#8b5cf6" },
                { v: "green", label: t("set.skinGreen"), c: "#10b981" },
                { v: "orange", label: t("set.skinOrange"), c: "#f59e0b" },
              ] as const
            ).map(({ v, label, c }) => (
              <button
                key={v}
                type="button"
                className={cx("seg-btn", draft.ui_skin === v && "active")}
                onClick={() => patch({ ui_skin: v })}
                style={{ display: "inline-flex", alignItems: "center", gap: 6 }}
              >
                <span
                  style={{
                    width: 10,
                    height: 10,
                    borderRadius: "50%",
                    background: c,
                    display: "inline-block",
                  }}
                />
                {label}
              </button>
            ))}
          </div>
        </div>
        <div className="field" style={{ marginTop: 10 }}>
          <label className="field-label">{t("set.btnStyle")}</label>
          <div className="seg-control">
            {(
              [
                { v: "rounded", label: t("set.btnRounded") },
                { v: "flat", label: t("set.btnFlat") },
                { v: "outline", label: t("set.btnOutline") },
              ] as const
            ).map(({ v, label }) => (
              <button
                key={v}
                type="button"
                className={cx("seg-btn", draft.btn_style === v && "active")}
                onClick={() => patch({ btn_style: v })}
              >
                {label}
              </button>
            ))}
          </div>
        </div>
        <div className="field" style={{ marginTop: 10 }}>
          <label className="field-label">{t("set.taskMeta")}</label>
          <div style={{ display: "flex", flexWrap: "wrap", gap: 8, marginTop: 4 }}>
            {(
              [
                { v: "size", label: t("meta.size") },
                { v: "speed", label: t("meta.speed") },
                { v: "eta", label: t("meta.eta") },
                { v: "mirrors", label: t("meta.mirrors") },
                { v: "date", label: t("meta.date") },
              ] as const
            ).map(({ v, label }) => {
              const on = draft.task_meta.includes(v);
              return (
                <button
                  key={v}
                  type="button"
                  className={cx("seg-btn", on && "active")}
                  onClick={() =>
                    patch({
                      task_meta: on
                        ? draft.task_meta.filter((x) => x !== v)
                        : [...draft.task_meta, v],
                    })
                  }
                >
                  {label}
                </button>
              );
            })}
          </div>
        </div>
      </div>

      {/* 语言 */}
      <div className="card">
        <div className="card-head">
          <div className="card-icon">
            <IconGlobe />
          </div>
          <div className="card-title">{t("set.language")}</div>
        </div>
        <div className="seg-control" style={{ marginTop: 12 }}>
          {(
            [
              { v: "zh", label: "简体中文" },
              { v: "en", label: "English" },
            ] as const
          ).map(({ v, label }) => (
            <button
              key={v}
              type="button"
              className={cx("seg-btn", lang === v && "active")}
              onClick={() => setLang(v)}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      {/* 关于与更新 */}
      <div className="card card-wide">
        <div className="card-head">
          <div className="card-icon">
            <IconGlobe size={15} />
          </div>
          <div className="card-title">{t("set.about")}</div>
        </div>
        <div className="card-sub">{t("set.aboutSub", { v: updateInfo?.current_version ?? "…" })}</div>

        <div style={{ display: "flex", gap: 8, marginTop: 12, alignItems: "center", flexWrap: "wrap" }}>
          <button className="btn btn-soft" onClick={checkUpdate} disabled={checking} type="button">
            {checking ? t("set.checking") : t("set.checkUpdate")}
          </button>
          {updateInfo && (
            <span style={{ fontSize: 12, color: "var(--text-mute)" }}>
              {updateInfo.has_update
                ? t("set.newFound", { v: updateInfo.latest_version })
                : t("set.latestRemote", { v: updateInfo.latest_version || "-" })}
            </span>
          )}
          {updateInfo?.has_update && (
            <button
              className="btn btn-primary"
              onClick={() =>
                api.openUrl(updateInfo.release_url).catch(() => toast(t("upd.openFail"), "error"))
              }
              type="button"
            >
              {t("set.goDownload")}
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
        <div className="settings-save">
          <div className="spacer" style={{ flex: 1 }} />
          <button className="btn btn-ghost" onClick={() => setDraft(settings)}>
            {t("set.discard")}
          </button>
          <button className="btn btn-primary" onClick={save}>
            {saved ? (
              <>
                <IconCheck size={14} /> {t("set.savedBtn")}
              </>
            ) : (
              t("set.save")
            )}
          </button>
        </div>
      )}
    </div>
  );
}
