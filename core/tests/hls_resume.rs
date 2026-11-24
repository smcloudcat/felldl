//! HLS 暂停 → 恢复：恢复后必须继续走分片合并（而不是把 m3u8 播放列表当文件下载）。

use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use felldl_core::engine::Engine;
use felldl_core::model::TaskState;
use std::sync::Arc;

/// 3 个 64KB 分片，每片延迟 400ms（给暂停留出时间窗口）
#[tokio::test]
async fn hls_pause_then_resume_completes() {
    let segs: Arc<Vec<Vec<u8>>> = Arc::new(
        (0..3u8)
            .map(|i| (0..64 * 1024u32).map(|j| ((i as u32 * 31 + j) % 253) as u8).collect())
            .collect(),
    );

    async fn segment(
        axum::extract::Path(name): axum::extract::Path<String>,
        State((segs, playlist)): State<(Arc<Vec<Vec<u8>>>, Arc<String>)>,
    ) -> Response {
        if name == "index.m3u8" {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
                Body::from((*playlist).clone()),
            )
                .into_response();
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let i = name
            .trim_start_matches("seg")
            .trim_end_matches(".ts")
            .parse::<usize>()
            .unwrap_or(usize::MAX);
        let body = segs.get(i).cloned().unwrap_or_default();
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "video/mp2t")],
            Body::from(body),
        )
            .into_response()
    }

    let mut playlist = String::from("#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:1
");
    for i in 0..segs.len() {
        playlist.push_str(&format!("#EXTINF:1.0,
seg{i}.ts
"));
    }
    playlist.push_str("#EXT-X-ENDLIST
");

    let app = Router::new()
        .route("/{file}", get(segment))
        .with_state((segs.clone(), Arc::new(playlist)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = std::env::temp_dir().join(format!("felldl-hls-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let engine = Engine::new(&dir).unwrap();

    let id = engine
        .download_hls(
            &format!("http://127.0.0.1:{port}/index.m3u8"),
            Some(dir.to_string_lossy().to_string()),
            Some("out.ts".into()),
        )
        .await
        .unwrap();

    // 下载到一半时暂停
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    engine.pause(&id).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let paused = engine.get(&id).unwrap();
    assert_eq!(paused.state, TaskState::Paused, "暂停后应处于 Paused");

    // 恢复：此前会退化成「把 m3u8 当普通文件下载」，现在必须回到分片 runner
    engine.resume(&id).await.unwrap();
    let mut final_task = None;
    for _ in 0..300 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let t = engine.get(&id).unwrap();
        if t.state == TaskState::Completed || t.state == TaskState::Error {
            final_task = Some(t);
            break;
        }
    }
    let t = final_task.expect("恢复后未在超时内结束");
    assert_eq!(t.state, TaskState::Completed, "任务出错: {:?}", t.error);

    let expected: Vec<u8> = segs.iter().flat_map(|s| s.clone()).collect();
    let got = std::fs::read(dir.join("out.ts")).unwrap();
    assert_eq!(got.len(), expected.len(), "合并后长度不符（可能下成了播放列表）");
    assert!(got == expected, "分片合并内容与源不一致");
    assert_eq!(t.downloaded as usize, expected.len());

    let _ = std::fs::remove_dir_all(&dir);
}
