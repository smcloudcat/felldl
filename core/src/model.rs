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

/// 断点可信锚：记录「上次保存进度时」落盘文件的样子。
///
/// 分片进度（`Segment::done`）本身不包含任何文件证据：文件被删掉、被截短、
/// 或被同名文件替换后，进度仍然声称「已下完 100MB」，续传会直接跳过这些分片，
/// 结果是一个稀疏文件冒充已完成的下载。恢复前用这个锚点核对文件身份，
/// 对不上就把进度整体作废重下。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeAnchor {
    /// 上次记录时的文件长度（字节）
    pub len: u64,
    /// 文件创建时间（毫秒时间戳）。平台不提供时写 0，此时只能靠长度判断
    #[serde(default)]
    pub created_ms: i64,
}

impl ResumeAnchor {
    /// 与当前文件元数据是否仍匹配。
    ///
    /// - 长度变短 → 文件被截断，进度无效
    /// - 创建时间与锚点不同 → 文件被删除后重建（长度可能恰好相同），进度同样无效
    /// 长度变长是允许的（分片预分配会把文件拉到总长度）。
    pub fn still_matches(&self, current: &ResumeAnchor) -> bool {
        if current.len < self.len {
            return false;
        }
        if self.created_ms != 0 && current.created_ms != 0 && self.created_ms != current.created_ms {
            return false;
        }
        true
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
    /// 下载分类规则（按顺序匹配，第一个命中的生效）
    #[serde(default = "default_categories")]
    pub categories: Vec<Category>,
    /// 全部下载完成后执行的动作："none" | "shutdown" | "sleep"
    #[serde(default)]
    pub after_complete: String,
    /// 绑定的本地网络接口 IP（空 = 自动选择）；支持 IPv4 / IPv6
    #[serde(default)]
    pub bind_ip: String,
    /// 浏览器扩展下载面板显示方式："notify" | "minimize" | "none"
    #[serde(default = "default_browser_panel")]
    pub browser_panel: String,
    /// 界面皮肤（强调色主题）："blue" | "purple" | "green" | "orange"
    #[serde(default = "default_skin")]
    pub ui_skin: String,
    /// 按钮风格："rounded" | "flat" | "outline"
    #[serde(default = "default_btn_style")]
    pub btn_style: String,
    /// 任务卡片显示的信息项（顺序即显示顺序）
    #[serde(default = "default_task_meta")]
    pub task_meta: Vec<String>,
    /// DNS 模式："system"（跟随系统） | "custom"（使用下方自定义服务器）
    #[serde(default = "default_dns_mode")]
    pub dns_mode: String,
    /// 自定义 DNS 服务器，逗号/空格分隔，如 "1.1.1.1, 8.8.8.8"
    #[serde(default)]
    pub dns_servers: String,
    /// 自定义背景图片的本地路径（空 = 不使用）
    #[serde(default)]
    pub ui_background: String,
    /// 背景模糊半径（px，0~40）
    #[serde(default = "default_ui_blur")]
    pub ui_blur: u32,
    /// 背景遮罩浓度（0~90，百分比）
    #[serde(default = "default_ui_dim")]
    pub ui_dim: u32,
    /// 面板不透明度（40~100，百分比，越小越通透）
    #[serde(default = "default_ui_glass")]
    pub ui_glass: u32,
    /// 是否为 GitHub 等站点启用镜像加速（默认关闭）
    #[serde(default)]
    pub mirror_enabled: bool,
    /// 镜像线路："auto" | "ghfast" | "ghproxy" | "hf" | "custom"
    #[serde(default = "default_mirror_mode")]
    pub mirror_mode: String,
    /// 自定义镜像前缀（mirror_mode = "custom" 时生效），如 "https://ghfast.top/"
    #[serde(default)]
    pub mirror_custom: String,
}

fn default_mirror_mode() -> String {
    "auto".into()
}

/// 下载分类规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub name: String,
    /// 扩展名列表（不含点，小写）
    #[serde(default)]
    pub extensions: Vec<String>,
    /// 站点匹配（子串匹配 URL host，可选）
    #[serde(default)]
    pub site_pattern: Option<String>,
    /// 该分类的独立保存目录（空 = 跟随默认目录）
    #[serde(default)]
    pub save_dir: Option<String>,
}

fn default_categories() -> Vec<Category> {
    vec![
        Category { name: "视频".into(), extensions: vec!["mp4","mkv","avi","mov","webm","flv","ts","m3u8","wmv"].into_iter().map(String::from).collect(), site_pattern: None, save_dir: None },
        Category { name: "音乐".into(), extensions: vec!["mp3","flac","wav","aac","ogg","m4a","ape"].into_iter().map(String::from).collect(), site_pattern: None, save_dir: None },
        Category { name: "文档".into(), extensions: vec!["pdf","doc","docx","xls","xlsx","ppt","pptx","txt","md","epub","mobi"].into_iter().map(String::from).collect(), site_pattern: None, save_dir: None },
        Category { name: "压缩包".into(), extensions: vec!["zip","rar","7z","tar","gz","bz2","xz","iso"].into_iter().map(String::from).collect(), site_pattern: None, save_dir: None },
        Category { name: "程序".into(), extensions: vec!["exe","msi","apk","dmg","appimage","deb","rpm"].into_iter().map(String::from).collect(), site_pattern: None, save_dir: None },
        Category { name: "图片".into(), extensions: vec!["jpg","jpeg","png","gif","webp","bmp","svg"].into_iter().map(String::from).collect(), site_pattern: None, save_dir: None },
    ]
}
fn default_browser_panel() -> String {
    "notify".into()
}
fn default_skin() -> String {
    "blue".into()
}
fn default_btn_style() -> String {
    "rounded".into()
}
fn default_task_meta() -> Vec<String> {
    vec!["size".into(), "speed".into(), "eta".into(), "mirrors".into(), "date".into()]
}
fn default_dns_mode() -> String {
    "system".into()
}
fn default_ui_blur() -> u32 {
    18
}
fn default_ui_dim() -> u32 {
    32
}
fn default_ui_glass() -> u32 {
    78
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
            categories: default_categories(),
            after_complete: "none".into(),
            bind_ip: String::new(),
            browser_panel: default_browser_panel(),
            ui_skin: default_skin(),
            btn_style: default_btn_style(),
            task_meta: default_task_meta(),
            dns_mode: default_dns_mode(),
            dns_servers: String::new(),
            ui_background: String::new(),
            ui_blur: default_ui_blur(),
            ui_dim: default_ui_dim(),
            ui_glass: default_ui_glass(),
            mirror_enabled: false,
            mirror_mode: default_mirror_mode(),
            mirror_custom: String::new(),
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

/// 任务类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    /// 普通 HTTP(S) 多线程下载
    #[default]
    Http,
    /// BitTorrent（磁力 / 种子文件）
    Bt,
    /// HLS 直播/点播（m3u8 分片合并）
    Hls,
    /// FTP / FTPS（单流 + REST 续传）
    Ftp,
    /// SFTP（单流 + seek 续传）
    Sftp,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Http => "http",
            TaskKind::Bt => "bt",
            TaskKind::Hls => "hls",
            TaskKind::Ftp => "ftp",
            TaskKind::Sftp => "sftp",
        }
    }
    /// 从字符串（浏览器传来的小写）解析，默认 Http
    pub fn parse(s: &str) -> Self {
        match s {
            "bt" => TaskKind::Bt,
            "hls" => TaskKind::Hls,
            "ftp" => TaskKind::Ftp,
            "sftp" => TaskKind::Sftp,
            _ => TaskKind::Http,
        }
    }
}

/// 下载任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub url: String,
    /// 任务类型：http 多线程 / bt
    #[serde(default)]
    pub kind: TaskKind,
    /// 镜像地址列表（多源并行下载；与 url 同一文件的不同镜像站）
    #[serde(default)]
    pub mirrors: Vec<String>,
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
    /// 认证归属：Cookie / 令牌 / Basic 只允许发往与这个 origin（scheme+host+port）
    /// 一致的地址。`url` 可能是探测后的重定向终址，不能用来判定凭据归属；
    /// 为空时回退到 `url` 的 origin（兼容旧任务 JSON）。
    #[serde(default)]
    pub auth_origin: String,
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
    /// BT：做种数（实时）
    #[serde(default)]
    pub peers: u32,
    /// BT：上传速度（字节/秒）
    #[serde(default)]
    pub upload_speed: u64,
    /// BT：info_hash（hex），用于关联 librqbit 会话中的种子
    #[serde(default)]
    pub bt_info_hash: Option<String>,
    /// 下载分类（分类名）
    #[serde(default)]
    pub category: Option<String>,
    /// HTTP Basic / FTP / SFTP 认证用户名
    #[serde(default)]
    pub username: Option<String>,
    /// 认证密码（本地存储）
    #[serde(default)]
    pub password: Option<String>,
    /// 周期性同步：完成后每 N 秒自动重新下载（0 = 关闭）
    #[serde(default)]
    pub interval_secs: u64,
    /// 断点可信锚：文件被删/截短/替换后，旧分片进度不得再被采信
    #[serde(default)]
    pub resume_anchor: Option<ResumeAnchor>,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// 镜像地址（多源并行下载）
    #[serde(default)]
    pub mirrors: Vec<String>,
    /// 认证用户名（HTTP Basic / FTP / SFTP）
    #[serde(default)]
    pub username: Option<String>,
    /// 认证密码
    #[serde(default)]
    pub password: Option<String>,
    /// 周期性同步间隔（秒，0 = 一次性下载）
    #[serde(default)]
    pub interval_secs: u64,
    /// 指定分类名（不指定则自动归类）
    #[serde(default)]
    pub category: Option<String>,
}

/// 从 URL 中剥离 userinfo（user:pass@host）→ 返回 (剥离后的 URL, user, pass)
pub fn split_url_userinfo(url: &str) -> (String, Option<String>, Option<String>) {
    if let Some(scheme_end) = url.find("://") {
        let after = &url[scheme_end + 3..];
        if let Some(at) = after.find('@') {
            // 只把最后一个 @ 前的部分当凭据（密码里可能带 @，取 host 段前的第一个 @ 不可靠，
            // 但 host 段不含 @，因此取第一个出现在路径之前的 @）
            let head = &after[..at];
            if head.contains('/') || head.contains('?') || head.contains('#') {
                return (url.to_string(), None, None);
            }
            let (u, p) = match head.split_once(':') {
                Some((u, p)) => (u.to_string(), p.to_string()),
                None => (head.to_string(), String::new()),
            };
            let cleaned = format!("{}://{}", &url[..scheme_end + 3], &after[at + 1..]);
            return (
                cleaned,
                if u.is_empty() { None } else { Some(u) },
                if p.is_empty() { None } else { Some(p) },
            );
        }
    }
    (url.to_string(), None, None)
}

/// 引擎事件（推送给 UI / 订阅者）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "camelCase")]
pub enum EngineEvent {
    TaskUpdated(Task),
    /// 轻量进度增量：只含会变化的字段，避免每个进度 tick 都重复传输
    /// URL / 镜像 / Cookie / 认证 / 文件名等静态信息（可占数 KB）。
    TaskProgress(Box<TaskProgress>),
    TaskRemoved(TaskId),
    SettingsUpdated(Settings),
    Notice(String),
    /// 系统动作请求（如 "shutdown" / "sleep"），由宿主层执行
    SystemAction(String),
}

/// 下载进度增量。只携带会随 tick 变化的字段，
/// UI 收到后按 id 合并进已有 Task，避免重复传输静态信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskProgress {
    pub id: TaskId,
    pub state: TaskState,
    pub downloaded: u64,
    pub size: Option<u64>,
    pub speed: u64,
    pub peers: u32,
    pub upload_speed: u64,
    pub segments: Vec<Segment>,
    pub error: Option<String>,
    pub completed_at: Option<i64>,
}

impl Task {
    /// 生成进度增量快照（不含静态字段）
    pub fn progress_snapshot(&self) -> TaskProgress {
        TaskProgress {
            id: self.id.clone(),
            state: self.state,
            downloaded: self.downloaded,
            size: self.size,
            speed: self.speed,
            peers: self.peers,
            upload_speed: self.upload_speed,
            segments: self.segments.clone(),
            error: self.error.clone(),
            completed_at: self.completed_at,
        }
    }
}

/// 当前时间（毫秒）
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 按分类规则自动归类：先匹配扩展名，再匹配站点；都不命中返回 None
pub fn classify_category(
    file_name: &str,
    url: &str,
    categories: &[Category],
) -> Option<String> {
    let ext = file_name
        .rsplit('.')
        .next()
        .filter(|e| e.len() < 12 && !e.contains('/'))
        .map(|e| e.to_lowercase())?;
    let url_lower = url.to_lowercase();
    for c in categories {
        if c.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)) {
            return Some(c.name.clone());
        }
    }
    for c in categories {
        if let Some(p) = &c.site_pattern {
            if !p.trim().is_empty() && url_lower.contains(&p.to_lowercase()) {
                return Some(c.name.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod resume_anchor_tests {
    use super::ResumeAnchor;

    fn anchor(len: u64, created_ms: i64) -> ResumeAnchor {
        ResumeAnchor { len, created_ms }
    }

    /// 文件没动过 → 断点可信
    #[test]
    fn unchanged_file_still_matches() {
        assert!(anchor(100, 111).still_matches(&anchor(100, 111)));
        // 分片预分配可能把文件拉长
        assert!(anchor(100, 111).still_matches(&anchor(200, 111)));
    }

    /// 被截短 → 已写入的字节没了，断点必须作废
    #[test]
    fn truncated_file_is_rejected() {
        assert!(!anchor(100, 111).still_matches(&anchor(99, 111)));
        assert!(!anchor(100, 111).still_matches(&anchor(0, 111)));
    }

    /// 被删后重建：长度可能恰好相同，靠创建时间识别
    #[test]
    fn recreated_file_is_rejected() {
        assert!(!anchor(100, 111).still_matches(&anchor(100, 222)));
        // 平台不提供创建时间（0）时退化为只看长度
        assert!(anchor(100, 0).still_matches(&anchor(100, 222)));
    }
}
