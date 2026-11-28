//! MCP 工具：schema、摘要与参数裁剪都来自 `felldl_core::mcp`，这里只负责把调用
//! 转发到本地 HTTP API。
//!
//! 独立进程如今只剩一个用途——给「只支持 stdio、不认远程 URL」的客户端做桥。
//! 内嵌服务已经在 `POST /mcp` 上提供完全相同的工具集，协议分发也只有 core 那一份。

use crate::api::Api;
use anyhow::Result;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;

pub use felldl_core::mcp::{add_body, need_str, summarize, TOOL_NAMES};

fn pretty(v: Value) -> String {
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
}

async fn call_http(api: &Api, name: &str, args: &Value) -> Result<String> {
    match name {
        "felldl_stats" => Ok(pretty(api.stats().await?)),
        "felldl_list_tasks" => {
            let state = args.get("state").and_then(|v| v.as_str());
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .min(200) as usize;
            let tasks = api.list().await?;
            let filtered: Vec<Value> = tasks
                .iter()
                .filter(|t| state.is_none_or(|s| t.get("state").and_then(|v| v.as_str()) == Some(s)))
                .map(summarize)
                .collect();
            let total = filtered.len();
            let shown: Vec<Value> = filtered.into_iter().take(limit).collect();
            Ok(pretty(json!({
                "total": total,
                "returned": shown.len(),
                "tasks": shown
            })))
        }
        "felldl_get_task" => {
            let id = need_str(args, "id")?;
            Ok(pretty(summarize(&api.get(&id).await?)))
        }
        "felldl_add_download" => {
            let url = need_str(args, "url")?;
            if !url.contains("://") && !url.starts_with("magnet:") {
                anyhow::bail!("url 不是合法地址：{url}");
            }
            let resp = api.add(add_body(args)).await?;
            let id = resp
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(pretty(
                json!({ "ok": true, "id": id, "url": url, "hint": "用 felldl_get_task 轮询进度" }),
            ))
        }
        "felldl_pause_task" => {
            let id = need_str(args, "id")?;
            Ok(pretty(api.pause(&id).await?))
        }
        "felldl_resume_task" => {
            let id = need_str(args, "id")?;
            Ok(pretty(api.resume(&id).await?))
        }
        "felldl_cancel_task" => {
            let id = need_str(args, "id")?;
            let delete_file = args
                .get("delete_file")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(pretty(api.cancel(&id, delete_file).await?))
        }
        other => anyhow::bail!("未知工具：{other}（可用：{}）", TOOL_NAMES.join(", ")),
    }
}

impl felldl_core::mcp::ToolBackend for Api {
    fn call<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(call_http(self, name, args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Config;

    #[tokio::test]
    async fn unknown_tool_is_an_error() {
        // 只验证分发层拒绝对未知工具动网络
        let api = Api::new(Config::new(1, "t".into(), "test")).unwrap();
        let e = call_http(&api, "felldl_nope", &json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("未知工具"), "{e}");
    }

    #[tokio::test]
    async fn missing_required_argument_never_hits_network() {
        let api = Api::new(Config::new(1, "t".into(), "test")).unwrap();
        let e = call_http(&api, "felldl_get_task", &json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("缺少参数 id"), "{e}");
    }
}
