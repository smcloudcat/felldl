//! Felldl 命令行示例：用于验证引擎真实下载能力
//!
//! cargo run --release --example cli -- <URL> [--conn 16] [--out DIR] [--limit KBps] [--timeout SEC]

use felldl_core::{AddTaskRequest, Engine, TaskState};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        println!("用法: cli <URL> [--conn N] [--out DIR] [--limit KBps] [--timeout SEC]");
        return Ok(());
    }
    let mut url = String::new();
    let mut conn = 16usize;
    let mut out = std::env::temp_dir().join("felldl-test");
    let mut limit_kbps = 0u64;
    let mut timeout_sec = 600u64;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--conn" => {
                i += 1;
                conn = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(16);
            }
            "--out" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    out = std::path::PathBuf::from(v);
                }
            }
            "--limit" => {
                i += 1;
                limit_kbps = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            "--timeout" => {
                i += 1;
                timeout_sec = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(600);
            }
            other => url = other.to_string(),
        }
        i += 1;
    }

    std::fs::create_dir_all(&out)?;
    let data_dir = std::env::temp_dir().join(format!("felldl-cli-{}", std::process::id()));
    let engine = Engine::new(&data_dir)?;

    // 设置全局限速
    if limit_kbps > 0 {
        let mut s = engine.settings();
        s.global_speed_limit = limit_kbps * 1024;
        engine.update_settings(s)?;
    }

    println!("▶ 开始下载: {url}");
    println!("  连接数: {conn}  输出目录: {}", out.display());

    let id = engine
        .add(AddTaskRequest {
            url: url.clone(),
            save_dir: Some(out.to_string_lossy().to_string()),
            file_name: None,
            connections: Some(conn),
            speed_limit: None,
            scheduled_at: None,
            referer: None,
            cookie: None,
            headers: Default::default(),
            mirrors: Vec::new(),
            username: None,
            password: None,
            category: None,
            interval_secs: 0,
        })
        .await?;

    let mut last_print = std::time::Instant::now();
    let started = std::time::Instant::now();

    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let Some(t) = engine.get(&id) else { break };
        if last_print.elapsed() >= Duration::from_millis(1000) {
            last_print = std::time::Instant::now();
            let pct = t.progress() * 100.0;
            println!(
                "  {:>5.1}%  {} / {}  {}/s  [{}]",
                pct,
                felldl_core::util::human_bytes(t.downloaded),
                t.size
                    .map(felldl_core::util::human_bytes)
                    .unwrap_or_else(|| "?".into()),
                felldl_core::util::human_bytes(t.speed),
                state_label(t.state),
            );
        }
        match t.state {
            TaskState::Completed => {
                let el = started.elapsed().as_secs_f64();
                let avg = if el > 0.0 {
                    (t.size.unwrap_or(t.downloaded) as f64 / el) as u64
                } else {
                    0
                };
                println!(
                    "✅ 完成: {}  用时 {:.1}s  平均 {}/s",
                    t.save_path,
                    el,
                    felldl_core::util::human_bytes(avg)
                );
                println!("RESULT_PATH={}", t.save_path);
                break;
            }
            TaskState::Error => {
                println!("❌ 失败: {}", t.error.unwrap_or_default());
                std::process::exit(1);
            }
            _ => {}
        }
        if started.elapsed().as_secs() > timeout_sec {
            println!("⌛ 超时退出");
            std::process::exit(2);
        }
    }

    let _ = std::fs::remove_dir_all(&data_dir);
    Ok(())
}

fn state_label(s: TaskState) -> &'static str {
    match s {
        TaskState::Queued => "排队",
        TaskState::Probing => "探测",
        TaskState::Downloading => "下载",
        TaskState::Paused => "暂停",
        TaskState::Completed => "完成",
        TaskState::Error => "错误",
        TaskState::Scheduled => "定时",
    }
}
