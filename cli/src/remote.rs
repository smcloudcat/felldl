//! 本地 API 客户端：把 `fdl` 的远程模式翻译成 HTTP 请求。
//!
//! 只依赖 `/ping`、`/add`、`/tasks`、`/tasks/{id}`、`/tasks/{id}/{verb}`、`/stats`
//! 这几个已经稳定的端点，不碰 MCP 那套协议——CLI 不需要 JSON-RPC。

use anyhow::{bail, Result};
use serde_json::Value;
use std::time::Duration;

pub struct Remote {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Remote {
    pub fn new(port: u16, token: String) -> Self {
        let http = reqwest::Client::builder()
            // 本地回环，正常响应在毫秒级；10s 足够，也让「卡住」尽快变成错误
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            base: format!("http://127.0.0.1:{port}"),
            token,
            http,
        }
    }

    /// 健康检查。`/ping` 不需要鉴权，所以它只证明「端口上有 Felldl」。
    pub async fn ping(&self) -> bool {
        self.http
            .get(format!("{}/ping", self.base))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    pub async fn add(&self, body: &Value) -> Result<String> {
        let v = self
            .post_json("/add", Some(body))
            .await?
            .ok_or_else(|| anyhow::anyhow!("服务端没有返回任务 ID"))?;
        v.get("id")
            .and_then(|i| i.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("响应缺少 id 字段：{v}"))
    }

    pub async fn tasks(&self) -> Result<Vec<Value>> {
        let v = self
            .get_json("/tasks")
            .await?
            .ok_or_else(|| anyhow::anyhow!("响应缺少 tasks 字段"))?;
        match v.get("tasks").and_then(|t| t.as_array()) {
            Some(a) => Ok(a.clone()),
            None => bail!("响应缺少 tasks 数组"),
        }
    }

    pub async fn stats(&self) -> Result<Value> {
        self.get_json("/stats")
            .await?
            .ok_or_else(|| anyhow::anyhow!("服务端没有返回统计数据"))
    }

    /// 单个任务（用于操作后确认实际状态，而不是「我说暂停了就暂停了」）
    ///
    /// 注意返回的是 `{"ok":true,"task":{...}}`，不是裸任务对象。
    pub async fn task(&self, id: &str) -> Result<Value> {
        let v = self
            .get_json(&format!("/tasks/{id}"))
            .await?
            .ok_or_else(|| anyhow::anyhow!("服务端没有返回任务 {id}"))?;
        Ok(v.get("task").cloned().unwrap_or(v))
    }

    pub async fn action(&self, id: &str, verb: &str) -> Result<()> {
        self.post_json(&format!("/tasks/{id}/{verb}"), None).await?;
        Ok(())
    }

    pub async fn cancel(&self, id: &str, delete_file: bool) -> Result<()> {
        let body = serde_json::json!({ "delete_file": delete_file });
        self.post_json(&format!("/tasks/{id}/cancel"), Some(&body))
            .await?;
        Ok(())
    }

    // ───────────────────── 内部 ─────────────────────

    async fn get_json(&self, path: &str) -> Result<Option<Value>> {
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .header("X-Felldl-Token", &self.token)
            .send()
            .await?;
        finish(resp).await
    }

    async fn post_json(&self, path: &str, body: Option<&Value>) -> Result<Option<Value>> {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .header("X-Felldl-Token", &self.token);
        if let Some(b) = body {
            req = req.json(b);
        }
        finish(req.send().await?).await
    }
}

/// 统一处理响应：非 2xx 时把服务端的 `error` 原文抬出来。
///
/// 服务端的错误是中文人话（「任务不存在」「token 无效」），比 HTTP 状态码有用得多；
/// 只有状态码没有正文时才退回状态码。
async fn finish(resp: reqwest::Response) -> Result<Option<Value>> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("令牌无效（401）：请对照应用「设置 → 浏览器集成」里的令牌，或用 --token 指定");
    }
    if !status.is_success() {
        let msg = parsed
            .get("error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| text.trim().to_string());
        bail!("请求失败（HTTP {}）：{msg}", status.as_u16());
    }
    Ok(if parsed.is_null() { None } else { Some(parsed) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_is_loopback_only() {
        // 只允许回环：CLI 不该把令牌发到别的地址
        let r = Remote::new(8899, "t".into());
        assert_eq!(r.base, "http://127.0.0.1:8899");
    }
}
