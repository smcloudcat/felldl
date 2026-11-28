//! 本地模式：进程内直接跑引擎（不需要 GUI）。
//!
//! 任务库放在 `<应用数据目录>/cli`，与界面用的 `tasks/` 完全分开：
//! 两个进程同时写同一份任务文件会让任务互相覆盖（后写的赢），
//! 而这种丢任务的问题只会在用户机器上偶发，很难查。
//!
//! `.running` 是一把「单实例」锁：同一时刻只允许一个 `fdl download`
//! 操作这个任务库，否则两个引擎会各自启动同一个任务、同时写同一个 `.part`。

use crate::render::Out;
use anyhow::{bail, Context, Result};
use felldl_core::{AddTaskRequest, Engine, Settings, Task, TaskState};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct DownloadSpec {
    pub url: String,
    pub out: Option<PathBuf>,
    pub name: Option<String>,
    pub connections: usize,
    pub limit_kbps: u64,
    pub cookie: Option<String>,
    pub referer: Option<String>,
    pub headers: HashMap<String, String>,
    pub mirrors: Vec<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub fresh: bool,
}

/// 前台下载的结果：调用方据此决定退出码
pub enum Exit {
    Done,
    Interrupted,
}

/// 本地任务库根目录
pub fn cli_root(data_dir: &Path) -> PathBuf {
    data_dir.join("cli")
}

/// 读应用设置（`<数据目录>/settings.json`）——只为了拿端口与令牌，
/// 找不到或损坏都返回 None，由调用方给出人话提示。
pub fn load_settings(data_dir: &Path) -> Option<Settings> {
    let text = std::fs::read_to_string(data_dir.join("settings.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// 读本地任务库里的任务（只读，不启动引擎）。
///
/// 直接读 `tasks/*.json` 而不是 `Engine::new`：打开引擎会启动调度与写盘线程，
/// 对着一个可能被别的进程使用的目录做这种事，风险远大于收益。
pub fn tasks(data_dir: &Path) -> Result<Vec<serde_json::Value>> {
    let dir = cli_root(data_dir).join("tasks");
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        // 还没用过命令行下载：空列表比报错友好
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("读取 {} 失败", dir.display())),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // 单个任务文件损坏不能让整个列表失败
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(task) = serde_json::from_str::<Task>(&text) else {
            continue;
        };
        // 与 API 一致：对外只给脱敏副本
        out.push(serde_json::to_value(task.redacted())?);
    }
    // 按落盘顺序不稳，按 id 排一下，至少每次都一样
    out.sort_by(|a, b| {
        let id = |v: &serde_json::Value| {
            v.get("id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        };
        id(a).cmp(&id(b))
    });
    Ok(out)
}

/// 本地任务库的统计：口径与 `/stats` 一致（active 只算真正在传输的）
pub fn stats(data_dir: &Path) -> Result<serde_json::Value> {
    let list = tasks(data_dir)?;
    let active = list
        .iter()
        .filter(|t| matches!(state(&t["state"]).as_str(), "downloading" | "probing"))
        .count();
    let speed: u64 = list
        .iter()
        .map(|t| t.get("speed").and_then(|v| v.as_u64()).unwrap_or(0))
        .sum();
    Ok(serde_json::json!({
        "ok": true,
        "version": felldl_core::VERSION,
        "tasks": list.len(),
        "active": active,
        "speed": speed,
    }))
}

fn state(v: &serde_json::Value) -> String {
    v.as_str().unwrap_or("unknown").to_string()
}

pub async fn download(data_dir: &Path, spec: DownloadSpec, out: &Out) -> Result<Exit> {
    let root = cli_root(data_dir);
    std::fs::create_dir_all(&root)
        .with_context(|| format!("创建任务库 {} 失败", root.display()))?;
    let _lock = StoreLock::acquire(&root)?;

    let engine = Engine::new(&root).context("初始化下载引擎失败")?;

    // 限速与自动开始：终端里没有界面去点「开始」，必须保证加完就动。
    // 限速每次都按本次参数写回（--limit 0 = 不限），否则上一次的限速会一直生效。
    {
        let mut s = engine.settings();
        let mut dirty = false;
        if s.global_speed_limit != spec.limit_kbps * 1024 {
            s.global_speed_limit = spec.limit_kbps * 1024;
            dirty = true;
        }
        if !s.auto_start {
            s.auto_start = true;
            dirty = true;
        }
        if dirty {
            engine.update_settings(s)?;
        }
    }

    let save_dir = match &spec.out {
        Some(p) => absolute(p)?,
        None => std::env::current_dir().context("取当前目录失败")?,
    };
    std::fs::create_dir_all(&save_dir)
        .with_context(|| format!("创建目录 {} 失败", save_dir.display()))?;
    let save_dir_s = save_dir.to_string_lossy().to_string();

    // 同一个地址 + 同一个目录的历史任务（用于「续传」与「已下完就跳过」）
    let same = |t: &Task| t.url == spec.url && t.save_dir == save_dir_s;

    if !spec.fresh {
        // ① 已经下完且文件还在（大小对得上）：直接给路径。
        //    重下一份只会得到 `file (1).bin`，白耗流量还可能覆盖用户改过的文件。
        if let Some(t) = engine
            .snapshot()
            .into_iter()
            .find(|t| same(t) && t.state == TaskState::Completed)
        {
            let on_disk = std::fs::metadata(&t.save_path).map(|m| m.len()).ok();
            if on_disk.is_some() && on_disk == t.size {
                if out.json {
                    println!(
                        "{}",
                        serde_json::json!({"ok": true, "id": t.id, "path": t.save_path,
                                           "bytes": on_disk, "skipped": true})
                    );
                } else {
                    println!("{}", t.save_path);
                    if !out.quiet {
                        eprintln!("已存在且大小一致，跳过下载（要重下请加 --fresh）");
                    }
                }
                return Ok(Exit::Done);
            }
        }
    }

    // ② 没下完的接上：接着下，而不是再下一份
    let resumable = if spec.fresh {
        None
    } else {
        engine
            .snapshot()
            .into_iter()
            .find(|t| same(t) && t.state != TaskState::Completed)
    };

    let (id, resumed) = match resumable {
        Some(t) => {
            engine.resume(&t.id).await?;
            (t.id, true)
        }
        None => {
            let req = AddTaskRequest {
                url: spec.url.clone(),
                save_dir: Some(save_dir_s.clone()),
                file_name: spec.name.clone(),
                connections: Some(spec.connections),
                speed_limit: None,
                scheduled_at: None,
                referer: spec.referer.clone(),
                cookie: spec.cookie.clone(),
                headers: spec.headers.clone(),
                mirrors: spec.mirrors.clone(),
                username: spec.username.clone(),
                password: spec.password.clone(),
                interval_secs: 0,
                category: None,
            };
            (engine.add(req).await?, false)
        }
    };

    if !out.quiet && !out.json {
        eprintln!(
            "{} {}",
            if resumed {
                "继续下载"
            } else {
                "开始下载"
            },
            spec.url
        );
        eprintln!(
            "保存到 {}  ·  {} 个连接",
            save_dir.display(),
            spec.connections
        );
    }

    // 进度条只在真终端里画：重定向到文件时 \r 会变成一堆乱码
    let tty = std::io::stderr().is_terminal();
    let show = tty && !out.quiet && !out.json;
    let started = Instant::now();
    let mut last_draw = Instant::now();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                // 先暂停：pause 会落 checkpoint，下次 resume 才知道每段下到哪
                let _ = engine.pause(&id).await;
                if !out.quiet {
                    eprintln!("\n已中断；重新执行同一条命令可接着下");
                }
                return Ok(Exit::Interrupted);
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }

        let Some(t) = engine.get(&id) else {
            bail!("任务在下载过程中消失（任务库被其他进程改动？）");
        };

        match t.state {
            TaskState::Completed => {
                if show {
                    eprintln!();
                }
                if out.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "ok": true,
                            "id": id,
                            "path": t.save_path,
                            "bytes": t.size.unwrap_or(t.downloaded),
                            "seconds": started.elapsed().as_secs_f64(),
                        })
                    );
                } else {
                    // 结果走 stdout：脚本里 `f=$(fdl download ...)` 直接可用
                    println!("{}", t.save_path);
                    if !out.quiet {
                        eprintln!(
                            "完成  {}  用时 {:.1}s",
                            felldl_core::util::human_bytes(t.size.unwrap_or(t.downloaded)),
                            started.elapsed().as_secs_f64()
                        );
                    }
                }
                return Ok(Exit::Done);
            }
            TaskState::Error => {
                if show {
                    eprintln!();
                }
                let code = t.error_code.clone().unwrap_or_else(|| "unknown".into());
                let retry = if t.retryable == Some(true) {
                    "（可重试）"
                } else {
                    ""
                };
                bail!(
                    "下载失败 [{code}]{retry}: {}",
                    t.error.clone().unwrap_or_default()
                );
            }
            _ => {}
        }

        if show && last_draw.elapsed() >= Duration::from_millis(400) {
            last_draw = Instant::now();
            draw(&t);
        }
    }
}

/// 单行进度：百分比 / 已下载 / 总大小 / 速度 / 剩余时间
fn draw(t: &Task) {
    let size = t.size.unwrap_or(0);
    let pct = if size > 0 {
        (t.downloaded as f64 / size as f64 * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let total = if size > 0 {
        felldl_core::util::human_bytes(size)
    } else {
        "?".into()
    };
    let eta = if t.speed > 0 && size > t.downloaded {
        format_eta((size - t.downloaded) / t.speed)
    } else {
        "--:--".into()
    };
    // \r 回到行首，末尾空格把上一帧更长的尾巴盖掉
    eprint!(
        "\r{:>5.1}%  {:>10} / {:<10}  {:>11}/s  剩余 {:>8}  ",
        pct,
        felldl_core::util::human_bytes(t.downloaded),
        total,
        felldl_core::util::human_bytes(t.speed),
        eta,
    );
}

fn format_eta(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// 相对路径 → 绝对路径（引擎要求 save_dir 是绝对路径）
///
/// 顺带去掉 `.` 这类当前目录分量：`-o ./out` 会拼出 `G:\x\./out\a.bin`
/// 这种路径，虽然能用，但会原样写进任务库、也原样打印给用户，很难看。
fn absolute(p: &Path) -> Result<PathBuf> {
    let base = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()?.join(p)
    };
    Ok(base
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect())
}

/// 任务库单实例锁。
///
/// 用操作系统的排他锁（Unix `flock` / Windows `LockFileEx`）而不是「自己建个文件」：
/// 进程被 `kill -9`、SIGTERM 或断电时，内核会自动释放锁，不会留下一个「假占用」
/// 逼用户下次手动删文件——这正是自己建锁文件最容易踩的坑。
struct StoreLock {
    /// 句柄本身持锁，close 即释放
    _file: std::fs::File,
}

impl StoreLock {
    fn acquire(root: &Path) -> Result<Self> {
        let path = root.join(".lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("创建锁文件 {} 失败", path.display()))?;
        // `File::try_lock` 是 Rust 1.89 起稳定的标准库 API，不引第三方 crate
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            // 先把 TryLockError 归一成 io::Error，才能用 ErrorKind 区分「被占用」与真错误
            Err(e) => {
                let e = std::io::Error::from(e);
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    bail!(
                        "另一个 fdl 正在使用任务库 {}：同一任务库同时只允许一个下载进程，\
                         等它结束再试（或换一个 --data-dir）",
                        root.display()
                    )
                }
                Err(e).with_context(|| format!("锁定 {} 失败", path.display()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_store_is_separate_from_app_store() {
        let root = cli_root(Path::new("/data/app"));
        assert_eq!(root, Path::new("/data/app/cli"));
        // 绝不能等于应用自己的任务目录，否则两个进程会互相覆盖任务文件
        assert_ne!(root, Path::new("/data/app"));
    }

    #[test]
    fn missing_task_dir_reads_as_empty() {
        let dir = std::env::temp_dir().join(format!("fdl-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(tasks(&dir).unwrap().is_empty());
    }

    #[test]
    fn corrupt_task_file_is_skipped_not_fatal() {
        let dir = std::env::temp_dir().join(format!("fdl-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let tasks_dir = cli_root(&dir).join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::write(tasks_dir.join("bad.json"), b"{ not json").unwrap();
        assert!(tasks(&dir).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let dir = std::env::temp_dir().join(format!("fdl-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        {
            let _a = StoreLock::acquire(&dir).unwrap();
            assert!(StoreLock::acquire(&dir).is_err(), "第二个必须被拒绝");
        }
        // 作用域结束后可以重新获取
        assert!(StoreLock::acquire(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn eta_formats_hours_only_when_needed() {
        assert_eq!(format_eta(45), "00:45");
        assert_eq!(format_eta(125), "02:05");
        assert_eq!(format_eta(3661), "1:01:01");
    }
}
