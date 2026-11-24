//! 媒体解密：HLS AES-128-CBC 分片解密（手写 CBC 链 + PKCS7 去填充，避免多余依赖）

use anyhow::{anyhow, bail, Result};
use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit};

/// AES-128-CBC 解密，返回解密后的明文
pub fn aes128_cbc_decrypt(key: &[u8], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    if key.len() != 16 {
        bail!("密钥长度必须为 16 字节");
    }
    if data.is_empty() || data.len() % 16 != 0 {
        bail!("密文长度必须是 16 的倍数");
    }
    let mut buf = data.to_vec();
    let cipher = aes::Aes128::new_from_slice(key).map_err(|e| anyhow!("密钥无效: {e}"))?;
    let mut prev: [u8; 16] = *iv;
    for chunk in buf.chunks_exact_mut(16) {
        let mut block = GenericArray::clone_from_slice(chunk);
        let cur = block;
        cipher.decrypt_block(&mut block);
        for i in 0..16 {
            block[i] ^= prev[i];
        }
        prev = cur.into();
        chunk.copy_from_slice(&block);
    }
    // PKCS7 去填充
    let pad = *buf.last().unwrap() as usize;
    if pad == 0 || pad > 16 {
        bail!("PKCS7 填充无效");
    }
    if buf[buf.len() - pad..].iter().any(|&b| b as usize != pad) {
        bail!("PKCS7 填充校验失败");
    }
    buf.truncate(buf.len() - pad);
    Ok(buf)
}
