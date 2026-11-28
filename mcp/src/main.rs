//! Felldl MCP 服务（stdio）
//!
//! 传输：一行一个 JSON-RPC 2.0 消息（MCP stdio 约定）。
//! 注意：stdout 只允许出现协议消息，任何日志都必须走 stderr，否则客户端会解析失败。
//!
//! 协议与工具实现都在 `felldl_core::mcp`：本文件只做 stdio 收发，并把工具调用
//! 转给本地 HTTP API。主程序自带的 `POST /mcp` 才是首选接入方式，这个二进制是
//! 给只能用 stdio 的客户端兜底。

mod api;
mod tools;

use anyhow::Result;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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
         {{\"mcpServers\":{{\"felldl\":{{\"command\":\"felldl-mcp\",\"args\":[]}}}}}}\n\n\
         也可以不跑这个进程：Felldl 主程序自带 Streamable HTTP 端点，\n\
         直接配 http://127.0.0.1:8899/mcp?token=<令牌> 即可。",
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
            Ok(msg) => felldl_core::mcp::handle(&api, &msg).await,
            Err(e) => Some(felldl_core::mcp::rpc_error(
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
