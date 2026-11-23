//! 下载引擎：探测 → 分段 → 并发拉取 → 落盘 → 事件广播
//!
//! 关键设计：
//! * 文件**预先 set_len 分配**，各分片用独立文件句柄定位写入（pwrite），
//!   因此无需下载完成后合并，也没有额外的临时分片文件。
//! * 每个分片的 `done` 持久化在任务 JSON 里，进程重启后从 `start + done` 继续。
//! * 用 `If-Range` + ETag/Last-Modified 校验，服务器文件变了就整体重下，避免拼出坏文件。

use crate::model::*;
use crate::store::Store;
use crate::util::{self, RateLimiter};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use parking_lot::{Mutex, RwLock};
use reqwest::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, COOKIE, ETAG,
    LAST_MODIFIED, RANGE, REFERER, USER_AGENT,
};
use reqwest::{Client, StatusCode};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// 兜底 UA：被网站反盗链拒绝浏览器 UA 时使用
const FALLBACK_UA: &str = "felldl/0.1 (+https://github.com/felldl)";

pub struct Engine {
    store: Store,
    settings: RwLock<Settings>,
    tasks: Mutex<HashMap<TaskId, Arc<Runtime>>>,
    events: broadcast::Sender<EngineEvent>,
    /// 全局限速器
    global_limiter: Arc<RateLimiter>,
    pub http_token: RwLock<String>,
}

struct Runtime {
    task: Mutex<Task>,
    /// 实时已下载字节（比 task.downloaded 更新更频繁）
    downloaded: AtomicU64,
    /// 每次启动下载都换新 token，因此暂停 → 恢复 → 再暂停都能正确中断
    cancel: Mutex<CancellationToken>,
    /// 本次会话内是否正在运行
    running: Mutex<bool>,
}

impl Runtime {
    fn new(task: Task) -> Arc<Self> {
        Arc::new(Self {
            downloaded: AtomicU64::new(task.downloaded),
            cancel: Mutex::new(CancellationToken::new()),
            running: Mutex::new(false),
            task: Mutex::new(task),
        })
    }
}

// ───────────────────────── 平台相关的定位写入 ─────────────────────────

#[cfg(windows)]
fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut written = 0usize;
    while written < buf.len() {
        let n = file.seek_write(&buf[written..], offset + written as u64)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "写入返回 0"));
        }
        written += n;
    }
    Ok(())
}

#[cfg(unix)]
fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    let mut written = 0usize;
    while written < buf.len() {
        let n = file.write_at(&buf[written..], offset + written as u64)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "写入返回 0"));
        }
        written += n;
    }
    Ok(())
}

// ───────────────────────── 探测结果 ─────────────────────────

#[derive(Debug, Default, Clone)]
struct ProbeInfo {
    size: Option<u64>,
    supports_range: bool,
    file_name: Option<String>,
    content_type: Option<String>,
    etag: Option<String>,
    last_modified: Option<String>,
    final_url: String,
    /// 探测成功时使用的 UA（下载阶段沿用，避免同一站点再次被拒）
    ua: Option<String>,
}

impl Engine {
    pub fn new(store_root: impl Into<std::path::PathBuf>) -> Result<Arc<Self>> {
        let store = Store::new(store_root.into()).context("初始化数据目录失败")?;
        let mut settings = store.load_settings();
        if settings.api_token.trim().is_empty() {
            settings.api_token = uuid::Uuid::new_v4().simple().to_string();
            let _ = store.save_settings(&settings);
        }
        let (tx, _) = broadcast::channel(512);
        let limiter = Arc::new(RateLimiter::new(settings.global_speed_limit));
        let token = settings.api_token.clone();

        let engine = Arc::new(Self {
            store,
            settings: RwLock::new(settings),
            tasks: Mutex::new(HashMap::new()),
            events: tx,
            global_limiter: limiter,
            http_token: RwLock::new(token),
        });

        // 载入历史任务（进行中的已回落为 Paused）
        let loaded = engine.store.load_tasks();
        {
            let mut map = engine.tasks.lock();
            for t in loaded {
                let id = t.id.clone();
                map.insert(id, Runtime::new(t));
            }
        }
        // 后台循环负责：速度统计、进度广播、定时触发、并发泵
        engine.spawn_background();
        Ok(engine)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events.subscribe()
    }

    fn emit(&self, ev: EngineEvent) {
        let _ = self.events.send(ev);
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }

    /// 更新设置：同步限速器、落盘、广播
    pub fn update_settings(&self, mut s: Settings) -> Result<()> {
        if s.api_token.trim().is_empty() {
            s.api_token = uuid::Uuid::new_v4().simple().to_string();
        }
        s.max_concurrent_tasks = s.max_concurrent_tasks.clamp(1, 16);
        s.default_connections = s.default_connections.clamp(1, 64);
        *self.http_token.write() = s.api_token.clone();
        self.global_limiter.set_limit(s.global_speed_limit);
        let _ = std::fs::create_dir_all(&s.download_dir);
        self.store.save_settings(&s).context("保存设置失败")?;
        *self.settings.write() = s.clone();
        self.emit(EngineEvent::SettingsUpdated(s));
        Ok(())
    }

    /// 修改单个任务的限速（0 = 跟随全局）
    pub fn set_task_speed_limit(&self, id: &str, limit: u64) -> Result<()> {
        let Some(rt) = self.tasks.lock().get(id).cloned() else {
            bail!("任务不存在");
        };
        let mut t = rt.task.lock();
        t.speed_limit = limit;
        let snap = t.clone();
        drop(t);
        self.store.save_task(&snap)?;
        self.emit(EngineEvent::TaskUpdated(snap));
        Ok(())
    }

    /// 调整任务连接数（仅对未开始或已暂停的任务生效）
    pub fn set_task_connections(&self, id: &str, conns: usize) -> Result<()> {
        let Some(rt) = self.tasks.lock().get(id).cloned() else {
            bail!("任务不存在");
        };
        let mut t = rt.task.lock();
        if t.state.is_active() {
            bail!("任务运行中，请先暂停再调整连接数");
        }
        t.connections = conns.clamp(1, 64);
        // 重置分片，下次按新连接数重新规划（已下字节仍在文件里，会从头重下）
        t.segments.clear();
        let snap = t.clone();
        drop(t);
        self.store.save_task(&snap)?;
        self.emit(EngineEvent::TaskUpdated(snap));
        Ok(())
    }

    pub fn snapshot(&self) -> Vec<Task> {
        let map = self.tasks.lock();
        let mut v: Vec<Task> = map
            .values()
            .map(|r| {
                let mut t = r.task.lock().clone();
                t.downloaded = r.downloaded.load(Ordering::Relaxed);
                t
            })
            .collect();
        v.sort_by_key(|t| std::cmp::Reverse(t.created_at));
        v
    }

    pub fn get(&self, id: &str) -> Option<Task> {
        let map = self.tasks.lock();
        map.get(id).map(|r| {
            let mut t = r.task.lock().clone();
            t.downloaded = r.downloaded.load(Ordering::Relaxed);
            t
        })
    }

    fn client(&self, task: Option<&Task>) -> Result<Client> {
        let ua = self.settings.read().user_agent.clone();
        self.client_with_ua(task, &ua)
    }

    fn client_with_ua(&self, task: Option<&Task>, ua: &str) -> Result<Client> {
        let s = self.settings.read().clone();
        let mut b = Client::builder()
            .user_agent(ua)
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(3600))
            .pool_max_idle_per_host(64)
            // 不少下载站 / 镜像站的证书链不完整，这里保留校验但允许常见兼容
            .redirect(reqwest::redirect::Policy::limited(10));
        if let Some(url) = s.proxy.to_url() {
            let p = reqwest::Proxy::all(&url).with_context(|| format!("代理地址无效: {url}"))?;
            b = b.proxy(p);
        }
        if let Some(t) = task {
            if let Some(ref r) = t.referer {
                if let Ok(v) = reqwest::header::HeaderValue::from_str(r) {
                    let mut hm = reqwest::header::HeaderMap::new();
                    hm.insert(REFERER, v);
                    b = b.default_headers(hm);
                }
            }
        }
        Ok(b.build()?)
    }

    // ───────────────────────── 添加任务 ─────────────────────────

    /// 添加任务并返回任务 ID。会先做一次探测以获取大小与是否支持分段。
    pub async fn add(self: &Arc<Self>, req: AddTaskRequest) -> Result<TaskId> {
        let url = req.url.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            bail!("仅支持 http/https 链接");
        }
        let s = self.settings();

        let probe = match self.probe(&url, req.referer.as_deref(), req.cookie.as_deref()).await {
            Ok(p) => p,
            Err(e) => {
                // 探测失败也建任务，标记错误便于用户重试
                let mut t = self.build_task(&req, ProbeInfo::default(), &s);
                t.state = TaskState::Error;
                t.error = Some(format!("{e:#}"));
                self.insert_task(t.clone());
                return Ok(t.id);
            }
        };

        let mut t = self.build_task(&req, probe, &s);
        let scheduled = req.scheduled_at.filter(|&ms| ms > now_ms());
        if scheduled.is_some() {
            t.state = TaskState::Scheduled;
            t.scheduled_at = scheduled;
        }
        self.insert_task(t.clone());
        if scheduled.is_none() && s.auto_start {
            let _ = self.resume(&t.id).await;
        }
        Ok(t.id)
    }

    fn build_task(&self, req: &AddTaskRequest, probe: ProbeInfo, s: &Settings) -> Task {
        let save_dir = req
            .save_dir
            .clone()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| s.download_dir.clone());
        let _ = std::fs::create_dir_all(&save_dir);

        let raw_name = req
            .file_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| probe.file_name.clone())
            .or_else(|| util::filename_from_url(&probe.final_url))
            .or_else(|| util::filename_from_url(&req.url))
            .unwrap_or_else(|| "download".into());
        let name = util::ensure_extension(
            &util::sanitize_name(&raw_name),
            probe.content_type.as_deref(),
        );
        let path = util::unique_path(Path::new(&save_dir), &name);
        let final_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or(name);

        Task {
            id: uuid::Uuid::new_v4().to_string(),
            url: probe.final_url.clone().if_empty_then(req.url.clone()),
            file_name: final_name,
            save_dir,
            save_path: path.to_string_lossy().to_string(),
            size: probe.size,
            downloaded: 0,
            state: TaskState::Queued,
            error: None,
            connections: req.connections.unwrap_or(s.default_connections).clamp(1, 64),
            segments: Vec::new(),
            supports_range: probe.supports_range,
            etag: probe.etag,
            last_modified: probe.last_modified,
            content_type: probe.content_type,
            referer: req.referer.clone(),
            cookie: req.cookie.clone(),
            user_agent: probe.ua.clone(),
            speed_limit: req.speed_limit.unwrap_or(0),
            scheduled_at: None,
            created_at: now_ms(),
            completed_at: None,
            speed: 0,
        }
    }

    fn insert_task(&self, t: Task) {
        let id = t.id.clone();
        let rt = Runtime::new(t.clone());
        self.tasks.lock().insert(id, rt);
        let _ = self.store.save_task(&t);
        self.emit(EngineEvent::TaskUpdated(t));
    }

    // ───────────────────────── 探测 ─────────────────────────

    /// 探测：先取大小 / 是否支持分段 / 文件名。
    ///
    /// 注意 UA 回退：不少镜像站（如清华 TUNA）的反盗链会直接 403 掉
    /// 「浏览器 UA + Range」的请求。配置的 UA 被拒时会自动回退到下载器 UA。
    async fn probe(
        &self,
        url: &str,
        referer: Option<&str>,
        cookie: Option<&str>,
    ) -> Result<ProbeInfo> {
        let uas = self.probe_uas();
        let mut last_err = None;
        for ua in &uas {
            match self.probe_with_ua(url, referer, cookie, ua).await {
                Ok(info) => return Ok(info),
                Err(e) => {
                    let retriable = {
                        let m = format!("{e:#}");
                        m.contains("403") || m.contains("拒绝访问")
                    };
                    last_err = Some(e);
                    if !retriable {
                        break;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("探测失败")))
    }

    /// 候选 UA 列表：用户配置的优先，其后是下载器 UA 兜底
    fn probe_uas(&self) -> Vec<String> {
        let s = self.settings.read();
        let mut v = vec![s.user_agent.clone()];
        for ua in [FALLBACK_UA, "curl/8.5.0"] {
            if !v.iter().any(|x| x == ua) {
                v.push(ua.to_string());
            }
        }
        v
    }

    async fn probe_with_ua(
        &self,
        url: &str,
        referer: Option<&str>,
        cookie: Option<&str>,
        ua: &str,
    ) -> Result<ProbeInfo> {
        let client = self.client_with_ua(None, ua)?;
        let mut req = client
            .get(url)
            .header(RANGE, "bytes=0-1")
            .header(reqwest::header::ACCEPT_ENCODING, "identity");
        if let Some(r) = referer {
            req = req.header(REFERER, r);
        }
        if let Some(c) = cookie {
            req = req.header(COOKIE, c);
        }
        let resp = req.send().await.context("连接服务器失败")?;
        let status = resp.status();
        let h = resp.headers();

        let mut info = ProbeInfo {
            final_url: resp.url().to_string(),
            content_type: h
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
            etag: h
                .get(ETAG)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim_matches('"').to_string()),
            last_modified: h
                .get(LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string()),
            ..Default::default()
        };
        info.ua = Some(ua.to_string());
        info.file_name = h
            .get(CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(util::filename_from_disposition);

        if status == StatusCode::PARTIAL_CONTENT {
            info.supports_range = true;
            if let Some(cr) = h.get(CONTENT_RANGE).and_then(|v| v.to_str().ok()) {
                if let Some((_, _, total)) = util::parse_content_range(cr) {
                    info.size = total;
                }
            }
        } else if status == StatusCode::RANGE_NOT_SATISFIABLE {
            info.size = Some(0);
            info.supports_range = true;
        } else if status.is_success() {
            info.size = h
                .get(CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            info.supports_range = h
                .get(ACCEPT_RANGES)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.eq_ignore_ascii_case("bytes"))
                .unwrap_or(false)
                && info.size.unwrap_or(0) > 0;
        } else if status == StatusCode::NOT_FOUND {
            bail!("服务器返回 404，文件不存在");
        } else if status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED {
            bail!("服务器拒绝访问（403），可能需要 Referer / Cookie");
        } else {
            bail!("服务器返回异常状态 {}", status.as_u16());
        }
        Ok(info)
    }


    // ───────────────────────── 状态控制 ─────────────────────────

    pub async fn pause(&self, id: &str) -> Result<()> {
        let Some(rt) = self.tasks.lock().get(id).cloned() else {
            bail!("任务不存在");
        };
        rt.cancel.lock().cancel();
        // 等下载线程真正退出，避免 pause 与 resume 交叉写状态
        let waited = std::time::Instant::now();
        while *rt.running.lock() && waited.elapsed() < std::time::Duration::from_secs(5) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let mut t = rt.task.lock();
        if t.state.is_active() || t.state == TaskState::Queued || t.state == TaskState::Scheduled {
            t.state = TaskState::Paused;
            t.speed = 0;
            t.downloaded = rt.downloaded.load(Ordering::Relaxed);
            let snap = t.clone();
            drop(t);
            let _ = self.store.save_task(&snap);
            self.emit(EngineEvent::TaskUpdated(snap));
        }
        Ok(())
    }

    pub async fn resume(self: &Arc<Self>, id: &str) -> Result<()> {
        let Some(rt) = self.tasks.lock().get(id).cloned() else {
            bail!("任务不存在");
        };
        // 已完成或正在运行则忽略
        {
            let t = rt.task.lock();
            if t.state == TaskState::Completed {
                return Ok(());
            }
        }
        if *rt.running.lock() {
            return Ok(());
        }
        // 并发闸门
        let active = {
            let map = self.tasks.lock();
            map.values()
                .filter(|r| *r.running.lock())
                .count()
        };
        let max = self.settings.read().max_concurrent_tasks.max(1);
        if active >= max {
            let mut t = rt.task.lock();
            if t.state != TaskState::Queued {
                t.state = TaskState::Queued;
                t.error = None;
                let snap = t.clone();
                drop(t);
                let _ = self.store.save_task(&snap);
                self.emit(EngineEvent::TaskUpdated(snap));
            }
            return Ok(());
        }

        *rt.running.lock() = true;
        // 每次开工都换新 token，保证上一次暂停的取消信号不影响本轮
        let new_cancel = CancellationToken::new();
        *rt.cancel.lock() = new_cancel.clone();
        {
            let mut t = rt.task.lock();
            t.state = TaskState::Probing;
            t.error = None;
            t.speed = 0;
        }

        let engine = self.clone();
        let id2 = id.to_string();
        tokio::spawn(async move {
            let res = engine.clone().run_task(&id2, new_cancel).await;
            if let Err(e) = res {
                engine.fail_task(&id2, format!("{e:#}"));
            }
        });
        Ok(())
    }

    /// 取消：中断并删除任务（保留已下载文件可选）
    pub async fn cancel(&self, id: &str, delete_file: bool) -> Result<()> {
        let Some(rt) = self.tasks.lock().get(id).cloned() else {
            bail!("任务不存在");
        };
        rt.cancel.lock().cancel();
        let path = {
            let t = rt.task.lock();
            t.save_path.clone()
        };
        self.tasks.lock().remove(id);
        self.store.delete_task(id);
        if delete_file {
            let _ = std::fs::remove_file(&path);
        }
        self.emit(EngineEvent::TaskRemoved(id.to_string()));
        Ok(())
    }

    pub async fn remove(&self, id: &str) -> Result<()> {
        self.cancel(id, false).await
    }

    /// 清空已完成
    pub fn clear_completed(&self) {
        let ids: Vec<String> = {
            let map = self.tasks.lock();
            map.iter()
                .filter(|(_, r)| r.task.lock().state == TaskState::Completed)
                .map(|(k, _)| k.clone())
                .collect()
        };
        for id in ids {
            self.tasks.lock().remove(&id);
            self.store.delete_task(&id);
            self.emit(EngineEvent::TaskRemoved(id));
        }
    }

    fn fail_task(&self, id: &str, msg: String) {
        if let Some(rt) = self.tasks.lock().get(id).cloned() {
            *rt.running.lock() = false;
            let mut t = rt.task.lock();
            t.state = TaskState::Error;
            t.error = Some(msg);
            t.speed = 0;
            let snap = t.clone();
            drop(t);
            let _ = self.store.save_task(&snap);
            self.emit(EngineEvent::TaskUpdated(snap));
        }
    }

    // ───────────────────────── 主下载流程 ─────────────────────────

    async fn run_task(self: Arc<Self>, id: &str, cancel: CancellationToken) -> Result<()> {
        let rt = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("任务不存在"))?;

        let (url, save_path, total, supports_range, conns, etag, task_ua) = {
            let t = rt.task.lock();
            (
                t.url.clone(),
                t.save_path.clone(),
                t.size,
                t.supports_range,
                t.connections,
                t.etag.clone(),
                t.user_agent.clone(),
            )
        };

        // 若之前没有分片信息，则规划分片
        let need_plan = {
            let t = rt.task.lock();
            t.segments.is_empty()
        };
        if need_plan {
            let segs = plan_segments(total, supports_range, conns);
            let mut t = rt.task.lock();
            t.segments = segs;
        }

        {
            let mut t = rt.task.lock();
            t.state = TaskState::Downloading;
            t.error = None;
        }
        let snap = { rt.task.lock().clone() };
        self.emit(EngineEvent::TaskUpdated(snap));

        // 大小未知、0 字节、或明确不支持分段 → 单流下载
        if total.unwrap_or(0) == 0 || !supports_range {
            self.run_stream_single(&rt, &cancel).await?;
            {
                let mut t = rt.task.lock();
                if t.size.is_none() {
                    t.size = Some(rt.downloaded.load(Ordering::Relaxed));
                }
            }
            self.finish_task(&rt)?;
            return Ok(());
        }

        // 预分配文件（续传时不能截断）
        let file = Arc::new(
            OpenOptions::new()
                .create(true)
                .write(true)
                .read(true)
                .open(&save_path)
                .with_context(|| format!("无法创建文件: {save_path}"))?,
        );
        let meta_len = file.metadata()?.len();
        if meta_len < total.unwrap() {
            file.set_len(total.unwrap())?;
        }

        // 起始已下载量
        let already: u64 = {
            let t = rt.task.lock();
            t.segments.iter().map(|s| s.done.min(s.len())).sum()
        };
        rt.downloaded.store(already, Ordering::Relaxed);
        if already >= total.unwrap() {
            self.finish_task(&rt)?;
            return Ok(());
        }

        let client = {
            let task_snapshot = { rt.task.lock().clone() };
            match task_ua.as_deref() {
                Some(ua) => self.client_with_ua(Some(&task_snapshot), ua)?,
                None => self.client(Some(&task_snapshot))?,
            }
        };
        let s = self.settings();
        let task_limiter = Arc::new(RateLimiter::new(rt.task.lock().speed_limit));
        // 注意：不要在这里用 settings 覆盖全局限速器，update_settings 已经负责同步，
        // 否则用户在下载途中调整限速会被旧值冲掉。
        let _ = &s;

        let (tx, mut rx) = tokio::sync::mpsc::channel::<(usize, u64)>(256);
        let mut set: JoinSet<Result<()>> = JoinSet::new();

        let segs: Vec<Segment> = { rt.task.lock().segments.clone() };
        let pending: Vec<Segment> = segs.into_iter().filter(|s| !s.is_done()).collect();
        let seg_count = pending.len();

        for seg in pending {
            let client = client.clone();
            let url = url.clone();
            let file = file.clone();
            let limiter = task_limiter.clone();
            let global = self.global_limiter.clone();
            let tx = tx.clone();
            let etag = etag.clone();
            let (referer, cookie) = {
                let t = rt.task.lock();
                (t.referer.clone(), t.cookie.clone())
            };
            let cancel2 = cancel.clone();
            let seg_ua = task_ua.clone();
            set.spawn(async move {
                download_segment(
                    SegmentJob {
                        client,
                        url,
                        index: seg.index,
                        cursor: seg.cursor(),
                        end: seg.end,
                        file,
                        limiter,
                        global,
                        tx,
                        etag,
                        referer,
                        cookie,
                        ua: seg_ua,
                    },
                    cancel2,
                )
                .await
            });
        }
        drop(tx);

        let mut cancelled = false;
        if seg_count == 0 {
            // 没有待下载分片，直接完成
        } else {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        cancelled = true;
                        set.abort_all();
                        break;
                    }
                    msg = rx.recv() => {
                        match msg {
                            Some((idx, n)) => {
                                {
                                    let mut t = rt.task.lock();
                                    if let Some(seg) = t.segments.iter_mut().find(|s| s.index == idx) {
                                        seg.done = (seg.done + n).min(seg.len());
                                    }
                                }
                                rt.downloaded.fetch_add(n, Ordering::Relaxed);
                            }
                            // 所有分片都发完数据或出错（tx 已全部 drop）→ 退出
                            None => break,
                        }
                    }
                }
            }
        }
        // 兜底：通道关闭后仍需等 JoinSet 收尾（已在下方 join_next 处理）

        let mut first_err: Option<anyhow::Error> = None;
        while let Some(res) = set.join_next().await {
            match res {
                Ok(Err(e)) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
                Err(e) if !e.is_cancelled() => {
                    if first_err.is_none() {
                        first_err = Some(anyhow!("下载线程异常: {e}"));
                    }
                }
                _ => {}
            }
        }

        *rt.running.lock() = false;

        if cancelled {
            // 保存断点
            let mut t = rt.task.lock();
            t.state = TaskState::Paused;
            t.speed = 0;
            t.downloaded = rt.downloaded.load(Ordering::Relaxed);
            let snap = t.clone();
            drop(t);
            let _ = self.store.save_task(&snap);
            self.emit(EngineEvent::TaskUpdated(snap));
            return Ok(());
        }

        if let Some(e) = first_err {
            let msg = format!("{e:#}");
            // 分片出错：若已有进度，保留为错误态以便重试续传
            let mut t = rt.task.lock();
            t.state = TaskState::Error;
            t.error = Some(msg);
            t.speed = 0;
            t.downloaded = rt.downloaded.load(Ordering::Relaxed);
            let snap = t.clone();
            drop(t);
            let _ = self.store.save_task(&snap);
            self.emit(EngineEvent::TaskUpdated(snap));
            return Ok(());
        }

        self.finish_task(&rt)?;
        Ok(())
    }

    /// 无 Range 支持 / 大小未知时的单流下载
    async fn run_stream_single(
        self: &Arc<Self>,
        rt: &Arc<Runtime>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let (url, save_path, referer, cookie) = {
            let t = rt.task.lock();
            (
                t.url.clone(),
                t.save_path.clone(),
                t.referer.clone(),
                t.cookie.clone(),
            )
        };
        let client = self.client(None)?;
        let start = rt.downloaded.load(Ordering::Relaxed);
        let mut req = client.get(&url);
        if start > 0 {
            req = req.header(RANGE, format!("bytes={start}-"));
        }
        if let Some(r) = referer {
            req = req.header(REFERER, r);
        }
        if let Some(c) = cookie {
            req = req.header(COOKIE, c);
        }
        let resp = req.send().await.context("请求失败")?;
        if !resp.status().is_success() {
            bail!("服务器返回 {}", resp.status().as_u16());
        }
        // 请求了 Range 但服务器回 200（不支持续传）：必须从头写，否则文件会错位
        let resumed_by_server = resp.status() == StatusCode::PARTIAL_CONTENT;
        let mut offset = if start > 0 && resumed_by_server { start } else { 0 };

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&save_path)?;

        let mut stream = resp.bytes_stream();
        // 服务器不支持续传时，从头覆盖
        if offset == 0 && start > 0 {
            file.set_len(0)?;
        }

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    *rt.running.lock() = false;
                    let mut t = rt.task.lock();
                    t.state = TaskState::Paused;
                    t.downloaded = offset;
                    t.speed = 0;
                    if t.size.is_none() {
                        t.size = None;
                    }
                    let snap = t.clone();
                    drop(t);
                    let _ = self.store.save_task(&snap);
                    self.emit(EngineEvent::TaskUpdated(snap));
                    return Ok(());
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            let n = bytes.len() as u64;
                            self.global_limiter.acquire(n).await;
                            write_at(&file, &bytes, offset)?;
                            offset += n;
                            rt.downloaded.store(offset, Ordering::Relaxed);
                        }
                        Some(Err(e)) => return Err(anyhow!("传输中断: {e}")),
                        None => break,
                    }
                }
            }
        }
        // 单流完成后补上 size
        {
            let mut t = rt.task.lock();
            if t.size.is_none() {
                t.size = Some(offset);
            }
            t.downloaded = offset;
        }
        Ok(())
    }

    fn finish_task(&self, rt: &Arc<Runtime>) -> Result<()> {
        *rt.running.lock() = false;
        let mut t = rt.task.lock();
        let total = t.size.unwrap_or(rt.downloaded.load(Ordering::Relaxed));
        // 确保分片状态与总量一致
        let sum: u64 = t.segments.iter().map(|s| s.done.min(s.len())).sum();
        t.downloaded = if t.segments.is_empty() {
            rt.downloaded.load(Ordering::Relaxed)
        } else {
            sum.max(total)
        };
        if !t.segments.is_empty() {
            t.downloaded = total;
        }
        t.state = TaskState::Completed;
        t.completed_at = Some(now_ms());
        t.speed = 0;
        t.error = None;
        let snap = t.clone();
        drop(t);
        let _ = self.store.save_task(&snap);
        self.emit(EngineEvent::TaskUpdated(snap));
        Ok(())
    }

    // ───────────────────────── 后台循环 ─────────────────────────

    /// 启动定时器：进度上报、速度计算、定时任务触发、并发泵
    pub fn spawn_background(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut last: HashMap<TaskId, (u64, std::time::Instant)> = HashMap::new();
            let mut last_persist = std::time::Instant::now();
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                tick.tick().await;
                let now = std::time::Instant::now();
                let mut to_persist: Vec<Task> = Vec::new();
                let snapshots: Vec<(TaskId, u64)> = {
                    let map = me.tasks.lock();
                    map.iter()
                        .map(|(k, r)| (k.clone(), r.downloaded.load(Ordering::Relaxed)))
                        .collect()
                };
                for (id, dl) in snapshots {
                    let rt = {
                        let map = me.tasks.lock();
                        map.get(&id).cloned()
                    };
                    let Some(rt) = rt else { continue };
                    let running = *rt.running.lock();
                    let (mut snap, prev) = {
                        let mut t = rt.task.lock();
                        t.downloaded = dl;
                        let prev = last.get(&id).copied();
                        if running {
                            if let Some((pdl, pt)) = prev {
                                let dt = now.duration_since(pt).as_secs_f64();
                                if dt > 0.05 {
                                    t.speed = ((dl.saturating_sub(pdl)) as f64 / dt) as u64;
                                }
                            }
                        } else {
                            t.speed = 0;
                        }
                        (t.clone(), prev)
                    };
                    last.insert(id.clone(), (dl, now));
                    if running || prev.is_none() {
                        me.emit(EngineEvent::TaskUpdated(snap.clone()));
                    }
                    if running && last_persist.elapsed().as_secs() >= 2 {
                        snap.downloaded = dl;
                        to_persist.push(snap);
                    }
                }
                if last_persist.elapsed().as_secs() >= 2 {
                    last_persist = now;
                    for t in to_persist {
                        let _ = me.store.save_task(&t);
                    }
                }

                // 清理已不存在的任务的速度记录
                last.retain(|k, _| {
                    let map = me.tasks.lock();
                    map.contains_key(k)
                });

                // 定时任务触发
                let due: Vec<TaskId> = {
                    let map = me.tasks.lock();
                    map.iter()
                        .filter(|(_, r)| {
                            let t = r.task.lock();
                            t.state == TaskState::Scheduled
                                && t.scheduled_at.map(|ms| ms <= now_ms()).unwrap_or(false)
                        })
                        .map(|(k, _)| k.clone())
                        .collect()
                };
                for id in due {
                    let _ = me.resume(&id).await;
                }

                // 并发泵：把排队任务提起来
                let max = me.settings.read().max_concurrent_tasks.max(1);
                let running_cnt = {
                    let map = me.tasks.lock();
                    map.values().filter(|r| *r.running.lock()).count()
                };
                if running_cnt < max {
                    let queued: Vec<TaskId> = {
                        let map = me.tasks.lock();
                        map.iter()
                            .filter(|(_, r)| r.task.lock().state == TaskState::Queued)
                            .map(|(k, _)| k.clone())
                            .collect()
                    };
                    for id in queued.into_iter().take(max - running_cnt) {
                        let _ = me.resume(&id).await;
                    }
                }
            }
        });
    }
}

trait IfEmpty {
    fn if_empty_then(self, fallback: String) -> String;
}
impl IfEmpty for String {
    fn if_empty_then(self, fallback: String) -> String {
        if self.trim().is_empty() {
            fallback
        } else {
            self
        }
    }
}

/// 分片规划：按连接数切分；连接数不能超过剩余字节数
pub fn plan_segments(total: Option<u64>, supports_range: bool, conns: usize) -> Vec<Segment> {
    let Some(total) = total else {
        return vec![];
    };
    if total == 0 {
        return vec![];
    }
    if !supports_range {
        return vec![Segment {
            index: 0,
            start: 0,
            end: total - 1,
            done: 0,
        }];
    }
    // 每段至少 1MB，避免过多连接
    let min_seg = 1024 * 1024u64;
    let max_by_size = (total / min_seg).max(1) as usize;
    let n = conns.clamp(1, 64).min(max_by_size);
    let base = total / n as u64;
    let rem = total % n as u64;
    let mut out = Vec::with_capacity(n);
    let mut start = 0u64;
    for i in 0..n {
        let mut len = base;
        if (i as u64) < rem {
            len += 1;
        }
        if len == 0 {
            continue;
        }
        out.push(Segment {
            index: out.len(),
            start,
            end: start + len - 1,
            done: 0,
        });
        start += len;
    }
    out
}

struct SegmentJob {
    client: Client,
    url: String,
    index: usize,
    cursor: u64,
    end: u64,
    file: Arc<File>,
    limiter: Arc<RateLimiter>,
    global: Arc<RateLimiter>,
    tx: tokio::sync::mpsc::Sender<(usize, u64)>,
    etag: Option<String>,
    referer: Option<String>,
    cookie: Option<String>,
    ua: Option<String>,
}

async fn download_segment(job: SegmentJob, cancel: CancellationToken) -> Result<()> {
    // 关键：cursor 必须在重试之间保持，否则中断重连会从分片起点重新拉取，
    // 既浪费流量，又会让已下载计数重复累加（进度虚高 / 文件出现重复数据）。
    let mut cursor = job.cursor;
    let mut attempt = 0u32;
    loop {
        if cancel.is_cancelled() || cursor > job.end {
            return Ok(());
        }
        match fetch_segment(&job, &mut cursor, &cancel).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                attempt += 1;
                if attempt >= 6 || cancel.is_cancelled() {
                    return Err(e);
                }
                let msg = format!("{e:#}");
                // 429/503 是服务器主动限流，需要长退避；其余用短退避
                let backoff_ms = if msg.contains("429") || msg.contains("503") {
                    1200u64 * attempt as u64
                } else {
                    400u64 * attempt as u64
                };
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            }
        }
    }
}

/// 拉取本分片剩余部分。每成功写入一块就推进 `cursor`，失败时调用方可据此续拉。
async fn fetch_segment(
    job: &SegmentJob,
    cursor: &mut u64,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut req = job
        .client
        .get(&job.url)
        .header(RANGE, format!("bytes={}-{}", *cursor, job.end))
        .header(USER_AGENT, job.ua.clone().unwrap_or_else(|| user_agent_hdr().to_string()));
    if let Some(etag) = &job.etag {
        req = req.header(reqwest::header::IF_RANGE, format!("\"{etag}\""));
    }
    if let Some(r) = &job.referer {
        req = req.header(REFERER, r);
    }
    if let Some(c) = &job.cookie {
        req = req.header(COOKIE, c);
    }
    let resp = req.send().await?;
    let status = resp.status();

    if status == StatusCode::OK {
        // 服务器忽略了 Range（If-Range 校验失败或本身不支持）
        return Err(anyhow!("服务器不支持分段（返回 200），请减少连接数后重试"));
    }
    if !status.is_success() {
        return Err(anyhow!("分片请求失败: HTTP {}", status.as_u16()));
    }

    let mut stream = resp.bytes_stream();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            chunk = stream.next() => {
                match chunk {
                    Some(Ok(bytes)) => {
                        // 服务器可能多给数据，按分片边界截断
                        let remain = job.end + 1 - *cursor;
                        let take = (bytes.len() as u64).min(remain) as usize;
                        if take == 0 {
                            return Ok(());
                        }
                        let part = &bytes[..take];
                        job.global.acquire(part.len() as u64).await;
                        job.limiter.acquire(part.len() as u64).await;
                        write_at(&job.file, part, *cursor)?;
                        *cursor += part.len() as u64;
                        if job.tx.send((job.index, part.len() as u64)).await.is_err() {
                            // 接收端已关闭（取消/收工），正常退出
                            return Ok(());
                        }
                        if *cursor > job.end {
                            return Ok(());
                        }
                    }
                    Some(Err(e)) => return Err(anyhow!("读取数据失败: {e}")),
                    None => {
                        return Err(anyhow!("连接提前结束（{}/{}）", *cursor, job.end + 1));
                    }
                }
            }
        }
    }
}

fn user_agent_hdr() -> String {
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_segments_full_coverage() {
        let segs = plan_segments(Some(10_000_000), true, 8);
        assert_eq!(segs.len(), 8);
        assert_eq!(segs[0].start, 0);
        assert_eq!(segs.last().unwrap().end, 9_999_999);
        for w in segs.windows(2) {
            assert_eq!(w[0].end + 1, w[1].start, "分片必须连续无缝");
        }
        let total: u64 = segs.iter().map(|s| s.len()).sum();
        assert_eq!(total, 10_000_000);
    }

    #[test]
    fn plan_segments_small_file_uses_one() {
        let segs = plan_segments(Some(1000), true, 16);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].end, 999);
    }

    #[test]
    fn plan_segments_no_range() {
        let segs = plan_segments(Some(5_000_000), false, 16);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].len(), 5_000_000);
    }

    #[test]
    fn plan_segments_zero_and_none() {
        assert!(plan_segments(Some(0), true, 8).is_empty());
        assert!(plan_segments(None, true, 8).is_empty());
    }

    #[test]
    fn segment_cursor_tracks_resume() {
        let mut s = Segment {
            index: 0,
            start: 100,
            end: 199,
            done: 40,
        };
        assert_eq!(s.cursor(), 140);
        assert!(!s.is_done());
        s.done = 100;
        assert!(s.is_done());
        assert_eq!(s.len(), 100);
    }
}
