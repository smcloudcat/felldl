//! 「所有文件访问」权限的三态表示（纯逻辑，桌面端可单测）。
//!
//! 为什么不是布尔：这个权限从 Android 11（API 30）才存在，而本项目 minSdk 是 24。
//! 在更老的系统上「所有文件访问」这个概念本身不成立——既不该显示授权按钮，
//! 也不该告诉用户「缺少权限」，那时的判据只有「目标目录到底能不能写」。
//! 用 `NotApplicable` 把这两种情况分开，前端才能给出正确的话。
//!
//! 字符串形态是给 JNI 边界用的稳定契约（Kotlin 侧 `StorageAccess` 产出、IPC 再给前端）：
//! `granted` / `denied` / `na`。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllFiles {
    /// 已授权（API 30+ 且用户打开了开关）
    Granted,
    /// 有这个概念但没授权：可以引导用户去系统设置
    Denied,
    /// 系统没有这个权限（API 30 以下），不该提示也不需要按钮
    NotApplicable,
}

// 这些方法在安卓侧被 JNI 调用点使用；桌面端只跑测试，避免报 dead_code
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
impl AllFiles {
    /// 解析 Kotlin 侧的 `OK:<state>` / `ERR:<原因>`
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if let Some(state) = raw.strip_prefix("OK:") {
            return match state.trim() {
                "granted" => Ok(AllFiles::Granted),
                "denied" => Ok(AllFiles::Denied),
                "na" => Ok(AllFiles::NotApplicable),
                other => Err(format!("未知的存储权限状态: {other}")),
            };
        }
        Err(raw
            .strip_prefix("ERR:")
            .unwrap_or(raw)
            .trim()
            .to_string())
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AllFiles::Granted => "granted",
            AllFiles::Denied => "denied",
            AllFiles::NotApplicable => "na",
        }
    }

    /// 是否该显示「授权」按钮：只有明确被拒才有意义
    pub fn needs_grant(self) -> bool {
        self == AllFiles::Denied
    }

    /// 是否满足「写入公共目录」的权限前提。
    ///
    /// `na` 视为满足：老系统上没有这道闸门（真正的判据是目录可写性自检），
    /// 若在这里返回 false，API 24~29 的用户会被永久卡在「请去授权」上，
    /// 而那个授权页在他们的系统里根本不存在。
    pub fn satisfied(self) -> bool {
        self != AllFiles::Denied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_three_states() {
        assert_eq!(AllFiles::parse("OK:granted").unwrap(), AllFiles::Granted);
        assert_eq!(AllFiles::parse("OK:denied").unwrap(), AllFiles::Denied);
        assert_eq!(AllFiles::parse("OK:na").unwrap(), AllFiles::NotApplicable);
        // 容忍空白（JNI 回来的字符串不该带，但没必要为此失败）
        assert_eq!(AllFiles::parse(" OK:granted\n").unwrap(), AllFiles::Granted);
    }

    #[test]
    fn surfaces_kotlin_error() {
        let err = AllFiles::parse("ERR:读取存储权限状态失败：boom").unwrap_err();
        assert!(err.contains("boom"), "错误原因不能被吞掉: {err}");
        // 纯 Rust 侧的错误串也要能透出
        let err = AllFiles::parse("权限被系统拒绝").unwrap_err();
        assert_eq!(err, "权限被系统拒绝");
    }

    #[test]
    fn unknown_state_is_error_not_default() {
        // 宁可报错也不要默认成 granted —— 静默放行会变成「以为能写，实际写失败」
        assert!(AllFiles::parse("OK:maybe").is_err());
        assert!(AllFiles::parse("").is_err());
    }

    #[test]
    fn na_never_blocks_nor_asks_for_grant() {
        // API 30 以下：不该有按钮，也不该被判为「没权限」
        assert!(!AllFiles::NotApplicable.needs_grant());
        assert!(AllFiles::NotApplicable.satisfied());
        // 明确被拒：要给按钮，且不满足
        assert!(AllFiles::Denied.needs_grant());
        assert!(!AllFiles::Denied.satisfied());
        // 已授权：不用按钮
        assert!(!AllFiles::Granted.needs_grant());
        assert!(AllFiles::Granted.satisfied());
    }
}
