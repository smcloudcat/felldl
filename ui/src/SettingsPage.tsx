import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { Settings } from "./types";
import { api, type AllFilesAccess } from "./bridge";
import { toIpcError, type IpcError } from "./ipcError";
import { useUpdateJob, wireUpdateJob } from "./useUpdateJob";
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
import {
  applyAppearance,
  forgetBackground,
  primeBackground,
} from "./appearance";
import { settingsDraft } from "./settingsDraft";
import { useDialogA11y } from "./useDialogA11y";
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

/** 安卓常用下载目录（系统不允许任意选目录，给几个现成的） */
const ANDROID_DIRS = [
  { key: "set.dirDownload", path: "/storage/emulated/0/Download" },
  { key: "set.dirMovies", path: "/storage/emulated/0/Movies" },
  { key: "set.dirDcim", path: "/storage/emulated/0/DCIM" },
  { key: "set.dirPictures", path: "/storage/emulated/0/Pictures" },
  { key: "set.dirDocs", path: "/storage/emulated/0/Documents" },
];

export function SettingsPage({ settings, onChange, toast }: Props) {
  const { t, lang } = useI18n();
  // 草稿放在模块级 store：切到下载页再回来，改了半天的表单还在
  const draftState = useSyncExternalStore(
    settingsDraft.subscribe,
    settingsDraft.getState,
    settingsDraft.getState,
  );
  const draft = draftState.draft ?? settings;
  const [saved, setSaved] = useState(false);
  const [apiInfo, setApiInfo] = useState<{
    port: number;
    token: string;
    running: boolean;
  } | null>(null);
  // 结构化错误：code 决定动作（是否去授权页）、message 只负责展示、retryable 决定要不要给重试按钮
  // 下载/安装失败统一由全局 job 持有；这里只额外保留「安装暂存包」的错误
  const [stagedErr, setStagedErr] = useState<IpcError | null>(null);
  const [ifaces, setIfaces] = useState<string[]>([]);
  const [dnsTesting, setDnsTesting] = useState(false);
  const [dnsResult, setDnsResult] = useState<string | null>(null);
  const [bgBusy, setBgBusy] = useState(false);
  // 真实应用版本（来自后端），与「检查更新」的缓存结果解耦
  const [appVer, setAppVer] = useState("");
  // 真机验证安装链路用的入口：由后端能力探测决定，debug 安卓构建才出现。
  // release 包里 `debug_capabilities` 恒为 false，命令本身也只返回 install.unsupported。
  const [debugStaged, setDebugStaged] = useState(false);
  // 安卓存储：「所有文件访问」三态（granted/denied/na）与当前目录是否可写。
  // 不存布尔是因为 API 30 以下根本没有这个开关，得把「不适用」和「被拒」分开。
  const [storage, setStorage] = useState<AllFilesAccess | null>(null);
  const [dirOk, setDirOk] = useState<boolean | null>(null);
  const isAndroid = document.documentElement.dataset.platform === "android";

  // 更新状态放在模块级全局 job 里：离开设置页下载不中断，回来进度还在，
  // 也不会因为组件重挂而重复下载同一个包。
  const job = useUpdateJob();
  const updateInfo = job.info;
  const updating = job.phase !== "idle";
  const phase = job.phase;
  const percent = job.percent;
  // 有些 CDN 不给 Content-Length：此时不显示百分比，改走「不确定进度」样式
  const hasTotal = job.total > 0;
  const sizeHint =
    job.total > 0
      ? `${(job.downloaded / 1024 / 1024).toFixed(1)} / ${(job.total / 1024 / 1024).toFixed(1)} MB`
      : "";
  const updateErr = job.error ?? stagedErr ?? job.checkError;
  // 协议弹窗：null / privacy / terms
  const [legal, setLegal] = useState<"privacy" | "terms" | null>(null);
  // 协议弹窗同样支持 Esc / 焦点陷阱（关闭后焦点回到「隐私政策」按钮）
  const legalRef = useDialogA11y(() => setLegal(null), legal !== null);

  useEffect(() => {
    wireUpdateJob();
  }, []);

  // 调试能力探测：只有 debug 安卓构建会返回 staged_install=true，
  // 决定「安装暂存的 APK」按钮是否出现（release 用户看不到这个入口）。
  useEffect(() => {
    let dead = false;
    api
      .debugCapabilities()
      .then((c) => {
        if (!dead) setDebugStaged(!!c?.staged_install);
      })
      .catch(() => {
        /* 老构建没有这个命令：当作不支持 */
      });
    return () => {
      dead = true;
    };
  }, []);

  async function instantUpdate() {
    if (!updateInfo) return;
    // 不再在前端按文件名猜「有没有适配包」：平台/ABI 由后端按签名清单里的
    // platform + arch 权威选择，前端猜错会直接拦掉一次本来能成功的更新。
    setStagedErr(null);
    const r = await job.start();
    if (r.kind === "busy") {
      // 另一个入口已经在下载：不重复下载，也不打扰用户
      return;
    }
    if (r.kind === "done") {
      // Windows 走「退出→覆盖→重启」，这一步之后进程就没了；
      // 安卓返回 installing（已拉起系统安装器），其它平台返回 opened / manual
      if (r.mode === "restarting") return;
      toast(
        t(r.mode === "installing" ? "set.installing" : "set.manualInstall"),
      );
      return;
    }
    // 失败：job 已经把结构化错误放进全局状态，这里只负责一次性提示与授权跳转
    const err = r.error;
    if (err.code === "install.need_permission") {
      toast(t("set.needInstallPermission"), "error");
    } else if (!err.retryable) {
      toast(err.message, "error");
    }
  }

  // 真机验证用：把暂存在 data_dir/debug-updates 的 APK 交给后端安装。
  /// 走的是和真实更新完全相同的分支（权限检查 → 包名/签名/版本校验 → content:// 安装器）。
  async function installStagedApk() {
    setStagedErr(null);
    try {
      await api.debugInstallStagedUpdate();
      toast(t("set.installing"));
    } catch (e) {
      const err = toIpcError(e, "install.failed");
      setStagedErr(err);
      if (err.code === "install.need_permission") {
        toast(t("set.needInstallPermission"), "error");
        try {
          await api.openInstallPermissionSettings();
        } catch {
          /* 打开失败就只剩提示，用户可以自己去系统设置里开 */
        }
      } else {
        toast(err.message, "error");
      }
    }
  }

  // ── 设置页锚点导航 ──
  const navGroups = [
    { id: "sg-download", label: t("set.group.download"), Icon: IconDownload },
    { id: "sg-network", label: t("set.group.network"), Icon: IconGlobe },
    {
      id: "sg-integration",
      label: t("set.group.integration"),
      Icon: IconShield,
    },
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
    api
      .listInterfaces()
      .then(setIfaces)
      .catch(() => {});
    api
      .appVersion()
      .then(setAppVer)
      .catch(() => {});
  }, []);

  // 安卓：目录可写性自检（换目录 / 保存前都跑一遍）
  useEffect(() => {
    if (!isAndroid) return;
    let alive = true;
    api
      .dirWritable(draft.download_dir)
      .then((ok) => alive && setDirOk(ok))
      .catch(() => alive && setDirOk(false));
    return () => {
      alive = false;
    };
  }, [draft.download_dir, isAndroid]);

  useEffect(() => {
    if (!isAndroid) return;
    api
      .allFilesAccess()
      .then(setStorage)
      .catch(() => setStorage(null));
  }, [isAndroid]);

  /**
   * 从系统设置页回来后重新读一次真实状态。
   *
   * 原来这里是 `setTimeout(…, 1500)` 猜用户什么时候回来：猜早了读到旧值，
   * 猜晚了用户已经看着「未授权」干等。改成监听页面可见性与窗口焦点——
   * 安卓把设置页盖在上面时 WebView 会 hidden、回来时 visible，事件时机就是
   * 「用户真的回来了」，不需要任何时长假设。
   */
  useEffect(() => {
    if (!isAndroid) return;
    const refresh = () => {
      if (document.visibilityState !== "visible") return;
      api
        .allFilesAccess()
        .then(setStorage)
        .catch(() => {});
      api
        .dirWritable(draft.download_dir)
        .then(setDirOk)
        .catch(() => {});
    };
    document.addEventListener("visibilitychange", refresh);
    window.addEventListener("focus", refresh);
    return () => {
      document.removeEventListener("visibilitychange", refresh);
      window.removeEventListener("focus", refresh);
    };
  }, [isAndroid, draft.download_dir]);

  useEffect(() => {
    // 组件重挂不重复请求：job 已经检查过就直接用（后端本身也有 10 分钟缓存）
    if (job.info || job.checking) return;
    job.check(false).catch(() => {
      /* 启动时的静默检查：失败不打扰，错误留在 job.checkError 供设置页展示 */
    });
  }, [job]);

  // 后端设置变化时对齐草稿：没脏改动就整体跟上，有脏改动只更新基线
  // （不能在这里 setDraft，否则会把用户正在编辑的内容冲掉）
  useEffect(() => {
    settingsDraft.sync(settings);
  }, [settings]);

  useEffect(() => {
    api
      .getApiInfo()
      .then(setApiInfo)
      .catch(() => {});
  }, [settings.api_port, settings.api_enabled]);

  const dirty = draftState.dirty;

  function patch(p: Partial<Settings>) {
    settingsDraft.patch(p);
    setSaved(false);
  }

  async function save() {
    try {
      const next = await api.updateSettings(draftState.draft ?? settings);
      // 以服务端返回值为准：它可能规范化了端口、路径等字段
      settingsDraft.commit(next);
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
      else if (isAndroid) toast(t("set.androidDirHint"));
    } catch {}
  }

  /// 安卓切换到预设目录（系统禁止任意选目录，给几个常用公共目录）
  function usePresetDir(p: string) {
    patch({ download_dir: p });
  }

  async function grantStorage() {
    try {
      const next = await api.requestAllFilesAccess();
      // `na`：系统没有「所有文件访问」这个开关（API 30 以下），别让用户去设置里瞎找
      toast(
        next === "na" ? t("set.storageNaHint") : t("set.storageSettingsHint"),
      );
    } catch (e: any) {
      toast(typeof e === "string" ? e : String(e?.message ?? e), "error");
    }
  }

  async function copyText(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast(t("set.copied"));
    } catch {
      toast(t("set.copyFail"), "error");
    }
  }

  async function copyToken() {
    if (!apiInfo) return;
    await copyText(apiInfo.token);
  }

  // MCP 客户端要的是带 token 的完整 URL：单独复制令牌再手拼容易漏掉 `?token=`
  function mcpUrl(): string {
    if (!apiInfo) return "";
    return `http://127.0.0.1:${apiInfo.port}/mcp?token=${apiInfo.token}`;
  }

  async function checkUpdate() {
    try {
      // force=true：用户主动点击时不用 10 分钟的本地缓存；
      // 底层带 ETag，内容没变时 GitHub 返回 304，不消耗调用配额
      const info = await job.check(true);
      if (info?.note) {
        toast(info.note);
      } else if (info?.has_update) {
        toast(t("set.newFound", { v: info.latest_version }));
      } else {
        toast(t("set.latest", { v: info?.current_version ?? "…" }));
      }
    } catch (e) {
      const err = toIpcError(e, "update.check");
      toast(err.message || t("set.checkFail"), "error");
    }
  }

  const limitMB =
    Math.round((draft.global_speed_limit / 1024 / 1024) * 10) / 10;
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
                  placeholder="/storage/emulated/0/Download"
                />
                {!isAndroid && (
                  <button
                    className="btn btn-soft"
                    onClick={pickDir}
                    type="button"
                  >
                    {t("dlg.browse")}
                  </button>
                )}
                {isAndroid && storage === "denied" && (
                  <button
                    className="btn btn-soft"
                    onClick={grantStorage}
                    type="button"
                  >
                    {t("set.grantStorage")}
                  </button>
                )}
              </div>
              {isAndroid && (
                <>
                  <div className="dir-presets">
                    {ANDROID_DIRS.map((d) => (
                      <button
                        key={d.path}
                        type="button"
                        className={cx(
                          "dir-chip",
                          draft.download_dir === d.path && "on",
                        )}
                        onClick={() => usePresetDir(d.path)}
                        title={d.path}
                      >
                        {t(d.key)}
                      </button>
                    ))}
                  </div>
                  {(storage === "denied" || dirOk === false) && (
                    <div className="field-hint warn">
                      ⚠{" "}
                      {storage === "denied"
                        ? t("set.needStorage")
                        : t("set.dirNotWritable")}
                    </div>
                  )}
                  {storage !== "denied" && dirOk === true && (
                    <div className="field-hint ok">✓ {t("set.storageOk")}</div>
                  )}
                </>
              )}
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
                  onChange={(e) =>
                    patch({ default_connections: Number(e.target.value) })
                  }
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
                  onChange={(e) =>
                    patch({ max_concurrent_tasks: Number(e.target.value) })
                  }
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
              <Switch
                on={draft.auto_start}
                onChange={(v) => patch({ auto_start: v })}
              />
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
                  className={cx(
                    "seg-btn",
                    draft.after_complete === v && "active",
                  )}
                  onClick={() => patch({ after_complete: v })}
                >
                  {label}
                </button>
              ))}
            </div>
            {draft.after_complete === "shutdown" && (
              <div
                style={{
                  fontSize: 11.5,
                  color: "var(--text-mute)",
                  marginTop: 8,
                }}
              >
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
                {draft.global_speed_limit === 0
                  ? t("set.unlimited")
                  : `${limitMB} MB/s`}
              </strong>
            </div>
            <input
              type="range"
              min={0}
              max={50}
              step={0.5}
              value={limitMB}
              onChange={(e) =>
                patch({
                  global_speed_limit: Math.round(
                    Number(e.target.value) * 1024 * 1024,
                  ),
                })
              }
              style={{
                width: "100%",
                accentColor: "var(--brand-1)",
                cursor: "pointer",
              }}
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
                onChange={(v) =>
                  patch({ proxy: { ...draft.proxy, enabled: v } })
                }
              />
            </div>

            {draft.proxy.enabled && (
              <div style={{ marginTop: 10 }}>
                <div className="proxy-row">
                  <select
                    className="select"
                    value={draft.proxy.kind}
                    onChange={(e) =>
                      patch({ proxy: { ...draft.proxy, kind: e.target.value } })
                    }
                  >
                    <option value="http">HTTP</option>
                    <option value="socks5">SOCKS5</option>
                  </select>
                  <input
                    className="input"
                    placeholder="127.0.0.1"
                    value={draft.proxy.host}
                    onChange={(e) =>
                      patch({ proxy: { ...draft.proxy, host: e.target.value } })
                    }
                    spellCheck={false}
                  />
                  <input
                    className="input"
                    type="number"
                    placeholder="Port"
                    value={draft.proxy.port || ""}
                    onChange={(e) =>
                      patch({
                        proxy: {
                          ...draft.proxy,
                          port: Number(e.target.value) || 0,
                        },
                      })
                    }
                  />
                </div>
                <div className="field-grid-2" style={{ marginTop: 8 }}>
                  <input
                    className="input"
                    placeholder={t("set.proxyUser")}
                    value={draft.proxy.username}
                    onChange={(e) =>
                      patch({
                        proxy: { ...draft.proxy, username: e.target.value },
                      })
                    }
                    spellCheck={false}
                  />
                  <input
                    className="input"
                    type="password"
                    placeholder={t("set.proxyPass")}
                    value={draft.proxy.password}
                    onChange={(e) =>
                      patch({
                        proxy: { ...draft.proxy, password: e.target.value },
                      })
                    }
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
                {(ifaces.length
                  ? ifaces
                  : draft.bind_ip
                    ? [draft.bind_ip]
                    : []
                ).map((ip) => (
                  <option key={ip} value={ip}>
                    {ip}
                  </option>
                ))}
              </select>
              <div
                style={{
                  fontSize: 11.5,
                  color: "var(--text-mute)",
                  marginTop: 6,
                }}
              >
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
                  <div
                    style={{
                      fontSize: 11.5,
                      color: "var(--text-mute)",
                      marginTop: 6,
                    }}
                  >
                    {dnsResult || t("set.dnsHint")}
                  </div>
                </>
              )}
            </div>
          </div>

          {/* 镜像加速 */}
          <div className="card">
            <div className="card-head">
              <div className="card-icon">
                <IconZap size={15} />
              </div>
              <div className="card-title">{t("set.mirrorTitle")}</div>
              <button
                type="button"
                className={cx("switch", draft.mirror_enabled && "on")}
                onClick={() => patch({ mirror_enabled: !draft.mirror_enabled })}
                title={t("set.mirrorTitle")}
                aria-label={t("set.mirrorTitle")}
              />
            </div>
            <div className="card-sub">{t("set.mirrorSub")}</div>
            <div className="field" style={{ marginTop: 12 }}>
              <label className="field-label">{t("set.mirrorLine")}</label>
              <select
                className="select"
                value={draft.mirror_mode || "auto"}
                disabled={!draft.mirror_enabled}
                onChange={(e) => patch({ mirror_mode: e.target.value })}
              >
                <option value="auto">{t("set.mirrorAuto")}</option>
                <option value="ghfast">ghfast.top</option>
                <option value="ghproxy">ghproxy.net</option>
                <option value="hf">{t("set.mirrorHf")}</option>
                <option value="custom">{t("set.mirrorCustom")}</option>
              </select>
              {draft.mirror_mode === "custom" && (
                <input
                  className="input"
                  style={{ marginTop: 8 }}
                  placeholder="https://ghfast.top/"
                  value={draft.mirror_custom}
                  disabled={!draft.mirror_enabled}
                  onChange={(e) => patch({ mirror_custom: e.target.value })}
                  spellCheck={false}
                />
              )}
              <div className="field-hint" style={{ marginTop: 8 }}>
                {t("set.mirrorHint")}
              </div>
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
              <Switch
                on={draft.api_enabled}
                onChange={(v) => patch({ api_enabled: v })}
              />
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
                  <span style={{ color: "var(--text-mute)", minWidth: 52 }}>
                    {t("set.addr")}
                  </span>
                  <span style={{ color: "var(--brand-1)", fontWeight: 650 }}>
                    http://127.0.0.1:{apiInfo.port}
                  </span>
                  <span
                    style={{
                      marginLeft: "auto",
                      display: "flex",
                      alignItems: "center",
                      gap: 4,
                    }}
                  >
                    <span
                      style={{
                        width: 6,
                        height: 6,
                        borderRadius: "50%",
                        background: apiInfo.running
                          ? "var(--accent-ok)"
                          : "var(--text-mute)",
                      }}
                    />
                    <span style={{ fontSize: 11, color: "var(--text-mute)" }}>
                      {apiInfo.running ? t("set.running") : t("set.stopped")}
                    </span>
                  </span>
                </div>
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <span style={{ color: "var(--text-mute)", minWidth: 52 }}>
                    {t("set.token")}
                  </span>
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
                  <button
                    className="icon-btn"
                    onClick={copyToken}
                    title={t("set.copyToken")}
                  >
                    <IconCopy size={14} />
                  </button>
                </div>
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <span style={{ color: "var(--text-mute)", minWidth: 52 }}>
                    {t("set.mcp")}
                  </span>
                  <span
                    style={{
                      flex: 1,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                      color: "var(--text-soft)",
                    }}
                  >
                    {mcpUrl()}
                  </span>
                  <button
                    className="icon-btn"
                    onClick={() => copyText(mcpUrl())}
                    title={t("set.copyMcp")}
                  >
                    <IconCopy size={14} />
                  </button>
                </div>
                <div
                  style={{
                    fontSize: 11,
                    color: "var(--text-mute)",
                    lineHeight: 1.6,
                  }}
                >
                  {t("set.mcpHint")}
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
                      className={cx(
                        "seg-btn",
                        draft.browser_panel === v && "active",
                      )}
                      onClick={() => patch({ browser_panel: v })}
                    >
                      {label}
                    </button>
                  ))}
                </div>
                <div
                  style={{
                    fontSize: 11.5,
                    color: "var(--text-mute)",
                    marginTop: 8,
                    lineHeight: 1.6,
                  }}
                >
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
            <div
              style={{
                display: "flex",
                flexDirection: "column",
                gap: 8,
                marginTop: 12,
              }}
            >
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
                        extensions: e.target.value
                          .split(/[,\s]+/)
                          .filter(Boolean),
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
                      patch({
                        categories: draft.categories.filter((_, k) => k !== i),
                      })
                    }
                  >
                    <IconX size={14} />
                  </button>
                </div>
              ))}
              {draft.categories.length === 0 && (
                <div style={{ fontSize: 12, color: "var(--text-mute)" }}>
                  {t("set.catEmpty")}
                </div>
              )}
              <div>
                <button
                  className="btn btn-soft btn-sm"
                  type="button"
                  onClick={() =>
                    patch({
                      categories: [
                        ...draft.categories,
                        {
                          name: "",
                          extensions: [],
                          site_pattern: null,
                          save_dir: null,
                        },
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
                  {
                    v: "system",
                    label: t("set.themeSystem"),
                    Icon: IconMonitor,
                  },
                ] as const
              ).map(({ v, label, Icon }) => (
                <button
                  key={v}
                  type="button"
                  className={cx("seg-btn", draft.theme === v && "active")}
                  onClick={() => patch({ theme: v })}
                  style={{
                    display: "inline-flex",
                    alignItems: "center",
                    gap: 6,
                  }}
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
                <div
                  className={cx("bg-thumb", !draft.ui_background && "is-empty")}
                >
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
            <div className="card-sub" style={{ marginTop: 16 }}>
              {t("set.customizeSub")}
            </div>
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
                    style={{
                      display: "inline-flex",
                      alignItems: "center",
                      gap: 6,
                    }}
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
              <div
                style={{
                  display: "flex",
                  flexWrap: "wrap",
                  gap: 8,
                  marginTop: 4,
                }}
              >
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
            <div className="card-sub">
              {t("set.aboutSub", {
                v: appVer || updateInfo?.current_version || "…",
              })}
            </div>

            <div
              style={{
                display: "flex",
                gap: 8,
                marginTop: 12,
                alignItems: "center",
                flexWrap: "wrap",
              }}
            >
              <button
                className="btn btn-soft"
                onClick={checkUpdate}
                disabled={job.checking || updating}
                type="button"
              >
                {job.checking ? t("set.checking") : t("set.checkUpdate")}
              </button>
              {updateInfo && (
                <span style={{ fontSize: 12, color: "var(--text-mute)" }}>
                  {updateInfo.has_update
                    ? t("set.newFound", { v: updateInfo.latest_version })
                    : t("set.latestRemote", {
                        v: updateInfo.latest_version || "-",
                      })}
                </span>
              )}
              {updateInfo?.note && (
                <span
                  style={{
                    fontSize: 11.5,
                    color: "var(--text-mute)",
                    opacity: 0.85,
                  }}
                >
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
                      api
                        .openUrl(updateInfo.release_url)
                        .catch(() => toast(t("upd.openFail"), "error"))
                    }
                    type="button"
                  >
                    {t("set.goDownload")}
                  </button>
                </>
              )}
              {debugStaged && (
                <button
                  className="btn btn-ghost btn-sm"
                  onClick={installStagedApk}
                  disabled={updating}
                  type="button"
                  title="仅 debug 安卓构建：安装 data_dir/debug-updates 下最新的 APK"
                >
                  安装暂存的 APK（debug）
                </button>
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
                <div
                  className={cx(
                    "update-progress-track",
                    phase === "download" && !hasTotal && "indeterminate",
                  )}
                >
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
                {/* 悬停能看到机器码：反馈问题时报这个，比截图文案可靠 */}
                <span title={`错误码: ${updateErr.code}`}>
                  ⚠ {updateErr.message}
                </span>
                {/* 暂时性失败（网络抖动 / 源站 5xx / 刚授完权）直接给一键重试 */}
                {updateErr.retryable && updateInfo?.has_update && (
                  <button
                    className="btn btn-soft btn-sm"
                    type="button"
                    onClick={instantUpdate}
                    disabled={updating}
                  >
                    {t("set.retryUpdate")}
                  </button>
                )}
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
                onClick={() =>
                  api
                    .openUrl("https://lwcat.cn")
                    .catch(() => toast(t("upd.openFail"), "error"))
                }
              >
                <IconGlobe size={13} />
                {t("set.blog")}
                <span className="about-link-url">lwcat.cn</span>
              </button>
              <button
                type="button"
                className="about-link"
                onClick={() => setLegal("privacy")}
              >
                <IconShield size={13} />
                {t("set.privacy")}
              </button>
              <button
                type="button"
                className="about-link"
                onClick={() => setLegal("terms")}
              >
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
        <div
          className="overlay"
          onMouseDown={(e) => e.target === e.currentTarget && setLegal(null)}
        >
          <div
            className="dialog"
            role="dialog"
            aria-modal="true"
            tabIndex={-1}
            ref={legalRef}
          >
            <div className="dialog-head">
              <div style={{ flex: 1 }}>
                <div className="dialog-title">{doc.title}</div>
                <div className="dialog-desc">
                  {t("set.legalUpdated", { d: doc.updated })}
                </div>
              </div>
              <button
                className="icon-btn"
                onClick={() => setLegal(null)}
                aria-label={t("dlg.close")}
              >
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
              <button
                className="btn btn-primary"
                onClick={() => setLegal(null)}
                type="button"
              >
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
          <button
            className="btn btn-ghost"
            onClick={() => settingsDraft.discard()}
          >
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
