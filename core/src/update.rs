//! 更新包真实性校验：签名清单 + 产物摘要 + 平台/ABI 绑定。
//!
//! 设计要点：
//! * 清单（manifest）里的 `payload` 由**离线私钥**签名，应用只内置公钥；
//!   因此拿到发布源（GitHub / CDN / 中间人）也无法构造可安装的更新。
//! * 产物校验绑定 `platform + arch(abi) + version + size + sha256`，
//!   下载完成后必须再过一遍 `verify_downloaded`，得到不可伪造的 `VerifiedUpdate`。
//! * `VerifiedUpdate` 没有公开构造函数：只有走过签名与摘要校验才能拿到，
//!   安装流程只接受这个类型，避免"前端传任意路径就执行"的老问题。

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 内置的更新签名公钥（Ed25519，base64 原始 32 字节）。
///
/// 私钥离线保存在开发机（`src-tauri/keys/`，已 gitignore），**不入库**。
/// 轮换公钥意味着旧版本无法验证新清单，需保留旧公钥一段时间（见 `TRUSTED_PUBLIC_KEYS`）。
pub const UPDATE_PUBLIC_KEY: &str = "uqA1wUawkYVEnEYFMhO8VeFTwFG9wDD9uXRD/MjslPw=";

/// 参与信任的公钥集合：轮换过渡期内同时接受新旧公钥。
pub const TRUSTED_PUBLIC_KEYS: &[&str] = &[UPDATE_PUBLIC_KEY];

/// 清单里声明的单个平台产物
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifact {
    /// 目标平台：`windows` / `macos` / `linux` / `android`
    pub platform: String,
    /// 目标架构：`x86_64` / `aarch64` / `arm64-v8a` 等
    pub arch: String,
    /// 应用版本（与清单 version 一致，便于逐产物核对）
    pub version: String,
    /// 产物下载地址（必须 https）
    pub url: String,
    /// 字节长度
    pub size: u64,
    /// 小写十六进制 SHA256
    pub sha256: String,
    /// Android versionCode（其它平台为 None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_code: Option<u64>,
}

/// 被签名的内容
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Payload {
    /// 发布版本号
    pub version: String,
    /// 发布时间（RFC3339，仅展示用）
    #[serde(default)]
    pub published_at: String,
    /// 全部平台产物
    pub artifacts: Vec<Artifact>,
}

/// 完整清单文件：`payload` + 对 payload 规范化字节的签名
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedManifest {
    pub payload: Payload,
    /// Ed25519 签名（base64，签名对象见 `canonical_bytes`）
    pub signature: String,
}

/// 规范化签名原文：`payload` 的紧凑 JSON（结构体字段顺序固定，因此字节确定）。
pub fn canonical_bytes(payload: &Payload) -> Result<Vec<u8>> {
    serde_json::to_vec(payload).context("序列化清单内容失败")
}

fn decode_b64(s: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| anyhow!("base64 解码失败: {e}"))
}

/// 校验签名：`bytes` 必须与签名时的规范化字节完全一致。
pub fn verify_signature(bytes: &[u8], signature_b64: &str, public_key_b64: &str) -> Result<()> {
    let key = decode_b64(public_key_b64).context("更新公钥格式无效")?;
    let sig = decode_b64(signature_b64).context("更新签名格式无效")?;
    let public_key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key);
    public_key
        .verify(bytes, &sig)
        .map_err(|_| anyhow!("更新清单签名校验失败（内容被篡改或公钥不匹配）"))
}

/// 解析清单并验签，返回可信的 `Payload`。
pub fn parse_and_verify(text: &str) -> Result<Payload> {
    parse_and_verify_with_keys(TRUSTED_PUBLIC_KEYS, text)
}

fn parse_and_verify_with_keys(keys: &[&str], text: &str) -> Result<Payload> {
    let manifest: SignedManifest =
        serde_json::from_str(text).context("更新清单格式无效")?;
    let bytes = canonical_bytes(&manifest.payload)?;
    let mut last_err = None;
    for key in keys {
        match verify_signature(&bytes, &manifest.signature, key) {
            Ok(()) => return Ok(manifest.payload),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("没有可用的更新公钥")))
}

/// 从清单里挑选当前平台/架构的产物。
///
/// 找不到时明确报错，而不是"随便挑一个"——错架构的包装上去就是不可用。
pub fn pick_artifact<'a>(payload: &'a Payload, platform: &str, arch: &str) -> Result<&'a Artifact> {
    payload
        .artifacts
        .iter()
        .find(|a| a.platform.eq_ignore_ascii_case(platform) && a.arch.eq_ignore_ascii_case(arch))
        .ok_or_else(|| anyhow!("更新清单中没有 {platform}/{arch} 的产物"))
}

/// 版本比较：`a` 是否比 `b` 新（按点分段数字比较，忽略前导 v）。
pub fn version_gt(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim()
            .trim_start_matches(['v', 'V'])
            .split(['.', '-', '+'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    }
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 计算文件 SHA256（小写十六进制）
pub fn file_sha256(path: &Path) -> Result<String> {
    use ring::digest;
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("打开文件失败: {}", path.display()))?;
    let mut ctx = digest::Context::new(&digest::SHA256);
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf).context("读取文件失败")?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    let digest = ctx.finish();
    Ok(digest
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// 通过校验的更新包：只能由 [`verify_downloaded`] 构造。
#[derive(Debug, Clone)]
pub struct VerifiedUpdate {
    pub version: String,
    pub platform: String,
    pub arch: String,
    pub sha256: String,
    pub size: u64,
    pub version_code: Option<u64>,
    path: std::path::PathBuf,
}

impl VerifiedUpdate {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 校验已下载的产物：长度 + SHA256 必须与清单声明一致。
///
/// 这是"能安装"的唯一入口；任何失败都返回错误，不会产生 `VerifiedUpdate`。
pub fn verify_downloaded(artifact: &Artifact, path: &Path) -> Result<VerifiedUpdate> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("更新包不存在: {}", path.display()))?;
    if !meta.is_file() {
        bail!("更新包不是普通文件: {}", path.display());
    }
    if meta.len() != artifact.size {
        bail!(
            "更新包长度不符：期望 {} 字节，实际 {} 字节",
            artifact.size,
            meta.len()
        );
    }
    let actual = file_sha256(path)?;
    if !actual.eq_ignore_ascii_case(artifact.sha256.trim()) {
        bail!("更新包摘要不符：清单 {}，实际 {}", artifact.sha256.trim(), actual);
    }
    Ok(VerifiedUpdate {
        version: artifact.version.clone(),
        platform: artifact.platform.clone(),
        arch: artifact.arch.clone(),
        sha256: actual,
        size: meta.len(),
        version_code: artifact.version_code,
        path: path.to_path_buf(),
    })
}

/// 一次完成「清单验签 → 选产物 → 校验文件」，并确认版本确实比当前新。
pub fn verify_for_install(
    manifest_text: &str,
    platform: &str,
    arch: &str,
    current_version: &str,
    path: &Path,
) -> Result<VerifiedUpdate> {
    verify_for_install_with_keys(
        TRUSTED_PUBLIC_KEYS,
        manifest_text,
        platform,
        arch,
        current_version,
        path,
    )
}

fn verify_for_install_with_keys(
    keys: &[&str],
    manifest_text: &str,
    platform: &str,
    arch: &str,
    current_version: &str,
    path: &Path,
) -> Result<VerifiedUpdate> {
    let payload = parse_and_verify_with_keys(keys, manifest_text)?;
    if !version_gt(&payload.version, current_version) {
        bail!(
            "清单版本 {} 不高于当前版本 {}，拒绝安装",
            payload.version,
            current_version
        );
    }
    let artifact = pick_artifact(&payload, platform, arch)?;
    if !artifact.version.eq_ignore_ascii_case(&payload.version) {
        bail!(
            "产物版本 {} 与清单版本 {} 不一致",
            artifact.version,
            payload.version
        );
    }
    if !artifact.url.trim().to_ascii_lowercase().starts_with("https://") {
        bail!("产物地址必须是 https: {}", artifact.url);
    }
    verify_downloaded(artifact, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 examples/update-sign.rs 相同的签名实现，仅为测试固定样本生成签名
    fn sign(bytes: &[u8], key_pkcs8: &[u8]) -> String {
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(key_pkcs8).unwrap();
        base64::engine::general_purpose::STANDARD.encode(kp.sign(bytes).as_ref())
    }

    fn keypair() -> (Vec<u8>, String) {
        use ring::signature::KeyPair as _;
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let pubkey = base64::engine::general_purpose::STANDARD
            .encode(kp.public_key().as_ref());
        (pkcs8.as_ref().to_vec(), pubkey)
    }

    fn sample_payload() -> Payload {
        Payload {
            version: "0.4.0".into(),
            published_at: "2026-11-26T00:00:00Z".into(),
            artifacts: vec![Artifact {
                platform: "windows".into(),
                arch: "x86_64".into(),
                version: "0.4.0".into(),
                url: "https://example.com/Felldl-0.4.0-x64.exe".into(),
                size: 12,
                sha256: "00".repeat(32),
                version_code: None,
            }],
        }
    }

    fn manifest_with(payload: &Payload, key: &[u8]) -> String {
        let sig = sign(&canonical_bytes(payload).unwrap(), key);
        serde_json::json!({ "payload": payload, "signature": sig }).to_string()
    }

    #[test]
    fn signed_manifest_verifies_and_tamper_is_rejected() {
        let (key, pubkey) = keypair();
        let payload = sample_payload();
        let text = manifest_with(&payload, &key);

        let bytes = canonical_bytes(&payload).unwrap();
        let sig: String = serde_json::from_str::<serde_json::Value>(&text).unwrap()["signature"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(verify_signature(&bytes, &sig, &pubkey).is_ok());

        // 篡改任意一个字段 → 签名失效
        let mut tampered = payload.clone();
        tampered.artifacts[0].sha256 = "ff".repeat(32);
        let tbytes = canonical_bytes(&tampered).unwrap();
        assert!(verify_signature(&tbytes, &sig, &pubkey).is_err());

        // 换一个公钥 → 也校验不过
        let (_, other) = keypair();
        assert!(verify_signature(&bytes, &sig, &other).is_err());
    }

    /// 信任根只有内置公钥：任何其它密钥签出来的清单都必须被拒绝。
    #[test]
    fn manifest_signed_by_untrusted_key_is_rejected() {
        let (key, _pubkey) = keypair();
        let text = manifest_with(&sample_payload(), &key);
        let err = parse_and_verify(&text).unwrap_err().to_string();
        assert!(err.contains("签名校验失败"), "实际: {err}");
    }

    #[test]
    fn version_compare_works() {
        assert!(version_gt("0.4.0", "0.3.9"));
        assert!(version_gt("v0.10.0", "0.9.9"));
        assert!(!version_gt("0.3.9", "0.3.9"));
        assert!(!version_gt("0.3.8", "0.3.9"));
    }

    #[test]
    fn pick_artifact_requires_exact_platform_and_arch() {
        let payload = sample_payload();
        assert!(pick_artifact(&payload, "windows", "x86_64").is_ok());
        assert!(pick_artifact(&payload, "windows", "aarch64").is_err());
        assert!(pick_artifact(&payload, "android", "arm64-v8a").is_err());
    }

    #[test]
    fn downloaded_artifact_must_match_size_and_hash() {
        let dir = std::env::temp_dir().join(format!("felldl-verify-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pkg.bin");
        std::fs::write(&file, b"hello world!").unwrap(); // 12 字节

        let mut art = sample_payload().artifacts[0].clone();
        art.sha256 = file_sha256(&file).unwrap();
        let ok = verify_downloaded(&art, &file).expect("摘要与长度都对时应通过");
        assert_eq!(ok.size, 12);
        assert_eq!(ok.path(), file.as_path());

        // 长度不符
        let mut wrong_len = art.clone();
        wrong_len.size = 13;
        assert!(verify_downloaded(&wrong_len, &file).is_err());

        // 摘要不符
        let mut wrong_hash = art.clone();
        wrong_hash.sha256 = "11".repeat(32);
        assert!(verify_downloaded(&wrong_hash, &file).is_err());

        // 多写一个字节（篡改）
        std::fs::write(&file, b"hello world?").unwrap();
        assert!(verify_downloaded(&art, &file).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_rejects_low_version_wrong_arch_and_insecure_url() {
        let (key, pubkey) = keypair();
        let keys = [pubkey.as_str()];
        let dir = std::env::temp_dir().join(format!("felldl-verify-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pkg.bin");
        std::fs::write(&file, b"hello world!").unwrap();

        let mut payload = sample_payload();
        payload.artifacts[0].sha256 = file_sha256(&file).unwrap();
        let text = manifest_with(&payload, &key);

        // 正常路径：签名 + 版本 + 平台/架构 + 摘要都对
        let ok = verify_for_install_with_keys(&keys, &text, "windows", "x86_64", "0.3.9", &file)
            .expect("合法清单应通过");
        assert_eq!(ok.version, "0.4.0");

        // 低版本（等于当前版本）拒绝
        let err = verify_for_install_with_keys(&keys, &text, "windows", "x86_64", "0.4.0", &file)
            .unwrap_err()
            .to_string();
        assert!(err.contains("不高于当前版本"), "实际: {err}");

        // 错误架构拒绝，不会"随便挑一个"
        let err = verify_for_install_with_keys(&keys, &text, "windows", "aarch64", "0.3.9", &file)
            .unwrap_err()
            .to_string();
        assert!(err.contains("没有 windows/aarch64"), "实际: {err}");

        // 非 https 产物地址拒绝
        let mut insecure = sample_payload();
        insecure.artifacts[0].sha256 = file_sha256(&file).unwrap();
        insecure.artifacts[0].url = "http://example.com/pkg.exe".into();
        let text = manifest_with(&insecure, &key);
        let err = verify_for_install_with_keys(&keys, &text, "windows", "x86_64", "0.3.9", &file)
            .unwrap_err()
            .to_string();
        assert!(err.contains("必须是 https"), "实际: {err}");

        // 产物版本与清单不一致拒绝
        let mut mismatched = sample_payload();
        mismatched.artifacts[0].sha256 = file_sha256(&file).unwrap();
        mismatched.artifacts[0].version = "0.3.0".into();
        let text = manifest_with(&mismatched, &key);
        let err = verify_for_install_with_keys(&keys, &text, "windows", "x86_64", "0.3.9", &file)
            .unwrap_err()
            .to_string();
        assert!(err.contains("不一致"), "实际: {err}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
