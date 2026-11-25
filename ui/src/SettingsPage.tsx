import { useEffect, useRef, useState } from "react";
import type { Settings, UpdateInfo } from "./types";
import { api, subscribeUpdateProgress } from "./bridge";
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
  IconDownload,
  IconZap,
  IconImage,
  IconCode,
} from "./icons";
import { cx } from "./format";
import { applyAppearance, forgetBackground, primeBackground } from "./appearance";
import { PRIVACY, TERMS } from "./legal";

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

/** 从更新信息里挑出当前平台该装的包；资产列表为空（来自兜底源）时按发布命名规则拼地址 */
export function pickUpdateAsset(info: UpdateInfo): { url: string; name: string } | null {
  const ver = (info.latest_version || "").replace(/^v/, "").trim();
  const android = document.documentElement.dataset.platform === "android";
  const suffix = android ? "android-arm64.apk" : "win-x64.exe";
  const hit = (info.assets || []).find(
    (a) => a.download_url && a.name.toLowerCase().endsWith(suffix)
  );
  if (hit) return { url: hit.download_url, name: hit.name };
  if (!ver) return null;
  const name = `Felldl-${ver}-${suffix}`;
  return {
    url: `https://github.com/smcloudcat/felldl/releases/download/v${ver}/${name}`,
    name,
  };
}

/** 带数值回显的滑块（背景模糊 / 遮罩 / 面板透明度） */
function Slider({
  label,
  value,
  min,
  max,
  suffix,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  suffix?: string;
  onChange: (v: number) => void;
}) {
  return (
    <div className="field">
      <label className="field-label">
        {label}
        <span className="slider-value">
          {value}
          {suffix}
        </span>
      </label>
      <input
        className="range"
        type="range"
        min={min}
        max={max}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
      />
    </div>
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
  const [bgBusy, setBgBusy] = useState(false);

  // ── 应用内直接更新 ──
  const [phase, setPhase] = useState<"idle" | "download" | "apply">("idle");
  const updating = phase !== "idle";
  const [percent, setPercent] = useState(0);
  const [sizeHint, setSizeHint] = useState("");
  // 有些 CDN 不给 Content-Length：此时不显示百分比，改走「不确定进度」样式
  const [hasTotal, setHasTotal] = useState(false);
  // 协议弹窗：null / privacy / terms
  const [legal, setLegal] = useState<"privacy" | "terms" | null>(null);

  useEffect(() => {
    let un: (() => void) | undefined;
    let dead = false;
    subscribeUpdateProgress((p) => {
      setPercent(p.percent || 0);
      setHasTotal(p.total > 0);
      if (p.total > 0) {
        const mb = (n: number) => (n / 1024 / 1024).toFixed(1);
        setSizeHint(`${mb(p.downloaded)} / ${mb(p.total)} MB`);
      }
    })
      .then((f) => (dead ? f() : (un = f)))
      .catch(() => {});
    return () => {
      dead = true;
      un?.();
    };
  }, []);

  async function instantUpdate() {
    if (!updateInfo) return;
    const asset = pickUpdateAsset(updateInfo);
    if (!asset) {
      toast(t("set.updateNoAsset"), "error");
      return;
    }
    setPhase("download");
    setPercent(0);
    setSizeHint("");
    setHasTotal(false);
    try {
      const path = await api.downloadUpdate(asset.url, asset.name);
      setPhase("apply");
      const mode = await api.applyUpdate(path);
      // Windows 走「退出→覆盖→重启」，这一步之后进程就没了；
      // 其它平台返回 opened / manual，提示用户手动确认安装
      if (mode !== "restarting") {
        setPhase("idle");
        toast(t("set.manualInstall", { p: path }));
      }
    } catch (e: any) {
      setPhase("idle");
      setPercent(0);
      toast(typeof e === "string" ? e : String(e?.message ?? e), "error");
    }
  }

  // ── 设置页锚点导航 ──
  const navGroups = [
    { id: "sg-download", label: t("set.group.download"), Icon: IconDownload },
    { id: "sg-network", label: t("set.group.network"), Icon: IconGlobe },
    { id: "sg-integration", label: t("set.group.integration"), Icon: IconShield },
    { id: "sg-category", label: t("set.group.category"), Icon: IconFolder },
    { id: "sg-interface", label: t("set.group.interface"), Icon: IconSliders },
    { id: "sg-about", label: t("set.group.about"), Icon: IconZap },
  ];
  const [activeGroup, setActiveGroup] = useState("sg-download");
  const navRef = useRef<HTMLDivElement | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);

  // 滚动时高亮当前分组（.content 才是真正的滚动容器）
  useEffect(() => {
    const container = navRef.current?.closest(".content") as HTMLElement | null;
    if (!container) return;
    const onScroll = () => {
      const line = container.getBoundingClientRect().top + 72;
      let cur = "sg-download";
      for (const g of navGroups) {
        const el = document.getElementById(g.id);
        if (el && el.getBoundingClientRect().top <= line) cur = g.id;
      }
      setActiveGroup(cur);
    };
    onScroll();
    container.addEventListener("scroll", onScroll, { passive: true });
    return () => container.removeEventListener("scroll", onScroll);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 外观实时预览：拖动滑块立刻见效；离开设置页或关闭时还原为已保存值
  useEffect(() => {
    applyAppearance(draft);
    return () => applyAppearance(settings);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft.ui_background, draft.ui_blur, draft.ui_dim, draft.ui_glass]);

  function jumpTo(id: string) {
    const el = document.getElementById(id);
    if (!el) return;
    el.scrollIntoView({ behavior: "smooth", block: "start" });
    setActiveGroup(id);
  }

  async function pickBackground(file: File | null | undefined) {
    if (!file) return;
    setBgBusy(true);
    try {
      const dataUrl = await new Promise<string>((resolve, reject) => {
        const fr = new FileReader();
        fr.onload = () => resolve(String(fr.result || ""));
        fr.onerror = () => reject(new Error(t("set.bgReadFail")));
        fr.readAsDataURL(file);
      });
      const ext = (file.name.split(".").pop() || "png").toLowerCase();
      const path = await api.saveBackground(dataUrl, ext);
      primeBackground(path, dataUrl);
      patch({ ui_background: path });
      applyAppearance({ ...draft, ui_background: path });
      toast(t("set.bgPicked"));
    } catch (e: any) {
      toast(typeof e === "string" ? e : String(e?.message ?? e), "error");
    } finally {
      setBgBusy(false);
    }
  }

  function removeBackground() {
    // 只改设置，先不删文件：用户可能点「放弃」反悔，真删了背景就回不来了。
    // 文件由下一次保存新图时统一清理（后端 save_background 会清掉旧图）。
    forgetBackground(draft.ui_background);
    patch({ ui_background: "" });
    applyAppearance({ ...draft, ui_background: "" });
  }

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
      // force=true：用户主动点击时不用 10 分钟的本地缓存；
      // 底层带 ETag，内容没变时 GitHub 返回 304，不消耗调用配额
      const info = await api.checkUpdate(true);
      setUpdateInfo(info);
      if (info?.note) {
        toast(info.note);
      } else if (info?.has_update) {
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
  // 协议文本随语言切换；未打开弹窗时也算好，省去 JSX 里判空
  const doc = legal === "terms" ? TERMS[lang] : PRIVACY[lang];

  return (
    <div className="settings">
      {/* 锚点导航：吸顶一排分类 chip */}
      <div className="settings-nav" ref={navRef}>
        {navGroups.map(({ id, label, Icon }) => (
          <button
            key={id}
            type="button"
            className={cx("settings-nav-chip", activeGroup === id && "active")}
            onClick={() => jumpTo(id)}
          >
            <Icon />
            {label}
          </button>
        ))}
      </div>
      <input
        ref={fileRef}
        type="file"
        accept="image/png,image/jpeg,image/webp,image/gif,image/bmp,image/avif"
        style={{ display: "none" }}
        onChange={(e) => {
          const f = e.target.files?.[0];
          e.target.value = "";
          pickBackground(f);
        }}
      />
      <section className="settings-group" id="sg-download">
        <div className="settings-group-head">
          <IconDownload size={13} />
          <span>{t("set.group.download")}</span>
        </div>
        <div className="settings-group-grid">
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

          <div className="field-grid-2" style={{ marginTop: 14 }}>
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

      
        </div>
      </section>
      <section className="settings-group" id="sg-network">
        <div className="settings-group-head">
          <IconGlobe size={13} />
          <span>{t("set.group.network")}</span>
        </div>
        <div className="settings-group-grid">
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
              <div className="proxy-row">
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
              <div className="field-grid-2" style={{ marginTop: 8 }}>
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

      
        </div>
      </section>
      <section className="settings-group" id="sg-integration">
        <div className="settings-group-head">
          <IconShield size={13} />
          <span>{t("set.group.integration")}</span>
        </div>
        <div className="settings-group-grid">
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

      
        </div>
      </section>
      <section className="settings-group" id="sg-category">
        <div className="settings-group-head">
          <IconFolder size={13} />
          <span>{t("set.group.category")}</span>
        </div>
        <div className="settings-group-grid">
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
              <div key={i} className="cat-row">
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

      
        </div>
      </section>
      <section className="settings-group" id="sg-interface">
        <div className="settings-group-head">
          <IconSliders size={13} />
          <span>{t("set.group.interface")}</span>
        </div>
        <div className="settings-group-grid">
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

        {/* 背景图与磨砂 */}
          <div className="field" style={{ marginTop: 16 }}>
            <label className="field-label">
              <IconImage size={14} />
              {t("set.bgTitle")}
            </label>
            <div className="bg-row">
              <div className={cx("bg-thumb", !draft.ui_background && "is-empty")}>
                {!draft.ui_background && <IconImage size={17} />}
              </div>
              <div className="bg-actions">
                <div className="bg-actions-btns">
                  <button
                    type="button"
                    className="btn btn-soft btn-sm"
                    disabled={bgBusy}
                    onClick={() => fileRef.current?.click()}
                  >
                    {bgBusy
                      ? t("set.bgLoading")
                      : draft.ui_background
                        ? t("set.bgChange")
                        : t("set.bgPick")}
                  </button>
                  {draft.ui_background && (
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={removeBackground}
                    >
                      {t("set.bgRemove")}
                    </button>
                  )}
                </div>
              </div>
            </div>
            <div className="field-hint">{t("set.bgSub")}</div>
          </div>

          <div className="field-grid-2" style={{ marginTop: 14 }}>
            <Slider
              label={t("set.blur")}
              value={draft.ui_blur}
              min={0}
              max={40}
              suffix="px"
              onChange={(v) => patch({ ui_blur: v })}
            />
            <Slider
              label={t("set.dim")}
              value={draft.ui_dim}
              min={0}
              max={90}
              suffix="%"
              onChange={(v) => patch({ ui_dim: v })}
            />
          </div>
          <div style={{ marginTop: 12 }}>
            <Slider
              label={t("set.glass")}
              value={draft.ui_glass}
              min={40}
              max={100}
              suffix="%"
              onChange={(v) => patch({ ui_glass: v })}
            />
            <div className="field-hint" style={{ marginTop: 5 }}>
              {t("set.glassHint")}
            </div>
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

      
        </div>
      </section>
      <section className="settings-group" id="sg-about">
        <div className="settings-group-head">
          <IconZap size={13} />
          <span>{t("set.group.about")}</span>
        </div>
        <div className="settings-group-grid">
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
            <button className="btn btn-soft" onClick={checkUpdate} disabled={checking || updating} type="button">
              {checking ? t("set.checking") : t("set.checkUpdate")}
            </button>
            {updateInfo && (
              <span style={{ fontSize: 12, color: "var(--text-mute)" }}>
                {updateInfo.has_update
                  ? t("set.newFound", { v: updateInfo.latest_version })
                  : t("set.latestRemote", { v: updateInfo.latest_version || "-" })}
              </span>
            )}
            {updateInfo?.note && (
              <span style={{ fontSize: 11.5, color: "var(--text-mute)", opacity: 0.85 }}>
                · {updateInfo.note}
              </span>
            )}
            {updateInfo?.has_update && (
              <>
                {/* 一键更新：下载更新包 → 替换当前程序并重启 */}
                <button
                  className="btn btn-primary"
                  onClick={instantUpdate}
                  disabled={updating}
                  type="button"
                  title={t("set.instantHint")}
                >
                  <IconZap size={13} />
                  {updating
                    ? phase === "apply"
                      ? t("set.restarting")
                      : t("set.updating")
                    : t("set.instantUpdate")}
                </button>
                <button
                  className="btn btn-ghost btn-sm"
                  onClick={() =>
                    api.openUrl(updateInfo.release_url).catch(() => toast(t("upd.openFail"), "error"))
                  }
                  type="button"
                >
                  {t("set.goDownload")}
                </button>
              </>
            )}
          </div>

          {updateInfo?.has_update && !updating && (
            <div className="field-hint" style={{ marginTop: 8 }}>
              {t("set.updateHint")}
            </div>
          )}

          {/* 下载进度 */}
          {updating && (
            <div className="update-progress">
              <div className={cx("update-progress-track", phase === "download" && !hasTotal && "indeterminate")}>
                <div
                  className="update-progress-bar"
                  style={
                    phase === "apply" || hasTotal
                      ? { width: `${phase === "apply" ? 100 : percent}%` }
                      : undefined
                  }
                />
              </div>
              <div className="update-progress-text">
                {phase === "apply"
                  ? t("set.restarting")
                  : [
                      hasTotal ? `${percent.toFixed(1)}%` : t("set.updating"),
                      sizeHint,
                    ]
                      .filter(Boolean)
                      .join(" · ")}
              </div>
            </div>
          )}

          {updateErr && (
            <div
              style={{
                marginTop: 10,
                fontSize: 12,
                lineHeight: 1.6,
                color: "var(--accent-danger, #e5484d)",
                wordBreak: "break-word",
                display: "flex",
                alignItems: "center",
                gap: 10,
                flexWrap: "wrap",
              }}
            >
              <span>⚠ {updateErr}</span>
              {/* 查不到也能自己去看：直接打开 Release 页面 */}
              <button
                className="btn btn-ghost btn-sm"
                type="button"
                onClick={() =>
                  api
                    .openUrl("https://github.com/smcloudcat/felldl/releases")
                    .catch(() => toast(t("upd.openFail"), "error"))
                }
              >
                {t("set.openReleases")}
              </button>
            </div>
          )}
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

          {/* 博客 / 协议等入口 */}
          <div className="about-links">
            <button
              type="button"
              className="about-link"
              onClick={() => api.openUrl("https://lwcat.cn").catch(() => toast(t("upd.openFail"), "error"))}
            >
              <IconGlobe size={13} />
              {t("set.blog")}
              <span className="about-link-url">lwcat.cn</span>
            </button>
            <button type="button" className="about-link" onClick={() => setLegal("privacy")}>
              <IconShield size={13} />
              {t("set.privacy")}
            </button>
            <button type="button" className="about-link" onClick={() => setLegal("terms")}>
              <IconLayers size={13} />
              {t("set.terms")}
            </button>
            <button
              type="button"
              className="about-link"
              onClick={() =>
                api
                  .openUrl("https://github.com/smcloudcat/felldl")
                  .catch(() => toast(t("upd.openFail"), "error"))
              }
            >
              <IconCode size={13} />
              {t("set.repo")}
            </button>
          </div>
        </div>
        </div>
      </section>

      {legal && (
        <div className="overlay" onMouseDown={(e) => e.target === e.currentTarget && setLegal(null)}>
          <div className="dialog" role="dialog" aria-modal="true">
            <div className="dialog-head">
              <div style={{ flex: 1 }}>
                <div className="dialog-title">{doc.title}</div>
                <div className="dialog-desc">{t("set.legalUpdated", { d: doc.updated })}</div>
              </div>
              <button className="icon-btn" onClick={() => setLegal(null)} aria-label={t("dlg.close")}>
                <IconX />
              </button>
            </div>
            <div className="dialog-body legal-body">
              {doc.sections.map((s, i) => (
                <div key={i}>
                  <div className="legal-heading">{s.heading}</div>
                  <div className="legal-text">{s.body}</div>
                </div>
              ))}
            </div>
            <div className="dialog-foot">
              <div className="spacer" />
              <button className="btn btn-primary" onClick={() => setLegal(null)} type="button">
                {t("set.legalClose")}
              </button>
            </div>
          </div>
        </div>
      )}
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
