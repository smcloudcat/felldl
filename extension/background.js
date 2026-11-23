/**
 * Felldl 下载接管 — MV3 Service Worker
 *
 * 工作流程：
 *   1. 监听 chrome.downloads.onCreated，拿到下载 URL / referrer / 文件名
 *   2. 立即暂停并移除浏览器原生下载（避免重复占带宽）
 *   3. 收集该 URL 的 Cookie，POST 到本地 Felldl API（/add）
 *   4. 失败时回滚：恢复原生下载，保证不丢任务
 *
 * 配置（端口 / Token / 开关）来自 chrome.storage.local，由 popup 写入。
 */

const DEFAULT_PORT = 8899;

/** 读取配置 */
async function getConfig() {
  const c = await chrome.storage.local.get({
    enabled: true,
    port: DEFAULT_PORT,
    token: "",
    minSize: 0,          // 小于该字节数的文件不接管（0 = 全部接管）
    skipExts: "",        // 逗号分隔的扩展名白名单（这些交给浏览器）
    allowDomains: "",    // 逗号分隔；非空时只接管这些域名
  });
  return c;
}

/** 判断该下载是否应该交给 Felldl */
function shouldTakeOver(url, cfg) {
  if (!cfg.enabled) return false;
  if (!/^https?:\/\//i.test(url)) return false;
  // 本地地址不接管（避免把 Felldl 自己的更新等接管）
  if (/^https?:\/\/(127\.0\.0\.1|localhost|\[::1\])/i.test(url)) return false;

  try {
    const u = new URL(url);
    if (cfg.allowDomains.trim()) {
      const domains = cfg.allowDomains.split(",").map((s) => s.trim().toLowerCase()).filter(Boolean);
      if (!domains.some((d) => u.hostname.toLowerCase() === d || u.hostname.toLowerCase().endsWith("." + d))) {
        return false;
      }
    }
    if (cfg.skipExts.trim()) {
      const exts = cfg.skipExts.split(",").map((s) => s.trim().toLowerCase().replace(/^\./, "")).filter(Boolean);
      const path = u.pathname.toLowerCase();
      if (exts.some((e) => path.endsWith("." + e))) return false;
    }
  } catch {
    return false;
  }
  return true;
}

/** 收集目标 URL 上的 Cookie，拼成 Cookie 请求头 */
async function cookiesFor(url) {
  try {
    const list = await chrome.cookies.getAll({ url });
    return list.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch {
    return "";
  }
}

/** 调用本地 Felldl API 添加下载 */
async function sendToFelldl(item, cfg) {
  const cookie = await cookiesFor(item.url);
  const body = {
    url: item.url,
    referer: item.referrer || undefined,
    cookie: cookie || undefined,
    file_name: item.filename || undefined,
    headers: { "User-Agent": "Mozilla/5.0" },
  };
  const resp = await fetch(`http://127.0.0.1:${cfg.port}/add?token=${encodeURIComponent(cfg.token)}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!resp.ok) {
    const text = await resp.text().catch(() => "");
    throw new Error(`API ${resp.status}: ${text.slice(0, 120)}`);
  }
  const data = await resp.json();
  if (!data.ok) throw new Error(data.error || "未知错误");
  return data.id;
}

/** 探测Felldl是否在线 */
async function ping(cfg) {
  try {
    const r = await fetch(`http://127.0.0.1:${cfg.port}/ping`, { cache: "no-store" });
    const d = await r.json();
    return !!(d.ok && d.app === "felldl");
  } catch {
    return false;
  }
}

/** 主拦截逻辑 */
chrome.downloads.onCreated.addListener(async (item) => {
  // 忽略扩展自己触发的下载
  if (item.byExtensionId === chrome.runtime.id) return;
  if (item.state === "interrupted") return;

  const cfg = await getConfig();
  if (!shouldTakeOver(item.url, cfg)) return;

  // 先确认 Felldl 在线，离线则完全放行
  const online = await ping(cfg);
  if (!online) {
    await notify("Felldl 未运行", "已使用浏览器原生下载。启动 Felldl 后可自动接管。");
    return;
  }

  try {
    // 交给引擎
    await sendToFelldl(item, cfg);
    // 成功后取消原生下载
    await chrome.downloads.cancel(item.id).catch(() => {});
    await chrome.downloads.removeFile(item.id).catch(() => {});
    await chrome.downloads.erase({ id: item.id }).catch(() => {});
    await notify("已交给 Felldl", item.filename || shortUrl(item.url));
  } catch (e) {
    // 失败回滚：保留原生下载，不打断用户
    await chrome.downloads.resume(item.id).catch(() => {});
    await notify("Felldl 接管失败", String(e.message || e));
  }
});

function shortUrl(url) {
  try {
    const u = new URL(url);
    return u.pathname.split("/").pop() || u.hostname;
  } catch {
    return url.slice(0, 60);
  }
}

async function notify(title, message) {
  try {
    await chrome.notifications.create({
      type: "basic",
      iconUrl: "icons/icon128.png",
      title,
      message: message.slice(0, 200),
    });
  } catch {
    /* 通知权限未授予时静默 */
  }
}

// 安装时打开配置页
chrome.runtime.onInstalled.addListener(() => {
  chrome.action.openPopup?.().catch(() => {});
});
