//! Tauri 命令的结构化错误。
//!
//! 命令以前统一返回 `Result<T, String>`：前端只能靠 `startsWith("install.")` 猜类型，
//! 文案润色一次、判断就失效，也没法区分「重试有用」和「重试一百次也没用」。
//!
//! 现在错误带三个字段：
//! * `code`：稳定机器码（`install.need_permission` / `update.network` …），前端按它决定动作；
//! * `message`：给人看的完整说明，可以随便改；
//! * `retryable`：用户什么都不改、再点一次有没有可能成功。
//!
//! `code` 是契约，别为了文案好看去改它 —— 和 `felldl_core::error` 里的约定一致。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl IpcError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            retryable: false,
        }
    }

    /// 什么都不改、再试一次可能就成功（网络抖动、源站 5xx、刚授完权）。
    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    /// 按状态码判定是否值得重试：5xx / 408 / 429 是暂时的，其余 4xx 不是。
    pub fn http_status(code: &str, status: u16) -> Self {
        Self {
            code: code.to_string(),
            message: format!("更新源返回 HTTP {status}"),
            retryable: http_retryable(status),
        }
    }

    /// 复用 core 的分类：`anyhow` 错误链里如果带着 IO / reqwest 信息，
    /// 「是不是暂时性失败」就不用在这里重新猜一遍。
    pub fn from_anyhow(code: &str, err: &anyhow::Error) -> Self {
        let info = felldl_core::error::classify(err);
        Self {
            code: code.to_string(),
            message: format!("{err:#}"),
            retryable: info.retryable,
        }
    }

    /// 解析 `code: message` 形式的字符串（Kotlin 侧与 `android.rs` 仍在用这套）。
    ///
    /// 桌面端没有这些调用点（安卓模块被 cfg 掉了），保留是为了跨平台代码只写一份。
    ///
    /// 没有可识别前缀时整串当文案，`code` 退回 `fallback`——宁可少一个精确码，
    /// 也不要把「install.no_handler: ...」这种半机器串直接丢给用户看。
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn from_coded(raw: impl Into<String>, fallback: &str) -> Self {
        let raw = raw.into();
        match split_coded(&raw) {
            Some((code, msg)) => Self::new(code, msg),
            None => Self::new(fallback, raw.trim().to_string()),
        }
    }
}

/// `code: message` → (code, message)；前缀必须像 `a.b` 这样的点分小写标识。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn split_coded(raw: &str) -> Option<(&str, &str)> {
    let (head, tail) = raw.split_once(':')?;
    let head = head.trim();
    let ok = !head.is_empty()
        && head.len() <= 48
        && head.contains('.')
        && head
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_');
    if !ok {
        return None;
    }
    Some((head, tail.trim()))
}

/// 5xx / 408 / 429 通常是暂时的；其余 4xx 重试只是浪费时间。
pub fn http_retryable(status: u16) -> bool {
    status >= 500 || status == 408 || status == 429
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for IpcError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_coded_strings() {
        let e = IpcError::from_coded("install.no_handler: 系统没有可用的安装器", "install.failed");
        assert_eq!(e.code, "install.no_handler");
        assert_eq!(e.message, "系统没有可用的安装器");
        assert!(!e.retryable);
    }

    #[test]
    fn keeps_plain_message_and_uses_fallback_code() {
        // 没有点分前缀：整串是文案，不能被误当成 code
        let e = IpcError::from_coded("读取已安装版本失败：boom", "install.unavailable");
        assert_eq!(e.code, "install.unavailable");
        assert_eq!(e.message, "读取已安装版本失败：boom");

        // 中文冒号 / 句子里的冒号也不能被切
        let e = IpcError::from_coded("失败: 原因是 X", "update.verify");
        assert_eq!(e.code, "update.verify");
        assert_eq!(e.message, "失败: 原因是 X");
    }

    #[test]
    fn rejects_bogus_prefixes() {
        for raw in [
            "https://example.com/a:b", // 有冒号但不是 code
            "Install.Failed: x",       // 大写不是约定
            ": 没有前缀",
            "a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p.q.r.s.t.u.v.w.x.y.z: too long",
        ] {
            assert!(
                split_coded(raw).is_none(),
                "不该把 {raw:?} 当成 code: message"
            );
        }
    }

    #[test]
    fn http_status_retryability() {
        assert!(http_retryable(500));
        assert!(http_retryable(503));
        assert!(http_retryable(429));
        assert!(http_retryable(408));
        assert!(!http_retryable(400));
        assert!(!http_retryable(403));
        assert!(!http_retryable(404));
    }

    #[test]
    fn io_classification_reuses_core() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err = anyhow::Error::from(io).context("写入更新包失败");
        let e = IpcError::from_anyhow("update.disk", &err);
        assert_eq!(e.code, "update.disk");
        assert!(!e.retryable, "权限问题重试也没用");
        assert!(e.message.contains("写入更新包失败"), "{}", e.message);
    }

    #[test]
    fn retryable_builder_marks_flag() {
        assert!(
            IpcError::new("install.need_permission", "x")
                .retryable()
                .retryable
        );
    }
}
