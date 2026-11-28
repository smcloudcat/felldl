//! `fdl` — Felldl 的命令行入口
//!
//! 两种工作方式，同一套命令：
//!
//! * **远程**（默认）：通过本地 HTTP API 操作**正在运行**的 Felldl，
//!   任务与界面完全同步。适合「浏览器里点了下载，终端里看进度/暂停」。
//! * **本地**（`--local` / `fdl download`）：在进程内直接跑引擎，
//!   不需要 GUI，适合服务器与 macOS/Linux 终端用户。
//!
//! 端口与令牌默认从应用的 `settings.json` 读，保证与应用里显示的一致；
//! 也可以用 `--port` / `--token` / `FELLDL_PORT` / `FELLDL_TOKEN` 覆盖。
//!
//! 本地模式用的是**独立的任务库**（`<数据目录>/cli`），不会去写应用正在用的
//! `tasks/` 目录——两个进程同时写同一份任务文件，结果是任务凭空消失。

mod local;
mod remote;
mod render;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;

/// 前台下载的默认连接数：比 GUI 的默认值保守一些，终端里通常同时只有一个任务
const DEFAULT_CONNECTIONS: usize = 8;

#[derive(Parser)]
#[command(
    name = "fdl",
    version,
    about = "Felldl 命令行：无界面下载与任务管理",
    long_about = "无界面下载器，也可作为正在运行的 Felldl 的命令行遥控器。\n\
                  不带 --local 时通过本地 API 操作已启动的应用；\n\
                  带 --local 或使用 download 子命令时在进程内直接运行引擎。",
    disable_help_subcommand = true
)]
struct Cli {
    /// 以 JSON 输出（便于脚本处理，进度条自动关闭）
    #[arg(long, global = true)]
    json: bool,

    /// 数据目录（默认与应用一致：Windows %APPDATA%，macOS Application Support，Linux XDG）
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// 覆盖本地 API 端口（默认读应用设置）
    #[arg(long, global = true, value_name = "PORT")]
    port: Option<u16>,

    /// 覆盖本地 API 令牌（默认读应用设置）
    #[arg(long, global = true, value_name = "TOKEN")]
    token: Option<String>,

    /// 不打印进度与提示信息（只输出结果）
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 前台下载一个地址（无界面；中断后重新执行同一命令会接着下）
    Download(DownloadArgs),
    /// 把下载任务交给正在运行的 Felldl
    Add(AddArgs),
    /// 列出任务
    List(ListArgs),
    /// 查看单个任务
    Get(GetArgs),
    /// 暂停任务
    Pause {
        /// 任务 ID 或其前缀
        id: String,
    },
    /// 继续（断点续传）
    Resume {
        /// 任务 ID 或其前缀
        id: String,
    },
    /// 取消任务（可选连文件一起删）
    Cancel {
        /// 任务 ID 或其前缀
        id: String,
        /// 连同已下载的文件一起删除
        #[arg(long)]
        delete_file: bool,
    },
    /// 运行状态：版本、任务数、活跃数、总速度
    Stats(StatsArgs),
}

#[derive(Args)]
struct StatsArgs {
    /// 读本地任务库，而不是操作正在运行的应用
    #[arg(long)]
    local: bool,
}

#[derive(Args)]
struct DownloadArgs {
    /// 下载地址（http/https/ftp/sftp/magnet）
    url: String,

    /// 保存目录（默认当前目录）
    #[arg(short = 'o', long, value_name = "DIR")]
    out: Option<PathBuf>,

    /// 另存为的文件名（默认从地址/响应头推断）
    #[arg(short = 'n', long, value_name = "NAME")]
    name: Option<String>,

    /// 连接数（分片并发）
    #[arg(short = 'c', long, value_name = "N", default_value_t = DEFAULT_CONNECTIONS)]
    connections: usize,

    /// 全局限速，单位 KB/s（0 = 不限）
    #[arg(long, value_name = "KBPS", default_value_t = 0)]
    limit: u64,

    /// Cookie（整段原样发送）
    #[arg(long, value_name = "COOKIE")]
    cookie: Option<String>,

    /// Referer
    #[arg(long, value_name = "URL")]
    referer: Option<String>,

    /// 额外请求头，可重复："Authorization: Bearer xxx"
    #[arg(short = 'H', long = "header", value_name = "K:V")]
    headers: Vec<String>,

    /// 镜像地址，可重复（与主地址同一文件的其它源）
    #[arg(long, value_name = "URL")]
    mirrors: Vec<String>,

    /// HTTP Basic 认证："用户名:密码"
    #[arg(short = 'u', long = "user", value_name = "USER:PASS")]
    user: Option<String>,

    /// 放弃可续传的旧任务，从头开始
    #[arg(long)]
    fresh: bool,
}

#[derive(Args)]
struct AddArgs {
    /// 下载地址
    url: String,

    /// 保存目录（默认用应用设置里的下载目录）
    #[arg(short = 'o', long, value_name = "DIR")]
    out: Option<PathBuf>,

    /// 另存为的文件名
    #[arg(short = 'n', long, value_name = "NAME")]
    name: Option<String>,

    /// 连接数
    #[arg(short = 'c', long, value_name = "N")]
    connections: Option<usize>,

    /// Cookie
    #[arg(long, value_name = "COOKIE")]
    cookie: Option<String>,

    /// Referer
    #[arg(long, value_name = "URL")]
    referer: Option<String>,

    /// 额外请求头，可重复
    #[arg(short = 'H', long = "header", value_name = "K:V")]
    headers: Vec<String>,

    /// 镜像地址，可重复
    #[arg(long, value_name = "URL")]
    mirrors: Vec<String>,

    /// HTTP Basic 认证："用户名:密码"
    #[arg(short = 'u', long = "user", value_name = "USER:PASS")]
    user: Option<String>,
}

#[derive(Args)]
struct ListArgs {
    /// 只看某个状态：queued/downloading/paused/completed/error/scheduled
    #[arg(long, value_name = "STATE")]
    state: Option<String>,

    /// 最多显示多少条（默认 20）
    #[arg(short = 'n', long, value_name = "N", default_value_t = 20)]
    limit: usize,

    /// 读本地任务库，而不是操作正在运行的应用
    #[arg(long)]
    local: bool,
}

#[derive(Args)]
struct GetArgs {
    /// 任务 ID 或其前缀
    id: String,

    /// 读本地任务库
    #[arg(long)]
    local: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match run(cli).await {
        Ok(()) => 0,
        Err(e) => {
            // 错误统一走 stderr：脚本可以只看 stdout 拿结果
            eprintln!("fdl: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<()> {
    let data_dir = cli
        .data_dir
        .clone()
        .unwrap_or_else(felldl_core::util::default_data_dir);

    let out = render::Out {
        json: cli.json,
        quiet: cli.quiet,
    };

    match cli.cmd {
        Cmd::Download(a) => {
            let spec = a.into_spec()?;
            match local::download(&data_dir, spec, &out).await? {
                local::Exit::Done => {}
                // 与 curl/wget 一致：被 Ctrl-C 打断的退出码是 130
                local::Exit::Interrupted => std::process::exit(130),
            }
        }
        Cmd::Add(a) => {
            let client = connect(&data_dir, cli.port, cli.token).await?;
            let body = a.into_body()?;
            let id = client.add(&body).await?;
            if out.json {
                println!("{}", serde_json::json!({"ok": true, "id": id}));
            } else if !out.quiet {
                println!("已加入队列: {id}");
            }
        }
        Cmd::List(a) => {
            let mut tasks = if a.local {
                local::tasks(&data_dir)?
            } else {
                connect(&data_dir, cli.port, cli.token)
                    .await?
                    .tasks()
                    .await?
            };
            if let Some(st) = &a.state {
                let want = st.to_ascii_lowercase();
                tasks.retain(|t| render::state_of(t) == want);
            }
            tasks.truncate(a.limit);
            out.print_tasks(&tasks);
        }
        Cmd::Get(a) => {
            let tasks = if a.local {
                local::tasks(&data_dir)?
            } else {
                connect(&data_dir, cli.port, cli.token)
                    .await?
                    .tasks()
                    .await?
            };
            let t = render::pick_task(&tasks, &a.id)?;
            out.print_task(t);
        }
        Cmd::Pause { id } => {
            let client = connect(&data_dir, cli.port, cli.token).await?;
            let id = render::pick(&client.tasks().await?, &id)?;
            client.action(&id, "pause").await?;
            out.done(&format!(
                "已暂停 {id}（当前状态: {}）",
                confirm(&client, &id).await?
            ));
        }
        Cmd::Resume { id } => {
            let client = connect(&data_dir, cli.port, cli.token).await?;
            let id = render::pick(&client.tasks().await?, &id)?;
            client.action(&id, "resume").await?;
            out.done(&format!(
                "已继续 {id}（当前状态: {}）",
                confirm(&client, &id).await?
            ));
        }
        Cmd::Cancel { id, delete_file } => {
            let client = connect(&data_dir, cli.port, cli.token).await?;
            let id = render::pick(&client.tasks().await?, &id)?;
            client.cancel(&id, delete_file).await?;
            out.done(&format!("已取消 {id}"));
        }
        Cmd::Stats(a) => {
            let s = if a.local {
                local::stats(&data_dir)?
            } else {
                connect(&data_dir, cli.port, cli.token)
                    .await?
                    .stats()
                    .await?
            };
            out.print_stats(&s);
        }
    }
    Ok(())
}

/// 连接正在运行的应用：优先用命令行参数，其次应用设置，最后环境变量。
///
/// ping 一次再返回，是为了给出人话错误——「Connection refused」对普通用户太抽象，
/// 而这条命令 90% 的失败原因就是「应用没开」。
/// 操作后回读真实状态。
///
/// 服务端对「已暂停的任务再暂停」返回 ok，直接打印「已暂停」会骗人：
/// 任务其实早下完了。多一次 GET 换一句实话。
async fn confirm(client: &remote::Remote, id: &str) -> Result<String> {
    match client.task(id).await {
        Ok(t) => Ok(render::state_of(&t)),
        // 取消后再查必然 404；这种「查不到」不是错误
        Err(_) => Ok("已移除".into()),
    }
}

async fn connect(
    data_dir: &std::path::Path,
    port: Option<u16>,
    token: Option<String>,
) -> Result<remote::Remote> {
    let (port, token) = match (port, token) {
        (Some(p), Some(t)) => (p, t),
        (p, t) => {
            let s = local::load_settings(data_dir);
            let port = p
                .or_else(|| {
                    std::env::var("FELLDL_PORT")
                        .ok()
                        .and_then(|v| v.parse().ok())
                })
                .or_else(|| s.as_ref().map(|s| s.api_port))
                .context(
                    "找不到本地 API 端口：应用还没有启动过？\
                     也可以用 --port 指定，或改用 --local 直接读任务库",
                )?;
            let token = t
                .or_else(|| std::env::var("FELLDL_TOKEN").ok())
                .or_else(|| {
                    s.as_ref()
                        .map(|s| s.api_token.clone())
                        .filter(|t| !t.is_empty())
                })
                .context("找不到本地 API 令牌：请先启动 Felldl，或用 --token 指定")?;
            (port, token)
        }
    };
    let r = remote::Remote::new(port, token);
    if !r.ping().await {
        bail!(
            "连不上 127.0.0.1:{port} —— Felldl 没有在运行？\n\
             提示：先启动应用；或在服务器上直接用 `fdl download <URL>`"
        );
    }
    Ok(r)
}

// ───────────────────────── 参数 → 请求 ─────────────────────────

impl DownloadArgs {
    fn into_spec(self) -> Result<local::DownloadSpec> {
        let (username, password) = split_user(self.user.as_deref())?;
        Ok(local::DownloadSpec {
            url: self.url,
            out: self.out,
            name: self.name,
            connections: self.connections,
            limit_kbps: self.limit,
            cookie: self.cookie,
            referer: self.referer,
            headers: parse_headers(&self.headers)?,
            mirrors: self.mirrors,
            username,
            password,
            fresh: self.fresh,
        })
    }
}

impl AddArgs {
    fn into_body(self) -> Result<serde_json::Value> {
        let (username, password) = split_user(self.user.as_deref())?;
        let mut m = serde_json::Map::new();
        m.insert("url".into(), self.url.into());
        if let Some(v) = self.out {
            m.insert("save_dir".into(), v.to_string_lossy().to_string().into());
        }
        if let Some(v) = self.name {
            m.insert("file_name".into(), v.into());
        }
        if let Some(v) = self.connections {
            m.insert("connections".into(), v.into());
        }
        if let Some(v) = self.cookie {
            m.insert("cookie".into(), v.into());
        }
        if let Some(v) = self.referer {
            m.insert("referer".into(), v.into());
        }
        if !self.headers.is_empty() {
            m.insert("headers".into(), serde_json::to_value(self.headers)?);
        }
        if !self.mirrors.is_empty() {
            m.insert("mirrors".into(), serde_json::to_value(self.mirrors)?);
        }
        if let Some(v) = username {
            m.insert("username".into(), v.into());
        }
        if let Some(v) = password {
            m.insert("password".into(), v.into());
        }
        Ok(serde_json::Value::Object(m))
    }
}

/// `Authorization: Bearer xxx` → ("Authorization", "Bearer xxx")
///
/// 只按第一个冒号切分：值里带冒号（Bearer、URL、时间戳）很常见，
/// 按全部冒号切会把值截断。
fn parse_headers(raw: &[String]) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for line in raw {
        let Some((k, v)) = line.split_once(':') else {
            bail!("请求头格式应为 \"名字: 值\"：{line}");
        };
        let k = k.trim();
        if k.is_empty() {
            bail!("请求头名字为空：{line}");
        }
        map.insert(k.to_string(), v.trim().to_string());
    }
    Ok(map)
}

/// `用户名:密码` → 两段；密码里允许出现冒号，所以只切第一个
fn split_user(raw: Option<&str>) -> Result<(Option<String>, Option<String>)> {
    match raw {
        None => Ok((None, None)),
        Some(s) => {
            let Some((u, p)) = s.split_once(':') else {
                bail!("--user 需要写成 \"用户名:密码\"");
            };
            if u.is_empty() {
                bail!("--user 的用户名为空");
            }
            Ok((Some(u.to_string()), Some(p.to_string())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_value_keeps_colons() {
        let h = parse_headers(&["Authorization: Bearer a:b:c".into()]).unwrap();
        assert_eq!(h["Authorization"], "Bearer a:b:c");
    }

    #[test]
    fn header_without_colon_is_rejected() {
        assert!(parse_headers(&["没有冒号".into()]).is_err());
        assert!(parse_headers(&[": 值为空的名字".into()]).is_err());
    }

    #[test]
    fn user_splits_on_first_colon() {
        let (u, p) = split_user(Some("bob:p@ss:word")).unwrap();
        assert_eq!(u.as_deref(), Some("bob"));
        assert_eq!(p.as_deref(), Some("p@ss:word"));
        assert!(split_user(Some("只有用户名")).is_err());
    }
}
