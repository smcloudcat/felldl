//! 更新清单签名工具（发布流程用，不参与运行时）。
//!
//! 用法：
//! ```text
//! # 1) 生成密钥对（只做一次；私钥离线保存，绝不入库）
//! cargo run -p felldl-core --example update-sign -- gen --out ../src-tauri/keys
//!
//! # 2) 算出产物摘要，填进 artifacts（payload.json）
//! cargo run -p felldl-core --example update-sign -- hash --in dist/Felldl-0.4.0-x64.exe
//!
//! # 3) 对 payload.json 签名，产出分发给客户端/发布源的 update-manifest.json
//! cargo run -p felldl-core --example update-sign -- sign \
//!   --key ../src-tauri/keys/update-signing.pkcs8 \
//!   --in payload.json --out update-manifest.json
//! ```
//!
//! `gen` 打印的公钥（base64）需要粘贴到 `core/src/update.rs` 的
//! `UPDATE_PUBLIC_KEY`，之后打包发布的客户端才能验证清单。

use base64::Engine as _;
use felldl_core::update::{canonical_bytes, file_sha256, Payload};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{}", usage());
        std::process::exit(2);
    }
    let result = match args[0].as_str() {
        "gen" => cmd_gen(flag(&args, "--out")),
        "sign" => cmd_sign(
            flag(&args, "--key"),
            flag(&args, "--in"),
            flag(&args, "--out"),
        ),
        "hash" => cmd_hash(flag(&args, "--in")),
        "verify" => cmd_verify(flag(&args, "--in")),
        "-h" | "--help" | "help" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(format!("未知子命令: {other}\n{}", usage())),
    };
    if let Err(e) = result {
        eprintln!("[!] {e}");
        std::process::exit(1);
    }
}

fn usage() -> String {
    "用法:\n  update-sign gen    --out <目录>\n  update-sign sign   --key <私钥.pkcs8> --in <payload.json> --out <manifest.json>\n  update-sign hash   --in <文件>\n  update-sign verify --in <manifest.json>\n".into()
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn cmd_gen(out_dir: Option<String>) -> Result<(), String> {
    let out_dir = PathBuf::from(out_dir.ok_or("gen 需要 --out <目录>")?);
    let key_path = out_dir.join("update-signing.pkcs8");
    if key_path.exists() {
        return Err(format!(
            "{} 已存在：拒绝覆盖已有私钥（轮换请显式删除或换目录）",
            key_path.display()
        ));
    }
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("创建目录失败: {e}"))?;

    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e| format!("生成密钥失败: {e:?}"))?;
    let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|e| format!("加载生成的密钥失败: {e:?}"))?;
    let pubkey = base64::engine::general_purpose::STANDARD.encode(keypair.public_key().as_ref());

    std::fs::write(&key_path, pkcs8.as_ref()).map_err(|e| format!("写私钥失败: {e}"))?;
    std::fs::write(out_dir.join("update-signing.pub"), format!("{pubkey}\n"))
        .map_err(|e| format!("写公钥失败: {e}"))?;

    // 私钥文件在 Linux/macOS 上收权限；Windows 由目录 ACL 控制
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }

    println!("私钥（离线保存，不入库）: {}", key_path.display());
    println!("公钥（粘贴到 core/src/update.rs 的 UPDATE_PUBLIC_KEY）:");
    println!("{pubkey}");
    Ok(())
}

fn cmd_hash(input: Option<String>) -> Result<(), String> {
    let path = PathBuf::from(input.ok_or("hash 需要 --in <文件>")?);
    let size = std::fs::metadata(&path)
        .map_err(|e| format!("读取文件失败: {e}"))?
        .len();
    let sha256 = file_sha256(Path::new(&path)).map_err(|e| format!("{e:#}"))?;
    println!("size   = {size}");
    println!("sha256 = {sha256}");
    Ok(())
}

fn cmd_verify(input: Option<String>) -> Result<(), String> {
    let path = input.ok_or("verify 需要 --in <manifest.json>")?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读清单失败: {e}"))?;
    let payload =
        felldl_core::update::parse_and_verify(&text).map_err(|e| format!("验签失败: {e:#}"))?;
    println!("验签通过：版本 {}", payload.version);
    for a in &payload.artifacts {
        println!(
            "  {}/{}  {} 字节  sha256={}",
            a.platform, a.arch, a.size, a.sha256
        );
    }
    Ok(())
}

fn cmd_sign(
    key: Option<String>,
    input: Option<String>,
    output: Option<String>,
) -> Result<(), String> {
    let key_path = key.ok_or("sign 需要 --key <私钥.pkcs8>")?;
    let in_path = input.ok_or("sign 需要 --in <payload.json>")?;
    let out_path = output.ok_or("sign 需要 --out <manifest.json>")?;

    let pkcs8 = std::fs::read(&key_path).map_err(|e| format!("读私钥失败: {e}"))?;
    let keypair = Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|e| format!("私钥格式无效: {e:?}"))?;

    let text = std::fs::read_to_string(&in_path).map_err(|e| format!("读清单失败: {e}"))?;
    // payload.json 允许带或不带外层 payload 字段
    let payload: Payload = match serde_json::from_str::<Payload>(&text) {
        Ok(p) => p,
        Err(_) => {
            let v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| format!("清单 JSON 无效: {e}"))?;
            serde_json::from_value(v.get("payload").cloned().ok_or("缺少 payload 字段")?)
                .map_err(|e| format!("payload 结构无效: {e}"))?
        }
    };

    // 自检：结构上不允许明显错误的产物条目
    if payload.artifacts.is_empty() {
        return Err("payload.artifacts 为空".into());
    }
    for a in &payload.artifacts {
        if !a.url.to_ascii_lowercase().starts_with("https://") {
            return Err(format!(
                "{} / {} 的地址不是 https: {}",
                a.platform, a.arch, a.url
            ));
        }
        if a.sha256.len() != 64 {
            return Err(format!("{} / {} 的 sha256 长度不是 64", a.platform, a.arch));
        }
        if a.version != payload.version {
            return Err(format!(
                "{} / {} 的 version {} 与清单 {} 不一致",
                a.platform, a.arch, a.version, payload.version
            ));
        }
    }

    let bytes = canonical_bytes(&payload).map_err(|e| format!("{e:#}"))?;
    let signature = base64::engine::general_purpose::STANDARD.encode(keypair.sign(&bytes).as_ref());
    let manifest = serde_json::json!({ "payload": &payload, "signature": signature });
    let pretty = serde_json::to_string_pretty(&manifest).map_err(|e| format!("序列化失败: {e}"))?;
    std::fs::write(&out_path, format!("{pretty}\n")).map_err(|e| format!("写清单失败: {e}"))?;

    println!("已签名: {out_path}");
    println!(
        "版本: {}，产物 {} 个",
        payload.version,
        payload.artifacts.len()
    );
    Ok(())
}
