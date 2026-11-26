//! 本地 HTTP API 不得回传下载凭据。
//!
//! `/tasks` 直接序列化 Task，而 Task 里带 `cookie` 与 `password`（下载器自用的认证信息）。
//! 任何拿到 API token 的客户端——浏览器扩展面板、第三方脚本、被误贴进日志的响应——
//! 都会顺带拿到用户的账号。这里验证对外只给脱敏副本。

use felldl_core::engine::Engine;
use felldl_core::model::{now_ms, Task, TaskKind, TaskState};
use felldl_core::store::Store;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn temp_root(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "felldl-api-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn seed_task_with_secrets(root: &std::path::Path, save_dir: &str) -> String {
    let store = Store::new(root.to_path_buf()).unwrap();
    let task = Task {
        id: "api-redact-0001".into(),
        url: "https://example.com/private.bin".into(),
        kind: TaskKind::Http,
        mirrors: Vec::new(),
        file_name: "private.bin".into(),
        save_dir: save_dir.into(),
        save_path: std::path::Path::new(save_dir)
            .join("private.bin")
            .to_string_lossy()
            .to_string(),
        size: Some(1024),
        downloaded: 0,
        state: TaskState::Paused,
        error: None,
        error_code: None,
        retryable: None,
        rev: 0,
        connections: 8,
        segments: Vec::new(),
        supports_range: true,
        etag: Some("abc".into()),
        last_modified: None,
        content_type: None,
        referer: Some("https://example.com/page".into()),
        cookie: Some("SESSION=super-secret-cookie".into()),
        auth_origin: "https://example.com".into(),
        user_agent: None,
        speed_limit: 0,
        scheduled_at: None,
        created_at: now_ms(),
        completed_at: None,
        speed: 0,
        peers: 0,
        upload_speed: 0,
        bt_info_hash: None,
        category: None,
        username: Some("alice".into()),
        password: Some("super-secret-pass".into()),
        interval_secs: 0,
        resume_anchor: None,
    };
    store.save_task(&task).unwrap();
    task.id
}

async fn get(port: u16, path: &str, token: &str) -> String {
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("连接本地 API 失败");
    let req = format!(
        "GET {path}?token={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(req.as_bytes()).await.unwrap();
    let mut resp = String::new();
    sock.read_to_string(&mut resp).await.unwrap();
    resp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tasks_endpoint_never_returns_credentials() {
    let root = temp_root("redact");
    let out = root.join("dl");
    std::fs::create_dir_all(&out).unwrap();
    let id = seed_task_with_secrets(&root, &out.to_string_lossy());

    let engine = Engine::new(&root).unwrap();
    let token = engine.settings().api_token.clone();
    let port = felldl_core::api::serve(engine.clone() as Arc<Engine>, 0)
        .await
        .expect("启动本地 API 失败");

    let resp = get(port, "/tasks", &token).await;
    assert!(
        resp.contains(&id),
        "响应里应包含任务（否则这个用例没测到东西）: {resp}"
    );
    assert!(
        !resp.contains("super-secret-cookie"),
        "cookie 明文泄露到 API 响应: {resp}"
    );
    assert!(
        !resp.contains("super-secret-pass"),
        "password 明文泄露到 API 响应: {resp}"
    );
    assert!(
        resp.contains("***"),
        "应保留「有没有凭据」的信息量（脱敏为 ***）: {resp}"
    );
    // 非敏感字段仍然照常返回，脱敏不能把接口搞坏
    assert!(resp.contains("private.bin"), "文件名应保留: {resp}");
    assert!(resp.contains("alice"), "用户名应保留: {resp}");

    let _ = std::fs::remove_dir_all(&root);
}
