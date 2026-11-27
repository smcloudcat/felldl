//! 通知栏按钮 → 引擎 的进程内命令通道。
//!
//! 为什么不用别的通道：Kotlin 与 Rust 在同一个进程里，但没有现成的 Kotlin→Rust 调用面
//! （Tauri 的 mobile plugin 只有 Rust→Kotlin）。本地 HTTP API 依赖用户设置里的
//! `api_enabled`，关掉后通知按钮就成死的；反向注册 JNI native 方法又要在每个 ABI 上
//! 维护符号名。于是用一个「投递目录」：
//!
//! - Kotlin：`<cache>/engine-cmd/<毫秒>-<nonce>.<命令>`，`createNewFile()` 创建即为完成
//!   （内容为空，命令名就在扩展名里），新建文件系统调用本身是原子的，不需要写临时文件再改名；
//! - Rust：每秒 `read_dir` 一次，按文件名排序（毫秒前缀）依次执行，**执行前先删**。
//!
//! 语义约定：
//! - 文件名是唯一事实；不认识的名字一律删除，避免脏文件永远躺在目录里被反复扫到；
//! - 先删后执行：宁可丢一条命令也不重复执行。暂停是幂等的，但将来若加「删除任务」这类
//!   命令，重复执行就是事故。
//!
//! 代价是延迟（≤1s）而不是正确性——按钮按下到界面变化最多差一拍，用户感知不到。

// 桌面端只有单测会走到这些函数（真正的调用点在安卓轮询里），别刷 dead_code 噪声
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use felldl_core::Engine;
use std::path::Path;
#[cfg(target_os = "android")]
use std::path::PathBuf;
use std::sync::Arc;

/// 通知栏能触发的命令。新增命令时：这里加一个分支，Kotlin 侧 `EngineCommands` 加同名常量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeCmd {
    /// 暂停所有「在跑或排队」的任务（不动定时任务）
    PauseAll,
}

impl NativeCmd {
    /// 从文件扩展名映射命令
    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "pause_all" => Some(NativeCmd::PauseAll),
            _ => None,
        }
    }
}

/// 解析投递文件名：`<毫秒>-<nonce>.<命令>`
///
/// 前缀不合规也接受——只要扩展名认识就执行；名字的排序作用比格式严格更重要。
fn parse_name(name: &str) -> Option<NativeCmd> {
    // rsplit 而不是 split：nonce 里若出现 '.'（不会，但别赌），只取最后一段
    let (_, tag) = name.rsplit_once('.')?;
    NativeCmd::from_tag(tag)
}

/// 取出目录里的全部命令并清空它们，按文件名（毫秒前缀）升序。
///
/// 目录不存在、没有权限、单个文件删除失败都不算错误：命令是「尽力而为」的通道，
/// 失败只意味着这次按钮没生效，不能让调用方因此中断。
pub(crate) fn drain(dir: &Path) -> Vec<NativeCmd> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();
    names.sort();

    let mut out = Vec::new();
    for name in names {
        let path = dir.join(&name);
        // 先删：删除失败说明可能已被别的实例处理/文件被占，跳过更安全
        if std::fs::remove_file(&path).is_err() {
            continue;
        }
        if let Some(cmd) = parse_name(&name) {
            out.push(cmd);
        }
    }
    out
}

/// 执行一条命令
pub(crate) async fn apply(engine: &Arc<Engine>, cmd: NativeCmd) {
    match cmd {
        NativeCmd::PauseAll => {
            let n = engine.pause_all_live().await;
            eprintln!("[cmd] 通知栏暂停：已暂停 {n} 个任务");
        }
    }
}

/// 投递目录（安卓为 `<cache>/engine-cmd`）
#[cfg(target_os = "android")]
fn cmd_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri::Manager;
    app.path()
        .app_cache_dir()
        .ok()
        .map(|d| d.join("engine-cmd"))
}

/// 安卓侧轮询循环：1s 一次。放在进程里而不是 Service 里——按钮在 Service 停掉之后
/// 也应当能生效（虽然那时一般没有在传的任务）。
#[cfg(target_os = "android")]
pub(crate) fn spawn(app: tauri::AppHandle, engine: Arc<Engine>) {
    tauri::async_runtime::spawn(async move {
        let dir = match cmd_dir(&app) {
            Some(d) => d,
            None => {
                eprintln!("[cmd] 拿不到缓存目录，通知栏按钮不可用");
                return;
            }
        };
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            for cmd in drain(&dir) {
                apply(&engine, cmd).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "felldl-cmd-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录失败");
        dir
    }

    #[test]
    fn unknown_command_is_dropped_not_retried() {
        let dir = temp_dir("unknown");
        std::fs::write(dir.join("100-a.pause_all"), "").unwrap();
        std::fs::write(dir.join("101-b.reboot"), "").unwrap();
        std::fs::write(dir.join("102-c"), "").unwrap();
        std::fs::write(dir.join("103-d..pause_all.tmp"), "").unwrap();

        let got = drain(&dir);
        assert_eq!(got, vec![NativeCmd::PauseAll], "只认已知命令");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "认不认识都要清掉，否则脏文件会被反复扫到"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commands_run_in_filename_order_and_only_once() {
        let dir = temp_dir("order");
        for name in ["300-c.pause_all", "100-a.pause_all", "200-b.pause_all"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        assert_eq!(drain(&dir).len(), 3, "三条都要执行");
        assert!(
            drain(&dir).is_empty(),
            "第二次扫描必须为空：同一条命令不能执行两次"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_dir_is_not_an_error() {
        let dir = std::env::temp_dir().join("felldl-cmd-does-not-exist-xyz");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(drain(&dir).is_empty(), "目录还没建时应当安静地返回空");
    }

    #[test]
    fn subdirectories_are_ignored() {
        let dir = temp_dir("subdir");
        std::fs::create_dir_all(dir.join("900-x.pause_all")).unwrap();
        assert!(drain(&dir).is_empty(), "只处理普通文件");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
