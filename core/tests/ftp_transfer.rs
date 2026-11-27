//! FTP 传输正确性：二进制模式 + 传输完成应答 + 提前 EOF。
//!
//! 起一个最小的阻塞式 FTP 服务器（只实现本 crate 会用到的命令），
//! 用它验证三件此前被忽略的事：
//!   1. 下二进制文件必须显式 `TYPE I`，否则服务器按 ASCII 翻译换行，文件被改写；
//!   2. `RETR` 之后的完成应答（226/250）失败（426）不能被当成下载成功；
//!   3. 服务器提前关流时，长度与 SIZE 对不上必须报错，不能标成完成。

use felldl_core::{AddTaskRequest, Engine, TaskState};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 含 0x00 / 0x0D / 0x0A 的内容：ASCII 模式一定会改写它
fn make_binary_payload(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    for i in 0..len {
        v.push(match i % 5 {
            0 => 0x00,
            1 => b'\r',
            2 => b'\n',
            3 => (i % 251) as u8,
            _ => 0xff,
        });
    }
    v
}

#[derive(Clone, Copy, PartialEq)]
enum Ending {
    /// 正常收尾：数据发完 + 226
    Complete,
    /// 数据发完但回 426（服务器认为传输失败）
    Abort426,
    /// 只发一半就关流，随后仍回 226
    ShortBody,
}

struct MockFtp {
    addr: SocketAddr,
    /// 服务端收到的原始命令行
    commands: Arc<Mutex<Vec<String>>>,
}

fn spawn_ftp(body: Vec<u8>, ending: Ending) -> MockFtp {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let cmds = commands.clone();
    let body = Arc::new(body);

    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let body = body.clone();
            let cmds = cmds.clone();
            std::thread::spawn(move || {
                let mut writer = conn.try_clone().unwrap();
                let mut reader = BufReader::new(conn);
                let _ = writer.write_all(b"220 mock ftp ready\r\n");
                let mut rest: usize = 0;
                // PASV 建立的数据监听器
                let mut data_listener: Option<TcpListener> = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let cmd = line.trim_end().to_string();
                    cmds.lock().unwrap().push(cmd.clone());
                    let upper = cmd.to_ascii_uppercase();
                    let (verb, arg) = match upper.split_once(' ') {
                        Some((v, a)) => (v.to_string(), a.to_string()),
                        None => (upper.clone(), String::new()),
                    };
                    let reply: String = match verb.as_str() {
                        "USER" => "331 need password\r\n".into(),
                        "PASS" => "230 login ok\r\n".into(),
                        "TYPE" => "200 type set\r\n".into(),
                        "SIZE" => format!("213 {}\r\n", body.len()),
                        "REST" => {
                            rest = arg.trim().parse::<usize>().unwrap_or(0);
                            "350 restart ok\r\n".into()
                        }
                        "PASV" => {
                            let dl = TcpListener::bind("127.0.0.1:0").unwrap();
                            let p = dl.local_addr().unwrap().port();
                            data_listener = Some(dl);
                            format!(
                                "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                                p / 256,
                                p % 256
                            )
                        }
                        "RETR" => {
                            let _ = writer.write_all(b"150 opening data connection\r\n");
                            if let Some(dl) = data_listener.take() {
                                if let Ok((mut data, _)) = dl.accept() {
                                    let slice = &body[rest.min(body.len())..];
                                    match ending {
                                        Ending::ShortBody => {
                                            let half = slice.len() / 2;
                                            let _ = data.write_all(&slice[..half]);
                                        }
                                        _ => {
                                            let _ = data.write_all(slice);
                                        }
                                    }
                                    let _ = data.flush();
                                    drop(data);
                                }
                            }
                            match ending {
                                Ending::Abort426 => "426 transfer aborted\r\n".into(),
                                _ => "226 transfer complete\r\n".into(),
                            }
                        }
                        "QUIT" => {
                            let _ = writer.write_all(b"221 bye\r\n");
                            return;
                        }
                        _ => "200 ok\r\n".into(),
                    };
                    if writer.write_all(reply.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });

    MockFtp { addr, commands }
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "felldl-ftp-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn new_engine(root: &std::path::Path) -> Arc<Engine> {
    Engine::new(root).unwrap()
}

async fn wait_done(engine: &Arc<Engine>, id: &str, timeout: Duration) -> felldl_core::Task {
    let start = std::time::Instant::now();
    loop {
        let t = engine.get(id).unwrap();
        if t.state == TaskState::Completed || t.state == TaskState::Error {
            return t;
        }
        assert!(
            start.elapsed() < timeout,
            "任务在 {timeout:?} 内没有结束，当前状态 {:?}",
            t.state
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn ftp_url(addr: SocketAddr, name: &str) -> String {
    format!("ftp://user:pass@{}/{}", addr, name)
}

/// 正常下载：必须显式切换二进制模式，且内容逐字节一致
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ftp_download_uses_binary_mode() {
    let payload = make_binary_payload(256 * 1024);
    let srv = spawn_ftp(payload.clone(), Ending::Complete);
    let root = temp_root("bin");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let id = engine
        .add(AddTaskRequest {
            url: ftp_url(srv.addr, "blob.bin"),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let t = wait_done(&engine, &id, Duration::from_secs(60)).await;
    assert_eq!(t.state, TaskState::Completed, "错误: {:?}", t.error);

    let cmds = srv.commands.lock().unwrap().clone();
    let typed_binary = cmds
        .iter()
        .any(|c| c.eq_ignore_ascii_case("TYPE I") || c.eq_ignore_ascii_case("TYPE L 8"));
    assert!(
        typed_binary,
        "下载前必须发 TYPE I（否则服务器按 ASCII 翻译换行），实际命令序列: {cmds:?}"
    );

    let got = std::fs::read(&t.save_path).unwrap();
    assert!(
        got == payload,
        "FTP 下载内容与源不一致（{} / {} 字节）",
        got.len(),
        payload.len()
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// 服务器回 426：数据可能已经收全，但传输被判定失败，不能当成完成
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ftp_transfer_abort_is_error() {
    let payload = make_binary_payload(128 * 1024);
    let srv = spawn_ftp(payload, Ending::Abort426);
    let root = temp_root("abort");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let id = engine
        .add(AddTaskRequest {
            url: ftp_url(srv.addr, "abort.bin"),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let t = wait_done(&engine, &id, Duration::from_secs(60)).await;
    assert_eq!(
        t.state,
        TaskState::Error,
        "服务器回 426 却把任务标成完成（进度 {} / {:?}）",
        t.downloaded,
        t.size
    );
    let msg = t.error.clone().unwrap_or_default();
    assert!(
        msg.contains("传输") || msg.contains("426"),
        "错误信息应说明传输未正常结束，实际: {msg}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// 服务器提前关流（长度小于 SIZE）：必须报错，不能留下半个文件还标成完成
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ftp_short_body_is_error() {
    let payload = make_binary_payload(128 * 1024);
    let srv = spawn_ftp(payload.clone(), Ending::ShortBody);
    let root = temp_root("short");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let engine = new_engine(&root);

    let id = engine
        .add(AddTaskRequest {
            url: ftp_url(srv.addr, "short.bin"),
            save_dir: Some(out.to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let t = wait_done(&engine, &id, Duration::from_secs(60)).await;
    assert_eq!(
        t.state,
        TaskState::Error,
        "只收到 {} / {:?} 字节却标成完成",
        t.downloaded,
        t.size
    );
    let msg = t.error.clone().unwrap_or_default();
    assert!(
        msg.contains("提前结束") || msg.contains("不完整"),
        "错误信息应说明长度不符，实际: {msg}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
