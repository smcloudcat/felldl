//! Android 专用：用系统 Intent 打开文件 / 文件夹，以及「所有文件访问」权限。
//!
//! 为什么不用 tauri-plugin-opener：它在 Android 上把路径当成 URL 交给
//! `ACTION_VIEW`，`file://` 在 API 24+ 会直接抛 `FileUriExposedException`，
//! 目录更没戏。这里改用 `FileProvider` 生成 `content://` URI，
//! 并通过 JNI 在主线程上 `startActivity`。
#![cfg(target_os = "android")]

use crate::android_paths::{content_uri, document_uri, mime_of};
use crate::apk_update::{self, ApkInfo, InstalledInfo};
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

/// 「所有文件访问」状态：`Granted` / `Denied` / `NotApplicable`
///
/// **不能在这里直接调 `Environment.isExternalStorageManager()`**：该方法是 API 30 才有的，
/// minSdk 是 24。在 API 24~29 上 `call_static_method` 会撞 `NoSuchMethodError`，
/// 线程上留一个 pending 异常（和之前安装器闪退同一类问题）。版本判断交给 Kotlin 的
/// `StorageAccess.status`，Rust 只解析三态。
pub fn all_files_access(app: &tauri::AppHandle) -> Result<crate::storage_access::AllFiles, String> {
    let raw = exec_ret(app, |env, activity| {
        helper_string(
            env,
            activity,
            STORAGE_HELPER_CLASS,
            "status",
            "(Landroid/content/Context;)Ljava/lang/String;",
            &[],
        )
    })?;
    crate::storage_access::AllFiles::parse(&raw)
}

/// 打开「所有文件访问」授权页；API 30 以下返回 `NotApplicable`（没有这个开关）
pub fn request_all_files_access(
    app: &tauri::AppHandle,
) -> Result<crate::storage_access::AllFiles, String> {
    let pkg = app.config().identifier.clone();
    let raw = exec_ret(app, move |env, activity| {
        helper_string(
            env,
            activity,
            STORAGE_HELPER_CLASS,
            "openSettings",
            "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
            &[&pkg],
        )
    })?;
    // Kotlin 侧 `OK:` / `OK:na` 都算成功：`OK:na` 表示系统没有该开关，无需授权
    if raw.trim_start().starts_with("OK:") {
        return Ok(crate::storage_access::AllFiles::parse(&raw)
            .unwrap_or(crate::storage_access::AllFiles::Granted));
    }
    expect_ok(&raw)?;
    Ok(crate::storage_access::AllFiles::Granted)
}

// ───────────────────────── 更新包安装（APK） ─────────────────────────
//
// 安装器本身在 Kotlin 侧（`ApkInstaller.kt`）：Android 的签名/版本信息只有
// `PackageManager` 能可靠读取，用 JNI 一行行拼调用来读，既啰嗦又容易漏 API
// level 分支。Kotlin 侧统一返回 `OK:...` / `ERR:...` 字符串，异常不跨 JNI 边界；
// 解析与「能不能装」的判断在 `crate::apk_update`（纯 Rust，可单测）。

const APK_HELPER_CLASS: &str = "com/felldl/app/ApkInstaller";
const STORAGE_HELPER_CLASS: &str = "com/felldl/app/StorageAccess";

/// 清掉 JNI 调用留下的 pending 异常。
///
/// `FindClass` / `CallStaticMethod` 失败时 ART 会把异常挂在线程上；不清理的话，
/// native 调用返回 Java 时会被当成「未捕获异常」直接把进程杀掉（真机表现就是
/// 点一下闪退，logcat 里只有一句 `NoClassDefFoundError`，没有堆栈）。
fn clear_pending(env: &mut JNIEnv) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}

/// 找到应用自己的 Kotlin 辅助类。
///
/// **不能用 `env.find_class`**：在这个 JNI 线程上它走的是启动类加载器，只认识
/// `java.*` / `android.*`，找不到应用自己的类 —— 真机上实测是 `NoClassDefFoundError`
/// （Class not found using the boot class loader）直接闪退。改为从 Activity 取
/// AppClassLoader，再 `loadClass`。
fn helper_class<'a>(
    env: &mut JNIEnv<'a>,
    activity: &JObject,
    class: &str,
) -> Result<jni::objects::JClass<'a>, String> {
    let loader = match env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]) {
        Ok(v) => v.l().map_err(|e| format!("读取类加载器失败: {e}"))?,
        Err(e) => {
            clear_pending(env);
            return Err(format!("读取类加载器失败: {e}"));
        }
    };
    let dotted = class.replace('/', ".");
    let jname = env
        .new_string(&dotted)
        .map_err(|e| format!("构造类名失败: {e}"))?;
    let cls = match env.call_method(
        &loader,
        "loadClass",
        "(Ljava/lang/String;)Ljava/lang/Class;",
        &[JValue::Object(&jname)],
    ) {
        Ok(v) => v.l().map_err(|e| format!("加载辅助类失败: {e}"))?,
        Err(e) => {
            clear_pending(env);
            return Err(format!("找不到辅助类 {dotted}（APK 未包含该 Kotlin 文件）: {e}"));
        }
    };
    Ok(jni::objects::JClass::from(cls))
}

/// 调 Kotlin 辅助对象的静态方法：第一个参数固定是 Activity/Context，其余为字符串
fn helper_string(
    env: &mut JNIEnv,
    activity: &JObject,
    class: &str,
    method: &str,
    sig: &str,
    args: &[&str],
) -> Result<String, String> {
    let cls = helper_class(env, activity, class)?;
    let jargs: Vec<jni::objects::JString> = args
        .iter()
        .map(|a| env.new_string(a))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("构造参数失败: {e}"))?;
    let mut values: Vec<JValue> = Vec::with_capacity(jargs.len() + 1);
    values.push(JValue::Object(activity));
    for s in &jargs {
        values.push(JValue::Object(s));
    }
    let out = env
        .call_static_method(cls, method, sig, &values)
        .map_err(|e| format!("调用 {method} 失败: {e}"))?;
    let obj = out
        .l()
        .map_err(|e| format!("{method} 返回值类型异常: {e}"))?;
    if obj.is_null() {
        return Err(format!("{method} 没有返回结果"));
    }
    let s: String = env
        .get_string(&jni::objects::JString::from(obj))
        .map_err(|e| format!("读取 {method} 返回值失败: {e}"))?
        .into();
    Ok(s)
}

/// `OK:` 前缀视为成功，其它原样作为错误抛给上层
fn expect_ok(raw: &str) -> Result<(), String> {
    if raw.trim_start().starts_with("OK:") {
        Ok(())
    } else {
        Err(raw
            .trim()
            .strip_prefix("ERR:")
            .unwrap_or(raw)
            .trim()
            .to_string())
    }
}

/// 读取已安装版本的 versionCode 与签名摘要
pub fn installed_apk_info(app: &tauri::AppHandle, package: &str) -> Result<InstalledInfo, String> {
    let pkg = package.to_string();
    let raw = exec_ret(app, move |env, activity| {
        helper_string(
            env,
            activity,
            APK_HELPER_CLASS,
            "installedInfo",
            "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
            &[&pkg],
        )
    })?;
    apk_update::parse_installed_info(&raw)
}

/// 读取 APK 文件的包名 / versionCode / 签名摘要（不安装）
pub fn apk_info(app: &tauri::AppHandle, path: &str) -> Result<ApkInfo, String> {
    let p = path.to_string();
    let raw = exec_ret(app, move |env, activity| {
        helper_string(
            env,
            activity,
            APK_HELPER_CLASS,
            "apkInfo",
            "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
            &[&p],
        )
    })?;
    apk_update::parse_apk_info(&raw)
}

/// 是否已允许「安装未知应用」（API 26+；更低版本恒为 true）
pub fn can_install_packages(app: &tauri::AppHandle) -> Result<bool, String> {
    exec_ret(app, |env, activity| {
        let cls = helper_class(env, activity, APK_HELPER_CLASS)?;
        let out = env
            .call_static_method(
                cls,
                "canInstall",
                "(Landroid/content/Context;)Z",
                &[JValue::Object(activity)],
            )
            .map_err(|e| format!("查询安装权限失败: {e}"))?;
        out.z().map_err(|e| format!("读取安装权限结果失败: {e}"))
    })
}

/// 打开「安装未知应用」授权页
pub fn open_install_settings(app: &tauri::AppHandle, package: &str) -> Result<(), String> {
    let pkg = package.to_string();
    let raw = exec_ret(app, move |env, activity| {
        helper_string(
            env,
            activity,
            APK_HELPER_CLASS,
            "openInstallSettings",
            "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
            &[&pkg],
        )
    })?;
    expect_ok(&raw)
}

/// 用 FileProvider 的 `content://` URI 拉起系统安装器。
/// 传 `file://` 在 API 24+ 会抛 `FileUriExposedException`，所以必须先映射。
pub fn install_apk(app: &tauri::AppHandle, path: &str) -> Result<(), String> {
    let pkg = app.config().identifier.clone();
    let uri = content_uri(&pkg, path)
        .ok_or_else(|| format!("更新包不在可共享目录内，请到文件管理器手动安装：{path}"))?;
    let mime = mime_of(path);
    let raw = exec_ret(app, move |env, activity| {
        helper_string(
            env,
            activity,
            APK_HELPER_CLASS,
            "install",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            &[&uri, mime],
        )
    })?;
    expect_ok(&raw)
}
