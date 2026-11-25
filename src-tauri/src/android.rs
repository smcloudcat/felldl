//! Android 专用：用系统 Intent 打开文件 / 文件夹，以及「所有文件访问」权限。
//!
//! 为什么不用 tauri-plugin-opener：它在 Android 上把路径当成 URL 交给
//! `ACTION_VIEW`，`file://` 在 API 24+ 会直接抛 `FileUriExposedException`，
//! 目录更没戏。这里改用 `FileProvider` 生成 `content://` URI，
//! 并通过 JNI 在主线程上 `startActivity`。
#![cfg(target_os = "android")]

use crate::android_paths::{content_uri, document_uri, mime_of};
use jni::objects::{JObject, JValue};
use jni::JNIEnv;
use tauri::Manager;

const FLAG_GRANT_READ_URI_PERMISSION: i32 = 0x0000_0001;
const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;

/// 在主线程执行一段 JNI 代码，并把结果同步回调用方
fn exec_ret<T, F>(app: &tauri::AppHandle, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut JNIEnv, &JObject) -> Result<T, String> + Send + 'static,
{
    let win = app
        .get_webview_window("main")
        .ok_or_else(|| "找不到主窗口".to_string())?;
    let (tx, rx) = std::sync::mpsc::channel();
    win.with_webview(move |pw| {
        pw.jni_handle().exec(move |env, activity, _webview| {
            let _ = tx.send(f(env, activity));
        });
    })
    .map_err(|e| format!("无法访问窗口: {e}"))?;
    rx.recv_timeout(std::time::Duration::from_secs(8))
        .map_err(|_| "调起系统界面超时".to_string())?
}

fn exec<F>(app: &tauri::AppHandle, f: F) -> Result<(), String>
where
    F: FnOnce(&mut JNIEnv, &JObject) -> Result<(), String> + Send + 'static,
{
    exec_ret(app, f)
}

fn check_exception(env: &mut JNIEnv, what: &str) -> Result<(), String> {
    if env.exception_check().unwrap_or(false) {
        env.exception_clear().ok();
        return Err(what.to_string());
    }
    Ok(())
}

/// 没有应用能处理时给出可读错误
fn ensure_resolvable(
    env: &mut JNIEnv,
    activity: &JObject,
    intent: &JObject,
    err: &str,
) -> Result<(), String> {
    let pm = env
        .call_method(
            activity,
            "getPackageManager",
            "()Landroid/content/pm/PackageManager;",
            &[],
        )
        .and_then(|v| v.l())
        .map_err(|e| format!("读取包管理器失败: {e}"))?;
    let resolved = env
        .call_method(
            intent,
            "resolveActivity",
            "(Landroid/content/pm/PackageManager;)Landroid/content/ComponentName;",
            &[JValue::Object(&pm)],
        )
        .and_then(|v| v.l())
        .map_err(|e| format!("查询可用应用失败: {e}"))?;
    if resolved.is_null() {
        return Err(err.to_string());
    }
    Ok(())
}

fn build_view_intent<'a>(
    env: &mut JNIEnv<'a>,
    uri: &str,
    mime: &str,
) -> Result<JObject<'a>, String> {
    let uri_cls = env
        .find_class("android/net/Uri")
        .map_err(|e| format!("找不到 Uri 类: {e}"))?;
    let juri_str = env
        .new_string(uri)
        .map_err(|e| format!("构造 Uri 失败: {e}"))?;
    let juri = env
        .call_static_method(
            uri_cls,
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[JValue::Object(&juri_str)],
        )
        .and_then(|v| v.l())
        .map_err(|e| format!("解析 Uri 失败: {e}"))?;

    let intent_cls = env
        .find_class("android/content/Intent")
        .map_err(|e| format!("找不到 Intent 类: {e}"))?;
    let action = env
        .new_string("android.intent.action.VIEW")
        .map_err(|e| format!("构造 action 失败: {e}"))?;
    let jmime = env
        .new_string(mime)
        .map_err(|e| format!("构造 MIME 失败: {e}"))?;
    let intent = env
        .new_object(
            intent_cls,
            "(Ljava/lang/String;)V",
            &[JValue::Object(&action)],
        )
        .map_err(|e| format!("创建 Intent 失败: {e}"))?;
    env.call_method(
        &intent,
        "setDataAndType",
        "(Landroid/net/Uri;Ljava/lang/String;)Landroid/content/Intent;",
        &[JValue::Object(&juri), JValue::Object(&jmime)],
    )
    .map_err(|e| format!("设置 Intent 数据失败: {e}"))?;
    env.call_method(
        &intent,
        "addFlags",
        "(I)Landroid/content/Intent;",
        &[JValue::Int(FLAG_GRANT_READ_URI_PERMISSION | FLAG_ACTIVITY_NEW_TASK)],
    )
    .map_err(|e| format!("设置 Intent 标志失败: {e}"))?;
    Ok(intent)
}

pub fn open_file(app: &tauri::AppHandle, path: &str) -> Result<(), String> {
    let pkg = app.config().identifier.clone();
    let uri = content_uri(&pkg, path)
        .ok_or_else(|| format!("这个位置不在可共享目录内，请从文件管理器打开：{path}"))?;
    let mime = mime_of(path);
    exec(app, move |env, activity| {
        let mut intent = build_view_intent(env, &uri, mime)?;
        // 精确 MIME 没人接（比如 .json 没有查看器）时退回 */*，让用户自己挑一个应用
        if ensure_resolvable(env, activity, &intent, "").is_err() && mime != "*/*" {
            intent = build_view_intent(env, &uri, "*/*")?;
        }
        ensure_resolvable(env, activity, &intent, "手机上没有能打开该文件的应用")?;
        env.call_method(
            activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )
        .map_err(|e| format!("启动失败: {e}"))?;
        check_exception(env, "系统拒绝了打开请求")
    })
}

pub fn reveal(app: &tauri::AppHandle, path: &str) -> Result<(), String> {
    let pkg = app.config().identifier.clone();
    let dir = if path.contains('.') {
        std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    } else {
        path.to_string()
    };
    let dir_uri = content_uri(&pkg, &dir);
    exec(app, move |env, activity| {
        // 依次尝试：DocumentsUI 定位目录 → FileProvider 目录 + resource/folder → */*
        let mut tries: Vec<(String, String)> = Vec::new();
        if let Some(pair) = document_uri(&pkg, &dir) {
            tries.push(pair);
        }
        if let Some(uri) = dir_uri.clone() {
            tries.push((uri.clone(), "resource/folder".to_string()));
            tries.push((uri, "*/*".to_string()));
        }
        for (uri, mime) in tries {
            let intent = build_view_intent(env, &uri, &mime)?;
            if ensure_resolvable(env, activity, &intent, "").is_err() {
                continue;
            }
            env.call_method(
                activity,
                "startActivity",
                "(Landroid/content/Intent;)V",
                &[JValue::Object(&intent)],
            )
            .map_err(|e| format!("启动失败: {e}"))?;
            if check_exception(env, "").is_ok() {
                return Ok(());
            }
        }
        Err(format!("手机上没有可用的文件管理器，请手动前往：{dir}"))
    })
}

/// 是否已获得「所有文件访问」（Android 11+）
pub fn has_all_files_access(app: &tauri::AppHandle) -> Result<bool, String> {
    exec_ret(app, |env, _activity| {
        let env_cls = env
            .find_class("android/os/Environment")
            .map_err(|e| format!("找不到 Environment: {e}"))?;
        let granted = env
            .call_static_method(env_cls, "isExternalStorageManager", "()Z", &[])
            .map_err(|e| format!("查询权限失败: {e}"))?
            .z()
            .map_err(|e| format!("读取权限结果失败: {e}"))?;
        Ok(granted)
    })
}

/// 打开本应用的系统设置页，让用户手动授予「所有文件访问」
pub fn request_all_files_access(app: &tauri::AppHandle) -> Result<(), String> {
    let pkg = app.config().identifier.clone();
    exec(app, move |env, activity| {
        let uri_cls = env
            .find_class("android/net/Uri")
            .map_err(|e| format!("找不到 Uri: {e}"))?;
        let s = env
            .new_string(format!("package:{pkg}"))
            .map_err(|e| format!("构造 Uri 失败: {e}"))?;
        let juri = env
            .call_static_method(
                uri_cls,
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&s)],
            )
            .and_then(|v| v.l())
            .map_err(|e| format!("解析 Uri 失败: {e}"))?;
        let intent_cls = env
            .find_class("android/content/Intent")
            .map_err(|e| format!("找不到 Intent: {e}"))?;
        let action = env
            .new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")
            .map_err(|e| format!("构造 action 失败: {e}"))?;
        let intent = env
            .new_object(
                intent_cls,
                "(Ljava/lang/String;)V",
                &[JValue::Object(&action)],
            )
            .map_err(|e| format!("创建 Intent 失败: {e}"))?;
        env.call_method(
            &intent,
            "setData",
            "(Landroid/net/Uri;)Landroid/content/Intent;",
            &[JValue::Object(&juri)],
        )
        .map_err(|e| format!("设置数据失败: {e}"))?;
        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(FLAG_ACTIVITY_NEW_TASK)],
        )
        .map_err(|e| format!("设置标志失败: {e}"))?;
        env.call_method(
            activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )
        .map_err(|e| format!("启动设置页失败: {e}"))?;
        check_exception(env, "系统拒绝了设置页请求")
    })
}

