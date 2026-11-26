//! BitTorrent 集成：基于 librqbit（DHT / HTTP+UDP tracker / magnet / PEX 全支持）
//!
//! 职责：
//! * 惰性初始化 rqbit 会话（首次添加磁力时）
//! * `bt_add`：创建 BT 类型任务并加入会话
//! * `bt_update_all`：轮询会话状态，同步进度到任务模型
//! * 暂停 / 恢复 / 删除任务时联动会话

use crate::engine::{Engine, Runtime};
use crate::model::*;
use anyhow::{anyhow, bail, Context, Result};
use librqbit::api::TorrentIdOrHash;
use librqbit::{AddTorrent, AddTorrentOptions, Session, SessionOptions};
use librqbit_core::hash_id::Id;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// hex(40) → info_hash 字节数组
pub(crate) fn hex_to_20(s: &str) -> Option<[u8; 20]> {
    if s.len() < 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

pub struct BtManager {
    pub session: Arc<Session>,
}

impl BtManager {
    pub async fn new(download_dir: &str) -> Result<Self> {
        let session = Session::new_with_opts(
            std::path::PathBuf::from(download_dir),
            SessionOptions::default(),
        )
        .await
        .context("初始化 BitTorrent 会话失败")?;
        Ok(Self { session })
    }

    fn torrent(&self, info_hash_hex: &str) -> Option<Arc<librqbit::ManagedTorrent>> {
        let bytes = hex_to_20(info_hash_hex)?;
        self.session.get(TorrentIdOrHash::Hash(Id(bytes)))
    }
}

impl Engine {
    /// 获取（或惰性创建）BT 会话
    pub async fn bt(&self) -> Result<Arc<BtManager>> {
        let dir = self.settings().download_dir.clone();
        self.bt_session
            .get_or_try_init(|| async { BtManager::new(&dir).await.map(Arc::new) })
            .await
            .map(|a| a.clone())
            .map_err(|e| anyhow!("{e:#}"))
    }

    /// 添加磁力 / 种子任务（.torrent URL 也交给 rqbit）
    pub(crate) async fn bt_add(
        self: &Arc<Self>,
        url: &str,
        req: &AddTaskRequest,
    ) -> Result<TaskId> {
        if !url.starts_with("magnet:") && !url.starts_with("http") && !url.starts_with("https") {
            bail!("BT 任务需要 magnet: 链接或 .torrent 文件地址");
        }
        let s = self.settings();
        let save_dir = req
            .save_dir
            .clone()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| s.download_dir.clone());
        std::fs::create_dir_all(&save_dir).ok();

        let bt = self.bt().await?;
        let handle = bt
            .session
            .add_torrent(
                AddTorrent::from_url(url),
                Some(AddTorrentOptions {
                    output_folder: Some(save_dir.clone()),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| anyhow!("添加种子失败: {e}"))?
            .into_handle()
            .ok_or_else(|| anyhow!("种子已在任务列表中"))?;

        let info_hash = hex_string(&handle.info_hash().0);
        // metadata 可能还没有（磁力需从 peer 获取），先给个占位名
        let name = handle.name().unwrap_or_else(|| {
            url.split("dn=")
                .nth(1)
                .map(|n| n.split('&').next().unwrap_or("BT 任务").to_string())
                .unwrap_or_else(|| format!("BT {}", &info_hash[..8.min(info_hash.len())]))
        });
        let folder = handle
            .output_folder()
            .to_string_lossy()
            .to_string();
        // 顶层名称当作“单个路径组件”处理：sanitize_name 会替换 / \ : 等分隔符、
        // 去掉首尾点（含 ".."）、处理 Windows 保留名，避免种子名把路径带出 save_dir。
        let safe_name = crate::util::sanitize_name(&name);
        let path = std::path::Path::new(&folder).join(&safe_name);
        let display = path.to_string_lossy().to_string();

        let t = Task {
            id: uuid::Uuid::new_v4().to_string(),
            url: url.to_string(),
            kind: TaskKind::Bt,
            mirrors: Vec::new(),
            file_name: safe_name,
            save_dir: folder,
            save_path: display,
            size: None,
            downloaded: 0,
            state: TaskState::Downloading,
            error: None,
            connections: 0,
            segments: Vec::new(),
            supports_range: false,
            etag: None,
            last_modified: None,
            content_type: None,
            referer: None,
            cookie: None,
            auth_origin: String::new(),
            user_agent: None,
            speed_limit: 0,
            scheduled_at: None,
            created_at: now_ms(),
            completed_at: None,
            speed: 0,
            peers: 0,
            upload_speed: 0,
            bt_info_hash: Some(info_hash),
            category: None,
            username: None,
            password: None,
            interval_secs: 0,
        };
        let id = t.id.clone();
        let rt = Runtime::new(t.clone());
        *rt.running.lock() = true;
        self.tasks.lock().insert(id.clone(), rt);
        let _ = self.store.save_task(&t);
        self.emit(EngineEvent::TaskUpdated(t));
        Ok(id)
    }

    /// 暂停 BT 任务
    pub(crate) async fn bt_pause(&self, rt: &Arc<Runtime>) {
        let hash = { rt.task.lock().bt_info_hash.clone() };
        if let (Some(hash), Ok(bt)) = (hash, self.bt().await) {
            if let Some(tor) = bt.torrent(&hash) {
                let _ = bt.session.pause(&tor).await;
            }
        }
        *rt.running.lock() = false;
        let mut t = rt.task.lock();
        t.state = TaskState::Paused;
        t.speed = 0;
        t.upload_speed = 0;
        let snap = t.clone();
        drop(t);
        let _ = self.store.save_task(&snap);
        self.emit(EngineEvent::TaskUpdated(snap));
    }

    /// 恢复 BT 任务
    pub(crate) async fn bt_resume(&self, rt: &Arc<Runtime>) {
        let hash = { rt.task.lock().bt_info_hash.clone() };
        if let (Some(hash), Ok(bt)) = (hash, self.bt().await) {
            if let Some(tor) = bt.torrent(&hash) {
                let _ = bt.session.unpause(&tor).await;
            }
        }
        *rt.running.lock() = true;
        let mut t = rt.task.lock();
        t.state = TaskState::Downloading;
        t.error = None;
        let snap = t.clone();
        drop(t);
        self.emit(EngineEvent::TaskUpdated(snap));
    }

    /// 删除 BT 任务（从会话中移除，可选删文件）
    pub(crate) async fn bt_remove(&self, rt: &Arc<Runtime>, delete_files: bool) {
        let hash = { rt.task.lock().bt_info_hash.clone() };
        if let (Some(hash), Ok(bt)) = (hash, self.bt().await) {
            if let Some(tor) = bt.torrent(&hash) {
                let _ = bt
                    .session
                    .delete(TorrentIdOrHash::Id(tor.id()), delete_files)
                    .await;
            }
        }
    }

    /// 轮询全部 BT 任务状态（由后台循环每秒调用）
    pub fn bt_update_all(&self) {
        // 会话尚未创建 → 没有 BT 任务
        let Some(bt) = self.bt_session.get() else {
            return;
        };
        // 收集所有 BT 任务快照
        let bt_tasks: Vec<(String, Arc<Runtime>)> = {
            let map = self.tasks.lock();
            map.iter()
                .filter(|(_, r)| r.task.lock().kind == TaskKind::Bt)
                .map(|(k, r)| (k.clone(), r.clone()))
                .collect()
        };
        if bt_tasks.is_empty() {
            return;
        }
        for (id, rt) in bt_tasks {
            let hash = { rt.task.lock().bt_info_hash.clone() };
            let Some(hash) = hash else { continue };
            let Some(tor) = bt.torrent(&hash) else { continue };
            let stats = tor.stats();
            let name = tor.name();
            let total = stats.total_bytes;
            let done = stats.progress_bytes;
            let paused = tor.is_paused();

            // 速度与 peers：直接读 live 统计（rqbit 已维护滑动窗口）
            let (down_rate, up_rate, peers) = stats
                .live
                .as_ref()
                .map(|l| (
                    l.download_speed.as_bytes(),
                    l.upload_speed.as_bytes(),
                    l.snapshot.peer_stats.live,
                ))
                .unwrap_or((0, 0, 0));

            let mut t = rt.task.lock();
            // metadata 到手后补全文件名与大小
            if let Some(n) = name {
                if t.size.is_none() && total > 0 {
                    t.file_name = crate::util::sanitize_name(&n);
                    let folder = tor.output_folder().to_string_lossy().to_string();
                    t.save_dir = folder.clone();
                    t.save_path = std::path::Path::new(&folder)
                        .join(&t.file_name)
                        .to_string_lossy()
                        .to_string();
                }
            }
            t.size = if total > 0 { Some(total) } else { None };
            t.downloaded = done;
            t.speed = down_rate;
            t.upload_speed = up_rate;
            t.peers = peers as u32;
            t.error = stats.error.clone();
            let was_running = *rt.running.lock();
            if paused {
                if t.state.is_active() || t.state == TaskState::Queued {
                    t.state = TaskState::Paused;
                    t.speed = 0;
                    *rt.running.lock() = false;
                }
            } else if stats.error.is_some() {
                t.state = TaskState::Error;
                *rt.running.lock() = false;
            } else if stats.finished && total > 0 && done >= total {
                if t.state != TaskState::Completed {
                    t.state = TaskState::Completed;
                    t.completed_at = Some(now_ms());
                    *rt.running.lock() = false;
                }
            } else {
                if t.state == TaskState::Probing || t.state == TaskState::Queued || t.state == TaskState::Scheduled {
                    t.state = TaskState::Downloading;
                }
                *rt.running.lock() = true;
            }
            let snap = t.clone();
            drop(t);
            rt.downloaded.store(done, Ordering::Relaxed);
            // 状态变化立即落盘；纯进度推进走合并队列（与后台 tick、其他协议去重）
            if was_running != *rt.running.lock() {
                let _ = self.store.save_task(&snap);
            } else if snap.state.is_active() {
                self.store.enqueue_save(&snap);
            }
            if was_running != *rt.running.lock() || snap.state.is_active() {
                self.emit(EngineEvent::TaskUpdated(snap));
            }
            let _ = id;
        }
    }
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
