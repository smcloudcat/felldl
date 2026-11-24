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
    state.engine.clear_completed();
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

#[derive(serde::Serialize, Clone)]
pub struct UpdateAsset {
    pub name: String,
    pub download_url: String,
    pub size: u64,
}

#[derive(serde::Serialize, Clone)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub release_url: String,
    pub notes: String,
    pub published_at: String,
    pub assets: Vec<UpdateAsset>,
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

#[tauri::command]
async fn check_update() -> Result<UpdateInfo, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent("felldl-updater")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(UPDATE_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("网络请求失败: {e}"))?;
    // 私有仓库的 releases 匿名访问会 404，给出明确提示而不是笼统报错
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err("无法读取发布信息（仓库可能为私有，或还没有发布任何 Release）".into());
    }
    let resp = resp
        .error_for_status()
        .map_err(|e| format!("GitHub 返回错误: {e}"))?;
    let resp: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;

    // 从列表中挑最新的非草稿、非预发布版本（比 releases/latest 缓存更友好）
    let rel = resp
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|r| !r["draft"].as_bool().unwrap_or(false) && !r["prerelease"].as_bool().unwrap_or(false))
        })
        .ok_or_else(|| "还没有可用的正式版本".to_string())?;

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

    Ok(UpdateInfo {
        has_update: version_gt(&latest, &current),
        current_version: current,
        latest_version: latest,
        release_url: rel["html_url"].as_str().unwrap_or("").to_string(),
        notes: rel["body"].as_str().unwrap_or("").to_string(),
        published_at: rel["published_at"].as_str().unwrap_or("").to_string(),
        assets,
    })
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
    use super::{parse_ver, version_gt};

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

    /// 实测 GitHub Releases 接口解析（列表接口应返回数组）
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


