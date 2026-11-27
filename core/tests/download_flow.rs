//! 端到端测试：多镜像同时下载 + 断点续传
//!
//! 用 axum 起两个本地服务器（同一文件的两个"镜像站"），
//! 通过引擎添加带镜像的任务，验证最终文件与源一致。

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use felldl_core::engine::Engine;
use felldl_core::model::{AddTaskRequest, TaskState};
use std::sync::Arc;

#[tokio::test]
async fn mirror_download_produces_identical_file() {
    // 1MB 源文件（伪随机，保证可校验）
    let data: Vec<u8> = (0..1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let data = Arc::new(data);

    async fn handler(State(data): State<Arc<Vec<u8>>>, headers: HeaderMap) -> Response {
        let total = data.len() as u64;
        // 手动支持 Range（含多线程分段请求）
        let range = headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("bytes="))
            .and_then(|s| s.split('-').collect::<Vec<_>>()[0].parse::<u64>().ok());
        match range {
            Some(start) if start < total => {
                let end = total - 1;
                let body = data[start as usize..=end as usize].to_vec();
                (
                    StatusCode::PARTIAL_CONTENT,
                    [
                        (
                            header::CONTENT_RANGE,
                            format!("bytes {start}-{end}/{total}"),
                        ),
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
                Body::from((*data).clone()),
            )
                .into_response(),
        }
    }

    async fn serve(data: Arc<Vec<u8>>) -> u16 {
        let app = Router::new()
            .route("/file.bin", get(handler))
            .with_state(data);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        port
    }

    let p1 = serve(data.clone()).await;
    let p2 = serve(data.clone()).await;

    // 引擎
    let dir = std::env::temp_dir().join(format!("felldl-mirror-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::new(&dir).unwrap();

    let task_id = engine
        .add(AddTaskRequest {
            url: format!("http://127.0.0.1:{p1}/file.bin"),
            mirrors: vec![format!("http://127.0.0.1:{p2}/file.bin")],
            save_dir: Some(dir.to_string_lossy().to_string()),
            connections: Some(8),
            ..Default::default()
        })
        .await
        .unwrap();

    // 等待完成（最多 30s）
    let mut final_task = None;
    for _ in 0..300 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let t = engine.get(&task_id).unwrap();
        if t.state == TaskState::Completed || t.state == TaskState::Error {
            final_task = Some(t);
            break;
        }
    }
    let t = final_task.expect("任务在超时内未结束");
    assert_eq!(t.state, TaskState::Completed, "任务出错: {:?}", t.error);
    assert_eq!(t.mirrors.len(), 1, "镜像应被保留（大小一致）");
    assert_eq!(t.size, Some(1024 * 1024));
    assert_eq!(t.downloaded, 1024 * 1024);

    // 文件内容与源一致
    let got = std::fs::read(dir.join("file.bin")).unwrap();
    assert_eq!(got.len(), 1024 * 1024);
    assert!(got == *data, "下载文件与源不一致（镜像/分段拼接错误）");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 不带镜像的普通多线程下载也应正确
#[tokio::test]
async fn plain_download_ok() {
    let data: Vec<u8> = (0..512 * 1024u32).map(|i| (i * 7 % 256) as u8).collect();
    let data = Arc::new(data);

    async fn handler(State(data): State<Arc<Vec<u8>>>, headers: HeaderMap) -> Response {
        let total = data.len() as u64;
        let range = headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("bytes="))
            .and_then(|s| s.split('-').collect::<Vec<_>>()[0].parse::<u64>().ok());
        match range {
            Some(start) if start < total => {
                let end = total - 1;
                let body = data[start as usize..=end as usize].to_vec();
                (
                    StatusCode::PARTIAL_CONTENT,
                    [
                        (
                            header::CONTENT_RANGE,
                            format!("bytes {start}-{end}/{total}"),
                        ),
                        (header::ACCEPT_RANGES, "bytes".to_string()),
                    ],
                    Body::from(body),
                )
                    .into_response()
            }
            _ => (
                StatusCode::OK,
                [(header::CONTENT_LENGTH, total.to_string())],
                Body::from((*data).clone()),
            )
                .into_response(),
        }
    }

    let app = Router::new()
        .route("/f.bin", get(handler))
        .with_state(data.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = std::env::temp_dir().join(format!("felldl-plain-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::new(&dir).unwrap();

    let task_id = engine
        .add(AddTaskRequest {
            url: format!("http://127.0.0.1:{port}/f.bin"),
            save_dir: Some(dir.to_string_lossy().to_string()),
            connections: Some(4),
            mirrors: Vec::new(),
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

    let got = std::fs::read(dir.join("f.bin")).unwrap();
    assert_eq!(got, *data);

    let _ = std::fs::remove_dir_all(&dir);
}
