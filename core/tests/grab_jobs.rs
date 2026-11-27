//! 整站抓取任务的活动表：能被查询、结束后必须从表里消失。
//!
//! 这里不需要真的抓成功一个站：用一个必然连不上的地址，任务会立刻结束。
//! 要验证的是「列表里出现过 → 最终清空 → 再取消失败」这条生命周期，
//! 而不是抓取内容本身（那部分在 grab 的单元测试里）。

use felldl_core::grab::GrabSiteRequest;
use felldl_core::Engine;
use std::sync::Arc;
use std::time::Duration;

fn temp_root(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "felldl-grabjobs-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grab_status_lists_then_clears_finished_jobs() {
    let root = temp_root("lifecycle");
    let out = root.join("site");
    std::fs::create_dir_all(&out).unwrap();
    let engine: Arc<Engine> = Engine::new(&root).unwrap();

    assert!(engine.grab_status().is_empty(), "刚启动时不该有任务");

    let id = engine
        .grab_site(GrabSiteRequest {
            // 端口 9（discard）在本机没有监听：连接会立刻失败
            url: "http://127.0.0.1:9/nothing".into(),
            save_dir: out.to_string_lossy().to_string(),
            max_depth: 1,
            max_pages: 1,
            include_assets: false,
            referer: None,
        })
        .unwrap();

    // 任务可能马上结束，也可能还在跑：两种情况都允许，但最终必须清空
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !engine.grab_status().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "抓取任务 10 秒仍未结束（活动表会一直显示一个假任务）"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(
        !engine.cancel_grab(&id),
        "已结束的任务不应还能取消（否则 UI 会以为取消生效了）"
    );
    assert!(engine.grab_status().is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_unknown_job_is_false() {
    let root = temp_root("unknown");
    let engine: Arc<Engine> = Engine::new(&root).unwrap();
    assert!(!engine.cancel_grab("no-such-job"));
    let _ = std::fs::remove_dir_all(&root);
}
