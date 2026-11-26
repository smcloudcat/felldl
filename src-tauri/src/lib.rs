//! Felldl 桌面/移动端壳层
//!
//! 职责：把 `felldl-core` 引擎包装成 Tauri 命令，把引擎事件转发给前端，
//! 并处理托盘、单实例、通知等平台相关能力。

use felldl_core::api;
use felldl_core::{AddTaskRequest, Engine, EngineEvent, Settings, Task};
use std::sync::Arc;
use tauri::{Emitter, Manager, State};

// 托盘与菜单是桌面专属 API，移动端不存在
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    WindowEvent,
};

#[cfg(target_os = "android")]
mod android;
// 纯路径映射不带 cfg：桌面端也能跑它的单元测试
mod android_paths;
// APK 安装前的元数据校验（纯逻辑，桌面端可测）
mod apk_update;
// 更新落地 helper（同一 exe 的隐藏子命令，main 里在 Tauri 初始化前分流）
pub mod update_helper;

struct AppState {
    engine: Arc<Engine>,
    api_port: parking_lot_stub::PortCell,
}

/// 已通过「清单验签 + 摘要校验」、等待安装的更新包。
///
/// 前端只拿到一次性 token，**不能传路径**：安装入口从这个表里取文件，
/// 因此不存在「前端给个路径就执行」的信任漏洞。
#[derive(Default)]
struct VerifiedUpdates(std::sync::Mutex<std::collections::HashMap<String, felldl_core::update::VerifiedUpdate>>);

impl VerifiedUpdates {
    fn insert(&self, update: felldl_core::update::VerifiedUpdate) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        if let Ok(mut map) = self.0.lock() {
            // 只保留最近一次：同一时刻只允许一个待安装更新，避免并发覆盖
            map.clear();
            map.insert(token.clone(), update);
        }
        token
    }

    fn take(&self, token: &str) -> Option<felldl_core::update::VerifiedUpdate> {
        self.0.lock().ok().and_then(|mut map| map.remove(token))
    }
}

/// 清单里的 platform 值
fn current_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        "linux"
    }
}

/// 清单里的 arch 值：与 `std::env::consts::ARCH` 保持一致（x86_64 / aarch64 …），
/// Android arm64 也是 `aarch64`，用 platform 区分平台即可。
fn current_arch() -> &'static str {
    std::env::consts::ARCH
}

/// 极简可变端口容器（避免为一个小需求引入额外依赖）
mod parking_lot_stub {
    use std::sync::atomic::{AtomicU16, Ordering};
    pub struct PortCell(AtomicU16);
    impl PortCell {
        pub fn new(p: u16) -> Self {
            Self(AtomicU16::new(p))
        }
        pub fn get(&self) -> u16 {
            self.0.load(Ordering::Relaxed)
        }
        #[allow(dead_code)]
        pub fn set(&self, p: u16) {
            self.0.store(p, Ordering::Relaxed);
        }
    }
}

fn data_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("felldl"))
}

/// 清掉上次留下的更新包（含未下完的 `.part`）。
///
/// 为什么可以无脑删：安装入口只认 `VerifiedUpdates` 里的内存 token，
/// 进程一退出 token 就没了，磁盘上的旧包再也不会被用到。
/// 安卓安装过程中本进程会被系统杀掉，能在启动时跑到这里，说明安装已结束或已被取消。
fn cleanup_update_cache(app: &tauri::AppHandle) {
    let freed = remove_files_in(&data_dir(app).join("updates"));
    if freed > 0 {
        eprintln!(
            "[update] 已清理更新缓存 {:.1} MB",
            freed as f64 / 1024.0 / 1024.0
        );
    }
}

/// 删除目录下的普通文件，返回释放的字节数；子目录保留（不是我们创建的，不动）。
fn remove_files_in(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut freed: u64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(&path).is_ok() {
            freed += size;
        }
    }
    freed
}

// ───────────────────────── 命令 ─────────────────────────

#[tauri::command]
async fn list_tasks(state: State<'_, AppState>) -> Result<Vec<Task>, String> {
    Ok(state.engine.snapshot())
}

#[tauri::command]
async fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    Ok(state.engine.settings())
}

#[tauri::command]
async fn add_task(state: State<'_, AppState>, req: AddTaskRequest) -> Result<String, String> {
    state.engine.add(req).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn add_tasks(
    state: State<'_, AppState>,
    reqs: Vec<AddTaskRequest>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::new();
    for r in reqs {
        match state.engine.add(r).await {
            Ok(id) => out.push(serde_json::json!({ "id": id, "ok": true })),
            Err(e) => out.push(serde_json::json!({ "id": "", "ok": false, "error": format!("{e:#}") })),
        }
    }
    Ok(out)
}

#[tauri::command]
async fn pause_task(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.engine.pause(&id).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn resume_task(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.engine.resume(&id).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn remove_task(state: State<'_, AppState>, id: String, delete_file: bool) -> Result<(), String> {
    state
        .engine
        .cancel(&id, delete_file)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn clear_completed(state: State<'_, AppState>) -> Result<(), String> {
    state.engine.clear_completed().await;
    Ok(())
}

#[tauri::command]
async fn pause_all(state: State<'_, AppState>) -> Result<(), String> {
    for t in state.engine.snapshot() {
        if t.state.is_active() {
            let _ = state.engine.pause(&t.id).await;
        }
    }
    Ok(())
}

#[tauri::command]
async fn resume_all(state: State<'_, AppState>) -> Result<(), String> {
    for t in state.engine.snapshot() {
        if matches!(
            t.state,
            felldl_core::TaskState::Paused | felldl_core::TaskState::Error
        ) {
            let _ = state.engine.resume(&t.id).await;
        }
    }
    Ok(())
}

#[tauri::command]
async fn set_task_speed_limit(
    state: State<'_, AppState>,
    id: String,
    limit: u64,
) -> Result<(), String> {
    state
        .engine
        .set_task_speed_limit(&id, limit)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn update_settings(
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    state
        .engine
        .update_settings(settings)
        .map_err(|e| format!("{e:#}"))?;
    Ok(state.engine.settings())
}

/// 用指定 DNS 服务器解析域名（设置页「测试」按钮）
#[tauri::command]
async fn test_dns(servers: String, host: String) -> Result<Vec<String>, String> {
    let parsed = felldl_core::dns::parse_servers(&servers).map_err(|e| format!("{e:#}"))?;
    let host = host.trim();
    if host.is_empty() {
        return Err("请输入要解析的域名".into());
    }
    let ips = felldl_core::dns::resolve_with(&parsed, host)
        .await
        .map_err(|e| format!("{e:#}"))?;
    if ips.is_empty() {
        return Err("DNS 未返回任何地址".into());
    }
    Ok(ips.iter().map(|ip| ip.to_string()).collect())
}

#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
async fn get_api_info(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let s = state.engine.settings();
    Ok(serde_json::json!({
        "port": state.api_port.get(),
        "token": s.api_token,
        "running": s.api_enabled,
    }))
}

#[tauri::command]
async fn open_file(app: tauri::AppHandle, path: String) -> Result<(), String> {
    // 安卓：file:// 会被系统拒绝，必须走 FileProvider 的 content://
    #[cfg(target_os = "android")]
    {
        return android::open_file(&app, &path);
    }
    #[cfg(not(target_os = "android"))]
    {
        open_path(&app, &path)
    }
}

/// 安卓「所有文件访问」状态；其它平台恒为 true
#[tauri::command]
async fn all_files_access(app: tauri::AppHandle) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        return android::has_all_files_access(&app);
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(true)
    }
}

#[tauri::command]
async fn request_all_files_access(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        return android::request_all_files_access(&app);
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(())
    }
}

/// 安卓「安装未知应用」授权状态；其它平台恒为 true
#[tauri::command]
async fn can_install_packages(app: tauri::AppHandle) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        return android::can_install_packages(&app);
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(true)
    }
}

/// 打开「安装未知应用」授权页（仅安卓有意义）
#[tauri::command]
async fn open_install_permission_settings(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let pkg = app.config().identifier.clone();
        return android::open_install_settings(&app, &pkg);
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err("当前平台不需要安装授权".into())
    }
}

/// 目录是否可写（安卓改下载目录前先自检，免得任务全部失败）
#[tauri::command]
fn dir_writable(path: String) -> Result<bool, String> {
    let p = std::path::Path::new(path.trim());
    if p.as_os_str().is_empty() {
        return Ok(false);
    }
    let created = std::fs::create_dir_all(p).is_ok();
    if !created && !p.is_dir() {
        return Ok(false);
    }
    let probe = p.join(format!(".felldl-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

// ───────────────────────── 检查更新 ─────────────────────────

const UPDATE_RELEASE_API: &str =
    "https://api.github.com/repos/smcloudcat/felldl/releases?per_page=10";
/// 备用来源：Release 的 Atom 订阅。走 github.com 普通页面，
/// 不受 API「未登录每小时 60 次」的限制，限流时用它兜底。
const UPDATE_ATOM: &str = "https://github.com/smcloudcat/felldl/releases.atom";
/// 成功结果的缓存有效期：这段时间内重复点击 / 重启都用缓存，不再请求网络
const UPDATE_FRESH_SECS: u64 = 600;
/// 被限流或断网时，允许退回使用的最旧缓存（24 小时）
const UPDATE_STALE_SECS: u64 = 86_400;

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct UpdateAsset {
    pub name: String,
    pub download_url: String,
    pub size: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub release_url: String,
    pub notes: String,
    pub published_at: String,
    pub assets: Vec<UpdateAsset>,
    /// 非空表示这次结果不是「刚查到的」：来自缓存或降级来源
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 检查结果缓存（含 ETag；带 If-None-Match 的 304 不消耗配额）
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct UpdateCache {
    #[serde(default)]
    checked_at: u64,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    info: Option<UpdateInfo>,
    /// 写缓存时的应用版本。升级后版本号变化 → 整份缓存作废，
    /// 否则会拿旧版本算出来的 has_update / current_version 给用户看。
    #[serde(default)]
    app_version: String,
}

/// 用当前真实版本刷新一份可能来自缓存的检查结果
fn refresh_info(mut info: UpdateInfo) -> UpdateInfo {
    let current = env!("CARGO_PKG_VERSION").to_string();
    info.has_update = version_gt(&info.latest_version, &current);
    info.current_version = current;
    info
}

enum ApiOutcome {
    /// 有更新内容（带 ETag）
    Fresh { info: UpdateInfo, etag: Option<String> },
    /// 304：内容没变，沿用缓存
    NotModified,
    /// 有响应但没有可用的正式版本
    Empty,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn update_cache_path(app: &tauri::AppHandle) -> std::path::PathBuf {
    data_dir(app).join("update-cache.json")
}

fn load_cache(path: &std::path::Path) -> UpdateCache {
    let cache: UpdateCache = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    // 版本升级后旧缓存里的 latest/current 判断已经过期（会出现「更新完还显示旧版本」）
    if !cache.app_version.is_empty() && cache.app_version != env!("CARGO_PKG_VERSION") {
        return UpdateCache::default();
    }
    cache
}

fn save_cache(path: &std::path::Path, cache: &UpdateCache) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = std::fs::write(path, text);
    }
}

/// "v1.2.3" / "1.2.3" → [1, 2, 3]
fn parse_ver(v: &str) -> Vec<u32> {
    v.trim()
        .trim_start_matches('v')
        .split('.')
        .map(|s| s.trim().parse().unwrap_or(0))
        .collect()
}

fn version_gt(a: &str, b: &str) -> bool {
    let (a, b) = (parse_ver(a), parse_ver(b));
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 把 GitHub Release JSON 转成前端用的结构
fn build_info(rel: &serde_json::Value) -> UpdateInfo {
    // 去掉 tag 常见的 v 前缀，展示时统一为 vX.Y.Z 由前端拼
    let latest = rel["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim()
        .trim_start_matches('v')
        .to_string();
    let current = env!("CARGO_PKG_VERSION").to_string();
    let assets = rel["assets"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|a| UpdateAsset {
                    name: a["name"].as_str().unwrap_or("").to_string(),
                    download_url: a["browser_download_url"].as_str().unwrap_or("").to_string(),
                    size: a["size"].as_u64().unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default();

    UpdateInfo {
        has_update: version_gt(&latest, &current),
        current_version: current,
        latest_version: latest,
        release_url: rel["html_url"].as_str().unwrap_or("").to_string(),
        notes: rel["body"].as_str().unwrap_or("").to_string(),
        published_at: rel["published_at"].as_str().unwrap_or("").to_string(),
        assets,
        note: None,
    }
}

fn header_u64(resp: &reqwest::Response, name: &str) -> Option<u64> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse().ok())
}

async fn fetch_api(client: &reqwest::Client, etag: Option<&str>) -> Result<ApiOutcome, String> {
    let mut rb = client
        .get(UPDATE_RELEASE_API)
        .timeout(std::time::Duration::from_secs(8))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(e) = etag {
        rb = rb.header("If-None-Match", e);
    }
    let resp = rb.send().await.map_err(|e| format!("网络请求失败: {e}"))?;
    let st = resp.status();

    // 私有仓库的 releases 匿名访问会 404，给出明确提示而不是笼统报错
    if st == reqwest::StatusCode::NOT_FOUND {
        return Err("无法读取发布信息（仓库可能为私有，或还没有发布任何 Release）".into());
    }
    if st == reqwest::StatusCode::FORBIDDEN || st == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let remain = header_u64(&resp, "x-ratelimit-remaining").unwrap_or(0);
        let wait_min = header_u64(&resp, "x-ratelimit-reset")
            .map(|r| (r.saturating_sub(now_secs()) + 59) / 60)
            .unwrap_or(0);
        return Err(if wait_min > 0 {
            format!("GitHub API 请求超限（剩余 {remain} 次，约 {wait_min} 分钟后恢复）")
        } else {
            format!("GitHub API 请求超限（剩余 {remain} 次）")
        });
    }
    if st == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(ApiOutcome::NotModified);
    }
    if !st.is_success() {
        return Err(format!("GitHub 返回错误: HTTP {}", st.as_u16()));
    }

    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析 GitHub 响应失败: {e}"))?;

    // 从列表中挑最新的非草稿、非预发布版本（比 releases/latest 缓存更友好）
    let rel = body.as_array().and_then(|arr| {
        arr.iter().find(|r| {
            !r["draft"].as_bool().unwrap_or(false) && !r["prerelease"].as_bool().unwrap_or(false)
        })
    });
    match rel {
        Some(rel) => Ok(ApiOutcome::Fresh {
            info: build_info(rel),
            etag,
        }),
        None => Ok(ApiOutcome::Empty),
    }
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(&s[start..end])
}

fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Atom 兜底：只取标签名 / 发布时间 / Release 页面地址（没有 asset 列表）
async fn fetch_atom(client: &reqwest::Client) -> Result<UpdateInfo, String> {
    let resp = client
        .get(UPDATE_ATOM)
        .timeout(std::time::Duration::from_secs(6))
        .header("Accept", "application/atom+xml, application/xml;q=0.9, */*;q=0.5")
        .send()
        .await
        .map_err(|e| format!("Releases 订阅请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Releases 订阅返回 HTTP {}", resp.status().as_u16()));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取 Releases 订阅失败: {e}"))?;
    parse_atom(&text)
}

/// 解析 Releases Atom 订阅（单独拆出来便于测试）
fn parse_atom(text: &str) -> Result<UpdateInfo, String> {
    // 第一条 entry 即最新 Release
    let entry = text
        .split("<entry>")
        .nth(1)
        .ok_or_else(|| "Releases 订阅里没有条目".to_string())?;
    let title = between(entry, "<title>", "</title>")
        .map(|t| unescape_xml(t.trim()))
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Releases 订阅缺少版本号".to_string())?;
    let latest = title.trim_start_matches('v').to_string();
    let url = entry
        .split("href=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .filter(|s| s.contains("/releases/tag/"))
        .unwrap_or("https://github.com/smcloudcat/felldl/releases")
        .to_string();
    let published = between(entry, "<updated>", "</updated>")
        .unwrap_or("")
        .to_string();
    let current = env!("CARGO_PKG_VERSION").to_string();

    Ok(UpdateInfo {
        has_update: version_gt(&latest, &current),
        current_version: current,
        latest_version: latest,
        release_url: url,
        notes: String::new(),
        published_at: published,
        assets: Vec::new(),
        note: None,
    })
}

/// 最低一级兜底：仓库根目录的 version.json（随发布更新）
/// 走 raw.githubusercontent / jsDelivr CDN，不经过 GitHub API，不受匿名配额限制
const UPDATE_VERSION_JSON: [&str; 2] = [
    // jsDelivr 国内基本秒回；分支缓存最长约 12 小时，作为最低优先级兜底可以接受
    "https://cdn.jsdelivr.net/gh/smcloudcat/felldl@master/version.json",
    // raw 内容最新，但国内时通常连不上，所以排在后面并给更短超时
    "https://raw.githubusercontent.com/smcloudcat/felldl/master/version.json",
];

/// 签名更新清单（与 version.json 同一仓库、同一 CDN 策略）。
/// 清单由离线私钥签名，应用只内置公钥——发布源被篡改也装不上。
const UPDATE_MANIFEST_JSON: [&str; 2] = [
    "https://cdn.jsdelivr.net/gh/smcloudcat/felldl@master/update-manifest.json",
    "https://raw.githubusercontent.com/smcloudcat/felldl/master/update-manifest.json",
];

async fn fetch_version_json(client: &reqwest::Client) -> Result<UpdateInfo, String> {
    let mut last_err = String::from("version.json 源均不可用");
    for url in UPDATE_VERSION_JSON {
        let resp = match client.get(url).timeout(std::time::Duration::from_secs(4)).send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("{url} 请求失败: {e}");
                continue;
            }
        };
        if !resp.status().is_success() {
            last_err = format!("{url} 返回 HTTP {}", resp.status().as_u16());
            continue;
        }
        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                last_err = format!("{url} 解析失败: {e}");
                continue;
            }
        };
        match info_from_version_json(&body) {
            Ok(info) => return Ok(info),
            Err(e) => last_err = format!("{url}: {e}"),
        }
    }
    Err(last_err)
}

/// version.json → UpdateInfo（缺字段时给出可读错误）
fn info_from_version_json(body: &serde_json::Value) -> Result<UpdateInfo, String> {
    let ver = body["version"]
        .as_str()
        .unwrap_or("")
        .trim()
        .trim_start_matches('v')
        .to_string();
    if ver.is_empty() {
        return Err("缺少 version 字段".into());
    }
    let current = env!("CARGO_PKG_VERSION").to_string();
    Ok(UpdateInfo {
        has_update: version_gt(&ver, &current),
        current_version: current,
        latest_version: ver.clone(),
        release_url: body["release_url"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                format!("https://github.com/smcloudcat/felldl/releases/tag/v{ver}")
            }),
        notes: body["notes"].as_str().unwrap_or("").to_string(),
        published_at: body["published_at"].as_str().unwrap_or("").to_string(),
        assets: Vec::new(),
        note: None,
    })
}

/// 检查更新
///
/// 顺序：本地缓存（10 分钟内）→ GitHub API（带 ETag，304 不耗配额）
/// → Releases Atom 订阅（不受 API 限流影响）→ 24 小时内的旧缓存 → 报错。
#[tauri::command]
async fn check_update(app: tauri::AppHandle, force: Option<bool>) -> Result<UpdateInfo, String> {
    let path = update_cache_path(&app);
    let mut cache = load_cache(&path);
    let now = now_secs();

    // ① 新鲜缓存直接返回，避免连点把匿名配额烧光
    if !force.unwrap_or(false) {
        if let Some(info) = cache.info.clone() {
            if now.saturating_sub(cache.checked_at) < UPDATE_FRESH_SECS {
                return Ok(refresh_info(info));
            }
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent("felldl-updater")
        .build()
        .map_err(|e| e.to_string())?;

    let api_err = match fetch_api(&client, cache.etag.clone().as_deref()).await {
        // ② 304：内容没变，刷新时间戳即可
        Ok(ApiOutcome::NotModified) => {
            if let Some(info) = cache.info.clone() {
                cache.checked_at = now;
                cache.app_version = env!("CARGO_PKG_VERSION").into();
                save_cache(&path, &cache);
                return Ok(refresh_info(info));
            }
            "GitHub 返回 304，但本地没有可用缓存".to_string()
        }
        Ok(ApiOutcome::Fresh { info, etag }) => {
            let out = info.clone();
            cache = UpdateCache {
                checked_at: now,
                etag,
                info: Some(info),
                app_version: env!("CARGO_PKG_VERSION").into(),
            };
            save_cache(&path, &cache);
            return Ok(out);
        }
        Ok(ApiOutcome::Empty) => return Err("还没有可用的正式版本".into()),
        Err(e) => e,
    };

    // ③④ API 不可用时并行试两条兜底：
    //   · Releases Atom 订阅（github.com 普通页面，不受 API 限额）
    //   · 仓库里的 version.json（jsDelivr / raw CDN，不经过 GitHub API）
    // 并行跑，最坏等待从「相加」变成「取最长」，用户少等一半时间。
    let (atom_res, json_res) = tokio::join!(fetch_atom(&client), fetch_version_json(&client));

    let atom_err = match atom_res {
        Ok(mut info) => {
            info.note = Some("GitHub API 请求受限，已改用 Releases 订阅获取（信息略有延迟）".into());
            cache.checked_at = now;
            info = refresh_info(info);
            cache.info = Some(info.clone());
            cache.app_version = env!("CARGO_PKG_VERSION").into();
            save_cache(&path, &cache);
            return Ok(info);
        }
        Err(e) => e,
    };
    let json_err = match json_res {
        Ok(mut info) => {
            info.note = Some("GitHub 接口不可用，已改用 version.json 源获取".into());
            cache.checked_at = now;
            info = refresh_info(info);
            cache.info = Some(info.clone());
            cache.app_version = env!("CARGO_PKG_VERSION").into();
            save_cache(&path, &cache);
            return Ok(info);
        }
        Err(e) => e,
    };

    // ⑤ 全都不行：24 小时内的旧结果先顶上，至少让用户知道有没有新版本
    if let Some(info) = cache.info.clone() {
        let age = now.saturating_sub(cache.checked_at);
        if age < UPDATE_STALE_SECS {
            let mut info = refresh_info(info);
            info.note = Some(format!(
                "GitHub 暂时不可访问，以下是 {} 分钟前的检查结果",
                age / 60
            ));
            return Ok(info);
        }
    }
    Err(format!("{api_err}；{atom_err}；{json_err}"))
}

/// 用系统默认方式打开链接（桌面浏览器 / 移动端外部浏览器）
#[tauri::command]
async fn open_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    // 只放行 http(s)，避免被当成本地协议跳转
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("仅支持 http/https 链接".into());
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod update_tests {
    use super::{
        info_from_version_json, load_cache, parse_atom, parse_ver, save_cache, version_gt,
        UpdateCache,
    };

    #[test]
    fn version_compare_basic() {
        assert!(version_gt("v0.2.0", "0.1.0"));
        assert!(version_gt("0.1.1", "0.1.0"));
        assert!(version_gt("1.0", "0.9.9"));
        assert!(!version_gt("0.1.0", "v0.1.0"));
        assert!(!version_gt("0.1.0", "0.1.1"));
        assert!(!version_gt("0.1", "0.1.0")); // 缺位补 0，视为相等
    }

    #[test]
    fn parse_ver_shapes() {
        assert_eq!(parse_ver("v1.2.3"), vec![1, 2, 3]);
        assert_eq!(parse_ver("2.0"), vec![2, 0]);
        assert_eq!(parse_ver(""), vec![0]);
    }

    /// Atom 兜底解析：取第一条 entry 的版本、时间与 Release 页面
    #[test]
    fn atom_feed_parsing() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Release notes from felldl</title>
  <entry>
    <id>tag:github.com,2008:Repository/1/v9.9.9</id>
    <updated>2026-11-25T04:20:00Z</updated>
    <link rel="alternate" type="text/html" href="https://github.com/smcloudcat/felldl/releases/tag/v9.9.9"/>
    <title>v9.9.9</title>
  </entry>
  <entry>
    <id>tag:github.com,2008:Repository/1/v0.0.1</id>
    <updated>2024-01-01T00:00:00Z</updated>
    <link rel="alternate" type="text/html" href="https://github.com/smcloudcat/felldl/releases/tag/v0.0.1"/>
    <title>v0.0.1</title>
  </entry>
</feed>"#;
        let info = parse_atom(xml).expect("应能解析");
        assert_eq!(info.latest_version, "9.9.9");
        assert!(info.has_update, "9.9.9 应比当前版本新");
        assert_eq!(
            info.release_url,
            "https://github.com/smcloudcat/felldl/releases/tag/v9.9.9"
        );
        assert_eq!(info.published_at, "2026-11-25T04:20:00Z");
        assert!(info.assets.is_empty());
    }

    #[test]
    fn version_json_parsing() {
        let body = serde_json::json!({
            "version": "v9.9.9",
            "release_url": "https://example.invalid/tag/v9.9.9",
            "published_at": "2026-11-25T00:00:00Z",
            "notes": "hello"
        });
        let info = info_from_version_json(&body).expect("应能解析");
        assert_eq!(info.latest_version, "9.9.9");
        assert!(info.has_update);
        assert_eq!(info.release_url, "https://example.invalid/tag/v9.9.9");
        assert_eq!(info.notes, "hello");

        // 只给 version 时，release_url 自动按标签拼出来
        let minimal = serde_json::json!({ "version": "9.9.9" });
        let info = info_from_version_json(&minimal).unwrap();
        assert_eq!(
            info.release_url,
            "https://github.com/smcloudcat/felldl/releases/tag/v9.9.9"
        );

        // 缺字段要报错，而不是当成 0.0.0
        let err = info_from_version_json(&serde_json::json!({})).unwrap_err();
        assert!(err.contains("version"), "{err}");
    }

    #[test]
    fn atom_feed_without_entry_errors() {
        let err = parse_atom("<feed></feed>").unwrap_err();
        assert!(err.contains("没有条目"), "错误应说明缺少条目: {err}");
    }

    /// 缓存读写：写进去能原样读出来，文件损坏时退回默认值
    #[test]
    fn cache_roundtrip_and_corruption() {
        let dir = std::env::temp_dir().join(format!("felldl-upd-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("update-cache.json");

        let info = super::UpdateInfo {
            current_version: "0.3.6".into(),
            latest_version: "0.3.7".into(),
            has_update: true,
            release_url: "https://example.invalid/rel".into(),
            notes: "notes".into(),
            published_at: "2026-11-25T00:00:00Z".into(),
            assets: vec![super::UpdateAsset {
                name: "a.exe".into(),
                download_url: "https://example.invalid/a.exe".into(),
                size: 42,
            }],
            note: None,
        };
        let cache = UpdateCache {
            checked_at: 12345,
            etag: Some("W/\"abc\"".into()),
            info: Some(info),
            app_version: env!("CARGO_PKG_VERSION").into(),
        };
        save_cache(&path, &cache);

        let back = load_cache(&path);
        assert_eq!(back.checked_at, 12345);
        assert_eq!(back.etag.as_deref(), Some("W/\"abc\""));
        let info = back.info.expect("应能读回 info");
        assert_eq!(info.latest_version, "0.3.7");
        assert_eq!(info.assets.len(), 1);
        assert!(info.has_update);

        // 版本升级后旧缓存必须整份作废（否则会看到「更新完还显示旧版本」）
        let mut stale = load_cache(&path);
        stale.app_version = "0.0.1".into();
        save_cache(&path, &stale);
        let after_upgrade = load_cache(&path);
        assert!(after_upgrade.info.is_none(), "版本变化后不应复用旧结果");

        std::fs::write(&path, "{not json").unwrap();
        let broken = load_cache(&path);
        assert_eq!(broken.checked_at, 0);
        assert!(broken.info.is_none(), "损坏的缓存应退回默认值");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 背景图 base64 编解码：与真实图片字节完全一致，非法输入要报错
    #[test]
    fn background_base64_roundtrip() {
        use super::{b64_decode, b64_encode, image_mime};

        // 覆盖三种 padding 情况（长度 %3 = 0/1/2）
        for data in [
            b"".to_vec(),
            b"a".to_vec(),
            b"ab".to_vec(),
            b"abc".to_vec(),
            (0u8..=255).collect::<Vec<u8>>(),
        ] {
            let enc = b64_encode(&data);
            assert_eq!(b64_decode(&enc).unwrap(), data, "编码应可无损还原");
        }
        // 标准向量，确认没有写错字母表
        assert_eq!(b64_encode(b"Man"), "TWFu");
        assert_eq!(b64_encode(b"Ma"), "TWE=");

        // 前端传的是完整 data URL，也要能兼容
        assert_eq!(
            b64_decode("data:image/png;base64,TWFu").unwrap(),
            b"Man".to_vec()
        );
        assert!(b64_decode("!!!").is_err());
        assert!(b64_decode("TWF").is_err(), "长度不是 4 的倍数应报错");

        assert_eq!(image_mime("/a/b/c.PNG"), "image/png");
        assert_eq!(image_mime("x.jpeg"), "image/jpeg");
        assert_eq!(image_mime("x.webp"), "image/webp");
        assert_eq!(image_mime("x.unknown"), "image/png");
    }

    /// 更新包文件名过滤：只留 basename，挡住路径穿越
    #[test]
    fn update_file_name_is_sanitized() {
        use super::safe_file_name;
        assert_eq!(safe_file_name("Felldl-0.3.8-win-x64.exe"), "Felldl-0.3.8-win-x64.exe");
        assert_eq!(safe_file_name("../../evil/x.exe"), "x.exe");
        assert_eq!(safe_file_name("..\\..\\windows\\system32\\y.exe"), "y.exe");
        assert_eq!(safe_file_name("a b@c.exe"), "a_b_c.exe");
        // 全是点 / 空名不能当文件名用，退回固定名
        assert_eq!(safe_file_name("..."), "felldl-update.bin");
        assert_eq!(safe_file_name(""), "felldl-update.bin");
    }

    /// 实测 ETag 复用：第二次带 If-None-Match 应拿到 304（不消耗配额）
    #[tokio::test]
    async fn if_none_match_returns_304() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(12))
            .user_agent("felldl-updater")
            .build()
            .unwrap();
        let first = match client
            .get(crate::UPDATE_RELEASE_API)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("网络不可用，跳过: {e}");
                return;
            }
        };
        if first.status() != reqwest::StatusCode::OK {
            eprintln!("匿名配额不可用（{}），跳过", first.status());
            return;
        }
        let etag = match first
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
        {
            Some(e) => e,
            None => {
                eprintln!("响应没有 ETag，跳过");
                return;
            }
        };
        let second = client
            .get(crate::UPDATE_RELEASE_API)
            .header("Accept", "application/vnd.github+json")
            .header("If-None-Match", etag)
            .send()
            .await
            .expect("第二次请求失败");
        assert_eq!(
            second.status(),
            reqwest::StatusCode::NOT_MODIFIED,
            "带 If-None-Match 应返回 304"
        );
    }

    /// 实测 version.json 兜底源（仓库还没推 version.json 或网络不通时跳过）
    #[tokio::test]
    async fn version_json_live() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(12))
            .user_agent("felldl-updater")
            .build()
            .unwrap();
        match super::fetch_version_json(&client).await {
            Ok(info) => {
                assert!(!info.latest_version.is_empty(), "版本号不应为空");
                assert!(info.release_url.starts_with("http"), "发布页地址应为 http(s)");
            }
            Err(e) => eprintln!("version.json 源不可用，跳过: {e}"),
        }
    }

    /// 实测 GitHub Releases 接口（限流时跳过，避免把网络状况当成代码问题）
    ///
    /// 需要真实外网与可信证书链：离线、代理拦截（自签 CA）环境下必然失败，
    /// 默认忽略，用 `cargo test -- --ignored github_endpoint_reachable` 手动跑。
    #[ignore = "需要联网：离线或代理中间人环境下会因证书链失败"]
    #[tokio::test]
    async fn github_endpoint_reachable() {
        let body = reqwest::Client::new()
            .get(crate::UPDATE_RELEASE_API)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "felldl-updater")
            .timeout(std::time::Duration::from_secs(12))
            .send()
            .await
            .expect("请求 GitHub 失败");
        if body.status() == reqwest::StatusCode::FORBIDDEN
            || body.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            eprintln!("匿名配额已用尽，跳过接口结构校验");
            return;
        }
        assert!(body.status().is_success(), "releases 列表应可访问");
        let json: serde_json::Value = body.json().await.expect("解析 JSON 失败");
        let arr = json.as_array().expect("应返回数组");
        assert!(!arr.is_empty(), "至少存在一个 release");
        assert!(arr[0]["tag_name"].is_string(), "tag_name 缺失");
    }
}

#[tauri::command]
async fn show_in_folder(app: tauri::AppHandle, path: String) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        return android::reveal(&app, &path);
    }
    #[cfg(not(target_os = "android"))]
    {
        reveal_path(&app, &path)
    }
}

/// 列出本机网络接口的 IP 地址（IPv4 + IPv6），用于「绑定网络接口」设置
#[tauri::command]
async fn list_interfaces() -> Result<Vec<String>, String> {
    let mut v: Vec<std::net::IpAddr> = Vec::new();
    for ip in if_addrs::get_if_addrs()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|i| i.ip())
    {
        if !v.contains(&ip) {
            v.push(ip);
        }
    }
    Ok(v.into_iter().map(|i| i.to_string()).collect())
}

#[tauri::command]
async fn pick_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    // 桌面对话框支持选择目录
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        use tauri_plugin_dialog::DialogExt;
        let (tx, rx) = std::sync::mpsc::channel();
        app.dialog().file().pick_folder(move |p| {
            let _ = tx.send(p);
        });
        // 阻塞等待用户选择（在 async 命令里用 spawn_blocking 避免卡 runtime）
        let picked = tokio::task::spawn_blocking(move || rx.recv().ok().flatten())
            .await
            .map_err(|e| e.to_string())?;
        Ok(picked.map(|p| p.to_string()))
    }
    // 移动端 dialog 插件没有目录选择 API：返回 None，前端回落到系统默认下载目录
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = app;
        Ok(None)
    }
}

// ───────────────────────── 自定义背景图 ─────────────────────────
// 前端用 <input type=file> 读图（桌面 / 安卓都可用系统选择器），把 base64 传过来，
// 这里落盘到应用数据目录，启动时再读回 data URL。
// 刻意不把图片塞进 settings.json：几 MB 的 base64 会让每次读写设置都变慢。

const BACKGROUND_MAX_BYTES: usize = 16 * 1024 * 1024;

const B64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_TABLE[(n >> 18) as usize & 63] as char);
        out.push(B64_TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64_TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn b64_val(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    // 兼容完整 data URL：data:image/png;base64,xxxx
    let s = s.splitn(2, "base64,").last().unwrap_or(s);
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if bytes.len() % 4 != 0 {
        return Err("图片数据不完整".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        let mut pad = 0;
        for (i, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                pad += 1;
                n <<= 6;
                continue;
            }
            let v = b64_val(c).ok_or_else(|| "图片数据格式错误".to_string())?;
            n = (n << 6) | v as u32;
            let _ = i;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

fn image_mime(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        _ => "image/png",
    }
}

/// 保存背景图：`data` 可以是纯 base64 或 `data:image/png;base64,...`
#[tauri::command]
async fn save_background(
    app: tauri::AppHandle,
    data: String,
    ext: String,
) -> Result<String, String> {
    let payload = data.splitn(2, "base64,").last().unwrap_or(&data);
    let bytes = b64_decode(payload)?;
    if bytes.is_empty() {
        return Err("图片内容为空".into());
    }
    if bytes.len() > BACKGROUND_MAX_BYTES {
        return Err(format!(
            "图片过大（{:.1} MB），请选择小于 {} MB 的图片",
            bytes.len() as f64 / 1024.0 / 1024.0,
            BACKGROUND_MAX_BYTES / 1024 / 1024
        ));
    }
    let ext = {
        let e = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        match e.as_str() {
            "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "avif" | "svg" => e,
            _ => "png".to_string(),
        }
    };
    let dir = data_dir(&app);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败: {e}"))?;
    // 只保留一张背景图：先清掉旧的，避免换图后残留占空间
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("ui-background.") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let path = dir.join(format!("ui-background.{ext}"));
    std::fs::write(&path, &bytes).map_err(|e| format!("写入图片失败: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

/// 读回背景图，返回可直接用于 CSS `url()` 的 data URL
#[tauri::command]
async fn read_background(path: String) -> Result<String, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("读取背景图失败: {e}"))?;
    Ok(format!("data:{};base64,{}", image_mime(&path), b64_encode(&bytes)))
}

/// 删除已保存的背景图文件
#[tauri::command]
async fn clear_background(app: tauri::AppHandle) -> Result<(), String> {
    let dir = data_dir(&app);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("ui-background.") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

// ───────────────────────── 应用内直接更新 ─────────────────────────
// 更新包只有便携 exe / apk，没有安装器：
//   1. download_update 把新版本下载到应用数据目录，边下边发进度事件
//   2. apply_update（Windows）写一个延迟脚本：等本进程退出 → 覆盖旧 exe → 启动新 exe
//      （运行中的 exe 无法直接覆盖，所以必须借「退出后再拷贝」这一步）

#[derive(Clone, serde::Serialize)]
struct UpdateProgress {
    downloaded: u64,
    total: u64,
    percent: f64,
}

/// 只取文件名部分并过滤危险字符，防止路径穿越
fn safe_file_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned.starts_with('.') {
        "felldl-update.bin".into()
    } else {
        cleaned
    }
}

/// 拉取签名更新清单：任一条来源验签通过即可用。
async fn fetch_signed_manifest(client: &reqwest::Client) -> Result<(String, felldl_core::update::Payload), String> {
    let mut last_err = String::from("更新清单源均不可用");
    for url in UPDATE_MANIFEST_JSON {
        let resp = match client
            .get(url)
            .timeout(std::time::Duration::from_secs(6))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("{url} 请求失败: {e}");
                continue;
            }
        };
        if !resp.status().is_success() {
            last_err = format!("{url} 返回 HTTP {}", resp.status().as_u16());
            continue;
        }
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                last_err = format!("{url} 读取失败: {e}");
                continue;
            }
        };
        match felldl_core::update::parse_and_verify(&text) {
            Ok(payload) => return Ok((text, payload)),
            Err(e) => last_err = format!("{url}: {e:#}"),
        }
    }
    Err(last_err)
}

/// 下载更新包。**URL 与文件名都来自签名清单**，不接受前端传参：
/// 前端只能决定「要不要更新」，不能决定「从哪里下载什么」。
#[tauri::command]
async fn download_update(
    app: tauri::AppHandle,
    verified: State<'_, VerifiedUpdates>,
) -> Result<String, String> {
    let current = env!("CARGO_PKG_VERSION");
    let (platform, arch) = (current_platform(), current_arch());

    let client = reqwest::Client::builder()
        .user_agent(concat!("Felldl/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(|e| format!("创建下载客户端失败: {e}"))?;

    // ① 先拿签名清单，确认「这个版本 / 这个平台」确实存在且比当前新
    let (manifest_text, payload) = fetch_signed_manifest(&client)
        .await
        .map_err(|e| format!("无法获取更新签名清单（{e}），请到 Release 页面手动下载安装包"))?;
    if !felldl_core::update::version_gt(&payload.version, current) {
        return Err(format!(
            "清单版本 {} 不高于当前版本 {current}，无需更新",
            payload.version
        ));
    }
    let artifact = felldl_core::update::pick_artifact(&payload, platform, arch)
        .map_err(|e| format!("{e:#}"))?;
    if !artifact.url.to_ascii_lowercase().starts_with("https://") {
        return Err("更新清单里的产物地址不是 https，已拒绝下载".into());
    }

    // ② 先写 .part，校验通过后才改名到最终位置
    let dir = data_dir(&app).join("updates");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("创建更新目录失败: {e}"))?;
    let file_name = safe_file_name(
        artifact
            .url
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("felldl-update.bin"),
    );
    let final_path = dir.join(&file_name);
    let part_path = dir.join(format!("{file_name}.part"));
    let _ = tokio::fs::remove_file(&part_path).await;

    let total = artifact.size;
    let mut resp = client
        .get(&artifact.url)
        .send()
        .await
        .map_err(|e| format!("连接更新源失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("更新源返回 HTTP {}", resp.status().as_u16()));
    }
    {
        let mut file = tokio::fs::File::create(&part_path)
            .await
            .map_err(|e| format!("创建临时文件失败: {e}"))?;
        let mut downloaded: u64 = 0;
        let mut last_emit = std::time::Instant::now();
        while let Some(chunk) = resp.chunk().await.map_err(|e| format!("下载中断: {e}"))? {
            // 清单已经声明了长度：超长直接中止，避免被灌满磁盘
            if downloaded + chunk.len() as u64 > total {
                let _ = tokio::fs::remove_file(&part_path).await;
                return Err("更新包超过清单声明的长度，已中止".into());
            }
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(|e| format!("写入失败: {e}"))?;
            downloaded += chunk.len() as u64;
            if last_emit.elapsed().as_millis() >= 200 || downloaded == total {
                last_emit = std::time::Instant::now();
                let _ = app.emit(
                    "update://progress",
                    UpdateProgress {
                        downloaded,
                        total,
                        percent: (downloaded as f64 / total.max(1) as f64 * 100.0).min(100.0),
                    },
                );
            }
        }
        tokio::io::AsyncWriteExt::flush(&mut file)
            .await
            .map_err(|e| format!("写入失败: {e}"))?;
    }

    // ③ 验签 + 长度 + 摘要 + 平台/版本，全部通过才有 VerifiedUpdate
    let verified_update = match felldl_core::update::verify_for_install(
        &manifest_text,
        platform,
        arch,
        current,
        &part_path,
    ) {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(format!("更新包校验失败：{e:#}"));
        }
    };

    let _ = app.emit(
        "update://progress",
        UpdateProgress {
            downloaded: total,
            total,
            percent: 100.0,
        },
    );

    // ④ 校验通过后才改名到最终路径，并登记到一次性 token
    let verified_update = match verified_update.move_to(&final_path) {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(format!("更新包落地失败：{e:#}"));
        }
    };
    Ok(verified.insert(verified_update))
}

/// 应用更新：只接受 download_update 返回的 token（不接受路径）。
/// Windows 用自己的隐藏 helper 进程做替换并重启，其它平台交给系统安装。
#[tauri::command]
async fn apply_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    verified: State<'_, VerifiedUpdates>,
    token: String,
) -> Result<String, String> {
    // 安卓：没有「安装未知应用」授权时，连 token 都不该消费——
    // 用户去设置里授权回来后还能直接重试，不必把几十 MB 的更新包再下一遍。
    #[cfg(target_os = "android")]
    if !android::can_install_packages(&app)? {
        return Err("install.need_permission: 需要允许「安装未知应用」才能更新".into());
    }

    let update = verified
        .take(&token)
        .ok_or_else(|| "更新包未通过校验或已失效，请重新下载".to_string())?;

    // 先把进行中的任务暂停落盘（pause 会保存 checkpoint），
    // 否则更新重启后可能带着过期进度回来，把"填零"当成已下载。
    let running: Vec<String> = state
        .engine
        .snapshot()
        .into_iter()
        .filter(|t| {
            matches!(
                t.state,
                felldl_core::TaskState::Downloading | felldl_core::TaskState::Queued
            )
        })
        .map(|t| t.id)
        .collect();
    for id in running {
        let _ = state.engine.pause(&id).await;
    }

    #[cfg(target_os = "windows")]
    {
        let exe = std::env::current_exe().map_err(|e| format!("定位当前程序失败: {e}"))?;
        // 不再写 .cmd 脚本：直接以独立进程拉起「自己」的隐藏 helper，
        // 参数走 argv（文件名里的空格/&/%/括号不会被二次解析）。
        // helper 会等本进程真正退出后再替换，并负责失败回滚。
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg(crate::update_helper::HELPER_FLAG)
            .arg("--from")
            .arg(update.path())
            .arg("--to")
            .arg(&exe)
            .arg("--restart")
            .creation_flags(CREATE_NO_WINDOW);
        match cmd.spawn() {
            Ok(child) => eprintln!("[update] 已启动更新 helper pid={}", child.id()),
            Err(e) => return Err(format!("启动更新程序失败: {e}")),
        }
        // 给 helper 一点时间起跑再退出，避免极端情况下父子同时被回收
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        app.exit(0);
        Ok("restarting".into())
    }
    // 安卓：先核对「这确实是本应用的新版本、签名一致」，再用 content:// 拉起安装器。
    // 不能走 tauri-plugin-opener：它把路径当 URL 交给 ACTION_VIEW，Android 上
    // 要么找不到启动器（静默失败），要么因 file:// 直接 FileUriExposedException。
    #[cfg(target_os = "android")]
    {
        let pkg = app.config().identifier.clone();
        let path = update.path().to_string_lossy().to_string();

        let installed = android::installed_apk_info(&app, &pkg)
            .map_err(|e| format!("install.failed: 读取已安装版本信息失败：{e}"))?;
        let apk = android::apk_info(&app, &path)
            .map_err(|e| format!("install.failed: 更新包不可用：{e}"))?;
        apk_update::verify_installable(&pkg, &installed, &apk, update.version_code)
            .map_err(|e| format!("install.rejected: {e}"))?;
        android::install_apk(&app, &path)?;
        Ok("installing".into())
    }
    // 其它桌面平台：交回系统默认程序
    #[cfg(all(not(target_os = "windows"), not(target_os = "android")))]
    {
        use tauri_plugin_opener::OpenerExt;
        match app.opener().open_path(update.path(), None::<&str>) {
            Ok(_) => Ok("opened".into()),
            Err(_) => Ok("manual".into()),
        }
    }
}

/// 下载中的临时文件路径（与 `felldl_core::util::part_path` 保持一致）
#[cfg_attr(target_os = "android", allow(dead_code))] // 安卓没有「打开/定位文件」的桌面分支
fn in_progress_part(p: &std::path::Path) -> std::path::PathBuf {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    p.with_file_name(format!("{name}.part"))
}

#[cfg(not(target_os = "android"))]
fn open_path(app: &tauri::AppHandle, path: &str) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    // 只用原生打开接口（Windows 下为 ShellExecuteExW），**不把用户路径拼进
    // cmd /C start 之类的命令行**：文件名里的 & % 括号都是合法字符，一旦进入
    // 二次解析就成了命令注入面。这里仍然限制允许打开的目标类型。
    let p = std::path::Path::new(path);
    // 未完成的下载只有 `<名>.part` 有数据，最终路径是 0 字节占位：
    // 直接打开会得到空文件，必须明确告诉用户「还没下完」
    if in_progress_part(p).exists() {
        return Err("文件仍在下载或校验中，完成后才能打开".into());
    }
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() || m.is_dir() => {}
        Ok(_) => return Err("不支持打开该类型的路径".into()),
        Err(e) => return Err(format!("路径不可用：{e}")),
    }
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg(not(target_os = "android"))]
fn reveal_path(app: &tauri::AppHandle, path: &str) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    // 定位到文件所在目录并选中它：同样走插件原生实现，
    // 不再用 `explorer /select,{path}` 这种字符串拼接方式。
    let p = std::path::Path::new(path);
    if in_progress_part(p).exists() {
        return Err("文件仍在下载或校验中，完成后才能定位".into());
    }
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => app
            .opener()
            .reveal_item_in_dir(p)
            .map_err(|e| e.to_string()),
        // 目录本身：直接在文件管理器中打开
        Ok(m) if m.is_dir() => app
            .opener()
            .open_path(path, None::<&str>)
            .map_err(|e| e.to_string()),
        Ok(_) => Err("不支持定位该类型的路径".into()),
        Err(e) => Err(format!("路径不可用：{e}")),
    }
}

// ───────────────────────── 网页抓取 / 嗅探 / HLS ─────────────────────────

#[tauri::command]
async fn analyze_url(
    state: State<'_, AppState>,
    url: String,
    referer: Option<String>,
) -> Result<felldl_core::grab::PageAnalysis, String> {
    let s = state.engine.settings();
    let mut b = reqwest::Client::builder()
        .user_agent(&s.user_agent)
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(10));
    if let Some(pu) = s.proxy.to_url() {
        let p = reqwest::Proxy::all(&pu).map_err(|e| e.to_string())?;
        b = b.proxy(p);
    }
    let client = b.build().map_err(|e| e.to_string())?;
    felldl_core::grab::analyze_page(
        &client,
        &felldl_core::grab::AnalyzeRequest {
            url,
            referer: referer.filter(|r| !r.trim().is_empty()),
            cookie: None,
        },
    )
    .await
    .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn grab_site(
    state: State<'_, AppState>,
    url: String,
    save_dir: String,
    max_depth: u32,
    max_pages: u32,
    include_assets: bool,
) -> Result<String, String> {
    state
        .engine
        .grab_site(felldl_core::grab::GrabSiteRequest {
            url,
            save_dir,
            max_depth: max_depth.clamp(1, 5),
            max_pages: max_pages.clamp(1, 500),
            include_assets,
            referer: None,
        })
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn cancel_grab(state: State<'_, AppState>, job_id: String) -> Result<bool, String> {
    Ok(state.engine.cancel_grab(&job_id))
}

#[tauri::command]
async fn download_hls(
    state: State<'_, AppState>,
    url: String,
    save_dir: Option<String>,
    file_name: Option<String>,
) -> Result<String, String> {
    state
        .engine
        .download_hls(&url, save_dir, file_name)
        .await
        .map(|id| id)
        .map_err(|e| format!("{e:#}"))
}

// ───────────────────────── 启动 ─────────────────────────

/// 应用入口。
///
/// 桌面端由 `main.rs` 调用；移动端通过 `mobile_entry_point` 宏生成 JNI 入口，
/// 由 Android 的 MainActivity 直接调用（**这一行属性不能删**，否则 APK 启动崩溃）。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }));
    }

    builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let dir = data_dir(&handle);
            std::fs::create_dir_all(&dir).ok();
            // 更新包只在「安装那一刻」有用：安卓装完会在新进程里回来，
            // Windows 的 helper 也已经把 exe 换走了。每次启动清一次，避免几十 MB 常驻。
            cleanup_update_cache(&handle);

            // 引擎 + 后台循环
            let engine = tauri::async_runtime::block_on(async {
                Engine::new(&dir).expect("引擎初始化失败")
            });
            let settings = engine.settings();
            let port = settings.api_port;

            // 启动本地 HTTP API（浏览器集成）
            let engine_for_api = engine.clone();
            let enabled = settings.api_enabled;
            let bound_port = tauri::async_runtime::block_on(async move {
                if enabled {
                    api::serve(engine_for_api, port).await.unwrap_or(port)
                } else {
                    port
                }
            });

            app.manage(AppState {
                engine: engine.clone(),
                api_port: parking_lot_stub::PortCell::new(bound_port),
            });
            // 待安装的已校验更新包（token → VerifiedUpdate）
            app.manage(VerifiedUpdates::default());

            // 事件转发：引擎 → 前端
            let app_handle = handle.clone();
            let mut rx = engine.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            let _ = app_handle.emit("engine://event", &ev);
                            // 完成时发系统通知
                            if let EngineEvent::TaskUpdated(ref t) = ev {
                                if t.state == felldl_core::TaskState::Completed {
                                    notify_done(&app_handle, t);
                                }
                            }
                            // 下载后动作：关机 / 睡眠（仅桌面执行）
                            if let EngineEvent::SystemAction(action) = &ev {
                                execute_system_action(action);
                            }
                        }
                        // 广播缓冲溢出：事件已经丢了，而且不会补发。
                        // 典型后果是「最后一次事件是『已完成』」的任务永远停在旧状态。
                        // 这里显式通知前端重新拉一次权威快照（见 ui 的 subscribeResync）。
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("[event] 事件积压，丢弃 {n} 条，通知前端重同步");
                            let _ = app_handle.emit("engine://resync", n);
                        }
                        Err(_) => break,
                    }
                }
            });

            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            setup_tray(&handle)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭窗口时收进托盘而不是退出（桌面端）
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
            let _ = window;
            let _ = event;
        })
        .invoke_handler(tauri::generate_handler![
            list_tasks,
            get_settings,
            add_task,
            add_tasks,
            pause_task,
            resume_task,
            remove_task,
            clear_completed,
            pause_all,
            resume_all,
            set_task_speed_limit,
            update_settings,
            test_dns,
            get_api_info,
            app_version,
            all_files_access,
            request_all_files_access,
            can_install_packages,
            open_install_permission_settings,
            dir_writable,
            open_file,
            show_in_folder,
            pick_folder,
            save_background,
            read_background,
            clear_background,
            check_update,
            download_update,
            apply_update,
            open_url,
            analyze_url,
            grab_site,
            cancel_grab,
            download_hls,
            list_interfaces,
        ])
        .run(tauri::generate_context!())
        .expect("Felldl 启动失败");
}

/// 执行下载后动作。Windows：shutdown / 睡眠；其他平台暂不支持（静默忽略）。
fn execute_system_action(action: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new(match action {
            "shutdown" => "shutdown",
            "sleep" => "rundll32.exe",
            _ => return,
        })
        .args(match action {
            "shutdown" => vec!["/s", "/t", "60"],
            "sleep" => vec!["powrprof.dll,SetSuspendState", "0,1,0"],
            _ => vec![],
        })
        .spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = action;
    }
}

fn notify_done(app: &tauri::AppHandle, task: &Task) {    use tauri_plugin_notification::NotificationExt;
    let enabled = app
        .try_state::<AppState>()
        .map(|s| s.engine.settings().notify_on_complete)
        .unwrap_or(true);
    if !enabled {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title("下载完成")
        .body(&task.file_name)
        .show();
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let pause_all = MenuItem::with_id(app, "pause_all", "全部暂停", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &pause_all, &sep, &quit])?;

    let mut builder = TrayIconBuilder::new()
        .tooltip("Felldl · 极速下载")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "pause_all" => {
                if let Some(state) = app.try_state::<AppState>() {
                    let engine = state.engine.clone();
                    tauri::async_runtime::spawn(async move {
                        for t in engine.snapshot() {
                            if t.state.is_active() {
                                let _ = engine.pause(&t.id).await;
                            }
                        }
                    });
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(w) = app.get_webview_window("main") {
                    if w.is_visible().unwrap_or(false) {
                        let _ = w.hide();
                    } else {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                }
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}



#[cfg(test)]
mod verified_update_tests {
    use super::{current_arch, current_platform, VerifiedUpdates};
    use felldl_core::update::{file_sha256, verify_downloaded, Artifact, VerifiedUpdate};

    /// 造一个真正通过摘要校验的 VerifiedUpdate（它没有公开构造函数）
    fn verified_sample() -> (VerifiedUpdate, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("felldl-token-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pkg.bin");
        std::fs::write(&file, b"installer-bytes").unwrap();
        let artifact = Artifact {
            platform: current_platform().to_string(),
            arch: current_arch().to_string(),
            version: "9.9.9".into(),
            url: "https://example.com/pkg.bin".into(),
            size: 15,
            sha256: file_sha256(&file).unwrap(),
            version_code: None,
        };
        let v = verify_downloaded(&artifact, &file).expect("摘要匹配应通过");
        (v, dir)
    }

    #[test]
    fn token_is_single_use_and_unknown_token_is_rejected() {
        let reg = VerifiedUpdates::default();
        let (v, dir) = verified_sample();
        let token = reg.insert(v);

        assert!(reg.take("not-a-token").is_none(), "未知 token 必须拒绝安装");
        assert!(reg.take(&token).is_some(), "首次取用应成功");
        assert!(reg.take(&token).is_none(), "token 必须一次性，防止重复安装");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_latest_pending_update_survives() {
        let reg = VerifiedUpdates::default();
        let (v1, d1) = verified_sample();
        let t1 = reg.insert(v1);
        let (v2, d2) = verified_sample();
        let t2 = reg.insert(v2);

        assert!(reg.take(&t1).is_none(), "旧 token 应失效，避免装到过期产物");
        assert!(reg.take(&t2).is_some(), "最新一次下载应可安装");

        let _ = std::fs::remove_dir_all(&d1);
        let _ = std::fs::remove_dir_all(&d2);
    }

    /// 清单里的 platform/arch 取值必须与编译目标一致，否则永远选不到产物
    #[test]
    fn platform_and_arch_match_manifest_values() {
        assert!(
            matches!(current_platform(), "windows" | "macos" | "linux" | "android"),
            "未知平台: {}",
            current_platform()
        );
        assert!(!current_arch().is_empty());
    }
}

#[cfg(test)]
mod update_cache_tests {
    use super::remove_files_in;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("felldl-update-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn removes_only_regular_files_and_reports_bytes() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("keep")).unwrap();
        std::fs::write(dir.join("new.apk"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("new.apk.part"), vec![0u8; 23]).unwrap();
        std::fs::write(dir.join("keep").join("inside.txt"), b"x").unwrap();

        assert_eq!(remove_files_in(&dir), 123, "应统计被删文件的总字节数");
        assert!(!dir.join("new.apk").exists(), "装完/放弃的更新包应被清理");
        assert!(!dir.join("new.apk.part").exists(), "半截包也应清理");
        assert!(
            dir.join("keep").join("inside.txt").exists(),
            "不是本模块创建的子目录不做递归删除"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_dir_is_not_an_error() {
        let missing =
            std::env::temp_dir().join(format!("felldl-none-{}", uuid::Uuid::new_v4()));
        assert_eq!(remove_files_in(&missing), 0);
    }
}
