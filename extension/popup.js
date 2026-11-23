const $ = (id) => document.getElementById(id);
const DEFAULTS = {
  enabled: true,
  port: 8899,
  token: "",
  allowDomains: "",
  skipExts: "",
  minSize: 0,
};

const fields = {
  enabled: $("enabledSw"),
  port: $("port"),
  token: $("token"),
  allowDomains: $("allowDomains"),
  skipExts: $("skipExts"),
};

function toast(msg) {
  const t = $("toast");
  t.textContent = msg;
  t.classList.add("show");
  setTimeout(() => t.classList.remove("show"), 1800);
}

async function ping(port) {
  try {
    const r = await fetch(`http://127.0.0.1:${port}/ping`, { cache: "no-store" });
    const d = await r.json();
    return d.ok && d.app === "felldl" ? d : null;
  } catch {
    return null;
  }
}

async function refreshStatus(port) {
  const dot = $("statusDot");
  const text = $("statusText");
  const sub = $("statusSub");
  dot.className = "dot";
  text.textContent = "检测中…";
  sub.textContent = "";
  const d = await ping(port);
  if (d) {
    dot.classList.add("on");
    text.textContent = "Felldl 在线";
    sub.textContent = `v${d.version} · 127.0.0.1:${port}`;
  } else {
    dot.classList.add("off");
    text.textContent = "Felldl 未运行";
    sub.textContent = "启动桌面客户端后即可接管下载";
  }
}

async function load() {
  const c = await chrome.storage.local.get(DEFAULTS);
  fields.port.value = c.port;
  fields.token.value = c.token;
  fields.allowDomains.value = c.allowDomains;
  fields.skipExts.value = c.skipExts;
  fields.enabled.classList.toggle("on", !!c.enabled);
  await refreshStatus(c.port);
}

fields.enabled.addEventListener("click", () => {
  fields.enabled.classList.toggle("on");
});
$("saveBtn").addEventListener("click", async () => {
  const cfg = {
    enabled: fields.enabled.classList.contains("on"),
    port: Math.max(1, Math.min(65535, Number(fields.port.value) || 8899)),
    token: fields.token.value.trim(),
    allowDomains: fields.allowDomains.value.trim(),
    skipExts: fields.skipExts.value.trim(),
    minSize: 0,
  };
  await chrome.storage.local.set(cfg);
  toast("已保存");
  await refreshStatus(cfg.port);
});

load();
