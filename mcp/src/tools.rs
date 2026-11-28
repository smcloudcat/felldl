//! MCP 工具定义与调用分发
//!
//! 工具只做三件事：把参数翻译成 Felldl 本地 API 的请求、把结果压成模型能读的形状、
//! 把错误原文（含稳定 code）带回去。业务语义全部留在引擎里，这里不复制判断逻辑。

use crate::api::Api;
use anyhow::{bail, Result};
use serde_json::{json, Value};

pub const TOOL_NAMES: &[&str] = &[
    "felldl_add_download",
    "felldl_list_tasks",
    "felldl_get_task",
    "felldl_pause_task",
    "felldl_resume_task",
    "felldl_cancel_task",
    "felldl_stats",
];

/// tools/list 的返回值
pub fn list_tools() -> Value {
    json!([
        {
            "name": "felldl_add_download",
            "title": "添加下载任务",
            "description": "把一个 URL 交给 Felldl 下载。返回任务 id；随后可用 felldl_get_task 轮询进度。\
                            支持指定保存目录、文件名、连接数、请求头、Cookie、镜像地址。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "下载地址（http/https/ftp/magnet）" },
                    "save_dir": { "type": "string", "description": "保存目录；省略则用 Felldl 默认下载目录" },
                    "file_name": { "type": "string", "description": "保存的文件名；省略则由 URL/响应头推断" },
                    "connections": { "type": "integer", "minimum": 1, "maximum": 32, "description": "分段连接数（HTTP）" },
                    "referer": { "type": "string", "description": "Referer 请求头" },
                    "cookie": { "type": "string", "description": "Cookie 请求头" },
                    "headers": { "type": "object", "additionalProperties": { "type": "string" }, "description": "附加请求头" },
                    "category": { "type": "string", "description": "任务分类" },
                    "mirrors": { "type": "array", "items": { "type": "string" }, "description": "备用镜像地址" }
                },
                "required": ["url"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true }
        },
        {
            "name": "felldl_list_tasks",
            "title": "列出下载任务",
            "description": "列出 Felldl 里的任务（默认最近 20 条，可按状态过滤）。只返回摘要字段。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "state": {
                        "type": "string",
                        "enum": ["queued", "probing", "downloading", "paused", "completed", "error", "scheduled"],
                        "description": "只返回该状态的任务"
                    },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "最多返回多少条，默认 20" }
                }
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "openWorldHint": false }
        },
        {
            "name": "felldl_get_task",
            "title": "查看单个任务",
            "description": "按 id 查看任务的进度、速度、保存路径与错误码。",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string", "description": "任务 id" } },
                "required": ["id"]
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "openWorldHint": false }
        },
        {
            "name": "felldl_pause_task",
            "title": "暂停任务",
            "description": "暂停一个正在下载或排队中的任务；已下载的部分保留，可随时继续。",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        },
        {
            "name": "felldl_resume_task",
            "title": "继续任务",
            "description": "继续一个已暂停或出错的任务（断点续传）。",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        },
        {
            "name": "felldl_cancel_task",
            "title": "取消任务",
            "description": "取消任务并从列表移除；delete_file=true 时连同已下载文件一起删除（默认只移除任务）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "delete_file": { "type": "boolean", "description": "是否删除本地文件，默认 false" }
                },
                "required": ["id"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "openWorldHint": false }
        },
        {
            "name": "felldl_stats",
            "title": "运行状态",
            "description": "Felldl 版本、任务总数、活跃任务数与当前总速度。",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "openWorldHint": false }
        }
    ])
}

fn need_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("缺少参数 {key}"))
}

/// 把完整任务压成模型关心的字段：不再回传分片明细与凭据
pub fn summarize(t: &Value) -> Value {
    let downloaded = t.get("downloaded").and_then(|v| v.as_u64()).unwrap_or(0);
    let size = t.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
    let percent = if size > 0 {
        (downloaded as f64 * 1000.0 / size as f64).round() / 10.0
    } else {
        0.0
    };
    json!({
        "id": t.get("id"),
        "name": t.get("file_name"),
        "state": t.get("state"),
        "kind": t.get("kind"),
        "percent": percent,
        "downloaded_bytes": downloaded,
        "size_bytes": if size > 0 { json!(size) } else { Value::Null },
        "speed_bps": t.get("speed"),
        "connections": t.get("connections"),
        "save_path": t.get("save_path"),
        "url": t.get("url"),
        "error": t.get("error"),
        "error_code": t.get("error_code"),
        "retryable": t.get("retryable"),
    })
}

/// `/add` 的请求体：只透传引擎认识的字段
fn add_body(args: &Value) -> Value {
    let mut body = json!({ "url": args.get("url").cloned().unwrap_or(Value::Null) });
    for key in [
        "save_dir",
        "file_name",
        "referer",
        "cookie",
        "headers",
        "category",
        "mirrors",
        "connections",
        "username",
        "password",
        "interval_secs",
    ] {
        if let Some(v) = args.get(key) {
            if !v.is_null() {
                body[key] = v.clone();
            }
        }
    }
    body
}

fn pretty(v: Value) -> String {
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
}

/// 执行工具，返回给模型的文本
pub async fn call(api: &Api, name: &str, args: &Value) -> Result<String> {
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
                .filter(|t| {
                    state.is_none_or(|s| t.get("state").and_then(|v| v.as_str()) == Some(s))
                })
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
                bail!("url 不是合法地址：{url}");
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
        other => bail!("未知工具：{other}（可用：{}）", TOOL_NAMES.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_schema_and_description() {
        let tools = list_tools();
        let arr = tools.as_array().unwrap();
        assert_eq!(arr.len(), TOOL_NAMES.len());
        for t in arr {
            let name = t["name"].as_str().unwrap();
            assert!(TOOL_NAMES.contains(&name), "多出来的工具：{name}");
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
            assert!(
                t["inputSchema"]["type"] == "object",
                "{name} 缺少 inputSchema"
            );
        }
        for want in TOOL_NAMES {
            assert!(
                arr.iter().any(|t| t["name"] == *want),
                "缺少工具定义：{want}"
            );
        }
    }

    #[test]
    fn summarize_computes_percent_and_hides_secrets() {
        let task = json!({
            "id": "t1", "file_name": "a.bin", "state": "downloading", "kind": "http",
            "downloaded": 512, "size": 2048, "speed": 100, "connections": 8,
            "save_path": "/dl/a.bin", "url": "https://x/a.bin",
            "cookie": "SECRET=1", "password": "pw", "segments": [{"done": 1}]
        });
        let s = summarize(&task);
        assert_eq!(s["percent"], 25.0);
        assert_eq!(s["size_bytes"], 2048);
        assert!(s.get("cookie").is_none() && s.get("password").is_none());
        assert!(s.get("segments").is_none(), "分片明细不该回传");
    }

    #[test]
    fn summarize_unknown_size_is_null_not_zero() {
        let s = summarize(&json!({ "id": "t", "downloaded": 10, "state": "downloading" }));
        assert!(s["size_bytes"].is_null());
        assert_eq!(s["percent"], 0.0);
    }

    #[test]
    fn add_body_only_keeps_known_fields() {
        let body = add_body(&json!({
            "url": "https://x/a.bin", "connections": 4, "unknown": "drop", "save_dir": null
        }));
        assert_eq!(body["connections"], 4);
        assert!(body.get("unknown").is_none());
        assert!(body.get("save_dir").is_none(), "null 不该透传");
    }

    #[tokio::test]
    async fn unknown_tool_is_an_error() {
        // 只验证分发层拒绝对未知工具动网络
        let cfg = crate::api::Config::new(1, "t".into(), "test");
        let api = Api::new(cfg).unwrap();
        let e = call(&api, "felldl_nope", &json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("未知工具"), "{e}");
    }
}
