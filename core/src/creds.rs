//! 加密凭据库：cookie / password 不再跟着任务 JSON 明文落盘。
//!
//! 为什么单独存：任务 JSON 会被分享、被打包进诊断日志、被备份工具同步；
//! 凭据一旦明文躺在里面，就等于把账号交出去。而 IPC / HTTP API 只做「掩码」
//! （见 `Task::redacted`），掩码挡不住磁盘上的明文。
//!
//! 方案：整个凭据表序列化成一个 JSON，用 AES-256-GCM 加密后写 `creds.enc`；
//! 主密钥 32 字节随机生成，单独存 `creds.key`，在 Unix 上以 0600 建立。
//!
//! 边界说明：
//! - 这防的是「文件被拷走 / 被日志采集」，不防「攻击者已能读同用户目录」——
//!   那种情况下他能直接读 key 文件。要再进一步需要 DPAPI / Keystore，属后续工作。
//! - 主密钥丢失或损坏时无法解密：把坏文件改名 `.bad` 保留现场，然后当空库启动，
//!   用户重填凭据即可继续，不阻塞启动。
//! - 文件里的 AAD 固定为空：绑定文件名/版本会让迁移变复杂，收益有限。

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 6] = b"FDLCR1";
const KEY_FILE: &str = "creds.key";
const DATA_FILE: &str = "creds.enc";
const KEY_LEN: usize = 32;

/// 单个任务的凭据。用户名不在这里：它不是秘密，且界面上要显示。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCreds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl TaskCreds {
    pub fn is_empty(&self) -> bool {
        self.cookie.as_deref().unwrap_or("").is_empty()
            && self.password.as_deref().unwrap_or("").is_empty()
    }
}

pub struct CredStore {
    path: PathBuf,
    key: [u8; KEY_LEN],
    data: Mutex<HashMap<String, TaskCreds>>,
}

impl CredStore {
    /// 打开（必要时创建）凭据库。
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).context("创建凭据目录失败")?;
        let key = load_or_create_key(&root.join(KEY_FILE))?;
        let path = root.join(DATA_FILE);
        let data = match read_all(&path, &key) {
            Ok(d) => d,
            Err(e) => {
                // 损坏 / 换机器：保留现场，按空库启动，不把用户挡在门外
                let bad = path.with_extension("enc.bad");
                let _ = std::fs::rename(&path, &bad);
                eprintln!("[creds] 凭据库无法解密（已改名 {}）：{e:#}", bad.display());
                HashMap::new()
            }
        };
        Ok(Self {
            path,
            key,
            data: Mutex::new(data),
        })
    }

    pub fn get(&self, id: &str) -> Option<TaskCreds> {
        self.data.lock().get(id).cloned()
    }

    /// 写入/覆盖。空凭据等价于删除，避免残留一条全空记录。
    pub fn set(&self, id: &str, creds: &TaskCreds) -> Result<()> {
        {
            let mut g = self.data.lock();
            if creds.is_empty() {
                if g.remove(id).is_none() {
                    return Ok(());
                }
            } else {
                g.insert(id.to_string(), creds.clone());
            }
        }
        self.persist()
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        {
            let mut g = self.data.lock();
            if g.remove(id).is_none() {
                return Ok(());
            }
        }
        self.persist()
    }

    pub fn len(&self) -> usize {
        self.data.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn persist(&self) -> Result<()> {
        let plain = {
            let g = self.data.lock();
            serde_json::to_vec(&*g)?
        };
        let blob = seal(&self.key, &plain)?;
        write_atomic(&self.path, &blob)
    }
}

fn seal(key: &[u8; KEY_LEN], plain: &[u8]) -> Result<Vec<u8>> {
    let k = LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, key).map_err(|_| anyhow!("凭据主密钥长度无效"))?,
    );
    let mut nonce_bytes = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| anyhow!("生成随机数失败"))?;
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let mut buf = plain.to_vec();
    k.seal_in_place_append_tag(nonce, Aad::empty(), &mut buf)
        .map_err(|_| anyhow!("凭据加密失败"))?;
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + buf.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&buf);
    Ok(out)
}

fn open_blob(key: &[u8; KEY_LEN], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < MAGIC.len() + NONCE_LEN + 16 {
        return Err(anyhow!("凭据文件长度不足"));
    }
    if &blob[..MAGIC.len()] != MAGIC {
        return Err(anyhow!("凭据文件标识不匹配"));
    }
    let mut nonce_bytes = [0u8; NONCE_LEN];
    nonce_bytes.copy_from_slice(&blob[MAGIC.len()..MAGIC.len() + NONCE_LEN]);
    let k = LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, key).map_err(|_| anyhow!("凭据主密钥长度无效"))?,
    );
    let mut buf = blob[MAGIC.len() + NONCE_LEN..].to_vec();
    let plain = k
        .open_in_place(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::empty(),
            &mut buf,
        )
        .map_err(|_| anyhow!("凭据解密失败（文件被改动或换了机器）"))?;
    Ok(plain.to_vec())
}

fn read_all(path: &Path, key: &[u8; KEY_LEN]) -> Result<HashMap<String, TaskCreds>> {
    let blob = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e.into()),
    };
    let plain = open_blob(key, &blob)?;
    let map = serde_json::from_slice(&plain).context("凭据明文不是有效的 JSON")?;
    Ok(map)
}

fn load_or_create_key(path: &Path) -> Result<[u8; KEY_LEN]> {
    if path.exists() {
        let bytes = std::fs::read(path).context("读取凭据主密钥失败")?;
        if bytes.len() != KEY_LEN {
            return Err(anyhow!(
                "凭据主密钥长度异常：{} 字节（应为 {KEY_LEN}）",
                bytes.len()
            ));
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }
    let mut key = [0u8; KEY_LEN];
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| anyhow!("生成凭据主密钥失败"))?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 只有本用户可读写；父目录的权限仍然重要，但至少不是 0644
        opts.mode(0o600);
    }
    let mut f = opts.open(path).context("创建凭据主密钥失败")?;
    f.write_all(&key).context("写入凭据主密钥失败")?;
    f.sync_all().ok();
    Ok(key)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("enc.tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).context("创建凭据临时文件失败")?;
        f.write_all(data).context("写入凭据失败")?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path).context("替换凭据文件失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "felldl-creds-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn roundtrip_across_reopen() {
        let root = temp_root("roundtrip");
        {
            let s = CredStore::open(&root).unwrap();
            s.set(
                "t1",
                &TaskCreds {
                    cookie: Some("session=abc".into()),
                    password: Some("hunter2".into()),
                },
            )
            .unwrap();
            s.set("t2", &TaskCreds::default()).unwrap(); // 空 = 不存
            assert_eq!(s.len(), 1);
        }
        let s = CredStore::open(&root).unwrap();
        let c = s.get("t1").expect("重开后凭据应还在");
        assert_eq!(c.cookie.as_deref(), Some("session=abc"));
        assert_eq!(c.password.as_deref(), Some("hunter2"));
        assert!(s.get("t2").is_none(), "空凭据不该落成记录");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 磁盘上不能出现明文——这是这次改造的全部意义。
    #[test]
    fn secrets_are_not_plaintext_on_disk() {
        let root = temp_root("plain");
        let s = CredStore::open(&root).unwrap();
        s.set(
            "t1",
            &TaskCreds {
                cookie: Some("SESSION-SECRET-123".into()),
                password: Some("PASSWORD-SECRET-456".into()),
            },
        )
        .unwrap();
        let blob = std::fs::read(root.join(DATA_FILE)).unwrap();
        let text = String::from_utf8_lossy(&blob);
        assert!(!text.contains("SESSION-SECRET-123"));
        assert!(!text.contains("PASSWORD-SECRET-456"));
        // 主密钥本身也不能等于明文
        let key = std::fs::read(root.join(KEY_FILE)).unwrap();
        assert_eq!(key.len(), KEY_LEN);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 文件被改一个字节 → 解密失败 → 按空库启动并保留 .bad，不 panic。
    #[test]
    fn tampered_file_falls_back_to_empty() {
        let root = temp_root("tamper");
        {
            let s = CredStore::open(&root).unwrap();
            s.set(
                "t1",
                &TaskCreds {
                    cookie: Some("c".into()),
                    password: None,
                },
            )
            .unwrap();
        }
        let p = root.join(DATA_FILE);
        let mut blob = std::fs::read(&p).unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        std::fs::write(&p, &blob).unwrap();

        let s = CredStore::open(&root).unwrap();
        assert!(s.is_empty(), "被篡改的库应按空库启动");
        assert!(root.join("creds.enc.bad").exists(), "坏文件要保留现场");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_deletes_record() {
        let root = temp_root("remove");
        let s = CredStore::open(&root).unwrap();
        s.set(
            "t1",
            &TaskCreds {
                cookie: Some("c".into()),
                password: None,
            },
        )
        .unwrap();
        s.remove("t1").unwrap();
        assert!(s.get("t1").is_none());
        let reopened = CredStore::open(&root).unwrap();
        assert!(reopened.get("t1").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
