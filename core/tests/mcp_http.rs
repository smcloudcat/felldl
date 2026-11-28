//! 内嵌 MCP 端点的端到端用例：真的起 axum 服务、真的走 TCP。
//!
//! 单测只能证明 `handle()` 的分支正确，证明不了「路由挂上了、鉴权生效了、
//! 通知回 202 而不是空 200」——这些恰恰是客户端接入时最先撞到的地方。

use felldl_core::engine::Engine;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn temp_root(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "felldl-mcp-{tag}-{}-{}",
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

/// 返回 (状态码, 完整响应)
async fn request(port: u16, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("连接本地 API 失败");
    let body = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(req.as_bytes()).await.unwrap();
    let mut resp = String::new();
    sock.read_to_string(&mut resp).await.unwrap();
    let status = resp
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, resp)
}

fn token_of(engine: &Arc<Engine>) -> String {
    engine.settings().api_token.clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_endpoint_speaks_streamable_http() {
    let root = temp_root("http");
    let engine = Engine::new(&root).unwrap();
    let token = token_of(&engine);
    let port = felldl_core::api::serve(engine.clone() as Arc<Engine>, 0)
        .await
        .expect("启动本地 API 失败");

    // 1. 没带 token 必须拒绝：MCP 端点等于完整的下载控制面，不能因为「是本机」就放行
    let (code, _) = request(
        port,
        "POST",
        "/mcp",
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
    )
    .await;
    assert_eq!(code, 401, "无 token 的 MCP 请求必须 401");

    let path = format!("/mcp?token={token}");

    // 2. initialize 协商
    let (code, resp) = request(
        port,
        "POST",
        &path,
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#),
    )
    .await;
    assert_eq!(code, 200);
    assert!(
        resp.contains("\"protocolVersion\":\"2025-03-26\""),
        "协议版本未按请求协商: {resp}"
    );
    assert!(resp.contains("content-type: application/json"), "{resp}");

    // 3. tools/list 必须与 core 注册表一致
    let (code, resp) = request(
        port,
        "POST",
        &path,
        Some(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#),
    )
    .await;
    assert_eq!(code, 200);
    for name in felldl_core::mcp::TOOL_NAMES {
        assert!(resp.contains(name), "缺少工具 {name}: {resp}");
    }

    // 4. tools/call：stats 全靠引擎快照，不碰网络
    let (code, resp) = request(
        port,
        "POST",
        &path,
        Some(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"felldl_stats","arguments":{}}}"#),
    )
    .await;
    assert_eq!(code, 200);
    assert!(resp.contains("\"isError\":false"), "stats 不该失败: {resp}");
    assert!(resp.contains("\\\"tasks\\\""), "stats 应带任务数: {resp}");

    // 5. 通知没有 id：回 202 且不带 JSON-RPC 响应体
    let (code, resp) = request(
        port,
        "POST",
        &path,
        Some(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
    )
    .await;
    assert_eq!(code, 202, "通知应回 202: {resp}");
    assert!(!resp.contains("\"jsonrpc\""), "通知不该有响应体: {resp}");

    // 6. GET /mcp：不提供 SSE 流，明确 405 而不是挂住连接
    let (code, resp) = request(port, "GET", &path, None).await;
    assert_eq!(code, 405, "GET /mcp 应 405: {resp}");

    // 7. 非对象（批量）请求：MCP 2025-06-18 已移除 batching，按无效请求拒绝
    let (code, resp) = request(port, "POST", &path, Some(r#"[{"jsonrpc":"2.0","id":9}]"#)).await;
    assert_eq!(code, 200);
    assert!(resp.contains("-32600"), "批量请求应报无效请求: {resp}");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_tool_errors_are_text_not_rpc_errors() {
    let root = temp_root("toolerr");
    let engine = Engine::new(&root).unwrap();
    let token = token_of(&engine);
    let port = felldl_core::api::serve(engine.clone() as Arc<Engine>, 0)
        .await
        .expect("启动本地 API 失败");
    let path = format!("/mcp?token={token}");

    // 不存在的任务：必须是 isError=true 的文本结果，让模型能看到原因
    let (code, resp) = request(
        port,
        "POST",
        &path,
        Some(r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"felldl_get_task","arguments":{"id":"nope"}}}"#),
    )
    .await;
    assert_eq!(code, 200);
    assert!(resp.contains("\"isError\":true"), "{resp}");
    assert!(resp.contains("任务不存在"), "错误原文要带给模型: {resp}");
    assert!(
        !resp.contains("\"error\":{\"code\""),
        "工具失败不应变成 JSON-RPC error: {resp}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
