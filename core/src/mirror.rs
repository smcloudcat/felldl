//! 下载镜像加速：把 GitHub / HuggingFace 等常见站点改写成国内可用镜像地址。
//!
//! 设计原则：
//! - **默认关闭**，只在用户明确开启时才改写，且只对明确支持的站点生效；
//! - 只生成「候选镜像」，由引擎在分片重试时轮换使用，主地址始终排在第一位，
//!   镜像挂了会自动退回直连，不会把任务卡死；
//! - 不做任何 DNS / 证书层面的劫持，只是加一个 URL 前缀或换一个 host。
use crate::model::Settings;

/// 预设线路：前缀型镜像（`https://mirror/https://github.com/...`）
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub prefixes: &'static [&'static str],
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "auto",
        label: "自动（多线路轮换）",
        // 2026-11 实测可用；不可用的线路会在探测阶段被剔除，不影响任务
        prefixes: &["https://ghfast.top/", "https://ghproxy.net/"],
    },
    Preset {
        id: "ghfast",
        label: "ghfast.top",
        prefixes: &["https://ghfast.top/"],
    },
    Preset {
        id: "ghproxy",
        label: "ghproxy.net",
        prefixes: &["https://ghproxy.net/"],
    },
    Preset {
        id: "hf",
        label: "hf-mirror.com（HuggingFace）",
        prefixes: &[],
    },
    Preset {
        id: "custom",
        label: "自定义前缀",
        prefixes: &[],
    },
];

/// 支持前缀型镜像的 host
const PREFIX_HOSTS: &[&str] = &[
    "github.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "codeload.github.com",
    "gist.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    // 去掉端口与 userinfo
    let host = host.rsplit('@').next().unwrap_or(host);
    Some(host.split(':').next().unwrap_or(host))
}

/// 是否为 HuggingFace 资源地址
fn is_hf(url: &str) -> bool {
    matches!(host_of(url), Some("huggingface.co") | Some("cdn-lfs.huggingface.co"))
}

/// 构造候选镜像 URL（不含主地址本身）
///
/// - `mode`：预设线路 id（auto/ghfast/ghproxy/hf/custom）
/// - `custom`：自定义前缀，仅 `mode = "custom"` 时使用
pub fn mirror_urls(url: &str, mode: &str, custom: &str) -> Vec<String> {
    let url = url.trim();
    if url.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut push = |u: String| {
        if u != url && !out.contains(&u) {
            out.push(u);
        }
    };

    // HuggingFace：换 host（前面可能已有 https://）
    if mode == "hf" || mode == "auto" {
        if is_hf(url) {
            if let Some(rest) = url.split_once("://").map(|(_, r)| r) {
                let path = rest.split_once('/').map(|(_, p)| p).unwrap_or("");
                push(format!("https://hf-mirror.com/{path}"));
            }
            if mode == "hf" {
                return out;
            }
        }
    }
    if mode == "hf" {
        return out;
    }

    let prefixes: Vec<String> = if mode == "custom" {
        let p = custom.trim();
        vec![if p.is_empty() { String::new() } else { normalize_prefix(p) }]
    } else {
        PRESETS
            .iter()
            .find(|p| p.id == mode)
            .map(|p| p.prefixes.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    };

    if prefixes.iter().all(|p| p.is_empty()) {
        return out;
    }
    // 只对明确支持的 host 做前缀改写，其它站点原样返回
    let Some(host) = host_of(url) else {
        return out;
    };
    let supported = PREFIX_HOSTS.iter().any(|h| host.eq_ignore_ascii_case(h));
    if !supported {
        return out;
    }
    for p in prefixes {
        if p.is_empty() {
            continue;
        }
        push(format!("{p}{url}"));
    }
    out
}

fn normalize_prefix(p: &str) -> String {
    let p = p.trim();
    let p = if p.starts_with("http://") || p.starts_with("https://") {
        p.to_string()
    } else {
        format!("https://{p}")
    };
    if p.ends_with('/') {
        p
    } else {
        format!("{p}/")
    }
}

/// 按当前设置取镜像候选（关闭时返回空）
pub fn candidates(url: &str, s: &Settings) -> Vec<String> {
    if !s.mirror_enabled {
        return Vec::new();
    }
    mirror_urls(url, &s.mirror_mode, &s.mirror_custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_github_release_url() {
        let u = "https://github.com/o/r/releases/download/v1/a.zip";
        let v = mirror_urls(u, "auto", "");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], format!("https://ghfast.top/{u}"));
        assert!(v.iter().all(|m| m.ends_with("a.zip")));
    }

    #[test]
    fn ignores_unsupported_host() {
        let v = mirror_urls("https://example.com/a.zip", "auto", "");
        assert!(v.is_empty());
    }

    #[test]
    fn custom_prefix_normalized() {
        let v = mirror_urls("https://raw.githubusercontent.com/o/r/main/a.txt", "custom", "mirror.example");
        assert_eq!(v, vec!["https://mirror.example/https://raw.githubusercontent.com/o/r/main/a.txt"]);
    }

    #[test]
    fn hf_host_swap() {
        let v = mirror_urls("https://huggingface.co/big/model/resolve/main/w.bin", "hf", "");
        assert_eq!(v, vec!["https://hf-mirror.com/big/model/resolve/main/w.bin"]);
    }

    #[test]
    fn disabled_returns_nothing() {
        let mut s = Settings::default();
        s.mirror_enabled = false;
        assert!(candidates("https://github.com/o/r/archive/refs/heads/main.zip", &s).is_empty());
        s.mirror_enabled = true;
        assert!(!candidates("https://github.com/o/r/archive/refs/heads/main.zip", &s).is_empty());
    }
}
