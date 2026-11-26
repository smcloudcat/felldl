//! 下载引擎：探测 → 分段 → 并发拉取 → 落盘 → 事件广播
//!
//! 关键设计：
//! * 文件**预先 set_len 分配**，各分片用独立文件句柄定位写入（pwrite），
//!   因此无需下载完成后合并，也没有额外的临时分片文件。
//! * 每个分片的 `done` 持久化在任务 JSON 里，进程重启后从 `start + done` 继续。
//! * 用 `If-Range` + ETag/Last-Modified 校验，服务器文件变了就整体重下，避免拼出坏文件。

use crate::error::FellError;
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

/// 将用户填写的 Cookie / 令牌应用到请求：
/// - 以 "Bearer " 开头，或形如 "Authorization: <value>" 的值 → 作为 Authorization 头发送
/// - 其余 → 作为 Cookie 头发送
fn apply_auth_cookie(req: reqwest::RequestBuilder, c: &str) -> reqwest::RequestBuilder {
    let t = c.trim();
    let lower = t.to_lowercase();
    if lower.starts_with("bearer ") || lower.starts_with("authorization:") {
        let v = if lower.starts_with("authorization:") {
            t.split_once(':').map(|x| x.1.trim().to_string()).unwrap_or_default()
        } else {
            t.to_string()
        };
        if let Ok(hv) = reqwest::header::HeaderValue::from_str(&v) {
            return req.header(reqwest::header::AUTHORIZATION, hv);
        }
        return req;
    }
    req.header(COOKIE, t)
}

/// 一次请求可用的认证材料。核心约束：**只有目标地址与任务原始 origin 一致时**
/// 才会把 Cookie / 令牌 / Basic 施加到请求上。
///
/// 镜像站、CDN 回源、跨域跳转、http/https 互换、端口变化都属于不同 origin，
/// 默认永远收不到原站凭据；需要镜像认证时必须单独配置镜像自己的凭据。
#[derive(Clone, Copy, Default)]
struct Creds<'a> {
    /// 原始用户地址的 origin（规范化后的 scheme://host:port）
    origin: &'a str,
    cookie: Option<&'a str>,
    basic: Option<(&'a str, &'a str)>,
}

impl<'a> Creds<'a> {
    fn new(
        origin: &'a str,
        cookie: Option<&'a str>,
        basic: Option<(&'a str, &'a str)>,
    ) -> Self {
        Self { origin, cookie, basic }
    }

    /// 从任务快照取凭据归属；旧任务没有 auth_origin 时回退到 url 的 origin。
    fn from_task(t: &'a Task) -> Self {
        let origin = if t.auth_origin.is_empty() {
            t.url.as_str()
        } else {
            t.auth_origin.as_str()
        };
        let basic = match (t.username.as_deref(), t.password.as_deref()) {
            (Some(u), p) if !u.is_empty() => Some((u, p.unwrap_or(""))),
            _ => None,
        };
        Self::new(origin, t.cookie.as_deref(), basic)
    }

    /// 目标地址是否允许携带本组凭据（解析失败一律拒绝）
    fn allows(&self, url: &str) -> bool {
        !self.origin.is_empty() && util::same_origin(self.origin, url)
    }

    /// 仅在与原始 origin 一致时附加认证头，否则原样返回（不附带任何凭据）
    fn apply(&self, req: reqwest::RequestBuilder, url: &str) -> reqwest::RequestBuilder {
        if !self.allows(url) {
            return req;
        }
        let mut req = req;
        if let Some(c) = self.cookie {
            req = apply_auth_cookie(req, c);
        }
        if let Some((u, p)) = self.basic {
            req = req.basic_auth(u, Some(p));
        }
        req
    }
}
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

/// 客户端池上限：不同 UA / Referer / 网卡 / 代理组合过多时按插入顺序淘汰。
/// reqwest::Client 内部是 Arc，clone 只是引用计数，共享同一个连接池。
const CLIENT_CACHE_CAP: usize = 8;

#[derive(Default)]
struct ClientCache {
    map: HashMap<String, Client>,
    order: std::collections::VecDeque<String>,
}

impl ClientCache {
    fn get(&self, key: &str) -> Option<Client> {
        self.map.get(key).cloned()
    }

    fn put(&mut self, key: String, client: Client) {
        while self.order.len() >= CLIENT_CACHE_CAP {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            } else {
                break;
            }
        }
        if self.map.insert(key.clone(), client).is_none() {
            self.order.push_back(key);
        }
    }
}

pub struct Engine {
    pub(crate) store: Store,
    settings: RwLock<Settings>,
    pub(crate) tasks: Mutex<HashMap<TaskId, Arc<Runtime>>>,
    events: broadcast::Sender<EngineEvent>,
    /// 全局限速器
    global_limiter: Arc<RateLimiter>,
    /// HTTP 客户端池：按 (UA, Referer, 绑定网卡, 代理) 复用同一个 Client，
    /// 从而跨任务共享连接池与 TLS 会话，省掉每个任务重建连接池的内存与握手开销。
    clients: Mutex<ClientCache>,
    pub http_token: RwLock<String>,
    /// 整站抓取任务（可取消）
    pub(crate) grab_jobs: Mutex<HashMap<String, Arc<crate::grab::GrabJob>>>,
    /// BitTorrent 会话（首次添加磁力任务时惰性初始化）
    pub(crate) bt_session: tokio::sync::OnceCell<Arc<crate::bt::BtManager>>,
    /// 已经尝试过重新挂回会话的 BT 任务：rqbit 会话是内存态，
    /// 进程重启后需要按持久化记录重新 add，但失败不能每 500ms 重试一次
    pub(crate) bt_restore_tried: Mutex<std::collections::HashSet<TaskId>>,
}

pub(crate) struct Runtime {
    pub(crate) task: Mutex<Task>,
    /// 实时已下载字节（比 task.downloaded 更新更频繁）
    pub(crate) downloaded: AtomicU64,
    /// 每次启动下载都换新 token，因此暂停 → 恢复 → 再暂停都能正确中断
    pub(crate) cancel: Mutex<CancellationToken>,
    /// 本次会话内是否正在运行
    pub(crate) running: Mutex<bool>,
    /// 任务级限速器（运行中可由 set_task_speed_limit 动态调整）
    pub(crate) task_limiter: Mutex<Option<Arc<RateLimiter>>>,
    /// 运行代号（generation）：每次「开工」递增。
    ///
    /// worker 只允许用**自己那一代**写任务状态。这解决三类真实问题：
    /// 1) 暂停超时后用户点了恢复，旧 worker 收尾时把 `running` 清零，
    ///    导致并发槽被重复占用、同一文件被两个 worker 同时写；
    /// 2) 删除任务后 worker 迟到写回 Paused/Completed，任务在磁盘和 UI 上复活；
    /// 3) 旧 worker 的 Error 覆盖用户刚点的暂停。
    pub(crate) generation: AtomicU64,
}

impl Runtime {
    /// 开始新的一代运行（resume / 取消 都调用）
    pub(crate) fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// 当前代
    pub(crate) fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// 这一代是否仍然有效（没有恢复过、没有被取消/删除）
    pub(crate) fn is_current(&self, generation: u64) -> bool {
        self.current_generation() == generation
    }
}

/// 运行守卫：`run_task` 无论从哪条路径退出（完成 / 暂停 / 出错 / 解析失败），
/// 都保证 `running` 归零。否则并发槽被永久占用：任务再也 resume 不起来，
/// 队列里其他任务也被卡在「已满」。
///
/// 只清理**自己那一代**的 running：恢复后的新 worker 不能被旧 worker 的收尾误伤。
struct RunningGuard {
    rt: Arc<Runtime>,
    generation: u64,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        if self.rt.is_current(self.generation) {
            *self.rt.running.lock() = false;
        }
    }
}

impl Runtime {
    pub(crate) fn new(task: Task) -> Arc<Self> {
        Arc::new(Self {
            downloaded: AtomicU64::new(task.downloaded),
            cancel: Mutex::new(CancellationToken::new()),
            running: Mutex::new(false),
            task: Mutex::new(task),
            task_limiter: Mutex::new(None),
            generation: AtomicU64::new(0),
        })
    }
}

// ───────────────────────── 平台相关的定位写入 ─────────────────────────

#[cfg(windows)]
pub(crate) fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
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
pub(crate) fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
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
        // 依赖树里 ring 与 aws-lc-rs 可能同时编译进来，rustls 无法自动选定唯一
        // 默认 provider 时会 panic（release = panic=abort，等于应用直接退出）。
        // 这里显式安装一次 ring 作为进程默认，重复调用只会返回 Err，可安全忽略。
        let _ = rustls::crypto::ring::default_provider().install_default();
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
            clients: Mutex::new(ClientCache::default()),
            http_token: RwLock::new(token),
            grab_jobs: Mutex::new(HashMap::new()),
            bt_session: tokio::sync::OnceCell::new(),
            bt_restore_tried: Mutex::new(std::collections::HashSet::new()),
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

    pub(crate) fn emit(&self, ev: EngineEvent) {
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
        // 界面毛玻璃参数：收窄到合理区间，避免前端传入极端值把界面糊死
        s.ui_blur = s.ui_blur.min(40);
        s.ui_dim = s.ui_dim.min(90);
        s.ui_glass = s.ui_glass.clamp(40, 100);
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
        // 运行中的任务立即应用新限速（此前只在启动时读取一次）
        if let Some(l) = rt.task_limiter.lock().as_ref() {
            l.set_limit(limit);
        }
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

    pub(crate) fn client(&self, task: Option<&Task>) -> Result<Client> {
        let ua = self.settings.read().user_agent.clone();
        self.client_with_ua(task, &ua)
    }

    pub(crate) fn client_with_ua(&self, task: Option<&Task>, ua: &str) -> Result<Client> {
        let s = self.settings.read().clone();
        let referer = task.and_then(|t| t.referer.clone()).unwrap_or_default();
        let dns_active = s.dns_mode.trim() == "custom" && !s.dns_servers.trim().is_empty();
        // 池化键：影响底层连接池配置的全部因素（Referer 通过默认头实现，也一并计入）
        let key = format!(
            "{ua}\u{1}{referer}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
            s.bind_ip.trim(),
            s.proxy.to_url().unwrap_or_default(),
            if dns_active { s.dns_servers.trim() } else { "" },
            s.max_concurrent_tasks,
            s.default_connections
        );
        if let Some(c) = self.clients.lock().get(&key) {
            return Ok(c);
        }
        // 空闲连接上限按「同时在跑的任务数 × 每任务连接数」估算：
        // 多任务共享同一个连接池时够用，又不会为单任务预留过多连接（省内存与端口）
        let idle_cap = (s.max_concurrent_tasks.clamp(1, 32) * s.default_connections.clamp(1, 32))
            .clamp(8, 256);
        let mut b = Client::builder()
            .user_agent(ua)
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(3600))
            .pool_max_idle_per_host(idle_cap)
            // 不少下载站 / 镜像站的证书链不完整，这里保留校验但允许常见兼容
            .redirect(reqwest::redirect::Policy::limited(10));
        // 自定义 DNS：解析交给 hickory，失败则回退系统解析（不阻断下载）
        if dns_active {
            match crate::dns::parse_servers(&s.dns_servers) {
                Ok(servers) => match crate::dns::HickoryDns::new(&servers) {
                    Ok(r) => b = b.dns_resolver(Arc::new(r)),
                    Err(e) => eprintln!("自定义 DNS 初始化失败，回退系统 DNS: {e:#}"),
                },
                Err(e) => eprintln!("自定义 DNS 地址无效，回退系统 DNS: {e:#}"),
            }
        }
        // 绑定指定网络接口（IPv4 / IPv6 均可）
        if !s.bind_ip.trim().is_empty() {
            if let Ok(ip) = s.bind_ip.trim().parse::<std::net::IpAddr>() {
                b = b.local_address(ip);
            }
        }
        if let Some(url) = s.proxy.to_url() {
            let p = reqwest::Proxy::all(&url).with_context(|| format!("代理地址无效: {url}"))?;
            b = b.proxy(p);
        }
        if !referer.is_empty() {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(&referer) {
                let mut hm = reqwest::header::HeaderMap::new();
                hm.insert(REFERER, v);
                b = b.default_headers(hm);
            }
        }
        let client = b.build()?;
        self.clients.lock().put(key, client.clone());
        Ok(client)
    }

    // ───────────────────────── 添加任务 ─────────────────────────

    /// 添加任务并返回任务 ID。会先做一次探测以获取大小与是否支持分段。
    pub async fn add(self: &Arc<Self>, req: AddTaskRequest) -> Result<TaskId> {
        let url = req.url.trim().to_string();
        // 磁力 / .torrent 交给 BT 管理器处理
        if url.starts_with("magnet:") {
            return self.bt_add(&url, &req).await;
        }
        // FTP / SFTP：单流下载，无需 HTTP 探测
        let lower = url.to_lowercase();
        if lower.starts_with("ftp://") || lower.starts_with("ftps://") || lower.starts_with("sftp://") {
            return self.add_remote(req, url).await;
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            bail!("仅支持 http/https/ftp/sftp 链接");
        }
        // Metalink：解析出镜像列表后按普通多源任务下载
        let mut req = req;
        if lower.ends_with(".metalink") || lower.ends_with(".meta4") {
            req = self.resolve_metalink(req, &url).await?;
        }
        let s = self.settings();

        // URL 自带 userinfo（user:pass@host）时自动剥离为凭据
        let (clean_url, url_user, url_pass) = split_url_userinfo(&req.url);
        let username = req.username.clone().filter(|s| !s.is_empty()).or(url_user);
        let password = req.password.clone().filter(|s| !s.is_empty()).or(url_pass);
        req.url = clean_url;
        req.username = username.clone();
        req.password = password.clone();
        // 认证归属 = 用户输入的原始地址。镜像、metalink 备源、跨域跳转都是别的
        // origin，默认收不到这组凭据（避免把 Cookie / 令牌发给第三方站点）。
        let cred_origin = req.url.clone();
        let cred_cookie = req.cookie.clone();
        let creds = Creds::new(
            &cred_origin,
            cred_cookie.as_deref(),
            match (username.as_deref(), password.as_deref()) {
                (Some(u), p) => Some((u, p.unwrap_or(""))),
                _ => None,
            },
        );

        let probe = match self.probe(&req.url, req.referer.as_deref(), creds).await {
            Ok(p) => p,
            Err(e) => {
                // 探测失败也建任务，标记错误便于用户重试
                let mut t = self.build_task(&req, ProbeInfo::default(), &s)?;
                // 直连不通但开了镜像：用镜像的探测结果兜底建任务，
                // 否则「镜像加速」恰好在最需要它的时候用不上。
                if s.mirror_enabled {
                    let cands = crate::mirror::candidates(&t.url, &s);
                    let hits = self
                        .probe_mirrors(cands, req.referer.as_deref(), creds)
                        .await;
                    if let Some((_, info)) = hits.first() {
                        t.size = info.size;
                        t.etag = info.etag.clone();
                        t.supports_range = info.supports_range;
                        t.mirrors = hits.into_iter().map(|(u, _)| u).collect();
                        t.clear_error();
                        t.state = TaskState::Queued;
                        let scheduled = req.scheduled_at.filter(|&ms| ms > now_ms());
                        if scheduled.is_some() {
                            t.state = TaskState::Scheduled;
                            t.scheduled_at = scheduled;
                        }
                        self.insert_task(t.clone());
                        if scheduled.is_none() && s.auto_start {
                            let _ = self.resume(&t.id).await;
                        }
                        return Ok(t.id);
                    }
                }
                t.state = TaskState::Error;
                t.set_error(&e);
                self.insert_task(t.clone());
                return Ok(t.id);
            }
        };

        // metalink 解析出的镜像直接注入（不再二次校验）
        let metalink_mirrors = std::mem::take(&mut req.mirrors);
        let mut t = self.build_task(&req, probe, &s)?;
        // 校验镜像：逐个探测，仅保留与主地址大小一致的镜像
        for m in &metalink_mirrors {
            let m = m.trim();
            if m.is_empty() || m == &t.url {
                continue;
            }
            match self.probe(m, req.referer.as_deref(), creds).await {
                Ok(p) => {
                    let same_size = match (p.size, t.size) {
                        (Some(a), Some(b)) => a == b,
                        // 任一方大小未知则保守接受，靠 If-Range 兜底
                        _ => true,
                    };
                    if same_size {
                        t.mirrors.push(p.final_url);
                    }
                }
                Err(_) => continue,
            }
        }
        // 镜像加速：开启时给 GitHub / HuggingFace 之类站点附带候选镜像。
        // 并发探测，只保留「支持分段 + 大小一致 + ETag 一致」的镜像，
        // 否则 If-Range 校验会被镜像自己的 ETag 打回，浪费一次重试。
        if s.mirror_enabled && t.supports_range && t.mirrors.len() < 4 {
            let cands = crate::mirror::candidates(&t.url, &s);
            let main_size = t.size;
            let main_etag = t.etag.clone();
            let hits = self
                .probe_mirrors(cands, req.referer.as_deref(), creds)
                .await;
            for (m, info) in hits {
                // 只保留「同大小 + 同 ETag」的镜像，否则 If-Range 会被打回
                if let (Some(a), Some(b)) = (info.size, main_size) {
                    if a != b {
                        continue;
                    }
                }
                if let (Some(a), Some(b)) = (&info.etag, &main_etag) {
                    if !a.is_empty() && !b.is_empty() && a != b {
                        continue;
                    }
                }
                if m != t.url && !t.mirrors.contains(&m) && t.mirrors.len() < 4 {
                    t.mirrors.push(m);
                }
            }
        }
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

    /// FTP / SFTP 任务：无 HTTP 探测，直接建任务
    async fn add_remote(self: &Arc<Self>, req: AddTaskRequest, url: String) -> Result<TaskId> {
        let s = self.settings();
        let kind = if url.to_lowercase().starts_with("sftp://") {
            TaskKind::Sftp
        } else {
            TaskKind::Ftp
        };
        // URL 自带凭据剥离
        let (clean_url, u, p) = split_url_userinfo(&url);
        let username = req.username.clone().filter(|x| !x.is_empty()).or(u)
            .unwrap_or_else(|| "anonymous".into());
        let password = req
            .password
            .clone()
            .filter(|x| !x.is_empty())
            .or(p)
            .unwrap_or_else(|| "felldl@example.com".into());
        let mut r2 = req.clone();
        r2.url = clean_url;

        let file_name = util::filename_from_url(&r2.url).unwrap_or_else(|| "download".into());
        let probe = ProbeInfo {
            size: None,
            supports_range: true,
            file_name: Some(file_name),
            content_type: None,
            ..Default::default()
        };
        let mut t = self.build_task_with_auth(&r2, probe, &s, kind, Some(username), Some(password))?;
        t.supports_range = true;
        self.insert_task(t.clone());
        if s.auto_start {
            let _ = self.resume(&t.id).await;
        }
        Ok(t.id)
    }

    /// Metalink(.metalink/.meta4) 解析：提取全部 <url> 与文件名，
    /// 主 URL 建任务、其余 URL 注入镜像列表。
    async fn resolve_metalink(&self, mut req: AddTaskRequest, url: &str) -> Result<AddTaskRequest> {
        let client = self.client(None)?;
        let text = client
            .get(url)
            .send()
            .await
            .context("下载 metalink 文件失败")?
            .error_for_status()
            .context("下载 metalink 文件失败")?
            .text()
            .await?;
        let (urls, name) = crate::metalink::parse(&text)
            .ok_or_else(|| anyhow!("metalink 内容无效或不含可用 URL"))?;
        if urls.is_empty() {
            bail!("metalink 中没有可用 URL");
        }
        if req.file_name.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true) {
            if let Some(n) = name {
                req.file_name = Some(util::sanitize_name(&n));
            }
        }
        req.url = urls[0].clone();
        req.mirrors = urls[1..].to_vec();
        Ok(req)
    }

    fn build_task(
        &self,
        req: &AddTaskRequest,
        probe: ProbeInfo,
        s: &Settings,
    ) -> Result<Task> {
        let kind = TaskKind::Http;
        let auth = (req.username.clone(), req.password.clone());
        self.build_task_with_auth(req, probe, s, kind, auth.0, auth.1)
    }

    fn build_task_with_auth(
        &self,
        req: &AddTaskRequest,
        probe: ProbeInfo,
        s: &Settings,
        kind: TaskKind,
        username: Option<String>,
        password: Option<String>,
    ) -> Result<Task> {
        let save_dir = req
            .save_dir
            .clone()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| s.download_dir.clone());

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
        let final_name = name.clone();

        // 分类：显式指定 > 规则自动归类；命中的分类可提供独立保存目录
        let category = req
            .category
            .clone()
            .filter(|c| !c.trim().is_empty())
            .or_else(|| classify_category(&final_name, &req.url, &s.categories));
        let save_dir = category
            .as_ref()
            .and_then(|cat| {
                s.categories
                    .iter()
                    .find(|c| &c.name == cat)
                    .and_then(|c| c.save_dir.clone().filter(|d| !d.trim().is_empty()))
            })
            .unwrap_or(save_dir);
        let _ = std::fs::create_dir_all(&save_dir);
        // 保存目录因分类规则变化时重新预占路径；create_new 保证并发下也不会撞名
        let path = util::reserve_path(Path::new(&save_dir), &final_name)
            .with_context(|| format!("无法在 {save_dir} 分配文件名"))?;

        Ok(Task {
            id: uuid::Uuid::new_v4().to_string(),
            url: probe.final_url.clone().if_empty_then(req.url.clone()),
            kind,
            mirrors: Vec::new(),
            file_name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or(final_name),
            save_dir: save_dir.clone(),
            save_path: path.to_string_lossy().to_string(),
            size: probe.size,
            downloaded: 0,
            state: TaskState::Queued,
            error: None,
            error_code: None,
            retryable: None,
            connections: req.connections.unwrap_or(s.default_connections).clamp(1, 64),
            segments: Vec::new(),
            supports_range: probe.supports_range,
            etag: probe.etag,
            last_modified: probe.last_modified,
            content_type: probe.content_type,
            referer: req.referer.clone(),
            cookie: req.cookie.clone(),
            // 凭据绑定用户输入的原始地址（不是重定向终址），镜像/跨域跳转拿不到
            auth_origin: util::origin_of(&req.url).unwrap_or_default(),
            user_agent: probe.ua.clone(),
            speed_limit: req.speed_limit.unwrap_or(0),
            scheduled_at: None,
            created_at: now_ms(),
            completed_at: None,
            speed: 0,
            peers: 0,
            upload_speed: 0,
            bt_info_hash: None,
            category,
            username,
            password,
            interval_secs: req.interval_secs,
            resume_anchor: None,
        })
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
    /// 并发探测镜像候选（每个 5 秒上限），返回可用的 (镜像地址, 探测信息)
    /// 并发探测镜像候选（每个 5 秒上限），返回可用的 (镜像地址, 探测信息)。
    /// 镜像通常跨域，`creds` 只会在与原始 origin 一致时才生效。
    async fn probe_mirrors(
        &self,
        cands: Vec<String>,
        referer: Option<&str>,
        creds: Creds<'_>,
    ) -> Vec<(String, ProbeInfo)> {
        if cands.is_empty() {
            return Vec::new();
        }
        let futs = cands.into_iter().take(3).map(|m| async move {
            let info = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.probe(&m, referer, creds),
            )
            .await
            .ok()?
            .ok()?;
            if !info.supports_range {
                return None;
            }
            Some((m, info))
        });
        futures_util::future::join_all(futs)
            .await
            .into_iter()
            .flatten()
            .collect()
    }

    async fn probe(
        &self,
        url: &str,
        referer: Option<&str>,
        creds: Creds<'_>,
    ) -> Result<ProbeInfo> {
        let uas = self.probe_uas();
        let mut last_err = None;
        for ua in &uas {
            match self.probe_with_ua(url, referer, ua, creds).await {
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
        ua: &str,
        creds: Creds<'_>,
    ) -> Result<ProbeInfo> {
        let client = self.client_with_ua(None, ua)?;
        let mut req = client
            .get(url)
            .header(RANGE, "bytes=0-1")
            .header(reqwest::header::ACCEPT_ENCODING, "identity");
        if let Some(r) = referer {
            req = req.header(REFERER, r);
        }
        // 跨 origin（镜像/跨域跳转）不附加任何认证信息
        let req = creds.apply(req, url);
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
            return Err(FellError::http(404, "服务器返回 404，文件不存在").into());
        } else if status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED {
            return Err(FellError::http(
                status.as_u16(),
                format!("服务器拒绝访问（{}），可能需要 Referer / Cookie", status.as_u16()),
            )
            .into());
        } else {
            return Err(FellError::http(
                status.as_u16(),
                format!("服务器返回异常状态 {}", status.as_u16()),
            )
            .into());
        }
        Ok(info)
    }


    // ───────────────────────── 状态控制 ─────────────────────────

    pub async fn pause(&self, id: &str) -> Result<()> {
        let Some(rt) = self.tasks.lock().get(id).cloned() else {
            bail!("任务不存在");
        };
        // BT 任务走 rqbit 会话暂停
        if rt.task.lock().kind == TaskKind::Bt {
            self.bt_pause(&rt).await;
            return Ok(());
        }
        rt.cancel.lock().cancel();
        // 等下载线程真正退出，避免 pause 与 resume 交叉写状态
        let waited = std::time::Instant::now();
        while *rt.running.lock() && waited.elapsed() < std::time::Duration::from_secs(5) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // 用户操作代表最新意图：用当前代号写入，不会被超时未停的 worker 回退
        let generation = rt.current_generation();
        let active = {
            let t = rt.task.lock();
            t.state.is_active() || t.state == TaskState::Queued || t.state == TaskState::Scheduled
        };
        if active {
            let dl = rt.downloaded.load(Ordering::Relaxed);
            self.commit_state(&rt, generation, TaskState::Paused, |t| {
                t.speed = 0;
                t.downloaded = dl;
            });
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
        // BT 任务走 rqbit 会话恢复（自行管理 running 状态）
        if rt.task.lock().kind == TaskKind::Bt {
            self.bt_resume(&rt).await;
            return Ok(());
        }
        // 非 BT：先原子占位，避免并发 resume / 并发泵把同一任务启动两次
        // （两次 run_task 会同时写同一个文件并互相覆盖）
        {
            let mut g = rt.running.lock();
            if *g {
                return Ok(());
            }
            *g = true;
        }
        // 并发闸门：统计「其他」正在运行的任务数
        let active_others = {
            let map = self.tasks.lock();
            map.iter()
                .filter(|(k, r)| k.as_str() != id && *r.running.lock())
                .count()
        };
        let max = self.settings.read().max_concurrent_tasks.max(1);
        if active_others >= max {
            // 释放占位并转入排队
            *rt.running.lock() = false;
            let generation = rt.current_generation();
            self.commit_state(&rt, generation, TaskState::Queued, |t| t.clear_error());
            return Ok(());
        }

        // 每次开工都换新 token，保证上一次暂停的取消信号不影响本轮
        let new_cancel = CancellationToken::new();
        *rt.cancel.lock() = new_cancel.clone();
        // 新的一代：上一代 worker 若还活着（暂停超时），它的收尾从此一律被丢弃
        let generation = rt.next_generation();
        self.commit_state(&rt, generation, TaskState::Probing, |t| {
            t.clear_error();
            t.speed = 0;
        });

        let engine = self.clone();
        let id2 = id.to_string();
        tokio::spawn(async move {
            let res = engine.clone().run_task(&id2, generation, new_cancel).await;
            if let Err(e) = res {
                engine.fail_task(&id2, Some(generation), &e);
            }
        });
        Ok(())
    }

    /// 取消：中断并删除任务（保留已下载文件可选）
    pub async fn cancel(&self, id: &str, delete_file: bool) -> Result<()> {
        let Some(rt) = self.tasks.lock().get(id).cloned() else {
            bail!("任务不存在");
        };
        let is_bt = rt.task.lock().kind == TaskKind::Bt;
        // BT 任务先从 rqbit 会话移除；文件删除交给 rqbit 按其受控文件清单处理，
        // 不能再用通用的路径递归删除（顶层名可能对应原有同名目录）。
        if is_bt {
            self.bt_remove(&rt, delete_file).await;
        } else {
            rt.cancel.lock().cancel();
        }
        // 让在途 worker 立即作废：删除后它的任何状态写入 / 事件广播都被丢弃，
        // 否则用户删掉的任务会以「暂停中」的身份重新出现在列表和磁盘上。
        rt.next_generation();
        let path = {
            let t = rt.task.lock();
            t.save_path.clone()
        };
        self.tasks.lock().remove(id);
        self.store.delete_task(id);
        if delete_file && !is_bt {
            remove_downloaded(&path).await;
        }
        self.emit(EngineEvent::TaskRemoved(id.to_string()));
        Ok(())
    }

    pub async fn remove(&self, id: &str) -> Result<()> {
        self.cancel(id, false).await
    }

    /// 清空已完成
    pub async fn clear_completed(&self) {
        let entries: Vec<(String, bool)> = {
            let map = self.tasks.lock();
            map.iter()
                .filter(|(_, r)| r.task.lock().state == TaskState::Completed)
                .map(|(k, r)| (k.clone(), r.task.lock().kind == TaskKind::Bt))
                .collect()
        };
        for (id, is_bt) in entries {
            // BT 任务先从 rqbit 会话移除，否则种子会留在后台继续做种、
            // 且 UI 已不可见（资源泄漏）
            if is_bt {
                let rt = { self.tasks.lock().get(&id).cloned() };
                if let Some(rt) = rt {
                    self.bt_remove(&rt, false).await;
                }
            }
            self.tasks.lock().remove(&id);
            self.store.delete_task(&id);
            self.emit(EngineEvent::TaskRemoved(id));
        }
    }

    /// 任务状态的**唯一**写入口：状态迁移、落盘与广播都在这里发生。
    ///
    /// `generation` 是本次运行的代号（`Runtime::next_generation` 分配）。
    /// 下列任一情况直接丢弃（不写盘、不广播），返回 `false`：
    /// - 这一代已被新的运行 / 取消取代 —— 旧 worker 的迟到快照不得改状态；
    /// - 任务已被删除（store 墓碑）—— 否则删除后任务会复活；
    /// - 状态机拒绝的迁移（例如已完成被打回暂停）。
    fn commit_state<F>(&self, rt: &Arc<Runtime>, generation: u64, to: TaskState, apply: F) -> bool
    where
        F: FnOnce(&mut Task),
    {
        if !rt.is_current(generation) {
            return false;
        }
        let mut t = rt.task.lock();
        if self.store.is_dead(&t.id) {
            return false;
        }
        if !t.state.can_transition(to) {
            eprintln!(
                "[state] 拒绝迁移 {:?} -> {:?}（任务 {}，已是过期写入）",
                t.state, to, t.id
            );
            return false;
        }
        apply(&mut t);
        t.state = to;
        let snap = t.clone();
        drop(t);
        let _ = self.store.save_task(&snap);
        self.emit(EngineEvent::TaskUpdated(snap));
        true
    }

    /// 热路径进度落盘（FTP / SFTP / BT）：**取快照与入队必须在同一把任务锁内**，
    /// 且只在「仍处于下载中」时写。
    ///
    /// 否则 pause() 刚写下的 Paused 会被一条并发产生的 Downloading 快照覆盖，
    /// 用户看到「暂停中却还在下载」，重启后状态也会回跳到下载中。
    pub(crate) fn persist_progress(&self, rt: &Arc<Runtime>) {
        let mut t = rt.task.lock();
        t.downloaded = rt.downloaded.load(Ordering::Relaxed);
        if t.state == TaskState::Downloading {
            self.store.enqueue_save_owned(t.clone());
        }
    }

    /// 记录失败。`generation = None` 表示调用方自管生命周期（BT 会话），
    /// 按当前代处理——它没有「旧 worker 迟到」的问题，取消会直接从 map 移除。
    pub(crate) fn fail_task(&self, id: &str, generation: Option<u64>, err: &anyhow::Error) {
        let Some(rt) = self.tasks.lock().get(id).cloned() else {
            return;
        };
        let generation = generation.unwrap_or_else(|| rt.current_generation());
        let dl = rt.downloaded.load(Ordering::Relaxed);
        self.commit_state(&rt, generation, TaskState::Error, |t| {
            t.set_error(err);
            t.speed = 0;
            t.downloaded = dl;
        });
        if rt.is_current(generation) {
            *rt.running.lock() = false;
        }
    }

    // ───────────────────────── 主下载流程 ─────────────────────────

    /// 续传前核对「磁盘上的文件还是不是上次那个文件」。
    ///
    /// `Segment::done` 只是数字，没有任何文件证据：文件被删、被截短、或被同名文件
    /// 替换后，进度仍声称已下完若干 MB，续传会直接跳过这些分片，最后拿到一个
    /// 稀疏文件冒充完成品。这里用保存进度时记录的身份锚点（长度 + 创建时间）核对，
    /// 对不上就把进度整体作废并截断文件，强制从头下载。
    ///
    /// 返回 `true` = 可以按现有进度续传；`false` = 进度已作废（调用方必须从 0 开始）。
    fn verify_resume(&self, rt: &Arc<Runtime>, path: &Path) -> bool {
        let (anchor, has_progress) = {
            let t = rt.task.lock();
            let progress = t.downloaded > 0 || t.segments.iter().any(|s| s.done > 0);
            (t.resume_anchor.clone(), progress)
        };
        if !has_progress {
            return true;
        }
        let ok = match (&anchor, util::file_anchor(path)) {
            (Some(a), Some(cur)) => a.still_matches(&cur),
            // 老任务没有锚点 / 文件已不存在：一律不采信
            _ => false,
        };
        if ok {
            return true;
        }
        self.invalidate_progress(rt, path, "本地文件已变更，断点进度已作废，将从头重新下载");
        false
    }

    /// 作废断点：清分片、清字节数、截断文件（保留文件名占位以便继续占用），
    /// 并把原因作为提示推给界面。幂等，可重复调用。
    fn invalidate_progress(&self, rt: &Arc<Runtime>, path: &Path, reason: &str) {
        let notice = {
            let mut t = rt.task.lock();
            let lost = t.downloaded;
            let name = t.file_name.clone();
            t.segments.clear();
            t.downloaded = 0;
            t.resume_anchor = None;
            (lost > 0).then(|| format!("{reason}（{name}，已丢弃 {lost} 字节进度）"))
        };
        rt.downloaded.store(0, Ordering::Relaxed);
        if let Ok(f) = OpenOptions::new().write(true).open(path) {
            let _ = f.set_len(0);
        }
        if let Some(msg) = notice {
            self.emit(EngineEvent::Notice(msg));
        }
    }

    /// 续传前核对「远端还是不是上次那个文件」。
    ///
    /// 本地锚点只能证明「磁盘上那堆字节没被动过」，证明不了「它们还属于同一个远端实体」：
    /// 断点期间发布方重新打包（长度不变、内容变了）是很常见的情形，续传会把新旧两份
    /// 字节拼成一个长度正确、内容错乱的文件。这里用一次探测比对 ETag 与总长度，
    /// 不一致就作废旧进度、换用新的实体信息重新下载。
    ///
    /// 探测失败（网络抖动 / 站点临时拒绝）不毁断点：退回本地锚点判断，
    /// 后续 `If-Range` 与 206 的 ETag 比对仍会兜一层。
    async fn verify_remote(&self, rt: &Arc<Runtime>, path: &Path) -> bool {
        let snap = {
            let t = rt.task.lock();
            let progress = t.downloaded > 0 || t.segments.iter().any(|s| s.done > 0);
            if !progress || t.etag.is_none() {
                return true;
            }
            t.clone()
        };
        let prev_etag = snap.etag.clone().unwrap_or_default();
        let ua = snap
            .user_agent
            .clone()
            .unwrap_or_else(|| self.settings.read().user_agent.clone());
        let info = match self
            .probe_with_ua(&snap.url, snap.referer.as_deref(), &ua, Creds::from_task(&snap))
            .await
        {
            Ok(i) => i,
            Err(_) => return true,
        };

        let mut changed = false;
        if let Some(cur) = info.etag.as_deref() {
            if !cur.is_empty() && cur != prev_etag {
                changed = true;
            }
        }
        if let (Some(a), Some(b)) = (snap.size, info.size) {
            if a != b {
                changed = true;
            }
        }
        if !changed {
            return true;
        }
        // 换用新的实体信息，避免下一轮又按旧长度/旧 ETag 发请求
        {
            let mut t = rt.task.lock();
            t.etag = info.etag.clone();
            if let Some(sz) = info.size {
                t.size = Some(sz);
            }
            if info.supports_range {
                t.supports_range = true;
            }
        }
        self.invalidate_progress(rt, path, "远端文件已变化，断点进度已作废，将重新下载");
        false
    }

    /// 落暂停断点：状态 / 字节数 / 速度写入并立即持久化（幂等，可多次调用）。
    ///
    /// `generation` 必须是当前运行的代号：过期 worker（超时未停、已被恢复/取消）
    /// 调到这里时既不落盘也不清 `running`，否则会覆盖用户刚做的决定。
    pub(crate) fn save_paused(&self, rt: &Arc<Runtime>, generation: u64) {
        if !rt.is_current(generation) {
            return;
        }
        let dl = rt.downloaded.load(Ordering::Relaxed);
        self.commit_state(rt, generation, TaskState::Paused, |t| {
            t.speed = 0;
            t.downloaded = dl;
        });
        // 状态写被状态机拒绝（例如已完成）时也要放开并发槽
        if rt.is_current(generation) {
            *rt.running.lock() = false;
        }
    }

    async fn run_task(
        self: Arc<Self>,
        id: &str,
        generation: u64,
        cancel: CancellationToken,
    ) -> Result<()> {
        let rt = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("任务不存在"))?;
        // 所有退出路径统一清理运行标记（幂等：finish_task / fail_task 已经清过也没关系）。
        // 只清自己这一代：恢复后的新 worker 不能被旧 worker 的收尾误伤。
        let _running_guard = RunningGuard {
            rt: rt.clone(),
            generation,
        };

        // FTP / SFTP 单流下载分支
        {
            let kind = rt.task.lock().kind;
            if kind == TaskKind::Ftp || kind == TaskKind::Sftp {
                self.commit_state(&rt, generation, TaskState::Downloading, |t| {
                    t.clear_error();
                    t.speed = 0;
                });
                return crate::remote::download(self.clone(), rt, generation, &cancel).await;
            }
        }

        // HLS（m3u8）：必须走分片 runner，否则会被当成普通文件把播放列表本身下下来
        if rt.task.lock().kind == TaskKind::Hls {
            let url = { rt.task.lock().url.clone() };
            let client = self.client(None)?;
            let media = crate::grab::parse_hls(&client, &url).await?;
            // 分片流无法按字节对齐续传，恢复时从 0 重写（truncate）
            rt.downloaded.store(0, Ordering::Relaxed);
            self.commit_state(&rt, generation, TaskState::Downloading, |t| {
                t.clear_error();
                t.speed = 0;
                t.downloaded = 0;
            });
            return match self
                .clone()
                .hls_run(&rt, generation, &media, &client, &cancel)
                .await
            {
                // 用户取消：落暂停并保存进度，不能当成错误。
                // 必须走 save_paused：它会清 `running`，否则并发槽被永久占住，
                // 下一轮 resume 会被「已在运行」直接吞掉（HLS 只能暂停一次）。
                Err(_) if cancel.is_cancelled() => {
                    self.save_paused(&rt, generation);
                    Ok(())
                }
                other => other,
            };
        }

        // 探测信息在 verify_remote 之后要重新读一次，这里只取不受影响的部分
        let (url, save_path, conns, task_ua) = {
            let t = rt.task.lock();
            (
                t.url.clone(),
                t.save_path.clone(),
                t.connections,
                t.user_agent.clone(),
            )
        };

        // 写入永远落在 `<最终名>.part`，完成时才 rename 成最终名：
        // 这样暂停/中断/失败都不会在目录里留下一个「看起来已完成」的半成品。
        let local_path = std::path::PathBuf::from(&save_path);
        let work_path = util::part_path(&local_path);
        migrate_legacy_part(&local_path, &work_path);

        // 续传前先核对本地文件身份：被删除 / 截短 / 替换过，旧分片进度一律作废，
        // 否则会跳过“已完成”的分片，留下一个稀疏文件冒充完成品。
        self.verify_resume(&rt, &work_path);
        // 再核对远端实体：断点期间服务器换了包，续传会把新旧字节拼成「长度对、内容错」的文件。
        // 作废后分片会被重新规划，所以下面要重新读取探测信息。
        self.verify_remote(&rt, &work_path).await;
        let (total, supports_range) = {
            let t = rt.task.lock();
            (t.size, t.supports_range)
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

        self.commit_state(&rt, generation, TaskState::Downloading, |t| {
            t.clear_error();
        });

        // 大小未知、0 字节、或明确不支持分段 → 单流下载
        if total.unwrap_or(0) == 0 || !supports_range {
            // 返回 false 表示用户取消（已保存断点），不能当作下载完成
            let completed = self.run_stream_single(&rt, generation, &cancel).await?;
            if !completed {
                return Ok(());
            }
            {
                let mut t = rt.task.lock();
                if t.size.is_none() {
                    t.size = Some(rt.downloaded.load(Ordering::Relaxed));
                }
            }
            self.finish_task(&rt, generation)?;
            return Ok(());
        }

        // 预分配文件（续传时不能截断）
        let file = Arc::new(
            OpenOptions::new()
                .create(true)
                .write(true)
                .read(true)
                .open(&work_path)
                .with_context(|| format!("无法创建文件: {}", work_path.display()))?,
        );
        let meta_len = file.metadata()?.len();
        if meta_len < total.unwrap() {
            file.set_len(total.unwrap())?;
        }
        // 记录本次进度对应的文件身份，供下次续传核对
        {
            let mut t = rt.task.lock();
            t.resume_anchor = util::file_anchor(&work_path);
        }

        // 起始已下载量
        let already: u64 = {
            let t = rt.task.lock();
            t.segments.iter().map(|s| s.done.min(s.len())).sum()
        };
        rt.downloaded.store(already, Ordering::Relaxed);
        if already >= total.unwrap() {
            self.finish_task(&rt, generation)?;
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
        // 复用 Runtime 中的任务限速器（若已有），以便运行中调速度能即时生效
        let task_limiter = {
            let existing = rt.task_limiter.lock().clone();
            match existing {
                Some(l) => {
                    l.set_limit(rt.task.lock().speed_limit);
                    l
                }
                None => {
                    let l = Arc::new(RateLimiter::new(rt.task.lock().speed_limit));
                    *rt.task_limiter.lock() = Some(l.clone());
                    l
                }
            }
        };
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
            let urls = Arc::new({
                let mut v = vec![url.clone()];
                let mirrors = { rt.task.lock().mirrors.clone() };
                v.extend(mirrors);
                v
            });
            let mirror_start = seg.index % urls.len();
            let file = file.clone();
            let limiter = task_limiter.clone();
            let global = self.global_limiter.clone();
            let tx = tx.clone();
            let (referer, cred_origin, cookie, basic) = {
                let t = rt.task.lock();
                let basic = match (t.username.as_deref(), t.password.as_deref()) {
                    (Some(u), Some(p)) if !u.is_empty() => Some((u.to_string(), p.to_string())),
                    _ => None,
                };
                let origin = if t.auth_origin.is_empty() {
                    t.url.clone()
                } else {
                    t.auth_origin.clone()
                };
                (t.referer.clone(), origin, t.cookie.clone(), basic)
            };
            // ETag 也用任务当前值：verify_remote 可能刚换过实体
            let etag = { rt.task.lock().etag.clone() };
            let cancel2 = cancel.clone();
            let seg_ua = task_ua.clone();
            set.spawn(async move {
                download_segment(
                    SegmentJob {
                        client,
                        urls,
                        mirror_start,
                        index: seg.index,
                        cursor: seg.cursor(),
                        end: seg.end,
                        total,
                        file,
                        limiter,
                        global,
                        tx,
                        etag,
                        referer,
                        cred_origin,
                        cookie,
                        basic,
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

        if rt.is_current(generation) {
            *rt.running.lock() = false;
        }

        // 取消判定必须以令牌为准，不能只看 `cancelled` 标志：
        // `select!` 的两个分支可能同时就绪，取消时各分片迅速收工、通道随即关闭，
        // 若先命中 `rx.recv() == None` 分支，cancelled 仍是 false，
        // 半截文件就会被当成下载完成（数据丢失，进度还会被改写成总长度）。
        if cancelled || cancel.is_cancelled() {
            self.save_paused(&rt, generation);
            return Ok(());
        }

        if let Some(e) = first_err {
            // 分片出错：若已有进度，保留为错误态以便重试续传。
            // 过期 worker 的失败不能覆盖用户刚点的暂停 / 新一轮运行。
            self.fail_task(id, Some(generation), &e);
            return Ok(());
        }

        // 兜底：所有分片都真的写满才允许标记完成。
        // 通道提前关闭但既没取消也没错误时，宁可回落暂停保留断点。
        let unfinished = {
            let t = rt.task.lock();
            t.segments.iter().filter(|s| !s.is_done()).count()
        };
        if unfinished > 0 {
            self.save_paused(&rt, generation);
            return Ok(());
        }

        self.finish_task(&rt, generation)?;
        Ok(())
    }

    /// 无 Range 支持 / 大小未知时的单流下载
    /// 返回 true = 正常完成；false = 用户取消（断点已保存）
    async fn run_stream_single(
        self: &Arc<Self>,
        rt: &Arc<Runtime>,
        generation: u64,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let (url, save_path, referer, task_snapshot) = {
            let t = rt.task.lock();
            (
                t.url.clone(),
                t.save_path.clone(),
                t.referer.clone(),
                t.clone(),
            )
        };
        let client = self.client(Some(&task_snapshot))?;
        // 断点可信性：文件被删 / 截短 / 替换后不能按旧偏移发 Range，只能从头来
        let work_path = util::part_path(std::path::Path::new(&save_path));
        migrate_legacy_part(std::path::Path::new(&save_path), &work_path);
        let start = if self.verify_resume(rt, &work_path) {
            rt.downloaded.load(Ordering::Relaxed)
        } else {
            0
        };
        let mut req = client.get(&url);
        if start > 0 {
            req = req.header(RANGE, format!("bytes={start}-"));
        }
        if let Some(r) = referer {
            req = req.header(REFERER, r);
        }
        // 重定向终址可能已跨域：凭据只在原始 origin 才发送
        let creds = Creds::from_task(&task_snapshot);
        let req = creds.apply(req, &url);
        let resp = req.send().await.context("请求失败")?;
        let status = resp.status();
        // 请求了 Range 但服务器回 200（不支持续传）：必须从头写，否则文件会错位
        let resumed_by_server = status == StatusCode::PARTIAL_CONTENT;
        if resumed_by_server {
            // 206 必须正是请求的那个起点，且总长度与探测结果一致，
            // 否则说明服务器给了别的区间，按旧偏移落盘会写出错位数据
            let (expected_total, expected_etag) = {
                let t = rt.task.lock();
                (t.size, t.etag.clone())
            };
            check_partial_content(
                &resp,
                start,
                None,
                expected_total,
                expected_etag.as_deref(),
            )?;
        } else if !status.is_success() {
            bail!("服务器返回 {}", status.as_u16());
        }
        let mut offset = if start > 0 && resumed_by_server { start } else { 0 };

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&work_path)?;
        {
            let mut t = rt.task.lock();
            t.resume_anchor = util::file_anchor(&work_path);
        }

        let mut stream = resp.bytes_stream();
        // 服务器不支持续传时，从头覆盖
        if offset == 0 && start > 0 {
            file.set_len(0)?;
        }

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    rt.downloaded.store(offset, Ordering::Relaxed);
                    self.save_paused(rt, generation);
                    return Ok(false);
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            let n = bytes.len() as u64;
                            // 取消安全：暂停时不再卡在限速 sleep 里
                            if !self.global_limiter.acquire_cancellable(n, cancel).await {
                                rt.downloaded.store(offset, Ordering::Relaxed);
                                self.save_paused(rt, generation);
                                return Ok(false);
                            }
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
        // 取消与流结束可能同时发生：以令牌为准，绝不能把半截文件当完成
        if cancel.is_cancelled() {
            rt.downloaded.store(offset, Ordering::Relaxed);
            self.save_paused(rt, generation);
            return Ok(false);
        }
        // 已知总长度时，字节数对不上同样不能算完成（连接可能被中途关闭）
        let expected = { rt.task.lock().size };
        if let Some(total) = expected {
            if offset < total {
                rt.downloaded.store(offset, Ordering::Relaxed);
                self.save_paused(rt, generation);
                return Ok(false);
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
        Ok(true)
    }

    pub(crate) fn finish_task(&self, rt: &Arc<Runtime>, generation: u64) -> Result<()> {
        // 过期 worker（已被恢复 / 取消 / 删除）不得提交：
        // 否则它会把新一轮正在写的 `.part` 改名，或者把暂停的任务标成已完成。
        if !rt.is_current(generation) {
            return Ok(());
        }
        // 先落地再改状态：rename 失败（磁盘满/权限/被杀软占用）时任务必须是 Error，
        // 不能出现「状态 Completed 但文件还叫 xxx.part」这种自相矛盾的结果。
        {
            let (path, kind) = {
                let t = rt.task.lock();
                (t.save_path.clone(), t.kind)
            };
            // BT 的文件由 rqbit 自己管理（多文件/DHT 目录），不参与单文件 rename
            if kind != TaskKind::Bt {
                commit_part(Path::new(&path))
                    .with_context(|| format!("保存下载文件失败: {path}"))?;
            }
        }
        let dl = rt.downloaded.load(Ordering::Relaxed);
        let mut reschedule = false;
        let completed = self.commit_state(rt, generation, TaskState::Completed, |t| {
            let total = t.size.unwrap_or(dl);
            // 确保分片状态与总量一致
            let sum: u64 = t.segments.iter().map(|s| s.done.min(s.len())).sum();
            t.downloaded = if t.segments.is_empty() {
                dl
            } else {
                sum.max(total)
            };
            if !t.segments.is_empty() {
                t.downloaded = total;
            }
            t.completed_at = Some(now_ms());
            t.speed = 0;
            t.clear_error();
            reschedule = t.interval_secs > 0;
        });
        if rt.is_current(generation) {
            *rt.running.lock() = false;
        }
        if !completed {
            return Ok(());
        }

        // 周期性同步：完成后按间隔重新调度（保持文件始终最新）
        if reschedule {
            let now = now_ms();
            self.commit_state(rt, generation, TaskState::Scheduled, |t| {
                t.scheduled_at = Some(now + t.interval_secs as i64 * 1000);
                // 清零进度等待下一轮；**保留已下载文件**，到点后按 ETag/大小
                // 重新校验，内容未变化则无需重下（避免同步期间文件消失）
                t.downloaded = 0;
                t.segments.clear();
                t.completed_at = Some(now);
            });
            rt.downloaded.store(0, Ordering::Relaxed);
        } else {
            // 全部任务都完成 → 触发「下载后动作」（关机 / 睡眠）
            self.maybe_after_complete();
        }
        Ok(())
    }

    /// 所有任务都处于终态时，按设置触发系统动作
    fn maybe_after_complete(&self) {
        let action = self.settings.read().after_complete.clone();
        if action != "shutdown" && action != "sleep" {
            return;
        }
        let all_done = {
            let map = self.tasks.lock();
            !map.is_empty()
                && map.values().all(|r| {
                    let t = r.task.lock();
                    t.state.is_terminal()
                })
        };
        if all_done {
            self.emit(EngineEvent::SystemAction(action));
            self.emit(EngineEvent::Notice("全部下载完成，即将执行关机/睡眠动作".into()));
        }
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
                // 进程重启后 BT 会话是空的：先把持久化任务重新挂回去，再轮询状态
                me.bt_restore_pending().await;
                me.bt_update_all();

                let now = std::time::Instant::now();
                let persist_due = last_persist.elapsed().as_secs() >= 2;
                if persist_due {
                    last_persist = now;
                }

                // 只对「正在运行」的任务做深克隆与进度广播；
                // 历史/已完成任务只保留一份缓存，不再每 500ms 复制整个 Task。
                let running_list: Vec<(TaskId, Arc<Runtime>, u64)> = {
                    let map = me.tasks.lock();
                    map.iter()
                        .filter(|(_, r)| *r.running.lock())
                        .map(|(k, r)| {
                            (k.clone(), r.clone(), r.downloaded.load(Ordering::Relaxed))
                        })
                        .collect()
                };
                // 首次出现（或从非运行态转入）的任务，广播一次当前快照，保证 UI 不错过
                let fresh: Vec<(TaskId, Arc<Runtime>)> = {
                    let map = me.tasks.lock();
                    map.iter()
                        .filter(|(k, r)| !last.contains_key(*k) && !*r.running.lock())
                        .map(|(k, r)| (k.clone(), r.clone()))
                        .collect()
                };
                for (id, rt) in fresh {
                    let mut t = rt.task.lock();
                    t.speed = 0;
                    t.downloaded = rt.downloaded.load(Ordering::Relaxed);
                    let snap = t.clone();
                    drop(t);
                    last.insert(id, (snap.downloaded, now));
                    me.emit(EngineEvent::TaskUpdated(snap));
                }
                for (id, rt, dl) in running_list {
                    let progress = {
                        let mut t = rt.task.lock();
                        t.downloaded = dl;
                        if let Some((pdl, pt)) = last.get(&id).copied() {
                            let dt = now.duration_since(pt).as_secs_f64();
                            if dt > 0.05 {
                                t.speed = ((dl.saturating_sub(pdl)) as f64 / dt) as u64;
                            }
                        }
                        // 关键：取快照与入队必须在**同一把任务锁内**完成。
                        // 否则 pause() 恰好在两者之间把状态改成 Paused 并落盘，
                        // 这条带着 Downloading 的旧快照随后写入，会把暂停覆盖回下载中。
                        if persist_due && t.state == TaskState::Downloading {
                            me.store.enqueue_save_owned(t.clone());
                        }
                        t.progress_snapshot()
                    };
                    last.insert(id, (dl, now));
                    // 进度只发增量字段，静态信息（URL/镜像/凭据）不再重复传输
                    me.emit(EngineEvent::TaskProgress(Box::new(progress)));
                }

                // 清理已不存在的任务的速度记录（一次加锁完成）
                {
                    let map = me.tasks.lock();
                    last.retain(|k, _| map.contains_key(k));
                }

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
    /// 主地址 + 镜像池（轮转分配实现多源并行）
    urls: Arc<Vec<String>>,
    /// 本分片起始使用的镜像下标（= 分片序号对镜像数取模）
    mirror_start: usize,
    index: usize,
    cursor: u64,
    end: u64,
    /// 探测得到的总长度（未知为 None），用于核对 Content-Range 的 /total
    total: Option<u64>,
    file: Arc<File>,
    limiter: Arc<RateLimiter>,
    global: Arc<RateLimiter>,
    tx: tokio::sync::mpsc::Sender<(usize, u64)>,
    etag: Option<String>,
    referer: Option<String>,
    /// 认证归属 origin：镜像地址与它不一致时不发送 cookie / basic
    cred_origin: String,
    cookie: Option<String>,
    basic: Option<(String, String)>,
    ua: Option<String>,
}

/// 删除下载产物：分片下载直接写目标文件，未完成的也是同一个路径；
/// Windows 上文件句柄可能还没释放，所以带几次退避重试。
///
/// 安全边界：只删除“单个文件”。绝不对目录做递归删除——BT 的顶层名称可能
/// 与用户原有同名目录重合，`remove_dir_all` 会越出任务自有产物范围。
/// 把 `<最终名>.part` 落地为最终文件名。
///
/// 同盘 rename 是原子的：目录里要么是旧的完整文件，要么是新的完整文件，
/// 不会出现「一半新一半旧」。目标可能还留着建任务时预占的 0 字节占位文件，
/// Windows 下 std 的 rename 走 MoveFileExW(MOVEFILE_REPLACE_EXISTING)，可覆盖。
fn commit_part(final_path: &Path) -> Result<()> {
    let part = util::part_path(final_path);
    if !part.exists() {
        // 没有 .part：可能是重复调用（周期同步/已提交过），或协议自己写了最终文件
        return Ok(());
    }
    std::fs::rename(&part, final_path).map_err(|e| {
        FellError::from(e).context(format!(
            "无法把 {} 重命名为 {}",
            part.display(),
            final_path.display()
        ))
    })?;
    Ok(())
}

/// 老版本直接往最终文件名里写：升级后首次续传时把已有的部分数据搬到 `.part`，
/// 避免把用户已经下了一半的进度当垃圾丢掉。
fn migrate_legacy_part(final_path: &Path, part: &Path) {
    if part.exists() || final_path == part {
        return;
    }
    let Ok(meta) = std::fs::metadata(final_path) else {
        return;
    };
    if !meta.is_file() || meta.len() == 0 {
        return;
    }
    if let Err(e) = std::fs::rename(final_path, part) {
        eprintln!("[migrate] 无法把旧的部分文件搬到 .part（将从头重下）: {e}");
    }
}

async fn remove_downloaded(path: &str) {
    if path.trim().is_empty() {
        return;
    }
    // 未完成的下载落在 `<最终名>.part`：删任务必须连临时文件一起清，
    // 否则目录里会留下一堆没人认领的半成品
    let part = util::part_path(std::path::Path::new(path));
    let _ = std::fs::remove_file(&part);
    for i in 0..8u64 {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => {
                // 目录不是本函数的删除对象：交给协议自己的受控清单处理
                eprintln!("[delete] 跳过目录（不做递归删除）: {path}");
                return;
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(60 * (i + 1))).await;
                continue;
            }
        }
        match std::fs::remove_file(path) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(60 * (i + 1))).await,
        }
    }
    eprintln!("[delete] 文件删除失败（已重试）: {path}");
}

/// 校验 206 响应的 `Content-Range` 是否与本次请求严格对应。
///
/// 只信状态码是危险的：起点被换成 0、区间被截短、总长度变了都仍可能返回 206，
/// 数据会被写进错误偏移，最后得到一个长度对、内容乱的成品。这里逐项核对
/// 起点 / 终点（不得越过请求区间）/ 总长度（若已知），任何一项不符都报错重试。
fn check_partial_content(
    resp: &reqwest::Response,
    expected_start: u64,
    expected_end: Option<u64>,
    expected_total: Option<u64>,
    expected_etag: Option<&str>,
) -> Result<()> {
    // 有的服务器会忽略 If-Range 照旧回 206：区间看着对，实体却已经换了。
    // 这种情况只有比对响应 ETag 才能发现，否则会把新旧字节拼在一起。
    if let Some(want) = expected_etag.filter(|s| !s.is_empty()) {
        if let Some(got) = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim_matches('"'))
        {
            if !got.is_empty() && got != want {
                return Err(FellError::new(
                    crate::error::ErrorKind::Integrity,
                    format!("远端文件已变化（ETag 期望 \"{want}\"，实际 \"{got}\"），不能续传"),
                )
                .into());
            }
        }
    }
    let Some(cr) = resp
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
    else {
        return Err(FellError::new(
            crate::error::ErrorKind::Integrity,
            "服务器返回 206 但缺少 Content-Range，无法确认数据区间",
        )
        .into());
    };
    let Some((start, end, total)) = util::parse_content_range(cr) else {
        return Err(FellError::new(
            crate::error::ErrorKind::Integrity,
            format!("Content-Range 无法解析: {cr}"),
        )
        .into());
    };
    if end < start {
        return Err(FellError::new(
            crate::error::ErrorKind::Integrity,
            format!("Content-Range 区间非法（{start}-{end}）"),
        )
        .into());
    }
    if start != expected_start {
        return Err(FellError::new(
            crate::error::ErrorKind::Integrity,
            format!("服务器返回的区间起点不符（期望 {expected_start}，实际 {start}）"),
        )
        .into());
    }
    if let Some(want_end) = expected_end {
        if end > want_end {
            return Err(FellError::new(
                crate::error::ErrorKind::Integrity,
                format!("服务器返回的区间超出请求（请求到 {want_end}，实际到 {end}）"),
            )
            .into());
        }
    }
    if let (Some(want), Some(got)) = (expected_total, total) {
        if want != got {
            return Err(FellError::new(
                crate::error::ErrorKind::Integrity,
                format!("服务器声明的总长度不一致（期望 {want}，实际 {got}）"),
            )
            .into());
        }
    }
    if let (Some(e), Some(t)) = (expected_end, total) {
        if t <= e {
            return Err(FellError::new(
                crate::error::ErrorKind::Integrity,
                format!("Content-Range 总长度 {t} 不足以覆盖请求终点 {e}"),
            )
            .into());
        }
    }
    Ok(())
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
        // 每次重试轮换镜像：不同分片起点不同 → 多镜像同时参与下载；
        // 某镜像挂了自动切到下一个。
        let mirror_idx = (job.mirror_start + attempt as usize) % job.urls.len().max(1);
        match fetch_segment(&job, &mut cursor, mirror_idx, &cancel).await {
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
    mirror_idx: usize,
    cancel: &CancellationToken,
) -> Result<()> {
    let client = job.client.clone();
    let url = &job.urls[mirror_idx % job.urls.len()];
    let mut req = client
        .get(url)
        .header(RANGE, format!("bytes={}-{}", *cursor, job.end))
        .header(USER_AGENT, job.ua.clone().unwrap_or_else(|| user_agent_hdr().to_string()));
    if let Some(etag) = &job.etag {
        req = req.header(reqwest::header::IF_RANGE, format!("\"{etag}\""));
    }
    if let Some(r) = &job.referer {
        req = req.header(REFERER, r);
    }
    // 只有与任务原始 origin 一致的地址（主站）才携带凭据；镜像站一律不带
    let creds = Creds::new(
        &job.cred_origin,
        job.cookie.as_deref(),
        job.basic.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
    );
    let req = creds.apply(req, url);
    let resp = req.send().await.map_err(FellError::from)?;
    let status = resp.status();

    if status == StatusCode::OK {
        // 服务器忽略了 Range（If-Range 校验失败或本身不支持）
        return Err(FellError::new(
            crate::error::ErrorKind::Unsupported,
            "服务器不支持分段（返回 200），请减少连接数后重试",
        )
        .into());
    }
    if status != StatusCode::PARTIAL_CONTENT {
        return Err(FellError::http(status.as_u16(), format!("分片请求失败: HTTP {}", status.as_u16())).into());
    }
    // 206 必须与请求区间严格对应，否则写入位置会错位
    check_partial_content(&resp, *cursor, Some(job.end), job.total, job.etag.as_deref())?;

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
                        // 取消安全：暂停时退还预约，不留下「未来债务」阻塞其他任务
                        if !job.global.acquire_cancellable(part.len() as u64, cancel).await {
                            return Ok(());
                        }
                        if !job.limiter.acquire_cancellable(part.len() as u64, cancel).await {
                            return Ok(());
                        }
                        write_at(&job.file, part, *cursor).map_err(FellError::from)?;
                        *cursor += part.len() as u64;
                        if job.tx.send((job.index, part.len() as u64)).await.is_err() {
                            // 接收端已关闭（取消/收工），正常退出
                            return Ok(());
                        }
                        if *cursor > job.end {
                            return Ok(());
                        }
                    }
                    Some(Err(e)) => {
                        return Err(FellError::from(e).context("读取数据失败").into());
                    }
                    None => {
                        return Err(FellError::new(
                            crate::error::ErrorKind::Network,
                            format!("连接提前结束（{}/{}）", *cursor, job.end + 1),
                        )
                        .into());
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

    /// 凭据只发给原始 origin：镜像、换端口、http/https 互换都收不到 Cookie/Basic。
    #[test]
    fn creds_only_sent_to_original_origin() {
        use reqwest::header::AUTHORIZATION;
        let client = Client::new();
        let creds = Creds::new(
            "https://example.com/dir/file.zip",
            Some("sid=abc"),
            Some(("user", "pass")),
        );

        let build = |url: &str| {
            creds
                .apply(client.get(url), url)
                .build()
                .expect("请求应可构造")
        };

        // 同 origin（路径不同）→ 带凭据
        let same = build("https://example.com/other/path");
        assert_eq!(same.headers().get(COOKIE).unwrap(), "sid=abc");
        assert!(same.headers().get(AUTHORIZATION).is_some());

        // 不同 origin → 完全不带凭据
        for url in [
            "https://mirror.example/file.zip",
            "https://example.com:8443/file.zip",
            "http://example.com/file.zip",
            "https://cdn.example.com/file.zip",
        ] {
            let r = build(url);
            assert!(r.headers().get(COOKIE).is_none(), "{url} 不应收到 Cookie");
            assert!(r.headers().get(AUTHORIZATION).is_none(), "{url} 不应收到 Basic");
        }
    }

    /// 删除只允许作用于单个文件：目录（可能含用户原有内容）必须原样保留。
    #[tokio::test]
    async fn remove_downloaded_skips_directories() {
        let root = std::env::temp_dir().join(format!("felldl-del-{}", uuid::Uuid::new_v4()));
        let keep_dir = root.join("pre-existing");
        std::fs::create_dir_all(&keep_dir).unwrap();
        let sentinel = keep_dir.join("keep.txt");
        std::fs::write(&sentinel, b"do not touch").unwrap();
        let file = root.join("payload.bin");
        std::fs::write(&file, b"data").unwrap();

        remove_downloaded(file.to_string_lossy().as_ref()).await;
        assert!(!file.exists(), "普通文件应被删除");

        remove_downloaded(keep_dir.to_string_lossy().as_ref()).await;
        assert!(sentinel.exists(), "目录及其内容不得被递归删除");

        std::fs::remove_dir_all(&root).ok();
    }

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

    // ───────────── 状态机 / 运行代号 ─────────────

    fn test_task(id: &str, state: TaskState) -> Task {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "url": "https://example.com/f.bin",
            "file_name": "f.bin",
            "save_dir": "/tmp",
            "save_path": "/tmp/f.bin",
            "state": state,
        }))
        .expect("最小任务 JSON 应能反序列化")
    }

    /// 终态不能被「迟到」的写入打回去。
    #[test]
    fn terminal_states_reject_backwards_transitions() {
        use TaskState::*;
        for to in [Paused, Downloading, Queued] {
            assert!(!Completed.can_transition(to), "已完成 -> {to:?} 必须被拒绝");
        }
        for to in [Downloading, Paused, Completed] {
            assert!(!Error.can_transition(to), "出错 -> {to:?} 必须被拒绝");
        }
        // 正常流程必须放行
        assert!(Downloading.can_transition(Paused));
        assert!(Paused.can_transition(Probing));
        assert!(Probing.can_transition(Downloading));
        assert!(Downloading.can_transition(Completed));
        assert!(Completed.can_transition(Scheduled));
        assert!(Error.can_transition(Probing));
        // 同状态重复写入（幂等保存）永远允许
        for s in [
            Queued,
            Probing,
            Downloading,
            Paused,
            Completed,
            Error,
            Scheduled,
        ] {
            assert!(s.can_transition(s), "{s:?} 重复写入应允许");
        }
    }

    fn engine_in_temp(tag: &str) -> (Arc<Engine>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "felldl-gen-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (Engine::new(&dir).expect("引擎初始化失败"), dir)
    }

    /// 过期代（恢复/取消之前的 worker）不得改状态、不得清 running。
    #[tokio::test]
    async fn stale_generation_cannot_write_state() {
        let (engine, dir) = engine_in_temp("stale");
        let rt = Runtime::new(test_task("t1", TaskState::Downloading));
        *rt.running.lock() = true;
        let stale = rt.next_generation();
        let fresh = rt.next_generation();
        assert!(!rt.is_current(stale), "被取代的代号必须失效");
        assert!(rt.is_current(fresh));

        let wrote = engine.commit_state(&rt, stale, TaskState::Paused, |t| t.speed = 0);
        assert!(!wrote, "过期代的状态写入应被丢弃");
        assert_eq!(
            rt.task.lock().state,
            TaskState::Downloading,
            "状态不得被改动"
        );
        assert!(
            *rt.running.lock(),
            "过期代不得清 running（否则并发槽被重复占用）"
        );

        let wrote = engine.commit_state(&rt, fresh, TaskState::Paused, |t| t.speed = 0);
        assert!(wrote, "当前代应能正常写状态");
        assert_eq!(rt.task.lock().state, TaskState::Paused);
        assert!(engine.store.is_dead("t1") == false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 已删除的任务：任何代号都不能把它写回来。
    #[tokio::test]
    async fn deleted_task_rejects_state_writes() {
        let (engine, dir) = engine_in_temp("deleted");
        let rt = Runtime::new(test_task("gone", TaskState::Downloading));
        let gen = rt.next_generation();
        *rt.running.lock() = true;
        engine.store.delete_task("gone");

        let wrote = engine.commit_state(&rt, gen, TaskState::Paused, |t| t.downloaded = 1);
        assert!(!wrote, "已删除任务的状态写入应被丢弃");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
