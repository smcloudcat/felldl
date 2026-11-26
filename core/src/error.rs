//! 稳定的错误分类：给 UI / 自动化一个不随文案变化的 `code`，并声明「重试有没有意义」。
//!
//! 为什么需要它：此前所有失败都只是一串中文（`format!("{e:#}")`），
//! 前端无法区分「链接 404 了别再试」和「网络抖了一下，重试就好」，
//! 文案一改，任何按字符串匹配的逻辑就全废。
//!
//! 用法：
//! * 产生错误的地方用 `FellError::http(404, "...")` / `FellError::from(io_err)` 之类构造；
//! * 消费错误的地方调 [`classify`] 拿到 `code + retryable`，再连同原始文案一起存进任务。
//!
//! `code` 是稳定契约（会被前端与脚本使用），不要翻译、不要随手改；
//! `message` 才是给人看的，可以随便润色。

use std::fmt;

/// 错误大类。`Http` 会带上状态码，`code()` 再细分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Canceled,
    Network,
    Timeout,
    Dns,
    Tls,
    Http,
    Integrity,
    DiskFull,
    PermissionDenied,
    DiskIo,
    Unsupported,
    Other,
}

/// 带分类的下载错误。可以安全地塞进 `anyhow::Error`：
/// [`classify`] 会沿 cause 链把它找出来。
#[derive(Debug, Clone)]
pub struct FellError {
    kind: ErrorKind,
    /// 仅 `Http` 使用
    status: Option<u16>,
    message: String,
}

impl FellError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            message: message.into(),
        }
    }

    /// HTTP 状态类错误（探活/分片/抓取共用）
    pub fn http(status: u16, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Http,
            status: Some(status),
            message: message.into(),
        }
    }

    pub fn canceled() -> Self {
        Self::new(ErrorKind::Canceled, "任务已取消")
    }

    /// 保留分类，只给人话加一段前缀（`anyhow::Context` 会丢类型，这里不会）
    pub fn context(mut self, ctx: impl AsRef<str>) -> Self {
        self.message = format!("{}: {}", ctx.as_ref(), self.message);
        self
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn status(&self) -> Option<u16> {
        self.status
    }

    /// 稳定的机器可读标识
    pub fn code(&self) -> &'static str {
        match self.kind {
            ErrorKind::Canceled => "canceled",
            ErrorKind::Network => "net.error",
            ErrorKind::Timeout => "net.timeout",
            ErrorKind::Dns => "net.dns",
            ErrorKind::Tls => "net.tls",
            ErrorKind::Integrity => "integrity",
            ErrorKind::DiskFull => "disk.full",
            ErrorKind::PermissionDenied => "disk.permission",
            ErrorKind::DiskIo => "disk.io",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Other => "unknown",
            ErrorKind::Http => match self.status.unwrap_or(0) {
                404 => "http.not_found",
                410 => "http.gone",
                401 | 403 => "http.auth",
                407 => "http.proxy_auth",
                408 | 425 | 429 => "http.retry",
                416 => "http.range",
                500..=599 => "http.server",
                400..=499 => "http.client",
                _ => "http.status",
            },
        }
    }

    /// 自动重试是否有意义。注意这是「无人工干预下再试一次」的语义：
    /// 磁盘满属于可恢复（腾出空间再试），而 404 / 签名不符再怎么试都没用。
    pub fn retryable(&self) -> bool {
        match self.kind {
            ErrorKind::Network | ErrorKind::Timeout | ErrorKind::Dns => true,
            ErrorKind::DiskFull | ErrorKind::DiskIo => true,
            ErrorKind::Http => matches!(self.status.unwrap_or(0), 408 | 425 | 429 | 500..=599),
            _ => false,
        }
    }

    pub fn info(&self) -> ErrorInfo {
        ErrorInfo {
            code: self.code(),
            retryable: self.retryable(),
        }
    }
}

impl fmt::Display for FellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FellError {}

impl From<std::io::Error> for FellError {
    fn from(e: std::io::Error) -> Self {
        // 写盘错误里最常见、也最该区分的就是「盘满」和「没权限」：
        // 前者腾空间可恢复，后者改目录才有用。
        let kind = io_error_kind(&e);
        let message = if kind == ErrorKind::DiskFull {
            format!("磁盘空间不足：{e}")
        } else {
            format!("磁盘操作失败：{e}")
        };
        Self::new(kind, message)
    }
}

impl From<reqwest::Error> for FellError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            return Self::new(ErrorKind::Timeout, format!("连接超时：{e}"));
        }
        if e.is_status() {
            let status = e.status().map(|s| s.as_u16()).unwrap_or(0);
            return Self::http(status, format!("服务器返回 HTTP {status}"));
        }
        // 连接失败 / 请求构造 / body 读取：都归网络类，可重试
        Self::new(ErrorKind::Network, format!("网络错误：{e}"))
    }
}

fn io_error_kind(e: &std::io::Error) -> ErrorKind {
    if is_disk_full(e) {
        ErrorKind::DiskFull
    } else if e.kind() == std::io::ErrorKind::PermissionDenied {
        ErrorKind::PermissionDenied
    } else {
        ErrorKind::DiskIo
    }
}

fn is_disk_full(e: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        // ERROR_HANDLE_DISK_FULL / ERROR_DISK_FULL
        matches!(e.raw_os_error(), Some(39) | Some(112))
    }
    #[cfg(not(windows))]
    {
        e.raw_os_error() == Some(28) // ENOSPC
    }
}

/// 存进任务/事件的最小信息
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorInfo {
    pub code: &'static str,
    pub retryable: bool,
}

impl ErrorInfo {
    pub const UNKNOWN: ErrorInfo = ErrorInfo {
        code: "unknown",
        retryable: false,
    };
}

/// 沿 `anyhow` 的 cause 链找最具体的分类。
///
/// 找不到显式 `FellError` 时再看有没有裸的 `io::Error` / `reqwest::Error`
/// （`?` 直接转进 anyhow 的很常见），最后才回落到 `unknown`。
pub fn classify(err: &anyhow::Error) -> ErrorInfo {
    for cause in err.chain() {
        if let Some(f) = cause.downcast_ref::<FellError>() {
            return f.info();
        }
        // `io::Error` / `reqwest::Error` 都不是 Clone，只能就地把分类信息取出来
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return FellError::new(io_error_kind(io), io.to_string()).info();
        }
        if let Some(re) = cause.downcast_ref::<reqwest::Error>() {
            let kind = if re.is_timeout() {
                ErrorKind::Timeout
            } else if re.is_status() {
                ErrorKind::Http
            } else {
                ErrorKind::Network
            };
            let status = re.status().map(|s| s.as_u16());
            let mut f = FellError::new(kind, re.to_string());
            f.status = status;
            return f.info();
        }
    }
    ErrorInfo::UNKNOWN
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn http_codes_and_retryability() {
        let cases = [
            (404, "http.not_found", false),
            (410, "http.gone", false),
            (403, "http.auth", false),
            (408, "http.retry", true),
            (429, "http.retry", true),
            (416, "http.range", false),
            (503, "http.server", true),
            (400, "http.client", false),
        ];
        for (status, code, retryable) in cases {
            let e = FellError::http(status, "boom");
            assert_eq!(e.code(), code, "status {status} 的 code 不对");
            assert_eq!(
                e.retryable(),
                retryable,
                "status {status} 的 retryable 不对"
            );
        }
    }

    #[test]
    fn io_errors_are_classified() {
        let denied = FellError::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ));
        assert_eq!(denied.code(), "disk.permission");
        assert!(!denied.retryable(), "改目录才有用，自动重试没意义");

        let other = FellError::from(std::io::Error::new(std::io::ErrorKind::Other, "boom"));
        assert_eq!(other.code(), "disk.io");
        assert!(other.retryable(), "一般 IO 抖动可以重试");
    }

    #[test]
    fn context_keeps_classification() {
        let e = FellError::http(404, "文件不存在").context("探测失败");
        assert_eq!(e.code(), "http.not_found");
        assert!(e.to_string().contains("探测失败"), "上下文应保留：{e}");
        assert!(e.to_string().contains("文件不存在"));
    }

    #[test]
    fn classify_finds_nested_error() {
        let err = anyhow::Error::new(FellError::http(429, "限流")).context("拉取分片失败");
        let info = classify(&err);
        assert_eq!(info.code, "http.retry");
        assert!(info.retryable);

        // 裸 io::Error 经过 ? 进 anyhow 也要能识别
        let io_err: anyhow::Error =
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied").into();
        assert_eq!(classify(&io_err).code, "disk.permission");

        // 纯字符串错误：明确归为 unknown，而不是瞎猜可重试
        let plain = anyhow!("some weird failure");
        assert_eq!(classify(&plain), ErrorInfo::UNKNOWN);
        assert!(!classify(&plain).retryable);
    }
}
