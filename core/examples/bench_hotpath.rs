//! 引擎热路径基线：历史任务规模 N 对启动、快照、序列化的影响。
//!
//! 这不是 Criterion 基准（项目不引入额外依赖），只输出**同机同构建前后可对比**的
//! 原始数字，用来回答第十一节 11.1 的问题：「1000 任务的冷/热首屏，时间花在哪」。
//!
//! 用法（必须是 release，debug 数字没有参考价值）：
//!   cargo run --release -p felldl-core --example bench_hotpath -- 1000
//!   cargo run --release -p felldl-core --example bench_hotpath -- 10000
//!
//! 每轮都新建临时数据目录，先直接写 N 个任务 JSON 再起 Engine，模拟「进程重启」。
//! 输出：种子写入耗时、Engine::new（载入 + 索引 + 起后台 loop）、snapshot()、
//! 快照 JSON 字节数（≈ list_tasks 一次 IPC 的载荷）、空闲时下一次 tick 的等待时长。

use felldl_core::engine::Engine;
use std::io::Write;
use std::time::Instant;

fn task_json(i: usize) -> serde_json::Value {
    let size: u64 = 10_000_000 + i as u64;
    // 16 线程分片，和默认下载配置一致；片段会让 JSON 变大，影响序列化数字
    let segments: Vec<serde_json::Value> = (0..16)
        .map(|s| {
            let start = size / 16 * s as u64;
            let end = if s == 15 {
                size - 1
            } else {
                size / 16 * (s as u64 + 1) - 1
            };
            serde_json::json!({ "index": s, "start": start, "end": end, "done": end - start + 1 })
        })
        .collect();
    serde_json::json!({
        "id": format!("bench-{i:06}"),
        "url": format!("https://example.com/files/archive-{i}.bin"),
        "kind": "http",
        "file_name": format!("archive-{i}.bin"),
        "save_dir": "/tmp/bench",
        "save_path": format!("/tmp/bench/archive-{i}.bin"),
        "size": size,
        "downloaded": size,
        "state": "completed",
        "segments": segments,
        "supports_range": true,
        "etag": format!("\"etag-{i}\""),
        "content_type": "application/octet-stream",
        "created_at": 1_700_000_000_000i64 - (i as i64 * 60_000),
        "completed_at": 1_700_000_000_000i64 - (i as i64 * 60_000) + 5_000,
        "connections": 16,
        "cookie": if i % 10 == 0 { serde_json::json!("session=secret") } else { serde_json::Value::Null },
        "username": if i % 10 == 0 { serde_json::json!("alice") } else { serde_json::Value::Null },
        "password": if i % 10 == 0 { serde_json::json!("hunter2") } else { serde_json::Value::Null },
    })
}

fn redact(items: Vec<felldl_core::Task>) -> Vec<felldl_core::Task> {
    items.into_iter().map(|t| t.for_ui()).collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);

    let root = std::env::temp_dir().join(format!(
        "felldl-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(root.join("tasks"))?;

    let t_seed = Instant::now();
    for i in 0..n {
        let body = serde_json::to_vec(&task_json(i))?;
        let path = root.join("tasks").join(format!("bench-{i:06}.json"));
        let mut f = std::fs::File::create(path)?;
        f.write_all(&body)?;
    }
    let seed = t_seed.elapsed();

    let t_new = Instant::now();
    let engine = Engine::new(&root)?;
    let boot = t_new.elapsed();

    // 预热一次，排除首次分配/页缓存的干扰
    let _ = engine.snapshot();

    let mut snaps = Vec::with_capacity(5);
    for _ in 0..5 {
        let t = Instant::now();
        let s = engine.snapshot();
        snaps.push((t.elapsed(), s));
    }
    let snap_mid = snaps
        .iter()
        .map(|(d, _)| d.as_secs_f64())
        .collect::<Vec<_>>();
    let mut sorted = snap_mid.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let snap_median = sorted[sorted.len() / 2];

    let count = snaps[0].1.len();
    let t_ser = Instant::now();
    let bytes = serde_json::to_vec(&snaps[0].1)?.len();
    let ser = t_ser.elapsed();

    // 真正过 IPC 的那份：脱敏 + 非活跃任务去掉分片明细
    let t_ui = Instant::now();
    let ui_copy: Vec<_> = snaps[0].1.iter().map(|t| t.for_ui()).collect();
    let ui_bytes = serde_json::to_vec(&ui_copy)?.len();
    let ui_ser = t_ui.elapsed();

    // 分页首屏：UI 启动真正要传的那一页（80 条，脱敏 + 非活跃去分片）
    let t_page = Instant::now();
    let first = engine.page("all", None, 0, 80);
    let page_took = t_page.elapsed();
    let page_bytes = serde_json::to_vec(&redact(first.items))?.len();

    // 空闲时下一次 tick 的等待时长：没有活跃/到期任务时应接近 30s 上限
    let idle_wait = engine.next_tick_delay().as_secs_f64();

    println!(
        "N={n} 任务文件={:.1}KB/条  种子={seed:?}  启动={boot:?}\n\
         snapshot 中位数={:.1}ms  序列化={:.1}ms  JSON={:.1}MB\n\
         for_ui 序列化={:.1}ms  IPC JSON={:.1}MB（省 {:.0}%）\n\
         首屏一页(80条)={:.1}ms / {:.1}KB\n\
         载入条数={count}  空闲 tick 等待={idle_wait:.1}s",
        std::fs::metadata(root.join("tasks/bench-000000.json"))
            .map(|m| m.len() as f64 / 1024.0)
            .unwrap_or(0.0),
        snap_median * 1000.0,
        ser.as_secs_f64() * 1000.0,
        bytes as f64 / 1_048_576.0,
        ui_ser.as_secs_f64() * 1000.0,
        ui_bytes as f64 / 1_048_576.0,
        (1.0 - ui_bytes as f64 / bytes as f64) * 100.0,
        page_took.as_secs_f64() * 1000.0,
        page_bytes as f64 / 1024.0,
    );

    // 停掉引擎后台 loop 再删目录，避免 writer 线程还在写
    drop(engine);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
