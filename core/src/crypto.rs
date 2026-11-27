//! 媒体解密：HLS AES-128-CBC 分片解密（手写 CBC 链 + PKCS7 去填充，避免多余依赖）

use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use anyhow::{anyhow, bail, Result};

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

/// AES-128-CBC 加密（PKCS7 填充）。
///
/// 生产链路只用到解密；这里提供加密是为了让「默认 IV 取媒体序号」「密钥轮换」
/// 这类 HLS 语义能被端到端测试覆盖——否则只能测「不 panic」，测不出解出来的
/// 内容到底对不对。
pub fn aes128_cbc_encrypt(key: &[u8], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    if key.len() != 16 {
        bail!("密钥长度必须为 16 字节");
    }
    let cipher = aes::Aes128::new_from_slice(key).map_err(|e| anyhow!("密钥无效: {e}"))?;
    let pad = 16 - (data.len() % 16);
    let mut buf = data.to_vec();
    buf.extend(std::iter::repeat(pad as u8).take(pad));
    let mut prev: [u8; 16] = *iv;
    for chunk in buf.chunks_exact_mut(16) {
        for i in 0..16 {
            chunk[i] ^= prev[i];
        }
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
        prev.copy_from_slice(chunk);
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 加解密互为逆运算（含非整块长度，覆盖 PKCS7 填充）
    #[test]
    fn roundtrip_various_lengths() {
        let key = [0x2bu8; 16];
        let iv = [0x0fu8; 16];
        for len in [0usize, 1, 15, 16, 17, 1000] {
            let plain: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let ct = aes128_cbc_encrypt(&key, &iv, &plain).unwrap();
            assert_eq!(ct.len() % 16, 0);
            assert!(ct.len() > plain.len(), "必须带填充块");
            let back = aes128_cbc_decrypt(&key, &iv, &ct).unwrap();
            assert_eq!(back, plain, "长度 {len} 时往返不一致");
        }
    }

    /// 换 IV 解出的内容必须不同（证明 IV 真的参与了链）
    #[test]
    fn iv_actually_matters() {
        let key = [1u8; 16];
        let plain = b"0123456789abcdef-payload".to_vec();
        let ct = aes128_cbc_encrypt(&key, &[0u8; 16], &plain).unwrap();
        let wrong = aes128_cbc_decrypt(&key, &[9u8; 16], &ct);
        // 要么填充校验失败，要么解出别的内容——都不能等于原文
        assert!(match wrong {
            Ok(v) => v != plain,
            Err(_) => true,
        });
    }
}
