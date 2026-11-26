//! Android 更新包安装前的元数据校验（纯逻辑，桌面端可跑测试）。
//!
//! 签名清单只保证「这个 apk 是我们发布的、内容没被改过」，装到设备上还要确认：
//! * 包名是本应用（否则会把别的应用顶进来）；
//! * `versionCode` 高于已安装版本（否则就是降级覆盖）；
//! * APK 签名与已安装版本一致（否则系统会直接拒装，报错还很难懂）。
//!
//! 这些判断放在这里而不是 Kotlin 里，是因为它不依赖 Android 运行时，
//! 可以在桌面 CI 上直接测。Kotlin 侧只负责把 `PackageManager` 的原始信息读出来。
//!
//! 桌面构建里只有测试会用到，故允许 dead_code。
#![allow(dead_code)]

/// 从 APK 文件（未安装）里读到的信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApkInfo {
    pub package: String,
    pub version_code: u64,
    /// 当前签名证书的 SHA-256（小写十六进制）
    pub signer_sha256: String,
}

/// 已安装版本的信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledInfo {
    pub version_code: u64,
    pub signer_sha256: String,
}

/// 剥掉原生层约定的 `OK:` / `ERR:` 前缀
fn body(resp: &str) -> Result<&str, String> {
    let r = resp.trim();
    if let Some(rest) = r.strip_prefix("OK:") {
        Ok(rest)
    } else if let Some(msg) = r.strip_prefix("ERR:") {
        Err(msg.trim().to_string())
    } else {
        Err(format!("无法识别的原生返回值：{r}"))
    }
}

/// 解析 `OK:<包名>|<versionCode>|<签名摘要>`
pub fn parse_apk_info(resp: &str) -> Result<ApkInfo, String> {
    let b = body(resp)?;
    let mut it = b.split('|');
    let package = it.next().unwrap_or("").trim();
    let version = it.next().unwrap_or("").trim();
    let signer = it.next().unwrap_or("").trim();
    if package.is_empty() || version.is_empty() || signer.is_empty() {
        return Err(format!("安装包信息不完整：{b}"));
    }
    let version_code = version
        .parse::<u64>()
        .map_err(|_| format!("安装包 versionCode 无效：{version}"))?;
    Ok(ApkInfo {
        package: package.to_string(),
        version_code,
        signer_sha256: signer.to_ascii_lowercase(),
    })
}

/// 解析 `OK:<versionCode>|<签名摘要>`
pub fn parse_installed_info(resp: &str) -> Result<InstalledInfo, String> {
    let b = body(resp)?;
    let mut it = b.split('|');
    let version = it.next().unwrap_or("").trim();
    let signer = it.next().unwrap_or("").trim();
    if version.is_empty() || signer.is_empty() {
        return Err(format!("已安装版本信息不完整：{b}"));
    }
    let version_code = version
        .parse::<u64>()
        .map_err(|_| format!("已安装 versionCode 无效：{version}"))?;
    Ok(InstalledInfo {
        version_code,
        signer_sha256: signer.to_ascii_lowercase(),
    })
}

/// 安装前的最后一道校验。`declared_version_code` 来自签名清单，
/// 用来保证「系统要装的这个包」和「我们验过签的那个包」是同一个版本。
pub fn verify_installable(
    expected_package: &str,
    installed: &InstalledInfo,
    apk: &ApkInfo,
    declared_version_code: Option<u64>,
) -> Result<(), String> {
    if apk.package != expected_package {
        return Err(format!(
            "安装包包名是 {}，不是本应用（{}），已拒绝安装",
            apk.package, expected_package
        ));
    }
    if apk.signer_sha256.is_empty() || installed.signer_sha256.is_empty() {
        return Err("无法读取安装包签名，已拒绝安装".into());
    }
    if !apk
        .signer_sha256
        .eq_ignore_ascii_case(&installed.signer_sha256)
    {
        return Err(
            "安装包签名与当前已安装版本不一致：换过签名的包无法覆盖安装，请先卸载旧版本".into(),
        );
    }
    if apk.version_code <= installed.version_code {
        return Err(format!(
            "安装包 versionCode {} 不高于已安装的 {}，已拒绝降级安装",
            apk.version_code, installed.version_code
        ));
    }
    if let Some(declared) = declared_version_code {
        if declared != apk.version_code {
            return Err(format!(
                "安装包 versionCode {} 与更新清单声明的 {} 不一致，已拒绝安装",
                apk.version_code, declared
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed() -> InstalledInfo {
        InstalledInfo {
            version_code: 3009,
            signer_sha256: "aa".repeat(32),
        }
    }

    fn apk() -> ApkInfo {
        ApkInfo {
            package: "com.felldl.app".into(),
            version_code: 3010,
            signer_sha256: "aa".repeat(32),
        }
    }

    #[test]
    fn parses_apk_info() {
        let r = format!("OK:com.felldl.app|3010|{}", "AB".repeat(32));
        let info = parse_apk_info(&r).unwrap();
        assert_eq!(info.package, "com.felldl.app");
        assert_eq!(info.version_code, 3010);
        assert_eq!(info.signer_sha256, "ab".repeat(32), "签名摘要应统一为小写");
    }

    #[test]
    fn native_error_is_propagated() {
        let e = parse_apk_info("ERR:无法解析安装包（文件损坏或不是 APK）").unwrap_err();
        assert!(e.contains("文件损坏"));
        let e = parse_apk_info("WAT").unwrap_err();
        assert!(e.contains("无法识别"));
    }

    #[test]
    fn normal_upgrade_passes() {
        assert!(verify_installable("com.felldl.app", &installed(), &apk(), Some(3010)).is_ok());
        // 清单没声明 versionCode 时，只做包内自证
        assert!(verify_installable("com.felldl.app", &installed(), &apk(), None).is_ok());
    }

    #[test]
    fn rejects_same_or_lower_version() {
        let mut a = apk();
        a.version_code = 3009;
        assert!(verify_installable("com.felldl.app", &installed(), &a, None).is_err());
        a.version_code = 3008;
        let e = verify_installable("com.felldl.app", &installed(), &a, None).unwrap_err();
        assert!(e.contains("降级"), "应提示降级而不是泛泛失败: {e}");
    }

    #[test]
    fn rejects_other_package() {
        let mut a = apk();
        a.package = "com.evil.app".into();
        let e = verify_installable("com.felldl.app", &installed(), &a, None).unwrap_err();
        assert!(e.contains("包名"));
    }

    #[test]
    fn rejects_signer_mismatch() {
        let mut a = apk();
        a.signer_sha256 = "bb".repeat(32);
        let e = verify_installable("com.felldl.app", &installed(), &a, None).unwrap_err();
        assert!(e.contains("签名"));
        // 大小写不同不算不一致
        a.signer_sha256 = "AA".repeat(32);
        assert!(verify_installable("com.felldl.app", &installed(), &a, None).is_ok());
    }

    #[test]
    fn rejects_empty_signer() {
        let mut a = apk();
        a.signer_sha256.clear();
        assert!(verify_installable("com.felldl.app", &installed(), &a, None).is_err());
    }

    #[test]
    fn rejects_manifest_version_code_mismatch() {
        let e = verify_installable("com.felldl.app", &installed(), &apk(), Some(3011)).unwrap_err();
        assert!(e.contains("清单"), "应指出与清单不一致: {e}");
    }
}
