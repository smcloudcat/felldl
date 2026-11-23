//! Felldl core — 多线程分段下载引擎
//!
//! 设计要点：
//! - 单文件多写入者：预先分配文件，每个分片用独立句柄 seek 到自己的偏移写入，
//!   因此**不需要下载完成后合并分片**，断点续传只需记录每段的 `done`。
//! - 分片进度用原子变量，控制面（暂停/取消）用 CancellationToken。
//! - 引擎与 UI 解耦：通过 broadcast 事件与快照结构对外，Tauri / CLI / HTTP API 共用。

pub mod api;
pub mod engine;
pub mod model;
pub mod store;
pub mod util;

pub use engine::Engine;
pub use model::*;

/// 引擎版本号
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
