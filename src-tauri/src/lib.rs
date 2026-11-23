//! Felldl 桌面/移动端壳层
//!
//! 职责：把 `felldl-core` 引擎包装成 Tauri 命令，把引擎事件转发给前端，
//! 并处理托盘、单实例、通知等平台相关能力。

use felldl_core::api;
use felldl_core::{AddTaskRequest, Engine, EngineEvent, Settings, Task};
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, State, WindowEvent,
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

#[tauri::command]
async fn show_in_folder(path: String) -> Result<(), String> {
    reveal_path(&path)
}

#[tauri::command]
async fn pick_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
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

fn open_path(path: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", path])
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
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

// ───────────────────────── 启动 ─────────────────────────

pub fn run() {
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
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });

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
        ])
        .run(tauri::generate_context!())
        .expect("Felldl 启动失败");
}

fn notify_done(app: &tauri::AppHandle, task: &Task) {
    use tauri_plugin_notification::NotificationExt;
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


