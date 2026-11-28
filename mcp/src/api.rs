//! Felldl 本地 API 客户端 + 配置解析
//!
//! 只跟 127.0.0.1 上的 Felldl 进程说话，不直接读任务文件：
//! 任务的真值在引擎里，文件是我方复制的副本，绕过它会造成两边不一致。

use anyhow::{bail, Context, Result};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

/// 连接参数：端口与 token 来自 Felldl 设置（也可用环境变量/命令行覆盖）
#[derive(Clone, Debug)]
pub struct Config {
    pub base: String,
    pub token: String,
    /// 配置来源，便于出错时告诉用户该改哪里
    pub source: String,
}

impl Config {
    pub fn new(port: u16, token: String, source: impl Into<String>) -> Self {
        Self {
            base: format!("http://127.0.0.1:{port}"),
            token,
            source: source.into(),
        }
    }
}

/// 设置文件位置：与 Tauri 的 identifier 对应
pub fn settings_path() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|p| {
            PathBuf::from(p)
                .join("com.felldl.app")
                .join("settings.json")
        })
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p).join("Library/Application Support/com.felldl.app/settings.json")
        })
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
            .map(|p| p.join("com.felldl.app").join("settings.json"))
    }
}

fn settings_value() -> Option<Value> {
    let path = settings_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 解析配置：命令行 > 环境变量 > 设置文件
pub fn resolve(port_override: Option<u16>, token_override: Option<String>) -> Result<Config> {
    let settings = settings_value();

    let port = port_override
        .or_else(|| {
            std::env::var("FELLDL_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .or_else(|| {
            settings
                .as_ref()?
                .get("api_port")
                .and_then(|v| v.as_u64())
                .map(|v| v as u16)
        })
        .unwrap_or(8899);

    let (token, source) = match token_override {
        Some(t) => (t, "命令行 --token".to_string()),
        None => match std::env::var("FELLDL_TOKEN").ok().filter(|t| !t.is_empty()) {
            Some(t) => (t, "环境变量 FELLDL_TOKEN".to_string()),
            None => {
                let t = settings
                    .as_ref()
                    .and_then(|s| s.get("api_token"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let src = settings_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(未找到设置文件)".to_string());
                (t, src)
            }
        },
    };

    if token.is_empty() {
        bail!(
            "没有拿到 API Token。请在 Felldl「设置 → 浏览器集成」里复制 token，\
             或用环境变量 FELLDL_TOKEN / 参数 --token 传入（当前查找位置：{source}）"
        );
    }
    Ok(Config::new(port, token, source))
}

#[derive(Clone)]
pub struct Api {
    client: reqwest::Client,
    cfg: Config,
}

impl Api {
    pub fn new(cfg: Config) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .context("初始化 HTTP 客户端失败")?;
        Ok(Self { client, cfg })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{}", self.cfg.base, path);
        let mut req = self
            .client
            .request(method, &url)
            .header("X-Felldl-Token", &self.cfg.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.with_context(|| {
            format!(
                "连接 Felldl 失败（{}）。请确认 Felldl 正在运行、且「浏览器集成」已开启",
                self.cfg.base
            )
        })?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));

        if status == StatusCode::UNAUTHORIZED {
            bail!(
                "token 无效（来源：{}），请在 Felldl 设置里重新复制",
                self.cfg.source
            );
        }
        if !status.is_success() {
            let code = value
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("http_error");
            let msg = value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("请求失败");
            bail!("{code}: {msg}");
        }
        Ok(value)
    }

    pub async fn ping(&self) -> Result<Value> {
        self.call(Method::GET, "/ping", None).await
    }

    pub async fn stats(&self) -> Result<Value> {
        self.call(Method::GET, "/stats", None).await
    }

    pub async fn list(&self) -> Result<Vec<Value>> {
        let v = self.call(Method::GET, "/tasks", None).await?;
        Ok(v.get("tasks")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default())
    }

    pub async fn get(&self, id: &str) -> Result<Value> {
        let v = self
            .call(Method::GET, &format!("/tasks/{id}"), None)
            .await?;
        Ok(v.get("task").cloned().unwrap_or(Value::Null))
    }

    pub async fn add(&self, body: Value) -> Result<Value> {
        self.call(Method::POST, "/add", Some(body)).await
    }

    pub async fn pause(&self, id: &str) -> Result<Value> {
        self.call(Method::POST, &format!("/tasks/{id}/pause"), None)
            .await
    }

    pub async fn resume(&self, id: &str) -> Result<Value> {
        self.call(Method::POST, &format!("/tasks/{id}/resume"), None)
            .await
    }

    pub async fn cancel(&self, id: &str, delete_file: bool) -> Result<Value> {
        self.call(
            Method::POST,
            &format!("/tasks/{id}/cancel"),
            Some(json!({ "delete_file": delete_file })),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_builds_base_url() {
        let c = Config::new(8899, "t".into(), "test");
        assert_eq!(c.base, "http://127.0.0.1:8899");
    }

    #[test]
    fn missing_token_is_reported_with_hint() {
        // 只验证「空 token 一定报错」，不依赖机器上是否装了 Felldl
        let err = resolve(None, Some(String::new())).unwrap_err().to_string();
        assert!(
            err.contains("FELLDL_TOKEN"),
            "错误信息应给出获取方式：{err}"
        );
    }

    #[test]
    fn cli_token_wins() {
        let c = resolve(Some(9123), Some("abc".into())).unwrap();
        assert_eq!(c.token, "abc");
        assert_eq!(c.base, "http://127.0.0.1:9123");
        assert!(c.source.contains("命令行"));
    }
}
