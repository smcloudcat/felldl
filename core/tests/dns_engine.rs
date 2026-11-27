//! 引擎链路验证：设置自定义 DNS 后，HTTP 任务确实走指定 DNS 解析（而非系统 DNS）。
//!
//! 本地起一个只会把任意域名解析到 127.0.0.1 的 UDP DNS 服务器，
//! 再用一个系统 DNS 不可能解析的域名（felldl-dns-probe.felldltest）发起下载。

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use felldl_core::engine::Engine;
use felldl_core::model::{AddTaskRequest, TaskState};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;

fn build_reply(query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    let mut i = 12usize;
    loop {
        let len = *query.get(i)? as usize;
        if len == 0 {
            i += 1;
            break;
        }
        if len & 0xC0 != 0 {
            return None;
        }
        i += 1 + len;
    }
    let qend = i + 4;
    let question = query.get(12..qend)?;
    // 只回答 A 查询；AAAA 等返回「无记录」，与真实 DNS 行为一致
    let qtype = u16::from_be_bytes([*query.get(i)?, *query.get(i + 1)?]);
    let mut out = Vec::with_capacity(qend + 16);
    out.extend_from_slice(&query[0..2]);
    out.extend_from_slice(&[0x81, 0x80]);
    out.extend_from_slice(&[0x00, 0x01]);
    out.extend_from_slice(&[0x00, if qtype == 1 { 0x01 } else { 0x00 }]);
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    out.extend_from_slice(question);
    if qtype == 1 {
        out.extend_from_slice(&[0xC0, 0x0C]);
        out.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x3C, 0x00, 0x04, 127, 0, 0, 1]);
    }
    Some(out)
}

async fn stub_dns() -> SocketAddr {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 1500];
        loop {
            let (n, peer) = match sock.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(_) => break,
            };
            if let Some(reply) = build_reply(&buf[..n]) {
                let _ = sock.send_to(&reply, peer).await;
            }
        }
    });
    addr
}

async fn handler(State(data): State<Arc<Vec<u8>>>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_LENGTH, data.len().to_string())],
        Body::from((*data).clone()),
    )
        .into_response()
}

#[tokio::test]
async fn download_uses_custom_dns() {
    let data: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 253) as u8).collect();
    let data = Arc::new(data);

    let app = Router::new()
        .route("/d.bin", get(handler))
        .with_state(data.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dns = stub_dns().await;

    let dir = std::env::temp_dir().join(format!("felldl-dns-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::new(&dir).unwrap();

    let mut s = engine.settings();
    s.dns_mode = "custom".into();
    s.dns_servers = format!("127.0.0.1:{}", dns.port());
    engine.update_settings(s).unwrap();

    // felldltest 是虚构后缀，系统 DNS 必然解析失败；能下载成功就说明走了自定义 DNS
    let task_id = engine
        .add(AddTaskRequest {
            url: format!("http://felldl-dns-probe.felldltest:{http_port}/d.bin"),
            save_dir: Some(dir.to_string_lossy().to_string()),
            connections: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();

    let mut final_task = None;
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let t = engine.get(&task_id).unwrap();
        if t.state == TaskState::Completed || t.state == TaskState::Error {
            final_task = Some(t);
            break;
        }
    }
    let t = final_task.expect("任务在超时内未结束");
    assert_eq!(t.state, TaskState::Completed, "任务出错: {:?}", t.error);
    let got = std::fs::read(dir.join("d.bin")).unwrap();
    assert_eq!(got, *data);

    let _ = std::fs::remove_dir_all(&dir);
}
