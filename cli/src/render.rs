//! 输出格式：终端表格 / 单任务详情 / JSON。
//!
//! 进度与提示一律走 stderr，只有「结果」走 stdout——这样
//! `fdl download -o /tmp x.iso` 重定向 stdout 时不会被进度条污染。

use anyhow::{bail, Result};
use serde_json::Value;

pub struct Out {
    pub json: bool,
    pub quiet: bool,
}

pub fn state_of(t: &Value) -> String {
    t.get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string()
}

/// 按 ID 或「唯一前缀」找任务。
///
/// 任务 ID 是 32 位十六进制，让用户每次复制全串不现实；
/// 前缀必须唯一，否则报错并列出候选，绝不猜。
pub fn pick(tasks: &[Value], needle: &str) -> Result<String> {
    let needle = needle.trim();
    if needle.is_empty() {
        bail!("任务 ID 不能为空");
    }
    let mut exact = None;
    let mut prefix = Vec::new();
    for t in tasks {
        let Some(id) = t.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if id == needle {
            exact = Some(id.to_string());
            break;
        }
        if id.starts_with(needle) {
            prefix.push(id.to_string());
        }
    }
    if let Some(id) = exact {
        return Ok(id);
    }
    match prefix.len() {
        0 => bail!("找不到任务 {needle}（用 `fdl list` 看看有哪些）"),
        1 => Ok(prefix.remove(0)),
        _ => {
            let mut list: Vec<String> = prefix
                .iter()
                .map(|s| s[..s.len().min(8)].to_string())
                .collect();
            list.sort();
            bail!(
                "前缀 {needle} 不唯一，匹配到 {} 个：{}",
                prefix.len(),
                list.join(" ")
            )
        }
    }
}

/// 同 `pick`，但直接返回任务本身（详情展示用）
pub fn pick_task<'a>(tasks: &'a [Value], needle: &str) -> Result<&'a Value> {
    let id = pick(tasks, needle)?;
    tasks
        .iter()
        .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
        .ok_or_else(|| anyhow::anyhow!("找不到任务 {id}"))
}

impl Out {
    /// 操作成功的短提示（JSON 模式下也输出，方便脚本判断）
    pub fn done(&self, msg: &str) {
        if self.json {
            println!("{}", serde_json::json!({"ok": true, "message": msg}));
            return;
        }
        if !self.quiet {
            println!("{msg}");
        }
    }

    pub fn print_tasks(&self, tasks: &[Value]) {
        if self.json {
            println!("{}", serde_json::json!({ "ok": true, "tasks": tasks }));
            return;
        }
        if tasks.is_empty() {
            if !self.quiet {
                println!("（没有任务）");
            }
            return;
        }
        println!(
            "{} {} {} {} {}  {}",
            ljust("ID", 8),
            ljust("状态", 12),
            rjust("进度", 7),
            rjust("已下载", 11),
            rjust("速度", 13),
            "文件名"
        );
        for t in tasks {
            let id = short_id(t);
            let state = state_of(t);
            let pct = format!("{:.1}%", percent(t) * 100.0);
            let got = human(num(t, "downloaded"));
            let speed = match num(t, "speed") {
                0 => "-".to_string(),
                v => format!("{}/s", human(v)),
            };
            let name = t
                .get("file_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            println!(
                "{} {} {} {} {}  {}",
                ljust(&id, 8),
                ljust(&state, 12),
                rjust(&pct, 7),
                rjust(&got, 11),
                rjust(&speed, 13),
                name
            );
        }
    }

    pub fn print_task(&self, t: &Value) {
        if self.json {
            println!("{t}");
            return;
        }
        let get = |k: &str| -> String {
            t.get(k)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".into())
        };
        let size = match num(t, "size") {
            0 => "未知".to_string(),
            v => human(v),
        };
        println!("ID        {}", get("id"));
        println!("文件名    {}", get("file_name"));
        println!("状态      {}", state_of(t));
        println!(
            "进度      {:.1}%  {} / {}",
            percent(t) * 100.0,
            human(num(t, "downloaded")),
            size
        );
        println!(
            "速度      {}",
            match num(t, "speed") {
                0 => "-".to_string(),
                v => format!("{}/s", human(v)),
            }
        );
        println!("连接数    {}", num(t, "connections"));
        println!("保存到    {}", get("save_path"));
        println!("来源      {}", get("url"));
        if let Some(err) = t.get("error").and_then(|v| v.as_str()) {
            if !err.is_empty() {
                println!("错误      {err}");
                if let Some(code) = t.get("error_code").and_then(|v| v.as_str()) {
                    let retry = t
                        .get("retryable")
                        .and_then(|v| v.as_bool())
                        .map(|b| if b { "可重试" } else { "重试无意义" })
                        .unwrap_or("未知");
                    println!("错误码    {code}（{retry}）");
                }
            }
        }
    }

    pub fn print_stats(&self, s: &Value) {
        if self.json {
            println!("{s}");
            return;
        }
        println!("版本      {}", str_of(s, "version"));
        println!("任务总数  {}", num(s, "tasks"));
        println!("进行中    {}", num(s, "active"));
        println!("总速度    {}/s", human(num(s, "speed")));
    }
}

/// 终端显示宽度：中日韩全角字符占两列。
///
/// `{:<12}` 按 **char** 计数，中文表头会比英文少一半宽度，表格必然错位；
/// 这里按显示列数补齐。
fn disp_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            if matches!(c as u32,
                0x1100..=0x115F      // 谚文字母
                | 0x2E80..=0xA4CF    // CJK 部首、假名、汉字
                | 0xAC00..=0xD7A3    // 谚文音节
                | 0xF900..=0xFAFF    // 兼容汉字
                | 0xFE30..=0xFE6F    // 兼容形式
                | 0xFF00..=0xFF60    // 全角 ASCII
                | 0xFFE0..=0xFFE6
                | 0x20000..=0x3FFFD)
            // 扩展汉字
            {
                2
            } else {
                1
            }
        })
        .sum()
}

fn ljust(s: &str, n: usize) -> String {
    let mut o = s.to_string();
    o.push_str(&" ".repeat(n.saturating_sub(disp_width(s))));
    o
}

fn rjust(s: &str, n: usize) -> String {
    format!("{}{}", " ".repeat(n.saturating_sub(disp_width(s))), s)
}

fn str_of(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("-").to_string()
}

fn num(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn short_id(t: &Value) -> String {
    let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("");
    id.chars().take(8).collect()
}

fn percent(t: &Value) -> f64 {
    let size = num(t, "size");
    if size == 0 {
        return 0.0;
    }
    (num(t, "downloaded") as f64 / size as f64).clamp(0.0, 1.0)
}

fn human(n: u64) -> String {
    felldl_core::util::human_bytes(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(id: &str) -> Value {
        json!({"id": id, "state": "downloading", "size": 100, "downloaded": 50, "speed": 10,
               "file_name": "a.bin", "save_path": "/tmp/a.bin", "url": "http://x/a.bin",
               "connections": 4})
    }

    #[test]
    fn pick_accepts_full_id_and_unique_prefix() {
        let tasks = vec![task("abcdef0123456789"), task("ffff0000aaaa")];
        assert_eq!(
            pick(&tasks, "abcdef0123456789").unwrap(),
            "abcdef0123456789"
        );
        assert_eq!(pick(&tasks, "abc").unwrap(), "abcdef0123456789");
    }

    #[test]
    fn ambiguous_prefix_is_an_error() {
        let tasks = vec![task("aaa1"), task("aaa2")];
        let err = pick(&tasks, "aaa").unwrap_err().to_string();
        assert!(err.contains("不唯一"), "{err}");
    }

    #[test]
    fn missing_task_reports_not_found() {
        let tasks = vec![task("abc")];
        assert!(pick(&tasks, "zzz").is_err());
    }

    #[test]
    fn percent_is_clamped_and_safe_on_unknown_size() {
        let mut t = task("x");
        t["size"] = json!(0);
        assert_eq!(percent(&t), 0.0);
        t["size"] = json!(100);
        t["downloaded"] = json!(200);
        assert_eq!(percent(&t), 1.0);
    }

    #[test]
    fn state_falls_back_to_unknown() {
        assert_eq!(state_of(&json!({})), "unknown");
        assert_eq!(state_of(&task("x")), "downloading");
    }
}
