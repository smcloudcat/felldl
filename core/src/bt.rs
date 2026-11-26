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
            resume_anchor: None,
        };
        let id = t.id.clone();
        let rt = Runtime::new(t.clone());
        *rt.running.lock() = true;
        self.tasks.lock().insert(id.clone(), rt);
        let _ = self.store.save_task(&t);
        self.emit(EngineEvent::TaskUpdated(t));
        Ok(id)
    }

    /// 把任务重新挂到 rqbit 会话上（首次添加与重启恢复共用）。
    ///
    /// `overwrite: true` 是必须的：任务已有部分数据时，rqbit 只有允许覆盖写入
    /// 才会接受 resume/做种；不加这个参数，重启后恢复会直接失败。
    async fn bt_attach(&self, bt: &BtManager, t: &Task, paused: bool) -> Result<()> {
        bt.session
            .add_torrent(
                AddTorrent::from_url(&t.url),
                Some(AddTorrentOptions {
                    output_folder: Some(t.save_dir.clone()),
                    overwrite: true,
                    paused,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| anyhow!("重新加入种子失败: {e}"))?;
        Ok(())
    }

    /// 进程重启后把持久化的 BT 任务重新挂回会话。
    ///
    /// rqbit 的会话是纯内存态：进程退出即消失，而任务记录还在 store 里。
    /// 不重新 add 的话，`bt_update_all` 拿不到 torrent handle 会直接跳过，
    /// 任务永远停在「暂停」且点恢复也没反应——用户看到的是「重启后 BT 全废了」。
    ///
    /// **每个种子单独 spawn，不能在这里 await**：`add_torrent` 对磁力链要等
    /// 元数据（无网络时会一直等），在这里等就会把整个后台 tick（进度广播、
    /// 状态轮询、并发泵）一起卡死。
    pub(crate) async fn bt_restore_pending(self: &Arc<Self>) {
        let mut pending: Vec<Task> = Vec::new();
        {
            let mut tried = self.bt_restore_tried.lock();
            let map = self.tasks.lock();
            for rt in map.values() {
                let t = rt.task.lock().clone();
                if t.kind != TaskKind::Bt || tried.contains(&t.id) {
                    continue;
                }
                if !matches!(
                    t.state,
                    TaskState::Paused
                        | TaskState::Downloading
                        | TaskState::Queued
                        | TaskState::Probing
                        | TaskState::Scheduled
                ) {
                    continue;
                }
                // 无论成败只尝试一次：失败写进任务错误里，由用户点重试
                tried.insert(t.id.clone());
                pending.push(t);
            }
        }
        if pending.is_empty() {
            return;
        }
        let me = self.clone();
        let bt = match self.bt().await {
            Ok(b) => b,
            // 会话起不来（目录不可写等）：保持原样，用户点恢复时会再次触发挂载
            Err(_) => {
                for t in pending {
                    self.bt_restore_tried.lock().remove(&t.id);
                }
                return;
            }
        };
        for t in pending {
            if t.bt_info_hash.is_none() {
                continue;
            }
            let me2 = me.clone();
            let bt2 = bt.clone();
            tokio::spawn(async move {
                if let Some(hash) = t.bt_info_hash.as_deref() {
                    if bt2.torrent(hash).is_some() {
                        if let Some(rt) = me2.tasks.lock().get(&t.id).cloned() {
                            *rt.running.lock() = t.state != TaskState::Paused;
                        }
                        return;
                    }
                }
                let paused = t.state == TaskState::Paused;
                match me2.bt_attach(&bt2, &t, paused).await {
                    Ok(()) => {
                        if let Some(rt) = me2.tasks.lock().get(&t.id).cloned() {
                            *rt.running.lock() = !paused;
                        }
                    }
                    Err(e) => {
                        // 允许用户点「恢复」时再试一次
                        me2.bt_restore_tried.lock().remove(&t.id);
                        me2.fail_task(&t.id, format!("{e:#}"));
                    }
                }
            });
        }
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
        let (hash, snapshot) = {
            let t = rt.task.lock();
            (t.bt_info_hash.clone(), t.clone())
        };
        if let (Some(hash), Ok(bt)) = (hash, self.bt().await) {
            if let Some(tor) = bt.torrent(&hash) {
                let _ = bt.session.unpause(&tor).await;
            } else {
                // 会话里没有（进程重启过 / 上次恢复失败）：重新挂载，让「恢复」真的起作用
                self.bt_restore_tried.lock().remove(&snapshot.id);
                self.bt_attach(&bt, &snapshot, false).await.ok();
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

#[cfg(test)]
mod restore_tests {
    use super::*;
    use crate::model::{AddTaskRequest, Task, TaskKind, TaskState};

    const INFO_HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "felldl-btr-{tag}-{}-{}",
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

    /// 造一条「上次进程里暂停着的 BT 任务」，写进 store
    fn seed_task(root: &std::path::Path, save_dir: &str) -> String {
        let store = crate::store::Store::new(root.to_path_buf()).unwrap();
        let task = Task {
            id: "bt-restore-0001".into(),
            url: format!("magnet:?xt=urn:btih:{INFO_HASH}&dn=restore-test.bin"),
            kind: TaskKind::Bt,
            mirrors: Vec::new(),
            file_name: "restore-test.bin".into(),
            save_dir: save_dir.into(),
            save_path: std::path::Path::new(save_dir)
                .join("restore-test.bin")
                .to_string_lossy()
                .to_string(),
            size: None,
            downloaded: 0,
            state: TaskState::Paused,
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
            bt_info_hash: Some(INFO_HASH.into()),
            category: None,
            username: None,
            password: None,
            interval_secs: 0,
            resume_anchor: None,
        };
        store.save_task(&task).unwrap();
        task.id
    }

    /// 恢复动作必须「立即返回」：磁力元数据可能永远等不到，不能卡住后台循环。
    ///
    /// 早先直接在后台 tick 里 `await add_torrent`，一个无 peer 的磁力链就能让
    /// 整个循环（进度广播 / 状态轮询 / 并发泵）停摆——所有任务一起「假死」。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn restore_does_not_block_on_metadata() {
        let root = temp_root("nb");
        let out = root.join("downloads");
        std::fs::create_dir_all(&out).unwrap();
        let id = seed_task(&root, &out.to_string_lossy());

        let engine = Engine::new(&root).unwrap();
        assert_eq!(engine.get(&id).unwrap().state, TaskState::Paused);

        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            engine.bt_restore_pending(),
        )
        .await
        .expect("bt_restore_pending 卡在元数据获取上，会把整个后台循环拖死");

        assert!(
            engine.bt_restore_tried.lock().contains(&id),
            "恢复没有发起：任务没被记入 attempted 集合"
        );
        let t = engine.get(&id).unwrap();
        assert_eq!(t.state, TaskState::Paused, "恢复不应擅自改变暂停状态");
        assert!(t.error.is_none(), "恢复不应写错误: {:?}", t.error);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 只尝试一次：失败后不能每 500ms 重来一遍 add_torrent
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn restore_is_attempted_only_once() {
        let root = temp_root("once");
        let out = root.join("downloads");
        std::fs::create_dir_all(&out).unwrap();
        let id = seed_task(&root, &out.to_string_lossy());
        let engine = Engine::new(&root).unwrap();

        for _ in 0..3 {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                engine.bt_restore_pending(),
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let tried = engine.bt_restore_tried.lock().len();
        assert_eq!(tried, 1, "尝试标记应只记一次，实际 {tried} 条");
        assert!(engine.bt_restore_tried.lock().contains(&id));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// BT 恢复进行中，普通 HTTP 任务必须照常推进（证明后台循环没被拖住）
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn http_tasks_keep_running_while_bt_restore_pending() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let body = Arc::new(vec![7u8; 256 * 1024]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = body.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let b = served.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: none\r\nConnection: close\r\n\r\n",
                        b.len()
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(&b).await;
                    let _ = sock.flush().await;
                });
            }
        });

        let root = temp_root("mix");
        let out = root.join("downloads");
        std::fs::create_dir_all(&out).unwrap();
        let _bt = seed_task(&root, &out.to_string_lossy());
        let engine = Engine::new(&root).unwrap();

        let http_id = engine
            .add(AddTaskRequest {
                url: format!("http://{addr}/f.bin"),
                save_dir: Some(out.to_string_lossy().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        let start = std::time::Instant::now();
        loop {
            let t = engine.get(&http_id).unwrap();
            if t.state == TaskState::Completed {
                break;
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(30),
                "HTTP 任务在 BT 恢复期间没有完成（后台循环被拖住了？）当前 {:?}",
                t.state
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let _ = std::fs::remove_dir_all(&root);
    }
}
