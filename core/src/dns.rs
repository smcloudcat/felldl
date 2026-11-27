//! 自定义 DNS 支持
//!
//! 允许用户用自建 / 公共服务（如 1.1.1.1、223.5.5.5）替代系统 DNS，
//! 用于绕开被污染或过慢的运营商解析。解析结果由 reqwest 的
//! `dns_resolver` 接口接管，HTTP / HTTPS / FTP / HLS 全部共用。

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use hickory_resolver::config::{NameServerConfig, Protocol, ResolverConfig, ResolverOpts};
use hickory_resolver::TokioAsyncResolver;

/// 解析用户填写的 DNS 服务器列表。
///
/// 支持格式：`1.1.1.1`、`8.8.8.8:53`、`tcp://1.1.1.1`、`2001:4860:4860::8888`，
/// 多个服务器用逗号 / 空格 / 分号分隔。
pub fn parse_servers(input: &str) -> Result<Vec<(IpAddr, u16, Protocol)>> {
    let mut out = Vec::new();
    for raw in input.split([',', ';', ' ', '\n', '\t']) {
        let tok = raw.trim();
        if tok.is_empty() {
            continue;
        }
        let (proto, rest) = if let Some(r) = tok.strip_prefix("tcp://") {
            (Protocol::Tcp, r)
        } else if let Some(r) = tok.strip_prefix("udp://") {
            (Protocol::Udp, r)
        } else {
            (Protocol::Udp, tok)
        };
        let (ip, port) = if let Ok(sa) = rest.parse::<SocketAddr>() {
            (sa.ip(), sa.port())
        } else {
            let ip: IpAddr = rest
                .parse()
                .with_context(|| format!("DNS 服务器地址无效: {tok}"))?;
            (ip, 53)
        };
        out.push((ip, port, proto));
    }
    if out.is_empty() {
        anyhow::bail!("没有可用的 DNS 服务器地址");
    }
    Ok(out)
}

/// 构造只使用指定 nameserver 的解析器
pub fn build_resolver(servers: &[(IpAddr, u16, Protocol)]) -> Result<TokioAsyncResolver> {
    let mut cfg = ResolverConfig::new();
    for (ip, port, proto) in servers {
        cfg.add_name_server(NameServerConfig {
            socket_addr: SocketAddr::new(*ip, *port),
            protocol: *proto,
            tls_dns_name: None,
            bind_addr: None,
            trust_negative_responses: true,
        });
    }
    let mut opts = ResolverOpts::default();
    // 单服务器失败时快速切下一个，避免下载卡在解析上
    opts.timeout = std::time::Duration::from_secs(3);
    opts.attempts = 2;
    opts.validate = false;
    // 先查 A、无记录再查 AAAA：下载站绝大多数有 IPv4，
    // 且能避免「AAAA 无记录」被当作解析失败（部分 DNS 会这样回）
    opts.ip_strategy = hickory_resolver::config::LookupIpStrategy::Ipv4thenIpv6;
    // 进程内缓存，重复域名不再反复查询
    opts.cache_size = 512;
    Ok(TokioAsyncResolver::tokio(cfg, opts))
}

/// reqwest 的自定义解析器包装
#[derive(Clone)]
pub struct HickoryDns {
    pub resolver: Arc<TokioAsyncResolver>,
}

impl HickoryDns {
    pub fn new(servers: &[(IpAddr, u16, Protocol)]) -> Result<Self> {
        Ok(Self {
            resolver: Arc::new(build_resolver(servers)?),
        })
    }
}

impl reqwest::dns::Resolve for HickoryDns {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let resolver = self.resolver.clone();
        Box::pin(async move {
            let lookup = resolver
                .lookup_ip(name.as_str())
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
            let addrs: Vec<SocketAddr> = lookup.iter().map(|ip| SocketAddr::new(ip, 0)).collect();
            if addrs.is_empty() {
                return Err("DNS 未返回任何地址".into());
            }
            Ok(Box::new(addrs.into_iter())
                as Box<dyn Iterator<Item = SocketAddr> + Send + 'static>)
        })
    }
}

/// 用指定 DNS 服务器解析域名（供「测试 DNS」用）
pub async fn resolve_with(servers: &[(IpAddr, u16, Protocol)], host: &str) -> Result<Vec<IpAddr>> {
    let r = build_resolver(servers)?;
    let lookup = r
        .lookup_ip(host)
        .await
        .with_context(|| format!("解析 {host} 失败"))?;
    Ok(lookup.iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mixed_servers() {
        let s =
            parse_servers("1.1.1.1, 8.8.8.8:5353 tcp://223.5.5.5;2001:4860:4860::8888").unwrap();
        assert_eq!(s.len(), 4);
        assert_eq!(s[0], ("1.1.1.1".parse().unwrap(), 53, Protocol::Udp));
        assert_eq!(s[1], ("8.8.8.8".parse().unwrap(), 5353, Protocol::Udp));
        assert_eq!(s[2], ("223.5.5.5".parse().unwrap(), 53, Protocol::Tcp));
        assert_eq!(s[3].0, "2001:4860:4860::8888".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn parse_empty_is_error() {
        assert!(parse_servers("  , ; ").is_err());
        assert!(parse_servers("not-an-ip").is_err());
    }
}
