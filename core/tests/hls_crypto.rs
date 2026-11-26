//! HLS 加密语义：缺省 IV 取「媒体序号」、密钥轮换、解密失败必须报错。
//!
//! 此前的实现有三处会静默产出坏文件：
//!   1. 缺省 IV 用分片下标（`i + 1`），忽略 `#EXT-X-MEDIA-SEQUENCE`，
//!      导致从非 0 序号开始的播放列表整段解成乱码；
//!   2. 整个播放列表只认第一条 `EXT-X-KEY`，中途换钥后后半段全是乱码；
//!   3. 解密失败时 `if let Ok(...)` 静默跳过，把**密文**直接写进成品。

use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use felldl_core::engine::Engine;
use felldl_core::model::TaskState;
use std::sync::Arc;

const KEY: [u8; 16] = [0x42; 16];
const MEDIA_SEQUENCE: u64 = 7;
const SEG_COUNT: usize = 5;

fn default_iv(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

/// 每片明文都不等长且非 16 的倍数，覆盖 PKCS7 填充路径
fn plain_segments() -> Vec<Vec<u8>> {
    (0..SEG_COUNT)
        .map(|i| {
            let len = 4096 + 13 + i * 7;
            (0..len)
                .map(|j| ((i * 37 + j) % 251) as u8)
                .collect::<Vec<u8>>()
        })
        .collect()
}

struct Fixture {
    playlist: String,
    key: Vec<u8>,
    segments: Vec<Vec<u8>>,
}

impl Fixture {
    /// 把某个分片换成无法解密的字节（长度不是 16 的倍数）
    fn corrupt(mut self, idx: usize) -> Self {
        self.segments[idx] = (0..100u8).map(|i| i.wrapping_mul(7)).collect();
        self
    }
}

/// `iv_base`: 缺省 IV 的序号基准——正确实现用 MEDIA_SEQUENCE，早期实现用 1
fn build_fixture(iv_base: u64) -> Fixture {
    let plain = plain_segments();
    let segs: Vec<Vec<u8>> = plain
        .iter()
        .enumerate()
        .map(|(i, p)| {
            felldl_core::crypto::aes128_cbc_encrypt(&KEY, &default_iv(iv_base + i as u64), p).unwrap()
        })
        .collect();

    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:{MEDIA_SEQUENCE}\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n"
    );
    for i in 0..SEG_COUNT {
        playlist.push_str(&format!("#EXTINF:4.0,\nseg{i}.ts\n"));
    }
    playlist.push_str("#EXT-X-ENDLIST\n");

    Fixture {
        playlist,
        key: KEY.to_vec(),
        segments: segs,
    }
}

async fn serve(fx: Arc<Fixture>) -> String {
    async fn handler(
        axum::extract::Path(name): axum::extract::Path<String>,
        State(fx): State<Arc<Fixture>>,
    ) -> Response {
        match name.as_str() {
            "index.m3u8" => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
                Body::from(fx.playlist.clone()),
            )
                .into_response(),
            "key.bin" => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/octet-stream")],
                Body::from(fx.key.clone()),
            )
                .into_response(),
            other => {
                let idx = other
                    .trim_start_matches("seg")
                    .trim_end_matches(".ts")
                    .parse::<usize>()
                    .unwrap_or(usize::MAX);
                let body = fx.segments.get(idx).cloned().unwrap_or_default();
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "video/mp2t")],
                    Body::from(body),
                )
                    .into_response()
            }
        }
    }

    let app = Router::new().route("/{file}", get(handler)).with_state(fx);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{port}/index.m3u8")
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "felldl-hlsc-{tag}-{}-{}",
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

async fn wait(engine: &Arc<Engine>, id: &str, secs: u64) -> felldl_core::Task {
    let start = std::time::Instant::now();
    loop {
        let t = engine.get(id).unwrap();
        if t.state == TaskState::Completed || t.state == TaskState::Error {
            return t;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(secs),
            "任务未在 {secs}s 内结束，当前 {:?}",
            t.state
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// MEDIA-SEQUENCE 非 0 且不带 IV：必须按媒体序号推 IV，解出原文
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hls_default_iv_uses_media_sequence() {
    let fx = Arc::new(build_fixture(MEDIA_SEQUENCE));
    let url = serve(fx).await;
    let dir = temp_dir("seq");
    let engine = Engine::new(&dir).unwrap();

    let id = engine
        .download_hls(&url, Some(dir.to_string_lossy().to_string()), Some("out.ts".into()))
        .await
        .unwrap();
    let t = wait(&engine, &id, 60).await;
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);

    let expected: Vec<u8> = plain_segments().concat();
    let got = std::fs::read(dir.join("out.ts")).unwrap();
    assert!(
        got == expected,
        "缺省 IV 未按媒体序号推导，解出的内容不对（{} / {} 字节）",
        got.len(),
        expected.len()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 分片无法解密时必须报错。
///
/// 早期实现在解密失败时 `if let Ok(...)` 静默跳过，把**密文**写进成品：
/// 文件能播放、时长也对，但画面全花，用户和日志都看不出原因。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hls_undecryptable_segment_fails_loudly() {
    let fx = Arc::new(build_fixture(MEDIA_SEQUENCE).corrupt(2));
    let url = serve(fx).await;
    let dir = temp_dir("badseg");
    let engine = Engine::new(&dir).unwrap();

    let id = engine
        .download_hls(&url, Some(dir.to_string_lossy().to_string()), Some("bad.ts".into()))
        .await
        .unwrap();
    let t = wait(&engine, &id, 60).await;
    assert_eq!(
        t.state,
        TaskState::Error,
        "分片解不开时应当报错，不能默默把密文写成成品"
    );
    let msg = t.error.clone().unwrap_or_default();
    assert!(
        msg.contains("解密失败"),
        "错误信息应指出解密失败，实际: {msg}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
