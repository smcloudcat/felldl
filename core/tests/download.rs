//! 集成测试：本地起一个支持 Range 的 HTTP 服务器，验证
//!   1. 多线程分段下载内容正确（逐字节比对）
//!   2. 暂停 → 恢复续传不丢数据、不重复
//!   3. 不支持 Range 的服务器也能正确下载
//!   4. 限速生效
//!   5. 404 / 鉴权失败等错误路径

use felldl_core::{AddTaskRequest, Engine, TaskState};
use std::io::Write;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 生成确定性伪随机内容，避免压缩/全零掩盖错位
fn make_payload(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut x: u32 = 0x1234_5678;
    for _ in 0..len {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        v.push((x >> 16) as u8);
    }
    v
}

struct TestServer {
    addr: SocketAddr,
    /// 已服务的 Range 请求数
    pub range_hits: Arc<AtomicU64>,
    pub total_hits: Arc<AtomicU64>,
}

/// 极简 HTTP 服务器，支持 Range / 不支持 Range 两种模式
async fn spawn_server(body: Vec<u8>, support_range: bool) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let range_hits = Arc::new(AtomicU64::new(0));
    let total_hits = Arc::new(AtomicU64::new(0));
    let rh = range_hits.clone();
    let th = total_hits.clone();
    let body = Arc::new(body);

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            let rh = rh.clone();
            let th = th.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                th.fetch_add(1, Ordering::Relaxed);
                let lower = req.to_lowercase();

                // 路径 /deny 返回 403，/missing 返回 404
                if lower.starts_with("get /deny") {
                    let _ = sock
                        .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                        .await;
                    return;
                }
                if lower.starts_with("get /missing") {
                    let _ = sock
                        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                        .await;
                    return;
                }

                let total = body.len() as u64;
                let mut range: Option<(u64, u64)> = None;
                for line in req.lines() {
                    let ll = line.to_lowercase();
                    if ll.starts_with("range:") {
                        let v = line.split(':').nth(1).unwrap_or("").trim().to_string();
                        if let Some(spec) = v.strip_prefix("bytes=") {
                            let spec = spec.trim();
                            if let Some((a, b)) = spec.split_once('-') {
                                let start = a.trim().parse::<u64>().unwrap_or(0);
                                let end = if b.trim().is_empty() {
                                    total.saturating_sub(1)
                                } else {
                                    b.trim().parse::<u64>().unwrap_or(total - 1)
                                };
                                range = Some((start, end.min(total - 1)));
                            }
                        }
                    }
                }

                if support_range {
                    if let Some((s, e)) = range {
                        rh.fetch_add(1, Ordering::Relaxed);
                        if s >= total {
                            let _ = sock
                                .write_all(
                                    format!(
                                        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\n\r\n"
                                    )
                                    .as_bytes(),
                                )
                                .await;
                            return;
                        }
                        let slice = &body[s as usize..=(e as usize)];
                        let head = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nETag: \"abc123\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                            slice.len(), s, e, total
                        );
                        let _ = sock.write_all(head.as_bytes()).await;
                        let _ = sock.write_all(slice).await;
                        let _ = sock.flush().await;
                        return;
                    }
                }
                // 忽略 Range，返回整包 200
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nAccept-Ranges: {}\r\nConnection: close\r\n\r\n",
                    total,
                    if support_range { "bytes" } else { "none" }
                );
                let _ = sock.write_all(head.as_bytes()).await;
                // 分块写出，模拟真实网络
                for chunk in body.chunks(16 * 1024) {
                    if sock.write_all(chunk).await.is_err() {
                        return;
                    }
                }
                let _ = sock.flush().await;
            });
        }
    });

    TestServer {
        addr,
        range_hits,
        total_hits,
    }
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "felldl-it-{tag}-{}-{}",
        std::process::id(),
        felldl_core::model::now_ms()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn wait_done(engine: &Arc<Engine>, id: &str, timeout: Duration) -> felldl_core::Task {
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t = engine.get(id).expect("任务应存在");
        if matches!(t.state, TaskState::Completed | TaskState::Error) {
            return t;
        }
        if start.elapsed() > timeout {
            panic!(
                "下载超时，最终状态: {:?}, 进度 {}/{:?}",
                t.state, t.downloaded, t.size
            );
        }
    }
}

fn new_engine(root: &std::path::Path) -> Arc<Engine> {
    Engine::new(root).expect("引擎初始化失败")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn segmented_download_is_byte_exact() {
    let payload = make_payload(3 * 1024 * 1024 + 12345);
    let srv = spawn_server(payload.clone(), true).await;
    let root = temp_root("exact");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let url = format!("http://{}/file.bin", srv.addr);
    let id = engine
        .add(AddTaskRequest {
            url: url.clone(),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let t = wait_done(&engine, &id, Duration::from_secs(60)).await;
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);
    assert_eq!(t.size, Some(payload.len() as u64));
    assert_eq!(t.downloaded, payload.len() as u64);

    let got = std::fs::read(&t.save_path).unwrap();
    assert_eq!(got.len(), payload.len(), "文件长度不符");
    assert!(got == payload, "内容不一致（分片写入错位）");
    // 确实用了多段
    assert!(
        t.segments.len() >= 2,
        "应分成多段，实际 {}",
        t.segments.len()
    );
    assert!(
        srv.range_hits.load(Ordering::Relaxed) >= 2,
        "服务器应收到多个 Range 请求"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_then_resume_continues_correctly() {
    // 4MB，限速让它有足够时间被暂停
    let payload = make_payload(4 * 1024 * 1024);
    let srv = spawn_server(payload.clone(), true).await;
    let root = temp_root("resume");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    // 限速 2MB/s，确保下载不会在暂停前跑完
    let mut s = engine.settings();
    s.global_speed_limit = 2 * 1024 * 1024;
    engine.update_settings(s).unwrap();

    let url = format!("http://{}/big.bin", srv.addr);
    let id = engine
        .add(AddTaskRequest {
            url: url.clone(),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    // 等它真正开始并有进度
    let mut waited = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t = engine.get(&id).unwrap();
        if t.downloaded > 300 * 1024 || waited > 100 {
            break;
        }
        waited += 1;
    }
    let before = engine.get(&id).unwrap().downloaded;
    assert!(before > 0, "暂停前应已有进度");

    engine.pause(&id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let paused = engine.get(&id).unwrap();
    assert_eq!(paused.state, TaskState::Paused);
    let paused_at = paused.downloaded;
    assert!(paused_at >= before, "暂停后进度不应回退");

    // 恢复
    engine.resume(&id).await.unwrap();
    let t = wait_done(&engine, &id, Duration::from_secs(90)).await;
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);

    let got = std::fs::read(&t.save_path).unwrap();
    assert_eq!(got.len(), payload.len());
    assert!(got == payload, "续传后内容不一致");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_range_server_falls_back_single_stream() {
    let payload = make_payload(700 * 1024 + 7);
    let srv = spawn_server(payload.clone(), false).await;
    let root = temp_root("norange");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let url = format!("http://{}/plain.bin", srv.addr);
    let id = engine
        .add(AddTaskRequest {
            url: url.clone(),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let t = wait_done(&engine, &id, Duration::from_secs(60)).await;
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);
    let got = std::fs::read(&t.save_path).unwrap();
    assert!(got == payload, "单流下载内容不一致");
    assert_eq!(t.connections, 16);

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn speed_limit_actually_slows() {
    let payload = make_payload(1024 * 1024); // 1MB
    let srv = spawn_server(payload.clone(), true).await;
    let root = temp_root("limit");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    // 必须在添加任务前设定限速，否则任务可能在设限前就跑完了
    let mut s = engine.settings();
    s.global_speed_limit = 256 * 1024; // 256KB/s → 1MB 至少 3 秒
    engine.update_settings(s).unwrap();

    let url = format!("http://{}/s.bin", srv.addr);
    let t0 = std::time::Instant::now();
    let id = engine
        .add(AddTaskRequest {
            url: url.clone(),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let t = wait_done(&engine, &id, Duration::from_secs(120)).await;
    let el = t0.elapsed().as_secs_f64();
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);
    assert!(el > 2.5, "限速未生效，用时仅 {el:.2}s");
    assert!(std::fs::read(&t.save_path).unwrap() == payload);

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn error_paths_are_reported() {
    let payload = make_payload(1024);
    let srv = spawn_server(payload, true).await;
    let root = temp_root("err");
    let engine = new_engine(&root);

    for (path, expect_kw, expect_code) in [
        ("/deny", "403", "http.auth"),
        ("/missing", "404", "http.not_found"),
    ] {
        let url = format!("http://{}{}", srv.addr, path);
        let id = engine
            .add(AddTaskRequest {
                url,
                save_dir: Some(root.to_string_lossy().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let t = engine.get(&id).unwrap();
        assert_eq!(t.state, TaskState::Error, "{path} 应为错误态");
        let msg = t.error.clone().unwrap_or_default();
        assert!(
            msg.contains(expect_kw),
            "{path} 错误信息应包含 {expect_kw}，实际: {msg}"
        );
        assert_eq!(
            t.error_code.as_deref(),
            Some(expect_code),
            "{path} 的错误码应可机器判断，实际 {:?}",
            t.error_code
        );
        assert_eq!(t.retryable, Some(false), "{path} 不该被自动重试");
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_task_waits_then_runs() {
    let payload = make_payload(200 * 1024);
    let srv = spawn_server(payload.clone(), true).await;
    let root = temp_root("sched");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let url = format!("http://{}/sched.bin", srv.addr);
    let fire_at = felldl_core::model::now_ms() + 1500;
    let id = engine
        .add(AddTaskRequest {
            url,
            save_dir: Some(out.to_string_lossy().to_string()),
            scheduled_at: Some(fire_at),
            ..Default::default()
        })
        .await
        .unwrap();

    // 200ms 后应仍是 Scheduled
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(engine.get(&id).unwrap().state, TaskState::Scheduled);

    let t = wait_done(&engine, &id, Duration::from_secs(30)).await;
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);
    assert!(std::fs::read(&t.save_path).unwrap() == payload);

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_limit_queues_extra_tasks() {
    let payload = make_payload(300 * 1024);
    let srv = spawn_server(payload.clone(), true).await;
    let root = temp_root("conc");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let mut s = engine.settings();
    s.max_concurrent_tasks = 1; // 同时只跑 1 个
    s.global_speed_limit = 150 * 1024;
    engine.update_settings(s).unwrap();

    let mut ids = Vec::new();
    for i in 0..3 {
        let url = format!("http://{}/c{}.bin", srv.addr, i);
        let id = engine
            .add(AddTaskRequest {
                url,
                save_dir: Some(out.to_string_lossy().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        ids.push(id);
    }

    // 全部最终都应完成（排队机制工作）
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let all: Vec<_> = ids.iter().map(|id| engine.get(id).unwrap()).collect();
        if all.iter().all(|t| t.state == TaskState::Completed) {
            break;
        }
        if all.iter().any(|t| t.state == TaskState::Error) {
            panic!(
                "有任务出错: {:?}",
                all.iter().map(|t| t.error.clone()).collect::<Vec<_>>()
            );
        }
        assert!(start.elapsed() < Duration::from_secs(90), "排队任务未完成");
    }
    for t in ids.iter().map(|id| engine.get(id).unwrap()) {
        assert!(std::fs::read(&t.save_path).unwrap() == payload);
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_process_restart() {
    let payload = make_payload(2 * 1024 * 1024);
    let srv = spawn_server(payload.clone(), true).await;
    let root = temp_root("restart");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();

    // 限速 1MB/s，保证暂停前下载未完成
    let id_stored;
    {
        let engine = new_engine(&root);
        let mut s = engine.settings();
        s.global_speed_limit = 1024 * 1024;
        engine.update_settings(s).unwrap();
        let url = format!("http://{}/r.bin", srv.addr);
        let id = engine
            .add(AddTaskRequest {
                url,
                save_dir: Some(out.to_string_lossy().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        id_stored = id.clone();
        // 等有进度
        loop {
            tokio::time::sleep(Duration::from_millis(120)).await;
            if engine.get(&id).unwrap().downloaded > 200 * 1024 {
                break;
            }
        }
        engine.pause(&id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
    } // engine drop —— 模拟进程退出

    // 新引擎从磁盘恢复
    let engine2 = new_engine(&root);
    let t = engine2.get(&id_stored).expect("任务应从磁盘恢复");
    assert_eq!(t.state, TaskState::Paused, "重启后应回落为暂停");
    let resumed_from = t.downloaded;
    assert!(resumed_from > 0, "重启后应保留已下载进度");
    assert!(!t.segments.is_empty(), "重启后应保留分片信息");

    engine2.resume(&id_stored).await.unwrap();
    let t = wait_done(&engine2, &id_stored, Duration::from_secs(120)).await;
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);
    let got = std::fs::read(&t.save_path).unwrap();
    assert!(got == payload, "跨进程续传内容不一致");

    let _ = std::fs::remove_dir_all(&root);
}

// ── 服务器：连接中途断开，用于验证重试续拉不重复 ──

/// 前 `fail_after` 字节后强制断开连接（模拟网络抖动）
async fn spawn_flaky_server(body: Vec<u8>, fail_after_bytes: usize) -> SocketAddr {
    use std::sync::atomic::AtomicUsize;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body = Arc::new(body);
    let served = Arc::new(AtomicUsize::new(0));

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            let served = served.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let total = body.len() as u64;

                let mut start = 0u64;
                let mut end = total.saturating_sub(1);
                for line in req.lines() {
                    let ll = line.to_lowercase();
                    if ll.starts_with("range:") {
                        let v = line.split(':').nth(1).unwrap_or("").trim();
                        if let Some(spec) = v.strip_prefix("bytes=") {
                            if let Some((a, b)) = spec.split_once('-') {
                                start = a.trim().parse().unwrap_or(0);
                                end = if b.trim().is_empty() {
                                    total - 1
                                } else {
                                    b.trim().parse().unwrap_or(total - 1)
                                };
                            }
                        }
                    }
                }
                end = end.min(total - 1);
                if start >= total {
                    return;
                }
                let slice = &body[start as usize..=(end as usize)];
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                    slice.len(), start, end, total
                );
                let _ = sock.write_all(head.as_bytes()).await;

                // 每个连接只吐一部分数据就断开，迫使客户端重连续拉
                let send_now = slice.len().min(fail_after_bytes);
                let already = served.load(Ordering::Relaxed);
                let cut = if already == 0 && slice.len() > send_now {
                    send_now
                } else {
                    slice.len()
                };
                let _ = sock.write_all(&slice[..cut]).await;
                let _ = sock.flush().await;
                served.fetch_add(1, Ordering::Relaxed);
                // 主动断开
                drop(sock);
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_after_connection_drop_does_not_duplicate() {
    let payload = make_payload(900 * 1024);
    let addr = spawn_flaky_server(payload.clone(), 90 * 1024).await;
    let root = temp_root("flaky");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let url = format!("http://{}/flaky.bin", addr);
    let id = engine
        .add(AddTaskRequest {
            url: url.clone(),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let t = wait_done(&engine, &id, Duration::from_secs(90)).await;
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);

    // 进度必须精确等于文件大小，不能因重试而虚高
    assert_eq!(
        t.downloaded,
        payload.len() as u64,
        "重试导致下载计数重复累加"
    );
    let got = std::fs::read(&t.save_path).unwrap();
    assert_eq!(got.len(), payload.len());
    assert!(got == payload, "断线重试后内容不一致");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_task_starts_on_time() {
    let payload = make_payload(512 * 1024);
    let srv = spawn_server(payload.clone(), true).await;
    let root = temp_root("scheduled");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    // 设置为 1.5 秒后开始
    let when = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        + 1500) as i64;

    let url = format!("http://{}/dly.bin", srv.addr);
    let id = engine
        .add(AddTaskRequest {
            url,
            save_dir: Some(out.to_string_lossy().to_string()),
            scheduled_at: Some(when),
            ..Default::default()
        })
        .await
        .unwrap();

    // 刚添加时应处于 Scheduled，且未开始下载
    let t0 = engine.snapshot().into_iter().find(|t| t.id == id).unwrap();
    assert_eq!(t0.state, TaskState::Scheduled, "定时未生效");

    // 到期后应在数秒内自动开始并完成
    let t = wait_done(&engine, &id, Duration::from_secs(60)).await;
    assert_eq!(
        t.state,
        TaskState::Completed,
        "定时任务未自动开始: {:?}",
        t.error
    );
    assert_eq!(std::fs::read(&t.save_path).unwrap(), payload);

    let _ = std::fs::remove_dir_all(&root);
}

// 避免 unused warning
#[allow(dead_code)]
fn _touch(_: &mut dyn Write) {}

// ── R02：本地文件被删 / 截短后，断点进度必须作废重下 ──

/// 通用流程：下载到一部分 → 暂停 → 破坏本地文件 → 恢复 → 内容必须与源一致
async fn interrupted_then_damaged(tag: &str, damage: fn(&str)) {
    let payload = make_payload(2 * 1024 * 1024);
    let srv = spawn_server(payload.clone(), true).await;
    let root = temp_root(tag);
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();

    let engine = new_engine(&root);
    let mut s = engine.settings();
    s.global_speed_limit = 1024 * 1024; // 限速，保证能中途暂停
    engine.update_settings(s).unwrap();

    let url = format!("http://{}/d.bin", srv.addr);
    let id = engine
        .add(AddTaskRequest {
            url,
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if engine.get(&id).unwrap().downloaded > 300 * 1024 {
            break;
        }
    }
    engine.pause(&id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let paused = engine.get(&id).unwrap();
    assert_eq!(
        paused.state,
        TaskState::Paused,
        "暂停后状态异常，进度 {}/{:?}",
        paused.downloaded,
        paused.size
    );
    assert!(
        paused.segments.iter().any(|s| s.done > 0),
        "暂停后应有分片进度"
    );
    assert!(paused.resume_anchor.is_some(), "暂停后应记录文件身份锚点");

    damage(&paused.save_path);

    // 取消限速，避免测试过慢
    let mut s = engine.settings();
    s.global_speed_limit = 0;
    engine.update_settings(s).unwrap();
    engine.resume(&id).await.unwrap();

    let t = wait_done(&engine, &id, Duration::from_secs(120)).await;
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);
    let got = std::fs::read(&t.save_path).unwrap();
    assert!(
        got == payload,
        "本地文件被破坏后仍沿用了旧断点，内容不一致（{} / {} 字节）",
        got.len(),
        payload.len()
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_local_file_deleted() {
    interrupted_then_damaged("del", |path| {
        std::fs::remove_file(path).expect("删除本地文件失败");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_local_file_truncated() {
    interrupted_then_damaged("trunc", |path| {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_len(1024).expect("截断失败");
    })
    .await;
}

// ── R03：206 响应的 Content-Range 必须与请求严格对应 ──

/// 说谎的服务器：始终返回 206，但 Content-Range 与请求不符
async fn spawn_liar_server(body: Vec<u8>, mode: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body = Arc::new(body);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let total = body.len() as u64;
                let mut range: Option<(u64, u64)> = None;
                for line in req.lines() {
                    let ll = line.to_lowercase();
                    if ll.starts_with("range:") {
                        let v = line.split(':').nth(1).unwrap_or("").trim().to_string();
                        if let Some(spec) = v.strip_prefix("bytes=") {
                            if let Some((a, b)) = spec.trim().split_once('-') {
                                let start = a.trim().parse::<u64>().unwrap_or(0);
                                let end = if b.trim().is_empty() {
                                    total - 1
                                } else {
                                    b.trim().parse::<u64>().unwrap_or(total - 1)
                                };
                                range = Some((start, end.min(total - 1)));
                            }
                        }
                    }
                }
                let (req_start, req_end) = range.unwrap_or((0, total - 1));
                let req_start = req_start.min(total - 1);
                let req_end = req_end.min(total - 1).max(req_start);
                // 谎报：永远声称从 0 开始（对 start>0 的请求就是错位的数据）
                let claimed_start = match mode {
                    "wrong_start" => 0u64,
                    _ => req_start,
                };
                let want_len = (req_end - req_start + 1).min(total - claimed_start);
                let claimed_end = claimed_start + want_len - 1;
                let slice = &body[claimed_start as usize..=(claimed_end as usize)];
                let cr = match mode {
                    "no_header" => String::new(),
                    "wrong_total" => format!("Content-Range: bytes {}-{}/{}\r\n", claimed_start, claimed_start + claimed_end, total + 1024),
                    _ => format!(
                        "Content-Range: bytes {}-{}/{}\r\n",
                        claimed_start,
                        claimed_start + claimed_end,
                        total
                    ),
                };
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n{}ETag: \"abc123\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                    slice.len(), cr
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(slice).await;
                let _ = sock.flush().await;
            });
        }
    });
    addr
}

/// 206 区间不符时必须报错，而不是把错误区间的数据写进文件并标记完成
async fn rejects_bad_partial_content(tag: &str, mode: &'static str, expect_msg: &str) {
    // 必须大到能切出多个分片：1MB 只会规划出 1 个分片，起点永远对得上
    let payload = make_payload(4 * 1024 * 1024);
    let addr = spawn_liar_server(payload.clone(), mode).await;
    let root = temp_root(tag);
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let id = engine
        .add(AddTaskRequest {
            url: format!("http://{addr}/lie.bin"),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let t = wait_done(&engine, &id, Duration::from_secs(90)).await;
    assert_eq!(t.state, TaskState::Error, "不应把错误数据当成完成");
    let msg = t.error.clone().unwrap_or_default();
    assert!(!msg.is_empty(), "失败任务必须带上可读原因");
    // 206 说的和要的不一致 = 数据不可信；自动重试没有意义
    assert_eq!(
        t.error_code.as_deref(),
        Some("integrity"),
        "区间/实体不符应归为 integrity，实际 {:?}",
        t.error_code
    );
    assert_eq!(t.retryable, Some(false), "完整性错误不应被自动重试");
    if !expect_msg.is_empty() {
        assert!(
            msg.contains(expect_msg),
            "错误信息应说明原因（期望包含「{expect_msg}」），实际: {msg}"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejects_partial_content_with_wrong_start() {
    rejects_bad_partial_content("lie_start", "wrong_start", "区间起点不符").await;
}

/// 服务器谎报总长度：探测阶段就会被采信，后续请求必然越界或起点不符，
/// 具体报哪一条取决于越界方向，这里只要求「以错误结束且原因可读」
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejects_partial_content_with_wrong_total() {
    rejects_bad_partial_content("lie_total", "wrong_total", "").await;
}

/// 206 不给 Content-Range：单流下载也不能凭状态码就写到文件里
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejects_partial_content_without_header() {
    rejects_bad_partial_content("lie_nohdr", "no_header", "缺少 Content-Range").await;
}

// ── 远端实体校验：断点期间服务器换了包，不能把新旧字节拼在一起 ──

struct MutableServer {
    addr: SocketAddr,
    /// (内容, ETag)；测试中途替换即模拟「发布方重新打包」
    entity: Arc<std::sync::RwLock<(Vec<u8>, String)>>,
}

/// 支持 Range，内容与 ETag 可在运行时替换。
/// `flip_etag_after_probe = true` 时：探测请求（bytes=0-1）返回 v1，
/// 其余请求返回 flipped-v2，用来模拟「忽略 If-Range、照旧回 206 但实体已变」的服务器。
async fn spawn_mutable_server(
    body: Vec<u8>,
    etag: &str,
    flip_etag_after_probe: bool,
) -> MutableServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let entity = Arc::new(std::sync::RwLock::new((body, etag.to_string())));
    let ent = entity.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let ent = ent.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let (body, cur_etag) = {
                    let g = ent.read().unwrap();
                    (g.0.clone(), g.1.clone())
                };
                let total = body.len() as u64;
                let mut range: Option<(u64, u64)> = None;
                for line in req.lines() {
                    let ll = line.to_lowercase();
                    if ll.starts_with("range:") {
                        let v = line.split(':').nth(1).unwrap_or("").trim().to_string();
                        if let Some(spec) = v.strip_prefix("bytes=") {
                            if let Some((a, b)) = spec.trim().split_once('-') {
                                let start = a.trim().parse::<u64>().unwrap_or(0);
                                let end = if b.trim().is_empty() {
                                    total - 1
                                } else {
                                    b.trim().parse::<u64>().unwrap_or(total - 1)
                                };
                                range = Some((start, end.min(total - 1)));
                            }
                        }
                    }
                }
                let Some((s, e)) = range else {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nETag: \"{cur_etag}\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(&body).await;
                    return;
                };
                let slice = &body[s as usize..=(e as usize)];
                // 探测用的是 bytes=0-1，其余按分片区间处理
                let is_probe = s == 0 && e == 1;
                let reply_etag = if flip_etag_after_probe && !is_probe {
                    "flipped-v2".to_string()
                } else {
                    cur_etag
                };
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nETag: \"{}\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                    slice.len(), s, e, total, reply_etag
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(slice).await;
                let _ = sock.flush().await;
            });
        }
    });

    MutableServer { addr, entity }
}

/// 断点期间服务器换了内容（长度相同、ETag 不同）→ 必须重下，最终内容为新版本
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_remote_entity_changed() {
    let payload = make_payload(2 * 1024 * 1024);
    let srv = spawn_mutable_server(payload.clone(), "v1", false).await;
    let root = temp_root("entchg");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();

    let engine = new_engine(&root);
    let mut s = engine.settings();
    s.global_speed_limit = 1024 * 1024;
    engine.update_settings(s).unwrap();

    let id = engine
        .add(AddTaskRequest {
            url: format!("http://{}/chg.bin", srv.addr),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if engine.get(&id).unwrap().downloaded > 300 * 1024 {
            break;
        }
    }
    engine.pause(&id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let paused = engine.get(&id).unwrap();
    assert_eq!(paused.state, TaskState::Paused);
    assert_eq!(paused.etag.as_deref(), Some("v1"));

    // 发布方重新打包：长度不变、内容全变、ETag 更新
    let new_payload: Vec<u8> = payload.iter().map(|b| b ^ 0x5a).collect();
    {
        let mut g = srv.entity.write().unwrap();
        g.0 = new_payload.clone();
        g.1 = "v2".to_string();
    }

    let mut s = engine.settings();
    s.global_speed_limit = 0;
    engine.update_settings(s).unwrap();
    engine.resume(&id).await.unwrap();

    let t = wait_done(&engine, &id, Duration::from_secs(120)).await;
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);
    let got = std::fs::read(&t.save_path).unwrap();
    assert!(
        got == new_payload,
        "远端换了内容却沿用了旧断点，得到的是新旧字节拼接（{} / {} 字节）",
        got.len(),
        new_payload.len()
    );
    assert_eq!(t.etag.as_deref(), Some("v2"), "应换用新实体的 ETag");

    let _ = std::fs::remove_dir_all(&root);
}

/// 服务器忽略 If-Range，照旧回 206 但实体已变 → 必须报错，不能把错位数据写进文件
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejects_segment_when_etag_changed_mid_download() {
    let payload = make_payload(2 * 1024 * 1024);
    let srv = spawn_mutable_server(payload, "v1", true).await;
    let root = temp_root("etagflip");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let id = engine
        .add(AddTaskRequest {
            url: format!("http://{}/flip.bin", srv.addr),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let t = wait_done(&engine, &id, Duration::from_secs(90)).await;
    assert_eq!(t.state, TaskState::Error, "实体变了却当成完成（应为 Error）");
    let msg = t.error.clone().unwrap_or_default();
    assert!(
        msg.contains("远端文件已变化"),
        "错误信息应指出实体变化，实际: {msg}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ── `.part` 语义：未完成时目录里不应出现「看起来已完成」的文件 ──

fn part_of(path: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(path);
    p.with_file_name(format!(
        "{}.part",
        p.file_name().unwrap().to_string_lossy()
    ))
}

/// 暂停后：最终路径只能是 0 字节占位，真实进度在 `<名>.part` 里
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_download_writes_only_part_file() {
    let payload = make_payload(2 * 1024 * 1024);
    let srv = spawn_server(payload.clone(), true).await;
    let root = temp_root("partpaused");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();

    let engine = new_engine(&root);
    let mut s = engine.settings();
    s.global_speed_limit = 1024 * 1024;
    engine.update_settings(s).unwrap();

    let id = engine
        .add(AddTaskRequest {
            url: format!("http://{}/p.bin", srv.addr),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if engine.get(&id).unwrap().downloaded > 300 * 1024 {
            break;
        }
    }
    engine.pause(&id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let t = engine.get(&id).unwrap();
    assert_eq!(t.state, TaskState::Paused);

    let part = part_of(&t.save_path);
    let part_len = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    assert!(part_len > 0, "进度必须写在 .part 里，实际 {part_len} 字节");

    let final_len = std::fs::metadata(&t.save_path).map(|m| m.len()).unwrap_or(0);
    assert_eq!(
        final_len, 0,
        "未完成的下载不应在最终路径留下数据（否则用户点开就是坏文件）"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// 完成后：`.part` 消失，最终文件内容完整
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completed_download_commits_part_atomically() {
    let payload = make_payload(512 * 1024);
    let srv = spawn_server(payload.clone(), true).await;
    let root = temp_root("partdone");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let id = engine
        .add(AddTaskRequest {
            url: format!("http://{}/c.bin", srv.addr),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    let t = wait_done(&engine, &id, Duration::from_secs(60)).await;
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);

    let part = part_of(&t.save_path);
    assert!(!part.exists(), "完成后 .part 必须被清理（rename 走了）");
    let got = std::fs::read(&t.save_path).unwrap();
    assert!(got == payload, "最终文件内容不完整：{} / {} 字节", got.len(), payload.len());

    let _ = std::fs::remove_dir_all(&root);
}

/// 失败（404）：最终路径不能留下数据，任务必须是 Error
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_download_leaves_no_final_file() {
    let payload = make_payload(64 * 1024);
    let srv = spawn_server(payload, true).await;
    let root = temp_root("partfail");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let id = engine
        .add(AddTaskRequest {
            url: format!("http://{}/missing.bin", srv.addr),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    let t = wait_done(&engine, &id, Duration::from_secs(60)).await;
    assert_eq!(t.state, TaskState::Error);
    let final_len = std::fs::metadata(&t.save_path).map(|m| m.len()).unwrap_or(0);
    assert_eq!(final_len, 0, "失败的任务不应在最终路径留下数据");

    let _ = std::fs::remove_dir_all(&root);
}

/// 删除任务时连 `.part` 一起清掉，不留无人认领的半成品
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_with_delete_removes_part_file() {
    let payload = make_payload(2 * 1024 * 1024);
    let srv = spawn_server(payload, true).await;
    let root = temp_root("partcancel");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();

    let engine = new_engine(&root);
    let mut s = engine.settings();
    s.global_speed_limit = 512 * 1024;
    engine.update_settings(s).unwrap();

    let id = engine
        .add(AddTaskRequest {
            url: format!("http://{}/x.bin", srv.addr),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if engine.get(&id).unwrap().downloaded > 100 * 1024 {
            break;
        }
    }
    let save_path = engine.get(&id).unwrap().save_path;
    let part = part_of(&save_path);
    assert!(part.exists(), "下载中应有 .part");

    engine.cancel(&id, true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!part.exists(), "删除任务后 .part 必须一并清理");
    assert!(
        !std::path::Path::new(&save_path).exists(),
        "删除任务后占位文件也应清理"
    );

    let _ = std::fs::remove_dir_all(&root);
}
