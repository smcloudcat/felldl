//! 更新落地 helper（Windows）：不经过任何命令行解析，也不写 .cmd 脚本。
//!
//! 运行方式：由主程序以**独立进程**重新拉起自己，并带上隐藏参数：
//!
//! ```text
//! Felldl.exe --felldl-update-helper --from <新包路径> --to <当前 exe> [--restart]
//! ```
//!
//! 由于参数是 argv 独立元素（不是拼进字符串的命令行），文件名里的 `&`、`%`、
//! 空格、中文、括号都不会被二次解析。
//!
//! 落地流程：
//! 1. 反复尝试「把旧 exe 改名成 .old，再把新包改名到正式路径」；
//!    旧进程未退出时改名会失败，因此这个重试天然等价于"等待真实进程退出"，
//!    比固定 `timeout /t 2` 可靠。
//! 2. 新包改名失败 → 立即把 .old 改回来（回滚），不会留下半个程序。
//! 3. 成功后（可选）拉起新版本，并清理 .old。
//!
//! 每一步都追加到 `<目标目录>/felldl-update.log`，出问题可以直接看结果。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 隐藏参数：出现时 main 不启动 UI，只跑 helper 逻辑
pub const HELPER_FLAG: &str = "--felldl-update-helper";

/// 单步重试间隔与总超时（进程退出通常 1 秒内，机械盘上可能久一点）
const RETRY_INTERVAL: Duration = Duration::from_millis(200);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperArgs {
    pub from: PathBuf,
    pub to: PathBuf,
    pub restart: bool,
}

/// 从 argv 解析 helper 参数；返回 None 表示这不是一次 helper 调用
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Option<HelperArgs> {
    let mut it = args.into_iter();
    let mut is_helper = false;
    let mut from = None;
    let mut to = None;
    let mut restart = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            HELPER_FLAG => is_helper = true,
            "--from" => from = it.next().map(PathBuf::from),
            "--to" => to = it.next().map(PathBuf::from),
            "--restart" => restart = true,
            _ => {}
        }
    }
    if !is_helper {
        return None;
    }
    match (from, to) {
        (Some(from), Some(to)) => Some(HelperArgs { from, to, restart }),
        // 参数不全：仍然是 helper 调用（否则会在用户机器上弹出主界面）
        _ => Some(HelperArgs {
            from: PathBuf::new(),
            to: PathBuf::new(),
            restart: false,
        }),
    }
}

fn log_path_for(target: &Path) -> PathBuf {
    target
        .parent()
        .map(|p| p.join("felldl-update.log"))
        .unwrap_or_else(|| PathBuf::from("felldl-update.log"))
}

fn log(target: &Path, msg: &str) {
    let line = format!("[{}] {msg}\n", chrono_like_now());
    // 日志失败不能影响更新本身
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path_for(target))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// 无依赖的时间戳（不引入 chrono，只用于日志排序）
fn chrono_like_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

fn backup_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "felldl.exe".into());
    name.push_str(".old");
    target.with_file_name(name)
}

fn staged_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "felldl.exe".into());
    name.push_str(".new");
    target.with_file_name(name)
}

/// 把 `from` 落地到 `to`：先复制到目标同盘目录，再「备份 → 改名 → 清理」。
///
/// 为什么要先复制：更新包在 `%APPDATA%`（C 盘），程序可能装在 D 盘，
/// 跨盘 `rename` 会直接失败。先 `copy` 到目标目录，后续改名都是同盘原子操作。
///
/// 返回 Err 时保证 `to` 仍是原来的可执行文件（不会出现"程序被删了一半"）。
pub fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    let staged = staged_path(to);
    let _ = std::fs::remove_file(&staged);

    // ① 复制到目标同盘目录；失败时旧程序完全没动
    let copied = std::fs::copy(from, &staged)?;
    let expect = std::fs::metadata(from).map(|m| m.len()).unwrap_or(copied);
    if copied != expect {
        let _ = std::fs::remove_file(&staged);
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("复制不完整（{copied}/{expect} 字节）"),
        ));
    }

    // ② 备份旧文件：进程未退出时这一步会失败，重试逻辑即"等待真实进程退出"
    let backup = backup_path(to);
    if backup.exists() {
        let _ = std::fs::remove_file(&backup);
    }
    let had_old = to.exists();
    if had_old {
        if let Err(e) = std::fs::rename(to, &backup) {
            let _ = std::fs::remove_file(&staged);
            return Err(e);
        }
    }

    // ③ 落地；失败立刻回滚
    match std::fs::rename(&staged, to) {
        Ok(()) => {
            let _ = std::fs::remove_file(&backup);
            let _ = std::fs::remove_file(from);
            Ok(())
        }
        Err(e) => {
            if had_old {
                let _ = std::fs::rename(&backup, to);
            }
            let _ = std::fs::remove_file(&staged);
            Err(e)
        }
    }
}

/// helper 主流程：返回进程退出码
pub fn run(args: HelperArgs) -> i32 {
    if args.from.as_os_str().is_empty() || args.to.as_os_str().is_empty() {
        eprintln!("用法: {HELPER_FLAG} --from <新包> --to <当前程序> [--restart]");
        return 2;
    }
    log(
        &args.to,
        &format!("开始更新: {} → {}", args.from.display(), args.to.display()),
    );
    if !args.from.is_file() {
        log(&args.to, "新包不存在，放弃（保持旧版本）");
        return 3;
    }

    let deadline = std::time::Instant::now() + TOTAL_TIMEOUT;
    let mut last_err;
    loop {
        match replace_file(&args.from, &args.to) {
            Ok(()) => {
                log(&args.to, "替换成功");
                if args.restart {
                    match std::process::Command::new(&args.to).spawn() {
                        Ok(child) => log(&args.to, &format!("已启动新版本 pid={}", child.id())),
                        Err(e) => log(&args.to, &format!("启动新版本失败: {e}")),
                    }
                }
                return 0;
            }
            Err(e) => {
                last_err = e.to_string();
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(RETRY_INTERVAL);
            }
        }
    }
    log(
        &args.to,
        &format!("替换失败（旧版本已回滚，可继续使用）: {last_err}"),
    );
    4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_only_triggers_on_flag() {
        assert!(parse_args(vec!["felldl.exe".into()]).is_none());
        let a = parse_args(vec![
            "felldl.exe".into(),
            HELPER_FLAG.into(),
            "--from".into(),
            "C:\\dl\\new & 100%.exe".into(),
            "--to".into(),
            "C:\\Program Files\\Felldl\\felldl.exe".into(),
            "--restart".into(),
        ])
        .expect("应识别为 helper 调用");
        assert_eq!(a.from, PathBuf::from("C:\\dl\\new & 100%.exe"));
        assert_eq!(a.to, PathBuf::from("C:\\Program Files\\Felldl\\felldl.exe"));
        assert!(a.restart);

        // 参数不全时也要认出来（避免弹出主界面）
        assert!(parse_args(vec![HELPER_FLAG.into()]).is_some());
    }

    #[test]
    fn replace_file_swaps_and_cleans_backup() {
        let dir = std::env::temp_dir().join(format!("felldl-helper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("app.exe");
        let new = dir.join("new.exe");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&new, b"new-bytes").unwrap();

        replace_file(&new, &target).expect("正常替换应成功");
        assert_eq!(std::fs::read(&target).unwrap(), b"new-bytes");
        assert!(!new.exists(), "新包应被移走");
        assert!(!backup_path(&target).exists(), "备份应被清理");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_file_rolls_back_when_new_is_missing() {
        let dir = std::env::temp_dir().join(format!("felldl-helper-rb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("app.exe");
        std::fs::write(&target, b"old-app").unwrap();

        // 新包不存在：改名会失败，必须把旧程序放回原位
        let err = replace_file(&dir.join("missing.exe"), &target);
        assert!(err.is_err());
        assert!(target.exists(), "回滚后旧程序必须还在");
        assert_eq!(std::fs::read(&target).unwrap(), b"old-app");
        assert!(!backup_path(&target).exists(), "回滚后不应残留 .old");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
