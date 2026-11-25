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

struct AppState {
    engine: Arc<Engine>,
    api_port: parking_lot_stub::PortCell,
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
async fn get_api_info(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let s = state.engine.settings();
    Ok(serde_json::json!({
        "port": state.api_port.get(),
        "token": s.api_token,
        "running": s.api_enabled,
    }))
}

#[tauri::command]
async fn open_file(path: String) -> Result<(), String> {
    open_path(&path)
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
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
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
                return Ok(info);
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
                save_cache(&path, &cache);
                return Ok(info);
            }
            "GitHub 返回 304，但本地没有可用缓存".to_string()
        }
        Ok(ApiOutcome::Fresh { info, etag }) => {
            let out = info.clone();
            cache = UpdateCache {
                checked_at: now,
                etag,
                info: Some(info),
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
            cache.info = Some(info.clone());
            save_cache(&path, &cache);
            return Ok(info);
        }
        Err(e) => e,
    };
    let json_err = match json_res {
        Ok(mut info) => {
            info.note = Some("GitHub 接口不可用，已改用 version.json 源获取".into());
            cache.checked_at = now;
            cache.info = Some(info.clone());
            save_cache(&path, &cache);
            return Ok(info);
        }
        Err(e) => e,
    };

    // ⑤ 全都不行：24 小时内的旧结果先顶上，至少让用户知道有没有新版本
    if let Some(mut info) = cache.info.clone() {
        let age = now.saturating_sub(cache.checked_at);
        if age < UPDATE_STALE_SECS {
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
        };
        save_cache(&path, &cache);

        let back = load_cache(&path);
        assert_eq!(back.checked_at, 12345);
        assert_eq!(back.etag.as_deref(), Some("W/\"abc\""));
        let info = back.info.expect("应能读回 info");
        assert_eq!(info.latest_version, "0.3.7");
        assert_eq!(info.assets.len(), 1);
        assert!(info.has_update);

        std::fs::write(&path, "{not json").unwrap();
        let broken = load_cache(&path);
        assert_eq!(broken.checked_at, 0);
        assert!(broken.info.is_none(), "损坏的缓存应退回默认值");

        let _ = std::fs::remove_dir_all(&dir);
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
async fn show_in_folder(path: String) -> Result<(), String> {
    reveal_path(&path)
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

fn open_path(_path: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", _path])
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(_path)
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    {
        std::process::Command::new("xdg-open")
            .arg(_path)
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("当前平台不支持直接打开文件".into())
}

fn reveal_path(path: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // 用 explorer 选中文件
        std::process::Command::new("explorer")
            .arg(format!("/select,{path}"))
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", path])
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    {
        let dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
        open_path(&dir)
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
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
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
            open_file,
            show_in_folder,
            pick_folder,
            check_update,
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


