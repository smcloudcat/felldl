//! 内嵌 MCP（Model Context Protocol）服务
//!
//! 为什么放在 core：`felldl-mcp` 独立进程只能通过 HTTP API 间接操作引擎，用户得先
//! 把 exe 配进客户端、还得保证它和主程序同时活着。内嵌之后，MCP 端点与引擎同进程，
//! 桌面和安卓都能直接用，且工具实现只有这一份。
//!
//! 传输层在 `api`（Streamable HTTP，MCP 2025-03-26 起支持）：本模块只负责
//! 「一条 JSON-RPC 消息 → 一条响应」，不碰 TCP/SSE，因此 stdio 与 HTTP 共用同一套语义。
//!
//! 安全边界：工具结果只回摘要字段，Cookie / 密码 / 分片明细不出现在任何响应里——
//! 模型能看到的只有 `Task::redacted()` 经过 `summarize` 之后的那部分。

use crate::engine::Engine;
use crate::model::{AddTaskRequest, Task, TaskState};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub const SERVER_NAME: &str = "felldl-mcp";
pub const SERVER_TITLE: &str = "Felldl 下载器";
/// 协商失败时的回退版本（MCP 当前稳定版）
pub const DEFAULT_PROTOCOL: &str = "2025-06-18";
pub const SUPPORTED_PROTOCOLS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

pub const TOOL_NAMES: &[&str] = &[
    "felldl_add_download",
    "felldl_list_tasks",
    "felldl_get_task",
    "felldl_pause_task",
    "felldl_resume_task",
    "felldl_cancel_task",
    "felldl_stats",
];

/// tools/list 的返回值。
///
/// `annotations` 不只是装饰：`destructiveHint` 让客户端在调用 `felldl_cancel_task`
/// 前弹确认，`readOnlyHint` 则允许客户端在只读场景下跳过授权。
pub fn list_tools() -> Value {
    json!([
        {
            "name": "felldl_add_download",
            "title": "添加下载任务",
            "description": "把一个 URL 交给 Felldl 下载。返回任务 id；随后可用 felldl_get_task 轮询进度。支持指定保存目录、文件名、连接数、请求头、Cookie、镜像地址。",
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

pub fn need_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("缺少参数 {key}"))
}

/// 状态名与 `TaskState` 的 serde 线格式保持一致；测试里逐变体做过对照。
fn state_name(s: TaskState) -> &'static str {
    match s {
        TaskState::Queued => "queued",
        TaskState::Probing => "probing",
        TaskState::Downloading => "downloading",
        TaskState::Paused => "paused",
        TaskState::Completed => "completed",
        TaskState::Error => "error",
        TaskState::Scheduled => "scheduled",
    }
}

/// 把任务压成模型关心的字段。
///
/// 入参是 JSON 而不是 `Task`：`felldl-mcp` 独立进程拿到的是 HTTP 响应，
/// 内嵌版本则先 `serde_json::to_value(redacted())` 再走这里，两条路径共用同一份裁剪规则。
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

/// 直接从引擎任务生成摘要：先脱敏，再裁剪
pub fn summarize_task(t: &Task) -> Value {
    summarize(&serde_json::to_value(t.redacted()).unwrap_or(Value::Null))
}

fn pretty(v: Value) -> String {
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
}

/// `/add` 的请求体：只透传引擎认识的字段
pub fn add_body(args: &Value) -> Value {
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

/// 工具执行后端。
///
/// 内嵌服务直接持有 `Arc<Engine>`，`felldl-mcp` 独立进程则拿着一个 HTTP 客户端——
/// 协议分发（initialize / tools/list / tools/call）只写一份，两者只在这里分叉。
///
/// 手写 `Pin<Box<dyn Future + Send>>` 而不是 `async fn`：axum 的 handler 要求 future 是
/// `Send`，而 trait 里的 `async fn`（RPITIT）无法表达这个约束。
pub trait ToolBackend: Send + Sync {
    fn call<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
}

impl ToolBackend for Arc<Engine> {
    fn call<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(call_engine(self, name, args))
    }
}

/// 执行工具，返回给模型的文本。
///
/// 失败一律转成文本交给模型（`isError=true`），而不是 JSON-RPC error：
/// 模型需要看到「文件不存在」这类原文才能决定换地址还是放弃。
pub async fn call_engine(engine: &Arc<Engine>, name: &str, args: &Value) -> Result<String> {
    match name {
        "felldl_stats" => {
            let tasks = engine.snapshot();
            let active = tasks.iter().filter(|t| t.state.is_active()).count();
            let total_speed: u64 = tasks.iter().map(|t| t.speed).sum();
            Ok(pretty(json!({
                "ok": true,
                "version": crate::VERSION,
                "active": active,
                "tasks": tasks.len(),
                "speed": total_speed,
            })))
        }
        "felldl_list_tasks" => {
            let state = args.get("state").and_then(|v| v.as_str());
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .min(200) as usize;
            let tasks = engine.snapshot();
            let filtered: Vec<Value> = tasks
                .iter()
                .filter(|t| state.is_none_or(|s| state_name(t.state) == s))
                .map(summarize_task)
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
            let t = engine
                .get(&id)
                .ok_or_else(|| anyhow::anyhow!("任务不存在：{id}"))?;
            Ok(pretty(summarize_task(&t)))
        }
        "felldl_add_download" => {
            let url = need_str(args, "url")?;
            if !url.contains("://") && !url.starts_with("magnet:") {
                bail!("url 不是合法地址：{url}");
            }
            let req: AddTaskRequest = serde_json::from_value(add_body(args))
                .map_err(|e| anyhow::anyhow!("参数不合法：{e}"))?;
            let id = engine.add(req).await?;
            Ok(pretty(json!({
                "ok": true, "id": id, "url": url, "hint": "用 felldl_get_task 轮询进度"
            })))
        }
        "felldl_pause_task" => {
            let id = need_str(args, "id")?;
            engine.pause(&id).await?;
            Ok(pretty(json!({ "ok": true, "id": id, "state": "paused" })))
        }
        "felldl_resume_task" => {
            let id = need_str(args, "id")?;
            engine.resume(&id).await?;
            Ok(pretty(json!({ "ok": true, "id": id })))
        }
        "felldl_cancel_task" => {
            let id = need_str(args, "id")?;
            let delete_file = args
                .get("delete_file")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            engine.cancel(&id, delete_file).await?;
            Ok(pretty(
                json!({ "ok": true, "id": id, "delete_file": delete_file }),
            ))
        }
        other => bail!("未知工具：{other}（可用：{}）", TOOL_NAMES.join(", ")),
    }
}

pub fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// 处理单条消息；通知（没有 id）返回 `None`，按规范不回应。
///
/// 这里是 stdio 与 HTTP 两条传输的唯一交汇点：`felldl-mcp` 直接复用它，
/// `api::mcp_post` 也复用它，所以两边的协议行为不可能漂移。
pub async fn handle<B: ToolBackend + ?Sized>(backend: &B, msg: &Value) -> Option<Value> {
    if !msg.is_object() {
        return Some(rpc_error(Value::Null, -32600, "无效请求：只接受单个 JSON 对象"));
    }
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    if msg.get("id").is_none() {
        // notifications/initialized、notifications/cancelled：静默吞掉
        return None;
    }
    let id = msg.get("id").cloned().unwrap_or(Value::Null);

    Some(match method {
        "initialize" => {
            let requested = msg
                .pointer("/params/protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(DEFAULT_PROTOCOL);
            let version = if SUPPORTED_PROTOCOLS.contains(&requested) {
                requested
            } else {
                DEFAULT_PROTOCOL
            };
            rpc_ok(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": SERVER_NAME,
                        "version": crate::VERSION,
                        "title": SERVER_TITLE
                    },
                    "instructions": "Felldl 是多线程下载器。用 felldl_add_download 添加任务，\
                                    用 felldl_get_task / felldl_list_tasks 查看进度，\
                                    用 felldl_pause_task / felldl_resume_task / felldl_cancel_task 控制。"
                }),
            )
        }
        "ping" => rpc_ok(id, json!({})),
        "tools/list" => rpc_ok(id, json!({ "tools": list_tools() })),
        "tools/call" => {
            let name = msg
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if name.is_empty() {
                rpc_error(id, -32602, "tools/call 缺少 name")
            } else {
                match backend.call(&name, &args).await {
                    Ok(text) => rpc_ok(
                        id,
                        json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
                    ),
                    Err(e) => rpc_ok(
                        id,
                        json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true }),
                    ),
                }
            }
        }
        other => rpc_error(id, -32601, &format!("不支持的方法：{other}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只为验证协议层；工具本身的行为由 `call_engine` 的测试覆盖
    struct MockBackend;

    impl ToolBackend for MockBackend {
        fn call<'a>(
            &'a self,
            name: &'a str,
            _args: &'a Value,
        ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
            Box::pin(async move {
                if name == "felldl_stats" {
                    Ok("{}".into())
                } else {
                    bail!("未知工具：{name}")
                }
            })
        }
    }

    #[tokio::test]
    async fn initialize_negotiates_protocol() {
        let msg = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}});
        let r = handle(&MockBackend, &msg).await.unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], SERVER_NAME);
        assert!(r["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn unknown_protocol_falls_back() {
        let msg = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}});
        let r = handle(&MockBackend, &msg).await.unwrap();
        assert_eq!(r["result"]["protocolVersion"], DEFAULT_PROTOCOL);
    }

    #[tokio::test]
    async fn notifications_are_not_answered() {
        let msg = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert!(handle(&MockBackend, &msg).await.is_none());
    }

    #[tokio::test]
    async fn tools_list_matches_registry() {
        let r = handle(&MockBackend, &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .await
            .unwrap();
        assert_eq!(r["result"]["tools"].as_array().unwrap().len(), TOOL_NAMES.len());
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let r = handle(&MockBackend, &json!({"jsonrpc":"2.0","id":3,"method":"resources/list"}))
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn tool_error_is_reported_inside_result() {
        let msg = json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"felldl_nope","arguments":{}}});
        let r = handle(&MockBackend, &msg).await.unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("未知工具"));
    }

    #[tokio::test]
    async fn empty_name_is_invalid_params_not_tool_error() {
        let msg = json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"arguments":{}}});
        let r = handle(&MockBackend, &msg).await.unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn non_object_message_is_invalid_request() {
        let r = handle(&MockBackend, &json!([{"jsonrpc":"2.0","id":1,"method":"ping"}]))
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], -32600);
    }

    #[test]
    fn every_tool_has_schema_and_description() {
        let tools = list_tools();
        let arr = tools.as_array().unwrap();
        assert_eq!(arr.len(), TOOL_NAMES.len());
        for t in arr {
            let name = t["name"].as_str().unwrap();
            assert!(TOOL_NAMES.contains(&name), "多出来的工具：{name}");
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
            assert!(t["inputSchema"]["type"] == "object", "{name} 缺少 inputSchema");
        }
        for want in TOOL_NAMES {
            assert!(arr.iter().any(|t| t["name"] == *want), "缺少工具定义：{want}");
        }
    }

    /// 手写的 state_name 必须和 serde 的线格式逐字一致，否则按状态过滤会静默失效
    #[test]
    fn state_names_match_serde() {
        for s in [
            TaskState::Queued,
            TaskState::Probing,
            TaskState::Downloading,
            TaskState::Paused,
            TaskState::Completed,
            TaskState::Error,
            TaskState::Scheduled,
        ] {
            let wire = serde_json::to_value(s).unwrap();
            assert_eq!(wire.as_str(), Some(state_name(s)), "状态 {s:?} 名称不一致");
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
}

