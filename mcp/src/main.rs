//! Felldl MCP 服务（stdio）
//!
//! 传输：一行一个 JSON-RPC 2.0 消息（MCP stdio 约定）。
//! 注意：stdout 只允许出现协议消息，任何日志都必须走 stderr，否则客户端会解析失败。

mod api;
mod tools;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const SERVER_NAME: &str = "felldl-mcp";
/// 协商失败时的回退版本（MCP 最新稳定版）
const DEFAULT_PROTOCOL: &str = "2025-06-18";
const SUPPORTED_PROTOCOLS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("[felldl-mcp] {e:#}");
        std::process::exit(1);
    }
}

fn print_help() {
    println!(
        "felldl-mcp {} — Felldl 的 MCP 服务（stdio）\n\n\
         用法：felldl-mcp [--port 8899] [--token <API_TOKEN>]\n\n\
         配置优先级：命令行 > 环境变量（FELLDL_PORT / FELLDL_TOKEN）> Felldl 设置文件。\n\
         在 MCP 客户端里通常这样配：\n\
         {{\"mcpServers\":{{\"felldl\":{{\"command\":\"felldl-mcp\",\"args\":[]}}}}}}",
        env!("CARGO_PKG_VERSION")
    );
}

async fn run() -> Result<()> {
    let mut port: Option<u16> = None;
    let mut token: Option<String> = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "-V" | "--version" => {
                println!("felldl-mcp {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--port" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--port 需要一个端口号"))?;
                port = Some(
                    v.parse()
                        .map_err(|_| anyhow::anyhow!("端口不是数字：{v}"))?,
                );
            }
            "--token" => {
                token = Some(
                    it.next()
                        .ok_or_else(|| anyhow::anyhow!("--token 需要一个值"))?,
                );
            }
            other => anyhow::bail!("未知参数：{other}（用 --help 查看用法）"),
        }
    }

    let cfg = api::resolve(port, token)?;
    let api = api::Api::new(cfg)?;
    // 启动时探一次：连不上不是致命错误（应用可能还没起来），只提示，调用时再试
    match api.ping().await {
        Ok(v) => eprintln!(
            "[felldl-mcp] 已连接 Felldl {}（{}）",
            v.get("version").and_then(|x| x.as_str()).unwrap_or("?"),
            api.config().base
        ),
        Err(e) => eprintln!(
            "[felldl-mcp] 暂时连不上 {}：{e:#}（工具调用时会重试；token 来源：{}）",
            api.config().base,
            api.config().source
        ),
    }
    serve(api).await
}

async fn serve(api: api::Api) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(msg) => handle(&msg, &api).await,
            Err(e) => Some(rpc_error(
                Value::Null,
                -32700,
                &format!("JSON 解析失败：{e}"),
            )),
        };
        if let Some(v) = reply {
            let mut buf = serde_json::to_vec(&v)?;
            buf.push(b'\n');
            out.write_all(&buf).await?;
            out.flush().await?;
        }
    }
    Ok(())
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// 处理单条消息；通知（没有 id）返回 None，不回应
async fn handle(msg: &Value, api: &api::Api) -> Option<Value> {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    if msg.get("id").is_none() {
        // notifications/initialized、notifications/cancelled 等：按规范静默
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
                        "version": env!("CARGO_PKG_VERSION"),
                        "title": "Felldl 下载器"
                    },
                    "instructions": "Felldl 是多线程下载器。用 felldl_add_download 添加任务，\
                                    用 felldl_get_task / felldl_list_tasks 查看进度，\
                                    用 felldl_pause_task / felldl_resume_task / felldl_cancel_task 控制。"
                }),
            )
        }
        "ping" => rpc_ok(id, json!({})),
        "tools/list" => rpc_ok(id, json!({ "tools": tools::list_tools() })),
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
                // 工具执行失败按 MCP 约定放在 isError 里，而不是 JSON-RPC error：
                // 模型需要看到错误原文才能自己决定下一步
                match tools::call(api, &name, &args).await {
                    Ok(text) => rpc_ok(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": text }],
                            "isError": false
                        }),
                    ),
                    Err(e) => rpc_ok(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": format!("{e:#}") }],
                            "isError": true
                        }),
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
    use crate::api::{Api, Config};

    fn test_api() -> Api {
        // 指向一个不存在的端口：这些用例都不应该走到网络
        Api::new(Config::new(1, "t".into(), "test")).unwrap()
    }

    #[tokio::test]
    async fn initialize_negotiates_protocol() {
        let api = test_api();
        let msg = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}});
        let r = handle(&msg, &api).await.unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], "felldl-mcp");
        assert!(r["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn unknown_protocol_falls_back() {
        let api = test_api();
        let msg = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}});
        let r = handle(&msg, &api).await.unwrap();
        assert_eq!(r["result"]["protocolVersion"], DEFAULT_PROTOCOL);
    }

    #[tokio::test]
    async fn notifications_are_not_answered() {
        let api = test_api();
        let msg = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert!(handle(&msg, &api).await.is_none());
    }

    #[tokio::test]
    async fn tools_list_matches_registry() {
        let api = test_api();
        let r = handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), &api)
            .await
            .unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), tools::TOOL_NAMES.len());
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let api = test_api();
        let r = handle(
            &json!({"jsonrpc":"2.0","id":3,"method":"resources/list"}),
            &api,
        )
        .await
        .unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn tool_error_is_reported_inside_result() {
        let api = test_api();
        let msg = json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"felldl_nope","arguments":{}}});
        let r = handle(&msg, &api).await.unwrap();
        assert_eq!(r["result"]["isError"], true);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("未知工具"), "{text}");
    }

    #[tokio::test]
    async fn missing_required_argument_is_reported() {
        let api = test_api();
        // felldl_get_task 缺 id：不该发请求，直接报参数错
        let msg = json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"felldl_get_task","arguments":{}}});
        let r = handle(&msg, &api).await.unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("缺少参数 id"));
    }
}
