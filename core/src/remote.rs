//! FTP / FTPS / SFTP 单流下载
//!
//! 结构：worker（blocking 线程或 tokio 任务）负责连接与读流，直接写文件、
//! 推进 `rt.downloaded` 与任务持久化；外层 `download` 只负责等待完成或取消
//! （取消时置位 flag，pause() 负责落 Paused 状态）。
//! 断点续传：FTP 用 REST（resume_transfer），SFTP 用 seek，起始偏移取
//! `rt.downloaded`（任务持久化中已保存）。

use crate::engine::{write_at, Engine, Runtime};
use crate::model::*;
use anyhow::{anyhow, bail, Context, Result};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
struct RemoteTarget {
    host: String,
    port: u16,
    path: String,
    username: String,
    password: String,
}

fn parse_remote_url(url: &str, default_port: u16) -> Result<RemoteTarget> {
    let lower = url.to_lowercase();
    let scheme_len = if lower.starts_with("sftp://") || lower.starts_with("ftps://") {
        7
    } else {
        6 // ftp://
    };
    let rest = &url[scheme_len..];
    let (authority, path) = match rest.find('/') {
        Some(p) => (&rest[..p], rest[p..].to_string()),
        None => bail!("URL 缺少文件路径"),
    };
    let authority = authority.split('@').next_back().unwrap_or(authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.parse().context("端口无效")?)
        }
        _ => (authority.to_string(), default_port),
    };
    if host.is_empty() {
        bail!("URL 缺少主机名");
    }
    Ok(RemoteTarget {
        host,
        port,
        path: if path.is_empty() { "/".into() } else { path },
        username: String::new(),
        password: String::new(),
    })
}

/// worker 完成状态
#[derive(Debug)]
pub(crate) enum Outcome {
    /// 正常完成
    Done,
    /// 被取消（暂停）
    Cancelled,
}

const CHUNK: usize = 64 * 1024;

/// FTP 下载 worker（阻塞式，跑在 spawn_blocking）
fn ftp_worker<S>(
    mut ftp: suppaftp::ImplFtpStream<S>,
    target: &RemoteTarget,
    start: u64,
    file: &std::fs::File,
    engine: &Arc<Engine>,
    rt: &Arc<Runtime>,
    cancel: &AtomicBool,
    limit: Option<u64>,
) -> Result<Outcome>
where
    S: suppaftp::TlsStream,
{
    // 必须显式 TYPE I：RFC 959 默认是 ASCII，服务器会把 LF 翻译成 CRLF，
    // 二进制文件（exe/zip/图片）会被悄悄改写，长度也对不上。
    ftp.transfer_type(suppaftp::types::FileType::Binary)
        .map_err(|e| anyhow!("切换到二进制传输模式失败: {e}"))?;

    // 文件大小
    let size = ftp.size(&target.path).ok().map(|s| s as u64);
    {
        let mut t = rt.task.lock();
        if let Some(sz) = size {
            t.size = Some(sz);
        }
    }

    if start > 0 {
        if let Some(sz) = size {
            if start >= sz {
                return Ok(Outcome::Done);
            }
        }
        ftp.resume_transfer(start as usize)
            .map_err(|e| anyhow!("REST 续传失败: {e}"))?;
    }

    let mut stream = ftp
        .retr_as_stream(&target.path)
        .map_err(|e| anyhow!("RETR 失败: {e}"))?;

    let mut buf = vec![0u8; CHUNK];
    let mut offset = start;
    let mut last_persist = std::time::Instant::now();
    let mut window = std::time::Instant::now();
    let mut window_bytes = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Cancelled);
        }
        let n = match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(anyhow!("FTP 传输中断: {e}")),
        };
        write_at(file, &buf[..n], offset).context("写文件失败")?;
        offset += n as u64;
        rt.downloaded.store(offset, Ordering::Relaxed);

        // 同步限速：按窗口时间配速
        if let Some(lps) = limit {
            window_bytes += n as u64;
            let expected = std::time::Duration::from_secs_f64(window_bytes as f64 / lps as f64);
            let elapsed = window.elapsed();
            if elapsed < expected {
                std::thread::sleep(expected - elapsed);
            }
            if window.elapsed().as_secs() > 10 {
                window = std::time::Instant::now();
                window_bytes = 0;
            }
        }

        // 每 2 秒持久化一次进度（进程被杀也能续传）——走合并队列避免阻塞
        if last_persist.elapsed().as_secs() >= 2 {
            last_persist = std::time::Instant::now();
            let mut t = rt.task.lock();
            t.downloaded = offset;
            let snap = t.clone();
            drop(t);
            engine.store.enqueue_save(&snap);
        }
    }
    // 完成应答必须检查：服务器回 426 时数据可能已收到一部分甚至全部，
    // 但传输是被判定失败的，不能当成下载成功（此前错误被 `let _ =` 丢掉了）。
    stream
        .finish()
        .map_err(|e| anyhow!("FTP 传输未正常结束（完成应答失败）: {e}"))?;
    // 已知长度时校验实收字节：服务器提前关流会走到这里
    if let Some(sz) = size {
        if offset < sz {
            bail!("FTP 传输提前结束：只收到 {offset} / {sz} 字节");
        }
    }
    let _ = ftp.quit();
    Ok(Outcome::Done)
}

async fn sftp_worker(
    target: RemoteTarget,
    start: u64,
    file: &std::fs::File,
    engine: &Arc<Engine>,
    rt: &Arc<Runtime>,
    cancel: CancellationToken,
    limit: Option<u64>,
) -> Result<Outcome> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let config = Arc::new(russh::client::Config::default());
    // 主机密钥按 host+port 记忆在应用自己的 known_hosts 里：
    // 首次连接记录（TOFU），之后密钥变化直接拒绝，避免把口令发给冒充的服务器。
    let handler = SshHandler {
        host: target.host.clone(),
        port: target.port,
        known_hosts: engine.store.root.join("known_hosts"),
    };
    let mut session = russh::client::connect(config, (target.host.as_str(), target.port), handler)
        .await
        .map_err(|e| anyhow!("连接 SFTP 服务器失败: {e}"))?;
    let ok = session
        .authenticate_password(&target.username, &target.password)
        .await
        .map_err(|e| anyhow!("SFTP 认证失败: {e}"))?;
    if !ok.success() {
        bail!("SFTP 认证被拒绝（用户名或密码错误）");
    }
    let channel = session
        .channel_open_session()
        .await
        .map_err(|e| anyhow!("打开 SSH 通道失败: {e}"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| anyhow!("请求 sftp 子系统失败: {e}"))?;
    let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| anyhow!("初始化 SFTP 会话失败: {e}"))?;
    sftp.set_timeout(30);

    let meta = sftp
        .metadata(&target.path)
        .await
        .map_err(|e| anyhow!("获取远端文件信息失败: {e}"))?;
    let size = meta.len();
    {
        let mut t = rt.task.lock();
        t.size = Some(size);
    }

    if start > 0 && start >= size {
        sftp.close().await.ok();
        return Ok(Outcome::Done);
    }

    let mut remote = sftp
        .open(&target.path)
        .await
        .map_err(|e| anyhow!("打开远端文件失败: {e}"))?;
    if start > 0 {
        remote
            .seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| anyhow!("SFTP seek 失败: {e}"))?;
    }

    let mut buf = vec![0u8; CHUNK];
    let mut offset = start;
    let mut last_persist = std::time::Instant::now();
    let mut window = std::time::Instant::now();
    let mut window_bytes = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Ok(Outcome::Cancelled);
        }
        let n = match remote.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(anyhow!("SFTP 传输中断: {e}")),
        };
        write_at(file, &buf[..n], offset).context("写文件失败")?;
        offset += n as u64;
        rt.downloaded.store(offset, Ordering::Relaxed);

        if let Some(lps) = limit {
            window_bytes += n as u64;
            let expected = std::time::Duration::from_secs_f64(window_bytes as f64 / lps as f64);
            let elapsed = window.elapsed();
            if elapsed < expected {
                tokio::time::sleep(expected - elapsed).await;
            }
            if window.elapsed().as_secs() > 10 {
                window = std::time::Instant::now();
                window_bytes = 0;
            }
        }

        if last_persist.elapsed().as_secs() >= 2 {
            last_persist = std::time::Instant::now();
            let mut t = rt.task.lock();
            t.downloaded = offset;
            let snap = t.clone();
            drop(t);
            engine.store.enqueue_save(&snap);
        }
    }
    if offset < size {
        sftp.close().await.ok();
        bail!("SFTP 传输提前结束：只收到 {offset} / {size} 字节");
    }
    sftp.close().await.ok();
    Ok(Outcome::Done)
}

/// 主入口：在 run_task 的 FTP/SFTP 分支调用
pub(crate) async fn download(
    engine: Arc<Engine>,
    rt: Arc<Runtime>,
    cancel: &CancellationToken,
) -> Result<()> {
    let (kind, url, save_path, user, pass, speed_limit, start) = {
        let t = rt.task.lock();
        (
            t.kind,
            t.url.clone(),
            t.save_path.clone(),
            t.username.clone().unwrap_or_default(),
            t.password.clone().unwrap_or_default(),
            t.speed_limit,
            rt.downloaded.load(Ordering::Relaxed),
        )
    };
    let default_port = if kind == TaskKind::Sftp { 22 } else { 21 };
    let mut target = parse_remote_url(&url, default_port).map_err(|e| anyhow!("{e:#}"))?;
    if kind == TaskKind::Sftp {
        if user.is_empty() {
            bail!("SFTP 需要用户名（在新建任务里填写）");
        }
        target.username = user;
        target.password = pass;
    } else if !user.is_empty() {
        target.username = user;
        target.password = pass;
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .open(&save_path)
        .with_context(|| format!("无法创建文件: {save_path}"))?;
    if start == 0 {
        let _ = file.set_len(0);
    }

    // 取全局与单任务限速的较小者（worker 内同步配速）
    let s = engine.settings();
    let limit = {
        let g = if s.global_speed_limit > 0 {
            s.global_speed_limit
        } else {
            u64::MAX
        };
        let t = if speed_limit > 0 { speed_limit } else { u64::MAX };
        let min = g.min(t);
        if min == u64::MAX { None } else { Some(min) }
    };

    let outcome = if kind == TaskKind::Sftp {
        let cancel2 = cancel.clone();
        let engine2 = engine.clone();
        let rt2 = rt.clone();
        let joined = tokio::spawn(async move {
            sftp_worker(target, start, &file, &engine2, &rt2, cancel2, limit).await
        })
        .await;
        match joined {
            Ok(inner) => inner.map_err(|e| anyhow!("SFTP 任务异常: {e:#}"))?,
            Err(e) => return Err(anyhow!("SFTP 任务异常: {e}")),
        }
    } else {
        let flag = Arc::new(AtomicBool::new(false));
        let flag2 = flag.clone();
        let rt2 = rt.clone();
        let engine2 = engine.clone();
        let secure = url.to_lowercase().starts_with("ftps://");
        let handle = tokio::task::spawn_blocking(move || {
            ftp_connect_and_run(target, secure, start, &file, &engine2, &rt2, &flag2, limit)
        });
        // 桥接：blocking worker 无论何时退出，都兜底清理 running 与任务状态，
        // 避免暂停路径因 worker 卡在阻塞 IO 里而永久占用并发槽。
        let bridge_cancel = cancel.clone();
        let rt3 = rt.clone();
        let engine3 = engine.clone();
        let flag3 = flag.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<Outcome>>();
        tokio::spawn(async move {
            let res = match handle.await {
                Ok(inner) => inner,
                Err(e) => Err(anyhow!("FTP 任务异常: {e}")),
            };
            // worker 已真正退出：清理运行标记；若是取消触发的退出，落 Paused
            *rt3.running.lock() = false;
            if flag3.load(Ordering::Relaxed) || bridge_cancel.is_cancelled() {
                let mut t = rt3.task.lock();
                if t.state.is_active() {
                    t.state = TaskState::Paused;
                    t.speed = 0;
                    t.downloaded = rt3.downloaded.load(Ordering::Relaxed);
                }
                let snap = t.clone();
                drop(t);
                let _ = engine3.store.save_task(&snap);
                engine3.emit(EngineEvent::TaskUpdated(snap));
            }
            let _ = tx.send(res);
        });
        tokio::select! {
            res = rx => {
                let inner = res.map_err(|e| anyhow!("FTP 任务异常: {e}"))?;
                inner.map_err(|e| anyhow!("FTP 任务异常: {e:#}"))?
            }
            _ = cancel.cancelled() => {
                flag.store(true, Ordering::Relaxed);
                // bridge 任务会在 worker 退出后清理 running / 落 Paused
                return Ok(());
            }
        }
    };

    match outcome {
        Outcome::Done => {
            {
                let mut t = rt.task.lock();
                if t.size.is_none() {
                    t.size = Some(rt.downloaded.load(Ordering::Relaxed));
                }
            }
            engine.finish_task(&rt)?;
            Ok(())
        }
        Outcome::Cancelled => {
            *rt.running.lock() = false;
            let mut t = rt.task.lock();
            if t.state.is_active() {
                t.state = TaskState::Paused;
                t.speed = 0;
                t.downloaded = rt.downloaded.load(Ordering::Relaxed);
            }
            let snap = t.clone();
            drop(t);
            let _ = engine.store.save_task(&snap);
            engine.emit(EngineEvent::TaskUpdated(snap));
            Ok(())
        }
    }
}

/// FTPS 使用的 rustls 客户端配置。
///
/// **必须显式指定 provider**：依赖树里 `ring`（本 crate）与 `aws-lc-rs`
/// （其他依赖）可能同时被编译进来，此时 `ClientConfig::builder()` 找不到唯一
/// 默认 provider 会 panic，而 release 是 `panic=abort`，等同于应用直接退出。
/// 这里固定使用 ring 后端，与 `suppaftp` 的 `rustls-ring` 特性保持一致。
fn ftps_tls_config() -> Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("初始化 FTPS TLS 协议版本失败")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(cfg)
}

fn ftp_connect_and_run(
    target: RemoteTarget,
    secure: bool,
    start: u64,
    file: &std::fs::File,
    engine: &Arc<Engine>,
    rt: &Arc<Runtime>,
    cancel: &AtomicBool,
    limit: Option<u64>,
) -> Result<Outcome> {
    let addr = format!("{}:{}", target.host, target.port);
    if secure {
        // FTPS：显式 TLS（rustls）
        let cfg = ftps_tls_config()?;
        let conn = suppaftp::RustlsConnector::from(Arc::new(cfg));
        let stream = suppaftp::RustlsFtpStream::connect(&addr)
            .map_err(|e| anyhow!("连接 FTPS 服务器失败: {e}"))?;
        let stream = stream
            .into_secure(conn, &target.host)
            .map_err(|e| anyhow!("FTPS TLS 升级失败: {e}"))?;
        ftp_login_and_run(stream, target, start, file, engine, rt, cancel, limit)
    } else {
        let stream = suppaftp::FtpStream::connect(&addr)
            .map_err(|e| anyhow!("连接 FTP 服务器失败: {e}"))?;
        ftp_login_and_run(stream, target, start, file, engine, rt, cancel, limit)
    }
}

fn ftp_login_and_run<S>(
    mut ftp: suppaftp::ImplFtpStream<S>,
    target: RemoteTarget,
    start: u64,
    file: &std::fs::File,
    engine: &Arc<Engine>,
    rt: &Arc<Runtime>,
    cancel: &AtomicBool,
    limit: Option<u64>,
) -> Result<Outcome>
where
    S: suppaftp::TlsStream,
{
    let user = if target.username.is_empty() {
        "anonymous".to_string()
    } else {
        target.username.clone()
    };
    let pass = if target.username.is_empty() {
        "felldl@example.com".to_string()
    } else {
        target.password.clone()
    };
    ftp.login(&user, &pass)
        .map_err(|e| anyhow!("FTP 登录失败: {e}"))?;
    ftp_worker(ftp, &target, start, file, engine, rt, cancel, limit)
}

#[derive(Debug)]
struct SshHandler {
    host: String,
    port: u16,
    known_hosts: std::path::PathBuf,
}

impl russh::client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        use russh::keys::known_hosts::{check_known_hosts_path, learn_known_hosts_path};

        let key = server_public_key.public_key();
        match check_known_hosts_path(&self.host, self.port, &key, &self.known_hosts) {
            // 已记录且一致
            Ok(true) => Ok(true),
            // 首次见到该主机：记录后继续（TOFU，仅此一次）
            Ok(false) => {
                if let Err(e) = learn_known_hosts_path(&self.host, self.port, &key, &self.known_hosts)
                {
                    eprintln!("[sftp] 无法保存主机密钥指纹（本次仍继续）: {e}");
                } else {
                    eprintln!(
                        "[sftp] 首次连接 {}:{}，已记录主机密钥 {}（如非预期请清理 known_hosts）",
                        self.host,
                        self.port,
                        key.fingerprint(russh::keys::HashAlg::Sha256)
                    );
                }
                Ok(true)
            }
            // 密钥变化：必须拒绝，不能把密码发给可能被冒充的服务器
            Err(e) => {
                eprintln!(
                    "[sftp] 主机密钥与已记录不一致，已拒绝连接 {}:{}（{}）。如确认服务器已换钥，请清理 {}",
                    self.host,
                    self.port,
                    e,
                    self.known_hosts.display()
                );
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FTPS 作为“进程内第一个 TLS 操作”也必须能建出配置：
    /// 依赖树同时编译 ring 与 aws-lc-rs 时，`ClientConfig::builder()` 会 panic。
    #[test]
    fn ftps_tls_config_builds_without_default_provider() {
        let handle = std::thread::spawn(|| {
            let cfg = ftps_tls_config().expect("FTPS TLS 配置应可构建");
            // 再建一次，确认不依赖一次性全局安装
            let cfg2 = ftps_tls_config().expect("重复构建也应成功");
            (cfg.enable_sni, cfg2.enable_sni)
        });
        let (a, b) = handle.join().expect("构建 FTPS TLS 配置不得 panic");
        assert!(a && b);
    }
}
