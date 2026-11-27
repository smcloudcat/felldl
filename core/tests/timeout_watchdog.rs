//! 集成测试：三种「连接看起来还活着、其实已经死了」的服务器，引擎必须在有限时间内报错。
//!
//! 为什么单独一个文件：这些用例要等真实的 30s / 60s 超时，跑一遍近两分钟，
//! 不适合放进每次 `cargo test`；默认 `#[ignore]`，需要手动跑：
//!
//! ```text
//! cargo test -p felldl-core --test timeout_watchdog -- --ignored --nocapture
//! ```
//!
//! 覆盖的是 A03 批次 C 加的三道防线里最容易漏掉的两道：
//!   1. `RESPONSE_HEADER_TIMEOUT`（30s）—— 服务器接受连接但一个字节都不回
//!   2. `READ_IDLE_TIMEOUT`（60s）  —— 响应头正常，吐了几十 KB 后就静默
//!
//! 这两种情况 reqwest 的 `timeout()` 都拦不住（它管的是整个请求，大文件要设成小时级），
//! 实测表现是「任务永远停在下载中、进度一动不动、点重试也没用」。

use felldl_core::{AddTaskRequest, Engine, TaskState};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn new_engine(root: &std::path::Path) -> Arc<Engine> {
    Engine::new(root).expect("引擎初始化失败")
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "felldl-timeout-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 等任务进入终态，返回（任务快照，从开始到现在的时长）
async fn wait_terminal(
    engine: &Arc<Engine>,
    id: &str,
    limit: Duration,
) -> (felldl_core::Task, Duration) {
    let t0 = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t = engine.get(id).expect("任务应存在");
        if matches!(t.state, TaskState::Completed | TaskState::Error) {
            return (t, t0.elapsed());
        }
        assert!(
            t0.elapsed() < limit,
            "超过 {limit:?} 仍未结束，状态 {:?}，进度 {}/{:?} —— 说明死连接没被发现",
            t.state,
            t.downloaded,
            t.size
        );
    }
}

/// 只 accept、只读请求、永不回响应头。
///
/// 这正是「服务器进程卡死 / 中间设备黑洞」的样子：TCP 握手成功，内核缓冲区能收下请求，
/// 于是客户端认为请求已经发出，然后永远等不到响应头。
async fn spawn_silent_server() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            // 读走请求但一个字都不写；把 socket 存起来保持连接不关
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            held.push(sock);
        }
    });
    addr
}

/// HEAD 正常回大小，GET 发完响应头和 64KB 就静默。
///
/// 刻意不声明 `Accept-Ranges`，让引擎走单流路径——单流和分片两条读取路径
/// 各自都有空闲看门狗，这个用例覆盖单流那条。
async fn spawn_stalling_server() -> std::net::SocketAddr {
    const TOTAL: u64 = 8 * 1024 * 1024;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {TOTAL}\r\nContent-Type: application/octet-stream\r\n\r\n"
                );
                if req.starts_with("head ") {
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.shutdown().await;
                    return;
                }
                let _ = sock.write_all(head.as_bytes()).await;
                // 吐一点数据：证明连接确实建立过、也确实传了字节
                let _ = sock.write_all(&vec![0u8; 64 * 1024]).await;
                // 之后不再写、也不关闭 —— 客户端应自己判定「60 秒零字节 = 死连接」
                std::future::pending::<()>().await;
            });
        }
    });
    addr
}

/// 服务器一个字节都不回时，必须在 ~30s 内报错，而不是永远停在「下载中」。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "要等真实的 30s 超时；用 --ignored 单独跑"]
async fn header_timeout_fires_when_server_never_responds() {
    let addr = spawn_silent_server().await;
    let root = temp_root("header");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    // 注意：`add()` 会同步探测（GET Range: 0-1），等响应头的 30s 超时发生在这里，
    // 所以计时必须从 add 之前开始，否则只会量到「拿快照」的几十毫秒。
    let t0 = Instant::now();
    let id = engine
        .add(AddTaskRequest {
            url: format!("http://{addr}/file.bin"),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    let (t, _) = wait_terminal(&engine, &id, Duration::from_secs(10)).await;
    let took = t0.elapsed();
    eprintln!("[header] 从 add 到判定失败共 {took:?}");

    assert_eq!(t.state, TaskState::Error, "应判失败");
    let msg = t.error.clone().unwrap_or_default();
    assert!(
        msg.contains("没有返回响应头"),
        "文案要让人看懂是「等响应头超时」，实际：{msg}"
    );
    assert!(
        took >= Duration::from_secs(25),
        "30 秒超时不应当提前触发（{took:?}）"
    );
    assert!(
        took <= Duration::from_secs(75),
        "响应头超时之外还有别的等待在叠加，总耗时 {took:?}（上限 75s = 30s×2 UA 重试）"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 响应头发完了但正文静默时，必须在 ~60s 内报错，且已下的字节要保留。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "要等真实的 60s 超时；用 --ignored 单独跑"]
async fn idle_watchdog_fires_when_body_stalls() {
    let addr = spawn_stalling_server().await;
    let root = temp_root("idle");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let id = engine
        .add(AddTaskRequest {
            url: format!("http://{addr}/file.bin"),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    // 上限 85s：60s 空闲超时 + 建连/调度余量
    let (t, took) = wait_terminal(&engine, &id, Duration::from_secs(85)).await;
    assert_eq!(t.state, TaskState::Error, "应判失败");
    let msg = t.error.clone().unwrap_or_default();
    assert!(
        msg.contains("没有收到任何数据"),
        "文案要让人看懂是「读空闲超时」，实际：{msg}"
    );
    assert!(
        took >= Duration::from_secs(55),
        "60 秒空闲超时不应当提前触发（{took:?}）"
    );
    // 已收到的 64KB 必须留在进度里：报错不等于把下的东西扔掉，
    // 否则每次抖动都从 0 开始，用户永远下不完大文件
    assert!(
        t.downloaded > 0,
        "判死连接之前收到的字节应保留，实际进度 {}",
        t.downloaded
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 一个连上就 RST 的服务器：对照组，确认「立刻失败」不会被新加的超时拖慢。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reset_server_fails_fast() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                break;
            };
            // 直接关掉，让客户端读到 EOF/RST
            drop(sock);
        }
    });

    let root = temp_root("reset");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let id = engine
        .add(AddTaskRequest {
            url: format!("http://{addr}/file.bin"),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let (t, took) = wait_terminal(&engine, &id, Duration::from_secs(20)).await;
    assert_eq!(t.state, TaskState::Error, "应判失败");
    assert!(
        took < Duration::from_secs(20),
        "连接直接被拒应当立刻失败，不该等到 30 秒超时：{took:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
