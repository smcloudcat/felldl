//! 自定义 DNS 端到端测试：起一个只回 A 记录的本地 UDP DNS 服务器，
//! 验证引擎的 DNS 配置真的把域名解析到我们指定的地址。

use std::net::SocketAddr;
use tokio::net::UdpSocket;

/// 造一个最小 DNS 应答：拷贝问题段，追加一条 A 记录 127.0.0.1
fn build_reply(query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    // 跳过 header，逐标签找到问题段结尾（含 QTYPE/QCLASS 4 字节）
    let mut i = 12usize;
    loop {
        let len = *query.get(i)? as usize;
        if len == 0 {
            i += 1;
            break;
        }
        if len & 0xC0 != 0 {
            return None; // 不支持压缩指针的查询
        }
        i += 1 + len;
    }
    let qend = i + 4;
    let question = query.get(12..qend)?;
    // 只回答 A 查询；AAAA 等返回「无记录」，与真实 DNS 行为一致
    let qtype = u16::from_be_bytes([*query.get(i)?, *query.get(i + 1)?]);

    let mut out = Vec::with_capacity(qend + 16);
    out.extend_from_slice(&query[0..2]); // ID
    out.extend_from_slice(&[0x81, 0x80]); // 标准应答 / 递归可用 / 无错误
    out.extend_from_slice(&[0x00, 0x01]); // QDCOUNT
    out.extend_from_slice(&[0x00, if qtype == 1 { 0x01 } else { 0x00 }]); // ANCOUNT
    out.extend_from_slice(&[0x00, 0x00]); // NSCOUNT
    out.extend_from_slice(&[0x00, 0x00]); // ARCOUNT
    out.extend_from_slice(question);
    if qtype == 1 {
        out.extend_from_slice(&[0xC0, 0x0C]); // 名字指向问题段
        out.extend_from_slice(&[0x00, 0x01]); // A
        out.extend_from_slice(&[0x00, 0x01]); // IN
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x3C]); // TTL 60
        out.extend_from_slice(&[0x00, 0x04]); // RDLENGTH
        out.extend_from_slice(&[127, 0, 0, 1]);
    }
    Some(out)
}

#[tokio::test]
async fn custom_dns_resolves_via_given_server() {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = sock.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 1500];
        for _ in 0..32 {
            let (n, peer) = match sock.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(_) => break,
            };
            if let Some(reply) = build_reply(&buf[..n]) {
                let _ = sock.send_to(&reply, peer).await;
            }
        }
    });

    let servers = felldl_core::dns::parse_servers(&format!("127.0.0.1:{}", addr.port())).unwrap();
    let ips = felldl_core::dns::resolve_with(&servers, "only-local.test")
        .await
        .unwrap();
    assert!(
        ips.iter().any(|ip| ip.to_string() == "127.0.0.1"),
        "{ips:?}"
    );
}
