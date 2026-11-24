/**
 * 网页抓取页：资源嗅探 / 整站抓取 / HLS 下载
 */
import { useState } from "react";
import type { GrabResource, Settings } from "./types";
import { api } from "./bridge";
import { cx } from "./format";
import { useI18n } from "./i18n";
import {
  IconBolt,
  IconCheck,
  IconFolder,
  IconGlobe,
  IconLink,
} from "./icons";

interface Props {
  settings: Settings;
  toast: (msg: string, kind?: "ok" | "error") => void;
}

type Tab = "resources" | "media" | "links";

const KIND_LABEL: Record<string, string> = {
  video: "MP4/WebM…",
  audio: "MP3/FLAC…",
  image: "JPG/PNG…",
  archive: "ZIP/7Z…",
  document: "PDF/DOC…",
  subtitle: "SRT/ASS…",
  torrent: ".torrent",
  hls: "m3u8",
  other: "FILE",
};

export function GrabPage({ settings, toast }: Props) {
  const { t } = useI18n();
  const [url, setUrl] = useState("");
  const [referer, setReferer] = useState("");
  const [analyzing, setAnalyzing] = useState(false);
  const [result, setResult] = useState<{
    url: string;
    title: string;
    resources: GrabResource[];
    links: string[];
  } | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("resources");
  const [selected, setSelected] = useState<Set<string>>(new Set());

  // 整站抓取参数
  const [depth, setDepth] = useState(1);
  const [maxPages, setMaxPages] = useState(30);
  const [includeAssets, setIncludeAssets] = useState(true);
  const [grabbing, setGrabbing] = useState(false);

  const media = (result?.resources ?? []).filter(
    (r) => r.kind === "video" || r.kind === "audio" || r.kind === "hls"
  );

  async function analyze() {
    const u = url.trim();
    if (!u.startsWith("http://") && !u.startsWith("https://")) {
      setErr(t("dlg.errEmpty"));
      return;
    }
    setAnalyzing(true);
    setErr(null);
    setResult(null);
    setSelected(new Set());
    try {
      const a = await api.analyzeUrl(u, referer.trim() || undefined);
      if (!a) {
        setErr(t("grab.analyzeFail"));
        return;
      }
      setResult({ url: a.url, title: a.title, resources: a.resources ?? [], links: a.links ?? [] });
      setTab("resources");
    } catch (e: any) {
      setErr(typeof e === "string" ? e : e?.message ?? t("grab.analyzeFail"));
    } finally {
      setAnalyzing(false);
    }
  }

  function toggle(urlKey: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(urlKey)) next.delete(urlKey);
      else next.add(urlKey);
      return next;
    });
  }

  async function downloadSelected() {
    const items = (result?.resources ?? []).filter((r) => selected.has(r.url));
    if (items.length === 0) return;
    try {
      const reqs = items.map((r) => ({
        url: r.url,
        file_name: r.name,
        save_dir: settings.download_dir || null,
        connections: 4,
        referer: url.trim() || null,
      }));
      if (reqs.length === 1) await api.addTask(reqs[0]);
      else await api.addTasks(reqs);
      toast(t("grab.added", { n: reqs.length }));
      setSelected(new Set());
    } catch (e: any) {
      toast(typeof e === "string" ? e : t("dlg.addFail"), "error");
    }
  }

  async function downloadHls(r: GrabResource) {
    try {
      await api.downloadHls(r.url, settings.download_dir, r.name.replace(/\.m3u8$/, ".ts"));
      toast(t("grab.hlsQueued"));
    } catch (e: any) {
      toast(typeof e === "string" ? e : t("grab.grabFail"), "error");
    }
  }

  async function grabSite() {
    const u = url.trim();
    if (!u.startsWith("http://") && !u.startsWith("https://")) {
      setErr(t("dlg.errEmpty"));
      return;
    }
    setGrabbing(true);
    try {
      await api.grabSite({
        url: u,
        save_dir: settings.download_dir,
        max_depth: depth,
        max_pages: maxPages,
        include_assets: includeAssets,
      });
      toast(t("grab.grabbing"));
    } catch (e: any) {
      toast(typeof e === "string" ? e : t("grab.grabFail"), "error");
    } finally {
      setGrabbing(false);
    }
  }

  const tabList: { key: Tab; label: string; count: number }[] = [
    { key: "resources", label: t("grab.tab.resources"), count: result?.resources.length ?? 0 },
    { key: "media", label: t("grab.tab.media"), count: media.length },
    { key: "links", label: t("grab.tab.links"), count: result?.links.length ?? 0 },
  ];

  return (
    <div className="grab-page">
      {/* 输入区 */}
      <div className="card">
        <div className="card-head">
          <div className="card-icon">
            <IconGlobe />
          </div>
          <div>
            <div className="card-title">{t("grab.title")}</div>
          </div>
        </div>
        <div className="card-sub">{t("grab.desc")}</div>

        <div className="field" style={{ marginTop: 10 }}>
          <label className="field-label">
            <IconLink size={14} />
            {t("grab.url")}
          </label>
          <div className="input-row">
            <input
              className="input"
              placeholder={t("grab.urlPlaceholder")}
              value={url}
              onChange={(e) => {
                setUrl(e.target.value);
                setErr(null);
              }}
              onKeyDown={(e) => e.key === "Enter" && !analyzing && analyze()}
              spellCheck={false}
            />
            <button
              className="btn btn-primary"
              onClick={analyze}
              disabled={analyzing || !url.trim()}
              type="button"
            >
              <IconBolt size={13} />
              {analyzing ? t("grab.analyzing") : t("grab.analyze")}
            </button>
          </div>
        </div>

        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12, marginTop: 12 }}>
          <div className="field" style={{ margin: 0 }}>
            <label className="field-label">{t("grab.depth")}</label>
            <select className="select" value={depth} onChange={(e) => setDepth(Number(e.target.value))}>
              {[1, 2, 3, 4, 5].map((n) => (
                <option key={n} value={n}>
                  {t("grab.depthN", { n })}
                </option>
              ))}
            </select>
          </div>
          <div className="field" style={{ margin: 0 }}>
            <label className="field-label">{t("grab.maxPages")}</label>
            <select
              className="select"
              value={maxPages}
              onChange={(e) => setMaxPages(Number(e.target.value))}
            >
              {[10, 30, 50, 100, 200].map((n) => (
                <option key={n} value={n}>
                  {t("grab.maxPagesN", { n })}
                </option>
              ))}
            </select>
          </div>
        </div>

        <label
          style={{
            display: "flex",
            alignItems: "center",
            gap: 8,
            marginTop: 12,
            fontSize: 12.6,
            color: "var(--text-soft)",
            cursor: "pointer",
          }}
        >
          <input
            type="checkbox"
            checked={includeAssets}
            onChange={(e) => setIncludeAssets(e.target.checked)}
            style={{ accentColor: "var(--brand-1)" }}
          />
          {t("grab.includeAssets")}
        </label>
        <div className="field-hint">{t("grab.selectHint")}</div>

        <div style={{ display: "flex", gap: 8, marginTop: 14, flexWrap: "wrap" }}>
          <button className="btn btn-primary" onClick={grabSite} disabled={grabbing || !url.trim()}>
            {t("grab.grabSite")}
          </button>
          {result && (
            <button
              className="btn btn-soft"
              onClick={async () => {
                try {
                  await api.grabSite({
                    url: result.url,
                    save_dir: settings.download_dir,
                    max_depth: 1,
                    max_pages: 1,
                    include_assets: includeAssets,
                  });
                  toast(t("grab.grabbing"));
                } catch (e: any) {
                  toast(typeof e === "string" ? e : t("grab.grabFail"), "error");
                }
              }}
            >
              {t("grab.grabSel")}
            </button>
          )}
        </div>

        {err && (
          <div
            style={{
              marginTop: 12,
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

      {/* 分析结果 */}
      {result && (
        <div className="card">
          <div className="card-head">
            <div className="card-icon">
              <IconFolder />
            </div>
            <div style={{ minWidth: 0 }}>
              <div className="card-title" style={{ wordBreak: "break-all" }}>
                {t("grab.analysisOf", { title: result.title || result.url })}
              </div>
              <div className="card-sub">{t("grab.found", { n: result.resources.length, m: result.links.length })}</div>
            </div>
          </div>

          <div className="seg-control" style={{ marginTop: 12 }}>
            {tabList.map(({ key, label, count }) => (
              <button
                key={key}
                type="button"
                className={cx("seg-btn", tab === key && "active")}
                onClick={() => setTab(key)}
              >
                {label} {count > 0 && `(${count})`}
              </button>
            ))}
          </div>

          <div style={{ marginTop: 12 }}>
            {tab === "resources" &&
              (result.resources.length === 0 ? (
                <Empty>{t("grab.none")}</Empty>
              ) : (
                <>
                  <div className="grab-actions">
                    <button className="btn btn-primary btn-sm" onClick={downloadSelected}>
                      <IconCheck size={13} />
                      {t("grab.downloadSel", { n: selected.size })}
                    </button>
                  </div>
                  <ResourceList
                    items={result.resources}
                    selected={selected}
                    onToggle={toggle}
                  />
                </>
              ))}

            {tab === "media" &&
              (media.length === 0 ? (
                <Empty>{t("grab.noMedia")}</Empty>
              ) : (
                <div className="grab-list">
                  {media.map((r) => (
                    <div key={r.url} className="grab-row">
                      <div className="grab-kind">{KIND_LABEL[r.kind] ?? r.kind}</div>
                      <div className="grab-name" title={r.url}>
                        {r.name}
                      </div>
                      <div className="grab-ops">
                        {r.kind === "hls" ? (
                          <button className="btn btn-primary btn-sm" onClick={() => downloadHls(r)}>
                            {t("grab.dlHls")}
                          </button>
                        ) : (
                          <button
                            className="btn btn-soft btn-sm"
                            onClick={() =>
                              api
                                .addTask({
                                  url: r.url,
                                  file_name: r.name,
                                  save_dir: settings.download_dir || null,
                                  connections: 8,
                                  referer: url.trim() || null,
                                })
                                .then(() => toast(t("dlg.started")))
                                .catch((e) => toast(String(e), "error"))
                            }
                          >
                            {t("dlg.submit")}
                          </button>
                        )}
                      </div>
                    </div>
                  ))}
                </div>
              ))}

            {tab === "links" &&
              (result.links.length === 0 ? (
                <Empty>{t("grab.noLinks")}</Empty>
              ) : (
                <div className="grab-list">
                  {result.links.map((l) => (
                    <button
                      key={l}
                      className="grab-link-row"
                      onClick={() => {
                        setUrl(l);
                        analyze();
                      }}
                      title={l}
                    >
                      <IconLink size={13} />
                      <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{l}</span>
                    </button>
                  ))}
                </div>
              ))}
          </div>
        </div>
      )}
    </div>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return <div className="grab-empty">{children}</div>;
}

function ResourceList({
  items,
  selected,
  onToggle,
}: {
  items: GrabResource[];
  selected: Set<string>;
  onToggle: (u: string) => void;
}) {
  return (
    <div className="grab-list">
      {items.map((r) => (
        <label key={r.url} className="grab-row selectable">
          <input
            type="checkbox"
            checked={selected.has(r.url)}
            onChange={() => onToggle(r.url)}
            style={{ accentColor: "var(--brand-1)", flexShrink: 0 }}
          />
          <div className="grab-kind">{KIND_LABEL[r.kind] ?? r.kind}</div>
          <div className="grab-name" title={r.url}>
            {r.name}
          </div>
        </label>
      ))}
    </div>
  );
}
