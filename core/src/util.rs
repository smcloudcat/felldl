//! 工具函数：文件名推断、路径去重、限速器、HTTP 头解析

use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use url::Url;

/// 从 Content-Disposition 解析文件名，支持 filename* (RFC 5987) 与 filename
pub fn filename_from_disposition(header: &str) -> Option<String> {
    if header.is_empty() {
        return None;
    }
    // 优先 filename*=UTF-8''xxx
    for part in header.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("filename*=") {
            let v = v.trim_matches('"');
            // 形如 UTF-8''%E4%B8%AD.txt
            let encoded = match v.find("''") {
                Some(i) => &v[i + 2..],
                None => v,
            };
            if let Ok(decoded) = percent_decode_str(encoded).decode_utf8() {
                let d = decoded.trim();
                if !d.is_empty() {
                    return Some(sanitize_name(d));
                }
            }
        }
    }
    for part in header.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("filename=") {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                // 有些服务器用 GBK，这里退化为 lossy
                return Some(sanitize_name(v));
            }
        }
    }
    None
}

/// 从 URL 猜测文件名
pub fn filename_from_url(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    let last = u.path_segments()?.last()?.to_string();
    if last.is_empty() {
        return None;
    }
    let decoded = percent_decode_str(&last).decode_utf8_lossy().to_string();
    if decoded.trim().is_empty() {
        None
    } else {
        Some(sanitize_name(&decoded))
    }
}

/// 过滤非法字符，保证跨平台安全（Windows 限制最多）
pub fn sanitize_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    s = s.trim().trim_matches('.').to_string();
    if s.is_empty() {
        s = "download".into();
    }
    // 避免 Windows 保留名
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = s.split('.').next().unwrap_or("").to_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        s = format!("_{s}");
    }
    // 长度保护（保留扩展名）
    if s.chars().count() > 180 {
        let ext = Path::new(&s)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let stem: String = s.chars().take(160).collect();
        s = format!("{stem}{ext}");
    }
    s
}

/// 若目标文件已存在，生成 `name (1).ext` 这类不冲突名称
pub fn unique_path(dir: &Path, file_name: &str) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let p = Path::new(file_name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());
    let ext = p.extension().map(|e| e.to_string_lossy().to_string());
    for i in 1..10000 {
        let name = match &ext {
            Some(e) => format!("{stem} ({i}).{e}"),
            None => format!("{stem} ({i})"),
        };
        let cand = dir.join(&name);
        if !cand.exists() {
            return cand;
        }
    }
    dir.join(format!("{stem}-{}.tmp", crate::model::now_ms()))
}

/// 根据 Content-Type 补扩展名
pub fn ensure_extension(name: &str, content_type: Option<&str>) -> String {
    let p = Path::new(name);
    let has_ext = p
        .extension()
        .map(|e| e.to_string_lossy().len() >= 1 && e.to_string_lossy().len() <= 5)
        .unwrap_or(false);
    if has_ext {
        return name.to_string();
    }
    if let Some(ct) = content_type {
        let ct = ct.split(';').next().unwrap_or("").trim();
        if let Some(ext) = mime_guess::get_mime_extensions_str(ct).and_then(|v| v.first()) {
            return format!("{name}.{ext}");
        }
    }
    name.to_string()
}

/// 解析 `Content-Range: bytes 0-1023/4096`
pub fn parse_content_range(v: &str) -> Option<(u64, u64, Option<u64>)> {
    let v = v.trim();
    let v = v.strip_prefix("bytes")?.trim();
    let (range, total) = v.split_once('/')?;
    let (s, e) = range.split_once('-')?;
    let start = s.trim().parse().ok()?;
    let end = e.trim().parse().ok()?;
    let total = if total.trim() == "*" {
        None
    } else {
        total.trim().parse().ok()
    };
    Some((start, end, total))
}

/// 人类可读字节
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{:.0} {}", v, UNITS[i])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

pub fn human_speed(bytes_per_sec: u64) -> String {
    format!("{}/s", human_bytes(bytes_per_sec))
}

/// 令牌桶限速器。`limit == 0` 表示不限速。
///
/// 用法：每次从网络读到 `n` 字节后 `acquire(n).await`，
/// 内部用「下一个可用时间戳」排队，从而把多个分片的**合计**速率压到 limit。
///
/// 为什么不用「余额+回填」写法：那种写法在单次申请量大于桶容量时会算不平，
/// 出现永远等待的死循环。这里改用预约制，一次性算准等待时长，无循环依赖。
pub struct RateLimiter {
    limit: AtomicU64,
    /// 下一个可发送数据的时刻
    next_slot: parking_lot::Mutex<std::time::Instant>,
}

impl RateLimiter {
    pub fn new(limit: u64) -> Self {
        Self {
            limit: AtomicU64::new(limit),
            next_slot: parking_lot::Mutex::new(std::time::Instant::now()),
        }
    }

    pub fn set_limit(&self, limit: u64) {
        self.limit.store(limit, Ordering::Relaxed);
        *self.next_slot.lock() = std::time::Instant::now();
    }

    pub fn limit(&self) -> u64 {
        self.limit.load(Ordering::Relaxed)
    }

    /// 消耗额度，必要时等待
    pub async fn acquire(&self, n: u64) {
        let limit = self.limit();
        if limit == 0 || n == 0 {
            return;
        }
        let wait = {
            let mut slot = self.next_slot.lock();
            let now = std::time::Instant::now();
            // 闲置太久则重置基准，避免「攒额度」导致瞬间突发
            if *slot < now {
                *slot = now;
            }
            let start = *slot;
            let cost = Duration::from_secs_f64(n as f64 / limit as f64);
            *slot = start + cost;
            start.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_disposition_plain() {
        let h = r#"attachment; filename="ubuntu-24.04.iso""#;
        assert_eq!(
            filename_from_disposition(h).as_deref(),
            Some("ubuntu-24.04.iso")
        );
    }

    #[test]
    fn parse_disposition_rfc5987() {
        let h = "attachment; filename*=UTF-8''%E4%B8%AD%E6%96%87.txt";
        assert_eq!(filename_from_disposition(h).as_deref(), Some("中文.txt"));
    }

    #[test]
    fn url_filename_decodes() {
        assert_eq!(
            filename_from_url("https://e.com/a/%E6%B5%8B%E8%AF%95.zip").as_deref(),
            Some("测试.zip")
        );
    }

    #[test]
    fn url_filename_no_path() {
        assert_eq!(filename_from_url("https://e.com/"), None);
    }

    #[test]
    fn sanitize_strips_bad_chars() {
        assert_eq!(sanitize_name("a/b:c*d?.txt"), "a_b_c_d_.txt");
    }

    #[test]
    fn sanitize_reserved() {
        assert_eq!(sanitize_name("CON.txt"), "_CON.txt");
    }

    #[test]
    fn content_range_parse() {
        assert_eq!(
            parse_content_range("bytes 0-99/1000"),
            Some((0, 99, Some(1000)))
        );
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, 9, None)));
        assert_eq!(parse_content_range("garbage"), None);
    }

    #[test]
    fn human_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(1024 * 1024 * 5), "5.0 MB");
    }

    #[test]
    fn ensure_ext_adds_from_mime() {
        assert_eq!(
            ensure_extension("file", Some("application/zip")),
            "file.zip"
        );
        assert_eq!(ensure_extension("file.zip", Some("text/plain")), "file.zip");
    }

    #[tokio::test]
    async fn rate_limiter_no_limit_returns_fast() {
        let rl = RateLimiter::new(0);
        let t = std::time::Instant::now();
        rl.acquire(10_000_000).await;
        assert!(t.elapsed().as_millis() < 50);
    }

    #[tokio::test]
    async fn rate_limiter_enforces() {
        // 限速 100KB/s，申请 200KB 应该等待约 1 秒
        let rl = RateLimiter::new(100 * 1024);
        let t = std::time::Instant::now();
        rl.acquire(100 * 1024).await; // 用掉整桶
        rl.acquire(100 * 1024).await; // 需等待补充
        let el = t.elapsed().as_secs_f64();
        assert!(el > 0.5, "elapsed {el} should exceed 0.5s");
    }
}
