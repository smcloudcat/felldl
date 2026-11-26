//! S03 验收：认证信息只能发往任务原始 origin。
//!
//! 起两个本地服务器（不同端口 = 不同 origin），分别在主站与镜像/重定向目标上
//! 记录收到的 Cookie / Authorization 头：
//! * 主站必须收到凭据（否则任务本身就下不动）；
//! * 镜像站、跨域重定向的落地端一个字节的凭据都不能收到。

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use felldl_core::engine::Engine;
use felldl_core::model::{AddTaskRequest, TaskState};
use std::sync::{Arc, Mutex};

const COOKIE_VALUE: &str = "sid=super-secret";

/// 一条收到的请求记录（只看与凭据相关的头）
#[derive(Debug, Clone)]
struct Rec {
    path: String,
    cookie: Option<String>,
    auth: Option<String>,
}

#[derive(Clone)]
struct Ctx {
    data: Arc<Vec<u8>>,
    log: Arc<Mutex<Vec<Rec>>>,
    /// Some(target) 时所有请求都 302 到 target
    redirect: Option<String>,
}

async fn handler(State(ctx): State<Ctx>, headers: HeaderMap, uri: axum::http::Uri) -> Response {
    ctx.log.lock().unwrap().push(Rec {
        path: uri.path().to_string(),
        cookie: headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        auth: headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
    });

    if let Some(target) = &ctx.redirect {
        return (StatusCode::FOUND, [(header::LOCATION, target.clone())]).into_response();
    }

    let total = ctx.data.len() as u64;
    let range = headers
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("bytes="))
        .and_then(|s| s.split('-').next())
        .and_then(|s| s.parse::<u64>().ok());
    match range {
        Some(start) if start < total => {
            let end = total - 1;
            let body = ctx.data[start as usize..=end as usize].to_vec();
            (
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}")),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                ],
                Body::from(body),
            )
                .into_response()
        }
        _ => (
            StatusCode::OK,
            [
                (header::CONTENT_LENGTH, total.to_string()),
                (header::ACCEPT_RANGES, "bytes".to_string()),
            ],
            Body::from((*ctx.data).clone()),
        )
            .into_response(),
    }
}

async fn serve(data: Arc<Vec<u8>>, redirect: Option<String>) -> (u16, Arc<Mutex<Vec<Rec>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/file.bin", get(handler))
        .route("/redir", get(handler))
        .with_state(Ctx {
            data,
            log: log.clone(),
            redirect,
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, log)
}

async fn wait_done(engine: &Engine, id: &str) -> felldl_core::model::Task {
    for _ in 0..300 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let t = engine.get(id).unwrap();
        if t.state == TaskState::Completed || t.state == TaskState::Error {
            return t;
        }
    }
    panic!("任务在超时内未结束");
}

fn assert_no_creds(recs: &[Rec], who: &str) {
    assert!(!recs.is_empty(), "{who} 应至少收到一次请求");
    for r in recs {
        assert!(
            r.cookie.is_none(),
            "{who} 收到了 Cookie（{}）：{:?}",
            r.path,
            r.cookie
        );
        assert!(
            r.auth.is_none(),
            "{who} 收到了 Authorization（{}）：{:?}",
            r.path,
            r.auth
        );
    }
}

/// 镜像（不同端口 = 不同 origin）不得收到原站 Cookie
#[tokio::test]
async fn mirror_never_receives_origin_credentials() {
    let data: Vec<u8> = (0..256 * 1024u32).map(|i| (i % 251) as u8).collect();
    let data = Arc::new(data);

    let (main_port, main_log) = serve(data.clone(), None).await;
    let (mirror_port, mirror_log) = serve(data.clone(), None).await;

    let dir = std::env::temp_dir().join(format!("felldl-cred-mirror-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::new(&dir).unwrap();

    let id = engine
        .add(AddTaskRequest {
            url: format!("http://127.0.0.1:{main_port}/file.bin"),
            mirrors: vec![format!("http://127.0.0.1:{mirror_port}/file.bin")],
            save_dir: Some(dir.to_string_lossy().to_string()),
            connections: Some(6),
            cookie: Some(COOKIE_VALUE.to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let t = wait_done(&engine, &id).await;
    assert_eq!(t.state, TaskState::Completed, "任务出错: {:?}", t.error);

    let main_recs = main_log.lock().unwrap().clone();
    let mirror_recs = mirror_log.lock().unwrap().clone();
    assert!(
        main_recs
            .iter()
            .any(|r| r.cookie.as_deref() == Some(COOKIE_VALUE)),
        "主站应收到任务 Cookie，实际: {main_recs:?}"
    );
    assert_no_creds(&mirror_recs, "镜像站");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 跨 origin 重定向：落地端不得收到原站 Cookie，即便任务最终 URL 已换成它
#[tokio::test]
async fn cross_origin_redirect_drops_credentials() {
    let data: Vec<u8> = (0..192 * 1024u32).map(|i| (i % 239) as u8).collect();
    let data = Arc::new(data);

    // 先起落地端，才知道重定向目标
    let (target_port, target_log) = serve(data.clone(), None).await;
    let redirect_to = format!("http://127.0.0.1:{target_port}/file.bin");
    let (main_port, main_log) = serve(data.clone(), Some(redirect_to)).await;

    let dir = std::env::temp_dir().join(format!("felldl-cred-redir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::new(&dir).unwrap();

    let id = engine
        .add(AddTaskRequest {
            url: format!("http://127.0.0.1:{main_port}/redir"),
            save_dir: Some(dir.to_string_lossy().to_string()),
            connections: Some(4),
            cookie: Some(COOKIE_VALUE.to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let t = wait_done(&engine, &id).await;
    assert_eq!(t.state, TaskState::Completed, "任务出错: {:?}", t.error);
    assert!(
        t.url.contains(&format!("127.0.0.1:{target_port}")),
        "任务 URL 应记录重定向终址，实际 {}",
        t.url
    );

    let main_recs = main_log.lock().unwrap().clone();
    let target_recs = target_log.lock().unwrap().clone();
    assert!(
        main_recs
            .iter()
            .any(|r| r.cookie.as_deref() == Some(COOKIE_VALUE)),
        "原始 origin 应收到 Cookie，实际: {main_recs:?}"
    );
    assert_no_creds(&target_recs, "重定向落地端");

    let _ = std::fs::remove_dir_all(&dir);
}
