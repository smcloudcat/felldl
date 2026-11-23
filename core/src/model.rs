//! 数据模型：任务、分片、设置、代理、事件

use serde::{Deserialize, Serialize};

pub type TaskId = String;

/// 任务状态机
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    /// 已排队，等待调度（受最大并发数限制）
    Queued,
    /// 正在探测服务器信息
    Probing,
    /// 正在下载
    Downloading,
    /// 已暂停（可续传）
    Paused,
    /// 已完成
    Completed,
    /// 出错
    Error,
    /// 定时等待中
    Scheduled,
}

impl TaskState {
    pub fn is_active(self) -> bool {
        matches!(self, TaskState::Probing | TaskState::Downloading)
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskState::Completed | TaskState::Error)
    }
}

/// 单个分段。`end` 为**闭区间**末尾字节偏移。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub index: usize,
    pub start: u64,
    pub end: u64,
    /// 该段已写入的字节数（续传基准）
    #[serde(default)]
    pub done: u64,
}

impl Segment {
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start) + 1
    }
    pub fn is_done(&self) -> bool {
        self.done >= self.len()
    }
    /// 下一段真实待写的起点
    pub fn cursor(&self) -> u64 {
        self.start + self.done
    }
}

/// 代理配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    #[serde(default)]
    pub enabled: bool,
    /// "http" | "socks5"
    #[serde(default = "default_proxy_kind")]
    pub kind: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// 逗号分隔的不走代理的域名
    #[serde(default)]
    pub no_proxy: String,
}

fn default_proxy_kind() -> String {
    "http".into()
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            kind: default_proxy_kind(),
            host: String::new(),
            port: 7890,
            username: String::new(),
            password: String::new(),
            no_proxy: String::new(),
        }
    }
}

impl ProxyConfig {
    /// 生成 reqwest 可用的代理 URL
    pub fn to_url(&self) -> Option<String> {
        if !self.enabled || self.host.trim().is_empty() {
            return None;
        }
        let scheme = match self.kind.as_str() {
            "socks5" => "socks5h",
            _ => "http",
        };
        let auth = if self.username.is_empty() {
            String::new()
        } else {
            format!("{}:{}@", self.username, self.password)
        };
        Some(format!("{}://{}{}:{}", scheme, auth, self.host.trim(), self.port))
    }
}

/// 全局设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_download_dir")]
    pub download_dir: String,
    /// 同时下载的任务数上限
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_tasks: usize,
    /// 新任务的默认连接数
    #[serde(default = "default_connections")]
    pub default_connections: usize,
    /// 全局限速（字节/秒），0 表示不限
    #[serde(default)]
    pub global_speed_limit: u64,
    #[serde(default)]
    pub proxy: ProxyConfig,
    /// "light" | "dark" | "system"
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 浏览器集成：本地 HTTP API 是否启用
    #[serde(default = "default_true")]
    pub api_enabled: bool,
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    /// API 访问令牌（空则自动生成）
    #[serde(default)]
    pub api_token: String,
    /// 下载完成后弹出系统通知
    #[serde(default = "default_true")]
    pub notify_on_complete: bool,
    #[serde(default = "default_ua")]
    pub user_agent: String,
    /// 添加任务后是否立即开始
    #[serde(default = "default_true")]
    pub auto_start: bool,
}

fn default_download_dir() -> String {
    default_download_dir_impl()
}
fn default_max_concurrent() -> usize {
    3
}
fn default_connections() -> usize {
    16
}
fn default_theme() -> String {
    "system".into()
}
fn default_true() -> bool {
    true
}
fn default_api_port() -> u16 {
    8899
}
fn default_ua() -> String {
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36".into()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir_impl(),
            max_concurrent_tasks: default_max_concurrent(),
            default_connections: default_connections(),
            global_speed_limit: 0,
            proxy: ProxyConfig::default(),
            theme: default_theme(),
            api_enabled: true,
            api_port: default_api_port(),
            api_token: String::new(),
            notify_on_complete: true,
            user_agent: default_ua(),
            auto_start: true,
        }
    }
}

fn default_download_dir_impl() -> String {
    #[cfg(windows)]
    {
        if let Ok(p) = std::env::var("USERPROFILE") {
            return format!("{p}\\Downloads");
        }
    }
    #[cfg(target_os = "android")]
    {
        return "/storage/emulated/0/Download".into();
    }
    #[allow(unreachable_code)]
    {
        if let Ok(p) = std::env::var("HOME") {
            return format!("{p}/Downloads");
        }
        ".".into()
    }
}

/// 下载任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub url: String,
    /// 最终落盘文件名
    pub file_name: String,
    pub save_dir: String,
    /// 完整路径（save_dir + file_name 去重后结果）
    pub save_path: String,
    /// 总大小，未知为 None
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub downloaded: u64,
    pub state: TaskState,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub connections: usize,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub supports_range: bool,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub cookie: Option<String>,
    /// 探测阶段确认可用的 User-Agent（部分站点会拒绝浏览器 UA，续传需沿用）
    #[serde(default)]
    pub user_agent: Option<String>,
    /// 单任务限速（字节/秒），0 = 跟随全局
    #[serde(default)]
    pub speed_limit: u64,
    /// 定时开始时间戳（毫秒）
    #[serde(default)]
    pub scheduled_at: Option<i64>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub completed_at: Option<i64>,
    /// 实时速度（字节/秒），仅内存态
    #[serde(default)]
    pub speed: u64,
}

impl Task {
    pub fn progress(&self) -> f64 {
        match self.size {
            Some(s) if s > 0 => (self.downloaded as f64 / s as f64).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }
    /// 是否可续传（有分片信息）
    pub fn resumable(&self) -> bool {
        !self.segments.is_empty()
    }
}

/// 新任务参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddTaskRequest {
    pub url: String,
    #[serde(default)]
    pub save_dir: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub connections: Option<usize>,
    #[serde(default)]
    pub speed_limit: Option<u64>,
    #[serde(default)]
    pub scheduled_at: Option<i64>,
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub cookie: Option<String>,
    /// 浏览器集成透传的自定义请求头
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
}

/// 引擎事件（推送给 UI / 订阅者）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "camelCase")]
pub enum EngineEvent {
    TaskUpdated(Task),
    TaskRemoved(TaskId),
    SettingsUpdated(Settings),
    Notice(String),
}

/// 当前时间（毫秒）
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
