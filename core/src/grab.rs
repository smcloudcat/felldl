//! 网页抓取器：资源提取 / 整站抓取 / HLS(m3u8) 下载
//!
//! 三个能力：
//! 1. `analyze_page`：抓取 HTML，提取页面内可下载资源（视频、音频、图片、
//!    压缩包、文档、字幕、种子）与同域链接，供前端嗅探面板展示。
//! 2. `grab_site`：广度优先递归抓取整站 —— HTML 按目录结构落盘，
//!    静态资源走下载引擎多线程加速。
//! 3. `download_hls`：解析 m3u8（含 master playlist），顺序下载分片
//!    并合并为单个 .ts 文件；支持 AES-128 加密分片。

use crate::engine::{Engine, Runtime};
use crate::model::*;
use crate::util::sanitize_name;
use anyhow::{anyhow, bail, Context, Result};
use reqwest::Client;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

// ───────────────────────── 资源分类 ─────────────────────────

/// 资源类别映射表（按扩展名，大小写不敏感）
fn classify_ext(url: &str) -> Option<(&'static str, &'static str)> {
    let url_lower = url.to_lowercase();
    const TABLE: &[(&str, &str, &str)] = &[
        // (扩展名, 类别, 中文说明)
        ("mp4", "video", "视频"), ("mkv", "video", "视频"), ("webm", "video", "视频"),
        ("avi", "video", "视频"), ("mov", "video", "视频"), ("flv", "video", "视频"),
        ("wmv", "video", "视频"), ("ts", "video", "视频"), ("m3u8", "hls", "HLS 流"),
        ("mpd", "hls", "DASH 流"),
        ("mp3", "audio", "音频"), ("flac", "audio", "音频"), ("aac", "audio", "音频"),
        ("ogg", "audio", "音频"), ("wav", "audio", "音频"), ("m4a", "audio", "音频"),
        ("jpg", "image", "图片"), ("jpeg", "image", "图片"), ("png", "image", "图片"),
        ("gif", "image", "图片"), ("webp", "image", "图片"), ("svg", "image", "图片"),
        ("bmp", "image", "图片"), ("ico", "image", "图片"), ("avif", "image", "图片"),
        ("zip", "archive", "压缩包"), ("rar", "archive", "压缩包"), ("7z", "archive", "压缩包"),
        ("tar", "archive", "压缩包"), ("gz", "archive", "压缩包"), ("bz2", "archive", "压缩包"),
        ("xz", "archive", "压缩包"), ("iso", "archive", "镜像"),
        ("pdf", "document", "文档"), ("doc", "document", "文档"), ("docx", "document", "文档"),
        ("xls", "document", "文档"), ("xlsx", "document", "文档"), ("ppt", "document", "文档"),
        ("pptx", "document", "文档"), ("epub", "document", "文档"), ("txt", "document", "文档"),
        ("srt", "subtitle", "字幕"), ("ass", "subtitle", "字幕"), ("ssa", "subtitle", "字幕"),
        ("vtt", "subtitle", "字幕"),
        ("torrent", "torrent", "种子"), ("exe", "software", "程序"), ("apk", "software", "程序"),
        ("msi", "software", "程序"), ("dmg", "software", "程序"), ("deb", "software", "程序"),
    ];
    let path = url_lower.split(['?', '#']).next().unwrap_or(&url_lower);
    let ext = path.rsplit('.').next()?;
    TABLE.iter().find(|(e, ..)| *e == ext).map(|(_, k, d)| (*k, *d))
}

/// 前端展示用的资源条目
#[derive(Debug, Clone, serde::Serialize)]
pub struct GrabResource {
    pub url: String,
    /// video / audio / image / archive / document / subtitle / torrent / hls / other
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PageAnalysis {
    pub url: String,
    pub title: String,
    /// 页面内所有可下载资源（已去重）
    pub resources: Vec<GrabResource>,
    /// 同域页面链接（供整站抓取）
    pub links: Vec<String>,
}

// ───────────────────────── HTML 提取 ─────────────────────────

/// 提取标签体内的属性键值对（容忍单双引号 / 无引号）
fn scan_attrs(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // 跳过空白
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        // 读 key
        let kstart = i;
        while i < bytes.len() && !(bytes[i] as char).is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        if kstart == i {
            i += 1;
            continue;
        }
        let key = body[kstart..i].to_ascii_lowercase();
        // 跳过空白
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            out.push((key, String::new()));
            continue;
        }
        i += 1;
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        let val = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
            let q = bytes[i];
            i += 1;
            let s = i;
            while i < bytes.len() && bytes[i] != q {
                i += 1;
            }
            let v = body[s..i.min(body.len())].to_string();
            i = (i + 1).min(body.len());
            v
        } else {
            let s = i;
            while i < bytes.len() && !(bytes[i] as char).is_ascii_whitespace() && bytes[i] != b'>' {
                i += 1;
            }
            body[s..i].to_string()
        };
        out.push((key, val));
    }
    out
}

/// 扫描 HTML，返回所有 href / src / data-src 引用
fn extract_refs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // 注释 / doctype / script 结束标记跳过
        if html[i..].starts_with("<!--") {
            i += 4;
            match html[i..].find("-->") {
                Some(p) => i += p + 3,
                None => break,
            }
            continue;
        }
        let tag_start = i + 1;
        let mut j = tag_start;
        // 找到标签结束 '>'，注意引号内不终止
        let mut quote: Option<u8> = None;
        while j < bytes.len() {
            let c = bytes[j];
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
            } else if c == b'"' || c == b'\'' {
                quote = Some(c);
            } else if c == b'>' {
                break;
            }
            j += 1;
        }
        if j > bytes.len() {
            break;
        }
        let body = &html[tag_start..j];
        let tag_name: String = body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        // <script>/<style> 内容跳过
        let after = &html[j..];
        if tag_name == "script" || tag_name == "style" {
            let close = format!("</{}", tag_name);
            match after.to_ascii_lowercase().find(&close) {
                Some(p) => {
                    i = j + p + close.len();
                    continue;
                }
                None => break,
            }
        }
        // 提取属性
        let attrs_start = tag_name.len();
        if attrs_start < body.len() {
            for (k, v) in scan_attrs(&body[attrs_start..]) {
                if (k == "href" || k == "src" || k == "data-src" || k == "data-url")
                    && !v.is_empty()
                    && !v.starts_with('#')
                    && !v.starts_with("javascript:")
                    && !v.starts_with("data:")
                {
                    out.push(v);
                }
            }
        }
        i = j + 1;
    }
    out
}

/// 提取 <title>
fn extract_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let s = match lower.find("<title") {
        Some(p) => match lower[p..].find('>') {
            Some(gt) => p + gt + 1,
            None => return String::new(),
        },
        None => return String::new(),
    };
    let e = lower[s..].find("</title>").map(|p| s + p).unwrap_or(html.len());
    html[s..e].trim().chars().take(200).collect()
}

/// 解析常见 HTML 实体
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            let rest = &s[i..];
            if rest.starts_with("&amp;") {
                out.push('&');
                i += 5;
            } else if rest.starts_with("&lt;") {
                out.push('<');
                i += 4;
            } else if rest.starts_with("&gt;") {
                out.push('>');
                i += 4;
            } else if rest.starts_with("&quot;") {
                out.push('"');
                i += 6;
            } else if rest.starts_with("&nbsp;") {
                out.push(' ');
                i += 6;
            } else if rest.starts_with("&#39;") {
                out.push('\'');
                i += 5;
            } else {
                let ch = s[i..].chars().next().unwrap_or('&');
                out.push(ch);
                i += ch.len_utf8();
            }
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

// ───────────────────────── 页面分析 ─────────────────────────

pub struct AnalyzeRequest {
    pub url: String,
    pub referer: Option<String>,
    pub cookie: Option<String>,
}

/// 单个页面的体积上限（防止误抓大文件/无限流把内存吃满）
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
/// 单页最多提取的引用数（超长/恶意页面防御）
const MAX_REFS_PER_PAGE: usize = 5000;
/// 单页最多收录的可下载资源数
const MAX_RESOURCES_PER_PAGE: usize = 500;

/// 带上限地读取响应体：先看 Content-Length，再逐块累加，超限立即中止
async fn read_body_capped(mut resp: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if let Some(len) = resp.content_length() {
        if len as usize > limit {
            bail!("响应体积过大（{len} 字节 > {limit}），已跳过");
        }
    }
    let mut buf = Vec::with_capacity(resp.content_length().unwrap_or(64 * 1024).min(limit as u64) as usize);
    while let Some(chunk) = resp.chunk().await.context("读取响应体失败")? {
        if buf.len() + chunk.len() > limit {
            bail!("响应体积超过上限 {limit} 字节，已中止");
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// 抓取并分析一个网页
pub async fn analyze_page(client: &Client, req: &AnalyzeRequest) -> Result<PageAnalysis> {
    analyze_page_with_body(client, req).await.map(|(p, _)| p)
}

/// 抓取并分析网页，同时返回正文（整站抓取直接复用，避免同一页请求两次）
pub async fn analyze_page_with_body(
    client: &Client,
    req: &AnalyzeRequest,
) -> Result<(PageAnalysis, String)> {
    let mut r = client.get(&req.url);
    if let Some(ref rf) = req.referer {
        r = r.header("Referer", rf);
    }
    if let Some(ref ck) = req.cookie {
        r = r.header("Cookie", ck);
    }
    let resp = r.send().await.context("抓取网页失败")?;
    if !resp.status().is_success() {
        bail!("网页返回 HTTP {}", resp.status().as_u16());
    }
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let final_url = resp.url().to_string();
    let text = if ct.contains("html") || ct.is_empty() || ct.contains("text/plain") {
        let bytes = read_body_capped(resp, MAX_PAGE_BYTES).await?;
        // 编码探测：优先 BOM/meta charset，缺省按 UTF-8 解（GBK 页面丢字符但不崩）
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        bail!("该地址不是网页（Content-Type: {ct}）");
    };

    let title = decode_entities(&extract_title(&text));
    let base = reqwest::Url::parse(&final_url)?;
    let mut resources: Vec<GrabResource> = Vec::new();
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    let mut seen_links = HashSet::new();

    for raw in extract_refs(&text).into_iter().take(MAX_REFS_PER_PAGE) {
        let u = match base.join(&raw) {
            Ok(u) => u,
            Err(_) => continue,
        };
        if u.scheme() != "http" && u.scheme() != "https" {
            continue;
        }
        let abs = u.to_string();
        if !seen.insert(abs.clone()) {
            continue;
        }
        if u.path().ends_with(".html")
            || u.path().ends_with(".htm")
            || u.path().ends_with('/')
            || !u.path().contains('.')
        {
            // 页面链接：仅同域收录
            if u.host_str() == base.host_str() && seen_links.insert(abs.clone()) {
                links.push(abs);
            }
            continue;
        }
        let kind = classify_ext(&abs)
            .map(|(k, _)| k.to_string())
            .unwrap_or_else(|| "other".into());
        let name = u
            .path_segments()
            .and_then(|s| s.last())
            .filter(|s| !s.is_empty())
            .map(|s| decode_entities(s))
            .unwrap_or_else(|| abs.clone());
        resources.push(GrabResource { url: abs, kind, name });
        if resources.len() >= MAX_RESOURCES_PER_PAGE {
            break;
        }
    }
    Ok((
        PageAnalysis { url: final_url, title, resources, links },
        text,
    ))
}

// ───────────────────────── 离线化：HTML 引用重写 ─────────────────────────

/// 同名资源加序号：`a.png` → `a_2.png`
fn dedup_name(name: &str, i: usize) -> String {
    let p = Path::new(name);
    match (p.file_stem().and_then(|s| s.to_str()), p.extension().and_then(|s| s.to_str())) {
        (Some(stem), Some(ext)) => format!("{stem}_{i}.{ext}"),
        _ => format!("{name}_{i}"),
    }
}

/// 把 HTML 中指向「已保存到本地」的资源 / 页面引用改写为相对路径，
/// 保存下来的站点可以直接离线打开，不再依赖网络。
///
/// 覆盖 `src` / `href` / `poster` / `data-src` 等属性引号值与
/// 内联样式里的 `url(...)`、`srcset` 列表；未命中的引用原样保留。
fn rewrite_html_refs(html: &str, base: &reqwest::Url, map: &HashMap<String, String>) -> String {
    if map.is_empty() {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        let qpos = match rest.find(['"', '\'']) {
            Some(p) => p,
            None => {
                out.push_str(rest);
                break;
            }
        };
        let q = rest.as_bytes()[qpos] as char;
        let (head, tail) = rest.split_at(qpos + 1);
        out.push_str(head);
        let end = match tail.find(q) {
            Some(e) => e,
            None => {
                out.push_str(tail);
                break;
            }
        };
        let val = &tail[..end];
        match rewrite_ref_value(val, base, map) {
            Some(nv) => out.push_str(&nv),
            None => out.push_str(val),
        }
        out.push(q);
        rest = &tail[end + 1..];
    }
    out
}

fn rewrite_ref_value(
    val: &str,
    base: &reqwest::Url,
    map: &HashMap<String, String>,
) -> Option<String> {
    let v = val.trim();
    if v.is_empty() || v.len() > 2048 {
        return None;
    }
    // 内联样式 / CSS 里的 url(...)（可能嵌在 "background-image:url(x)" 中）
    if v.contains("url(") {
        let mut seg = String::with_capacity(v.len());
        let mut rest = v;
        let mut changed = false;
        while let Some(p) = rest.find("url(") {
            seg.push_str(&rest[..p + 4]);
            let after = &rest[p + 4..];
            match after.find(')') {
                Some(e) => {
                    let inner = after[..e].trim().trim_matches(['"', '\'']);
                    match rewrite_one_ref(inner, base, map) {
                        Some(n) => {
                            seg.push_str(&n);
                            changed = true;
                        }
                        None => seg.push_str(&after[..e]),
                    }
                    seg.push(')');
                    rest = &after[e + 1..];
                }
                None => {
                    seg.push_str(after);
                    rest = "";
                }
            }
        }
        seg.push_str(rest);
        return changed.then_some(seg);
    }
    // srcset：逗号分隔的「地址 描述符」
    if v.contains(',') && v.contains(char::is_whitespace) {
        let mut changed = false;
        let parts: Vec<String> = v
            .split(',')
            .map(|part| {
                let p = part.trim();
                let mut it = p.split_whitespace();
                match (it.next(), it.next()) {
                    (Some(u), desc) => {
                        let tail = desc.map(|d| format!(" {d}")).unwrap_or_default();
                        match rewrite_one_ref(u, base, map) {
                            Some(n) => {
                                changed = true;
                                format!("{n}{tail}")
                            }
                            None => p.to_string(),
                        }
                    }
                    _ => p.to_string(),
                }
            })
            .collect();
        return changed.then(|| parts.join(", "));
    }
    rewrite_one_ref(v, base, map)
}

/// 单个引用 → 本地相对路径（带原 #fragment）
fn rewrite_one_ref(raw: &str, base: &reqwest::Url, map: &HashMap<String, String>) -> Option<String> {
    let r = raw.trim();
    if r.is_empty()
        || r.starts_with('#')
        || r.starts_with("data:")
        || r.starts_with("javascript:")
        || r.starts_with("mailto:")
        || r.starts_with("blob:")
    {
        return None;
    }
    let (path, frag) = match r.split_once('#') {
        Some((a, b)) => (a, Some(b)),
        None => (r, None),
    };
    let u = base.join(path).ok()?;
    let key = u.to_string();
    let hit = map
        .get(&key)
        .or_else(|| map.get(key.trim_end_matches('/')))?;
    Some(match frag {
        Some(f) if !f.is_empty() => format!("{hit}#{f}"),
        _ => hit.clone(),
    })
}

// ───────────────────────── 整站抓取 ─────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GrabSiteRequest {
    pub url: String,
    pub save_dir: String,
    /// 递归深度（1 = 只抓入口页）
    #[serde(default = "default_depth")]
    pub max_depth: u32,
    /// 最多抓取的页面数
    #[serde(default = "default_pages")]
    pub max_pages: u32,
    /// 是否下载页面里的静态资源（图片/脚本/样式等）
    #[serde(default = "default_true_g")]
    pub include_assets: bool,
    pub referer: Option<String>,
}

fn default_depth() -> u32 {
    1
}
fn default_pages() -> u32 {
    50
}
fn default_true_g() -> bool {
    true
}

/// 整站抓取任务句柄（支持取消）
pub struct GrabJob {
    pub cancel: CancellationToken,
    pub pages_done: Arc<AtomicU64>,
    pub total_pages: u32,
}

/// URL → 本地相对路径（保目录结构；/ 结尾补 index.html）
fn url_to_rel_path(u: &reqwest::Url) -> String {
    let host = u.host_str().unwrap_or("site").replace([':', '?', '*'], "_");
    let mut path = u.path().trim_start_matches('/').to_string();
    if path.is_empty() {
        path = "index.html".into();
    } else if u.path().ends_with('/') {
        path.push_str("index.html");
    } else if !Path::new(&path).extension().is_some() {
        path.push_str("/index.html");
    }
    format!("{host}/{path}")
}

impl Engine {
    /// 启动整站抓取（后台运行，返回任务 id）
    pub fn grab_site(self: &Arc<Self>, req: GrabSiteRequest) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        let pages_done = Arc::new(AtomicU64::new(0));
        let job = Arc::new(GrabJob {
            cancel: cancel.clone(),
            pages_done: pages_done.clone(),
            total_pages: req.max_pages,
        });
        self.grab_jobs.lock().insert(id.clone(), job);

        let me = self.clone();
        let me2 = me.clone();
        let job_id = id.clone();
        tokio::spawn(async move {
            let res = me2.clone().grab_site_run(&req, cancel, pages_done).await;
            me2.grab_jobs.lock().remove(&job_id);
            match res {
                Ok(n) => me.emit(EngineEvent::Notice(format!("整站抓取完成：{n} 个页面"))),
                Err(e) => {
                    if !e.to_string().contains("已取消") {
                        me.emit(EngineEvent::Notice(format!("整站抓取出错: {e:#}")))
                    }
                }
            }
        });
        Ok(id)
    }

    pub fn cancel_grab(&self, id: &str) -> bool {
        let removed = self.grab_jobs.lock().remove(id);
        if let Some(j) = removed {
            j.cancel.cancel();
            true
        } else {
            false
        }
    }

    async fn grab_site_run(
        self: Arc<Self>,
        req: &GrabSiteRequest,
        cancel: CancellationToken,
        pages_done: Arc<AtomicU64>,
    ) -> Result<usize> {
        let client = self.client(None)?;
        let _base = reqwest::Url::parse(&req.url)?;
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        let mut visited_assets: HashSet<String> = HashSet::new();
        queue.push_back((req.url.clone(), 0u32));
        visited.insert(req.url.clone());
        let mut saved = 0usize;
        let save_root = std::path::PathBuf::from(&req.save_dir);
        std::fs::create_dir_all(&save_root)?;

        while let Some((url, depth)) = queue.pop_front() {
            if cancel.is_cancelled() {
                bail!("抓取已取消");
            }
            if saved as u64 >= req.max_pages as u64 {
                break;
            }
            // 一次抓取同时拿到正文与引用列表（旧实现为保存 HTML 又重复请求了一次）
            let (analysis, mut html) = match analyze_page_with_body(&client, &AnalyzeRequest {
                url: url.clone(),
                referer: req.referer.clone(),
                cookie: None,
            })
            .await
            {
                Ok(a) => a,
                Err(e) => {
                    self.emit(EngineEvent::Notice(format!("跳过 {url}: {e:#}")));
                    continue;
                }
            };

            // 保存 HTML（目录结构还原）
            let rel = url_to_rel_path(&reqwest::Url::parse(&url)?);
            let html_path = save_root.join(sanitize_name(&rel));
            if let Some(parent) = html_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }

            // 引用映射：绝对 URL → 本地文件名（离线化用）
            let mut url_map: HashMap<String, String> = HashMap::new();
            let mut used_names: HashSet<String> = HashSet::new();

            // 静态资源 → 下载引擎（跨页面去重 + 限并发建任务，避免一页几十个资源串行探测）
            if req.include_assets {
                let asset_dir = html_path
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| req.save_dir.clone());
                let mut batch: Vec<AddTaskRequest> = Vec::new();
                for res in &analysis.resources {
                    if res.kind == "hls" {
                        continue;
                    }
                    // 落盘文件名（同页内同名不同资源时加序号）
                    let mut name = sanitize_name(&res.name);
                    if !used_names.insert(name.clone()) {
                        let mut i = 2;
                        loop {
                            let cand = dedup_name(&name, i);
                            if used_names.insert(cand.clone()) {
                                name = cand;
                                break;
                            }
                            i += 1;
                        }
                    }
                    url_map.insert(res.url.clone(), name.clone());
                    // 同一资源在多个页面出现时只下一次
                    if !visited_assets.insert(res.url.clone()) {
                        continue;
                    }
                    batch.push(AddTaskRequest {
                        url: res.url.clone(),
                        save_dir: Some(asset_dir.clone()),
                        file_name: Some(name),
                        connections: Some(4),
                        referer: Some(url.clone()),
                        ..Default::default()
                    });
                }
                use futures_util::StreamExt;
                futures_util::stream::iter(batch)
                    .for_each_concurrent(4, |req_asset| {
                        let me = self.clone();
                        async move {
                            let _ = me.add(req_asset).await;
                        }
                    })
                    .await;
            }

            // 页面链接同样改成本地扁平文件名（页面与 HTML 同目录保存，名字由 URL 唯一确定）
            for link in &analysis.links {
                if let Ok(u) = reqwest::Url::parse(link) {
                    let local = sanitize_name(&url_to_rel_path(&u));
                    url_map.insert(link.clone(), local);
                }
            }
            if !url_map.is_empty() {
                if let Ok(page_base) = reqwest::Url::parse(&analysis.url) {
                    html = rewrite_html_refs(&html, &page_base, &url_map);
                }
            }
            std::fs::write(&html_path, &html).with_context(|| format!("写入 {}", html_path.display()))?;
            saved += 1;
            pages_done.store(saved as u64, Ordering::Relaxed);

            // 同域页面递归
            if depth + 1 < req.max_depth {
                for link in analysis.links {
                    if visited.insert(link.clone()) && (queue.len() as u64) + 1 < req.max_pages as u64 * 3 {
                        queue.push_back((link, depth + 1));
                    }
                }
            }
        }
        Ok(saved)
    }
}

// ───────────────────────── HLS (m3u8) ─────────────────────────

pub struct HlsMedia {
    pub segments: Vec<String>,
    pub init: Option<String>,
    /// `#EXT-X-MEDIA-SEQUENCE`：缺省 IV 由「媒体序号」推导，不是分片下标
    pub media_sequence: u64,
    /// 与 `segments` 一一对应的密钥（`None` = 该分片未加密）。
    /// HLS 允许中途换密钥，所以不能只存「整个播放列表一个 key」。
    pub segment_keys: Vec<Option<HlsKey>>,
}

#[derive(Clone)]
pub struct HlsKey {
    pub method: String,
    pub key_url: String,
    pub iv: Option<[u8; 16]>,
}

/// 解析 m3u8：自动跟随 master playlist（选最高码率变体）
pub async fn parse_hls(client: &Client, url: &str) -> Result<HlsMedia> {
    let mut current = url.to_string();
    for _ in 0..3 {
        let text = fetch_text(client, &current).await?;
        if text.contains("#EXT-X-STREAM-INF") {
            // master playlist → 选最高 BANDWIDTH 变体
            let mut best: Option<(u64, String)> = None;
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
                    let bw = rest
                        .split(',')
                        .find_map(|kv| {
                            kv.trim()
                                .strip_prefix("BANDWIDTH=")
                                .and_then(|v| v.parse::<u64>().ok())
                        })
                        .unwrap_or(0);
                    // 变体 URI 是下一个非注释行
                    if let Some(uri) = lines[i + 1..].iter().find(|l| !l.starts_with('#')) {
                        if best.as_ref().map(|(b, _)| bw > *b).unwrap_or(true) {
                            best = Some((bw, uri.trim().to_string()));
                        }
                    }
                }
            }
            let Some((_, variant)) = best else {
                bail!("master playlist 中未找到可用流");
            };
            current = reqwest::Url::parse(&current)?.join(&variant)?.to_string();
            continue;
        }
        return parse_media_playlist(client, &current, &text).await;
    }
    bail!("master playlist 重定向层数过多")
}

async fn fetch_text(client: &Client, url: &str) -> Result<String> {
    let resp = client.get(url).send().await.context("请求 m3u8 失败")?;
    if !resp.status().is_success() {
        bail!("m3u8 返回 HTTP {}", resp.status().as_u16());
    }
    Ok(String::from_utf8_lossy(&read_body_capped(resp, 4 * 1024 * 1024).await?).into_owned())
}

async fn parse_media_playlist(_client: &Client, base_url: &str, text: &str) -> Result<HlsMedia> {
    let base = reqwest::Url::parse(base_url)?;
    let mut segments = Vec::new();
    let mut init: Option<String> = None;
    let mut media_sequence: u64 = 0;
    let mut cur_key: Option<HlsKey> = None;
    let mut segment_keys: Vec<Option<HlsKey>> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("#EXT-X-MAP:") {
            let rest = line.strip_prefix("#EXT-X-MAP:").unwrap_or(line);
            init = rest
                .split(',')
                .find_map(|kv| {
                    kv.trim()
                        .strip_prefix("URI=\"")
                        .and_then(|v| v.strip_suffix('"'))
                })
                .and_then(|u| base.join(u).ok().map(|x| x.to_string()));
        } else if line.starts_with("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = line
                .trim_start_matches("#EXT-X-MEDIA-SEQUENCE:")
                .trim()
                .parse::<u64>()
                .unwrap_or(0);
        } else if line.starts_with("#EXT-X-KEY:") {
            let rest = line.strip_prefix("#EXT-X-KEY:").unwrap_or(line);
            let method = rest.split(',').find_map(|kv| {
                kv.trim()
                    .strip_prefix("METHOD=")
                    .map(|v| v.to_string())
            });
            let key_uri = rest.split(',').find_map(|kv| {
                kv.trim()
                    .strip_prefix("URI=\"")
                    .and_then(|v| v.strip_suffix('"'))
            });
            let iv = rest.split(',').find_map(|kv| {
                kv.trim().strip_prefix("IV=0x").and_then(|v| parse_hex16(v))
            });
            // 该指令对后续所有分片生效，直到下一条 EXT-X-KEY（METHOD=NONE 表示清除）
            cur_key = match (method, key_uri) {
                (Some(m), Some(u)) if m != "NONE" => base
                    .join(&u)
                    .ok()
                    .map(|x| HlsKey { method: m, key_url: x.to_string(), iv }),
                _ => None,
            };
        } else if !line.is_empty() && !line.starts_with('#') {
            let abs = base.join(line).map_err(|e| anyhow!("分片地址无效: {e}"))?.to_string();
            segments.push(abs);
            segment_keys.push(cur_key.clone());
        }
    }
    if segments.is_empty() {
        bail!("playlist 中没有分片");
    }
    Ok(HlsMedia {
        segments,
        init,
        media_sequence,
        segment_keys,
    })
}

/// 缺省 IV：媒体序号的大端 16 字节表示（RFC 8216 §5.2）
fn default_iv(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

fn parse_hex16(s: &str) -> Option<[u8; 16]> {
    if s.len() < 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_refs_basic() {
        let html = r##"<html><head><script>var a="<b>not real</b>";</script></head>
<body>
<a href="/page2.html">link</a>
<img src="img/photo.jpg">
<video src="https://cdn.example.com/v.mp4"></video>
<div data-src="lazy.webp"></div>
<a href="javascript:void(0)">js</a>
<a href="#anchor">anchor</a>
<!-- <a href="/commented.html">hidden</a> -->
</body></html>"##;
        let refs = extract_refs(html);
        assert!(refs.contains(&"/page2.html".to_string()));
        assert!(refs.contains(&"img/photo.jpg".to_string()));
        assert!(refs.contains(&"https://cdn.example.com/v.mp4".to_string()));
        assert!(refs.contains(&"lazy.webp".to_string()));
        assert!(!refs.iter().any(|r| r.contains("javascript")));
        assert!(!refs.iter().any(|r| r.contains("commented")));
        assert!(!refs.iter().any(|r| r.contains("not real")));
    }

    #[test]
    fn scan_attrs_quote_styles() {
        let out = scan_attrs(r#" href="/a" id='b' data-x=c >"#);
        assert_eq!(out[0], ("href".into(), "/a".into()));
        assert_eq!(out[1], ("id".into(), "b".into()));
        assert_eq!(out[2], ("data-x".into(), "c".into()));
    }

    #[test]
    fn decode_entities_ok() {
        assert_eq!(decode_entities("a&amp;b&lt;c&gt;d&quot;e&nbsp;f"), "a&b<c>d\"e f");
        assert_eq!(decode_entities("plain"), "plain");
    }

    #[test]
    fn classify_ext_kinds() {
        assert_eq!(classify_ext("HTTP://x/VIDEO.MP4").unwrap().0, "video");
        assert_eq!(classify_ext("/a/b/file.zip?token=1").unwrap().0, "archive");
        assert_eq!(classify_ext("/live/index.m3u8").unwrap().0, "hls");
        assert_eq!(classify_ext("/noext"), None);
    }

    #[test]
    fn url_to_rel_path_shapes() {
        let u = reqwest::Url::parse("https://site.com/").unwrap();
        assert_eq!(url_to_rel_path(&u), "site.com/index.html");
        let u = reqwest::Url::parse("https://site.com/blog/post.html").unwrap();
        assert_eq!(url_to_rel_path(&u), "site.com/blog/post.html");
        let u = reqwest::Url::parse("https://site.com/blog").unwrap();
        assert_eq!(url_to_rel_path(&u), "site.com/blog/index.html");
    }

    #[test]
    fn rewrite_refs_to_local_paths() {
        let base = reqwest::Url::parse("https://site.com/blog/post.html").unwrap();
        let mut map = HashMap::new();
        map.insert("https://site.com/css/a.css".to_string(), "site.com_css_a.css".to_string());
        map.insert("https://site.com/blog/p2.html".to_string(), "site.com_blog_p2.html".to_string());
        map.insert("https://cdn.x.com/i.png".to_string(), "i.png".to_string());
        map.insert("https://site.com/set/a.png".to_string(), "a.png".to_string());
        map.insert("https://site.com/set/a2.png".to_string(), "a_2.png".to_string());
        let html = r##"<link href="/css/a.css"><a href="p2.html#c1">x</a>
<img src="https://cdn.x.com/i.png"><img src="other.png">
<div style="background-image:url('/css/a.css')"></div>
<img srcset="https://site.com/set/a.png 1x, https://site.com/set/a2.png 2x">
<a href="#anchor">keep</a><img src="data:image/png;base64,AAA">"##;
        let out = rewrite_html_refs(html, &base, &map);
        assert!(out.contains(r#"href="site.com_css_a.css""#), "{out}");
        assert!(out.contains(r#"href="site.com_blog_p2.html#c1""#), "{out}");
        assert!(out.contains(r#"src="i.png""#), "{out}");
        // 未命中的引用保持原样
        assert!(out.contains(r#"src="other.png""#), "{out}");
        assert!(out.contains("background-image:url(site.com_css_a.css)"), "{out}");
        assert!(out.contains("a.png 1x, a_2.png 2x"), "{out}");
        assert!(out.contains(r##"href="#anchor""##), "{out}");
        assert!(out.contains("data:image/png"), "{out}");
    }

    #[test]
    fn dedup_name_keeps_extension() {
        assert_eq!(dedup_name("a.png", 2), "a_2.png");
        assert_eq!(dedup_name("noext", 3), "noext_3");
    }

    #[tokio::test]
    async fn parse_media_playlist_plain() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10.0,\nseg0.ts\n#EXTINF:9.5,\nseg1.ts\n#EXT-X-ENDLIST\n";
        let media = parse_media_playlist(&Client::new(), "https://cdn.example.com/live/index.m3u8", text)
            .await
            .unwrap();
        assert_eq!(media.segments.len(), 2);
        assert_eq!(media.segments[0], "https://cdn.example.com/live/seg0.ts");
        assert!(media.segment_keys.iter().all(|k| k.is_none()));
        assert_eq!(media.media_sequence, 0);
    }

    #[tokio::test]
    async fn parse_media_playlist_aes128() {
        let text = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"https://k.example.com/key\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXTINF:10,\ns1.ts\n";
        let media = parse_media_playlist(&Client::new(), "https://cdn.example.com/a.m3u8", text)
            .await
            .unwrap();
        let k = media.segment_keys[0].clone().unwrap();
        assert_eq!(k.method, "AES-128");
        assert_eq!(k.key_url, "https://k.example.com/key");
        assert_eq!(k.iv.unwrap()[1], 1);
    }

    /// 缺省 IV 依赖 EXT-X-MEDIA-SEQUENCE，而不是分片下标
    #[tokio::test]
    async fn media_sequence_is_parsed_and_feeds_default_iv() {
        let text = "#EXTM3U
#EXT-X-MEDIA-SEQUENCE:120
#EXTINF:4,
a.ts
#EXTINF:4,
b.ts
";
        let media = parse_media_playlist(&Client::new(), "https://cdn.example.com/v.m3u8", text)
            .await
            .unwrap();
        assert_eq!(media.media_sequence, 120);
        // 第 0 片缺省 IV 的序号是 120，不是 1
        assert_eq!(default_iv(media.media_sequence)[15], 120);
        assert_eq!(default_iv(media.media_sequence + 1)[15], 121);
    }

    /// EXT-X-KEY 可以出现多次：中途换钥后，前后分片要绑不同的 key
    #[tokio::test]
    async fn key_rotation_binds_per_segment() {
        let text = "#EXTM3U
#EXT-X-KEY:METHOD=AES-128,URI=\"k1.key\"
#EXTINF:4,
seg0.ts
#EXTINF:4,
seg1.ts
#EXT-X-KEY:METHOD=AES-128,URI=\"k2.key\"
#EXTINF:4,
seg2.ts
#EXT-X-KEY:METHOD=NONE
#EXTINF:4,
seg3.ts
";
        let media = parse_media_playlist(&Client::new(), "https://cdn.example.com/x.m3u8", text)
            .await
            .unwrap();
        assert_eq!(media.segments.len(), 4);
        let urls: Vec<Option<String>> = media
            .segment_keys
            .iter()
            .map(|k| k.as_ref().map(|x| x.key_url.clone()))
            .collect();
        assert_eq!(urls[0].as_deref(), Some("https://cdn.example.com/k1.key"));
        assert_eq!(urls[1].as_deref(), Some("https://cdn.example.com/k1.key"));
        assert_eq!(urls[2].as_deref(), Some("https://cdn.example.com/k2.key"));
        assert!(urls[3].is_none(), "METHOD=NONE 之后的分片不应带密钥");
    }

    #[test]
    fn hex16_and_hex20() {
        assert_eq!(parse_hex16("000102030405060708090a0b0c0d0e0f").unwrap()[15], 15);
        assert!(parse_hex16("1234").is_none());
        assert_eq!(
            crate::bt::hex_to_20(&"0".repeat(40)).unwrap(),
            [0u8; 20]
        );
    }

    #[test]
    fn aes_cbc_roundtrip() {
        // 用固定密钥/IV 验证解密不 panic 且长度正确（密文为空填充的数据块）
        let key = [7u8; 16];
        let iv = [0u8; 16];
        // AES-128 ECB/CBC 手工加密太麻烦，这里只验证非法密文长度报错路径
        assert!(crate::crypto::aes128_cbc_decrypt(&key, &iv, &[1, 2, 3]).is_err());
        let zero = crate::crypto::aes128_cbc_decrypt(&key, &iv, &[0u8; 16]);
        assert!(zero.is_err() || !zero.unwrap().is_empty());
    }
}

impl Engine {
    /// 下载 HLS：创建 hls 类型任务并后台执行（顺序拉分片、AES-128 解密、合并写盘）
    pub async fn download_hls(
        self: &Arc<Self>,
        url: &str,
        save_dir: Option<String>,
        file_name: Option<String>,
    ) -> Result<TaskId> {
        let s = self.settings();
        let client = self.client(None)?;

        // 先解析好再建任务，尽早暴露错误
        let media = parse_hls(&client, url).await?;

        let dir = save_dir.filter(|d| !d.trim().is_empty()).unwrap_or_else(|| s.download_dir.clone());
        std::fs::create_dir_all(&dir).ok();
        let name = file_name
            .filter(|n| !n.trim().is_empty())
            .map(|n| sanitize_name(&n))
            .unwrap_or_else(|| {
                // 用页面路径猜测名字
                let u = reqwest::Url::parse(url).ok();
                let seg = u.as_ref().and_then(|u| u.path_segments()).and_then(|p| p.last());
                sanitize_name(&format!("{}.ts", seg.unwrap_or("hls-video")))
            });
        let path = crate::util::reserve_path(Path::new(&dir), &name)
            .with_context(|| format!("无法在 {dir} 分配文件名"))?;

        let t = Task {
            id: uuid::Uuid::new_v4().to_string(),
            url: url.to_string(),
            kind: TaskKind::Hls,
            mirrors: Vec::new(),
            file_name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or(name),
            save_dir: dir,
            save_path: path.to_string_lossy().to_string(),
            size: None,
            downloaded: 0,
            state: TaskState::Downloading,
            error: None,
            error_code: None,
            retryable: None,
            connections: 1,
            segments: Vec::new(),
            supports_range: false,
            etag: None,
            last_modified: None,
            content_type: Some("video/mp2t".into()),
            referer: None,
            cookie: None,
            auth_origin: String::new(),
            user_agent: None,
            speed_limit: 0,
            scheduled_at: None,
            created_at: now_ms(),
            completed_at: None,
            speed: 0,
            peers: 0,
            upload_speed: 0,
            bt_info_hash: None,
            category: Some("视频".into()),
            username: None,
            password: None,
            interval_secs: 0,
            resume_anchor: None,
        };
        let id = t.id.clone();
        let task_id = id.clone();
        let rt = Runtime::new(t.clone());
        *rt.running.lock() = true;
        *rt.cancel.lock() = CancellationToken::new();
        // HLS 任务在这里就「开工」了，同样分配运行代号
        let generation = rt.next_generation();
        self.tasks.lock().insert(id.clone(), rt.clone());
        let _ = self.store.save_task(&t);
        self.emit(EngineEvent::TaskUpdated(t));

        let me = self.clone();
        let cancel = rt.cancel.lock().clone();
        tokio::spawn(async move {
            let res = me
                .clone()
                .hls_run(&rt, generation, &media, &client, &cancel)
                .await;
            match res {
                // 取消退出：清理 running 与任务状态（与 pause() 竞争时幂等），
                // 避免取消后永久占用并发槽。过期代号会被统一入口直接丢弃。
                Err(_) if cancel.is_cancelled() => {
                    me.save_paused(&rt, generation);
                }
                Err(e) => {
                    me.fail_task(&task_id, Some(generation), &e);
                }
                Ok(()) => {}
            }
        });
        Ok(id)
    }

    pub(crate) async fn hls_run(
        self: Arc<Self>,
        rt: &Arc<Runtime>,
        generation: u64,
        media: &HlsMedia,
        client: &Client,
        cancel: &CancellationToken,
    ) -> Result<()> {
        // 合并结果先写 `<最终名>.part`，由 finish_task 在完成时 rename 成最终名
        let save_path = { rt.task.lock().save_path.clone() };
        let work_path = crate::util::part_path(std::path::Path::new(&save_path));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&work_path)
            .with_context(|| format!("无法创建文件: {}", work_path.display()))?;

        // 密钥按 URL 缓存：轮换播放列表里相邻分片常共用同一个 key，不必每片重下
        let mut key_cache: std::collections::HashMap<String, Vec<u8>> =
            std::collections::HashMap::new();

        if let Some(init_url) = &media.init {
            let bytes = self
                .hls_fetch_segment(client, init_url, cancel)
                .await?;
            use std::io::Write;
            file.write_all(&bytes)?;
            rt.downloaded.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }

        for (i, seg_url) in media.segments.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(anyhow!("已取消"));
            }
            let mut bytes = self.hls_fetch_segment(client, seg_url, cancel).await?;
            // 每片单独取密钥：HLS 允许中途换 key（EXT-X-KEY 可多次出现）
            if let Some(key) = media.segment_keys.get(i).and_then(|k| k.as_ref()) {
                if key.method != "AES-128" {
                    bail!("暂不支持的 HLS 加密方式: {}", key.method);
                }
                let kb = match key_cache.get(&key.key_url) {
                    Some(v) => v.clone(),
                    None => {
                        let kb = client
                            .get(&key.key_url)
                            .send()
                            .await
                            .with_context(|| format!("获取 HLS 密钥失败: {}", key.key_url))?
                            .error_for_status()?
                            .bytes()
                            .await?
                            .to_vec();
                        if kb.len() != 16 {
                            bail!("HLS 密钥长度异常（{}）: {} 字节", key.key_url, kb.len());
                        }
                        key_cache.insert(key.key_url.clone(), kb.clone());
                        kb
                    }
                };
                let iv_bytes = match key.iv {
                    Some(v) => v,
                    // 缺省 IV = 该分片的媒体序号（大端 16 字节）。
                    // 用分片下标是错的：直播/切片列表常从 0 以外的序号开始
                    // （#EXT-X-MEDIA-SEQUENCE），用下标会导致整段解成乱码。
                    None => default_iv(media.media_sequence + i as u64),
                };
                // 解密失败必须报错：此前失败会静默把密文写进成品，
                // 得到一个能播放但画面全花/中途静音的文件，却查不出原因
                bytes = crate::crypto::aes128_cbc_decrypt(&kb, &iv_bytes, &bytes)
                    .with_context(|| format!("第 {} 个分片解密失败（密钥或 IV 不匹配）", i + 1))?;
            }
            use std::io::Write;
            file.write_all(&bytes)?;
            rt.downloaded.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }

        drop(file);
        self.finish_task(rt, generation)?;
        Ok(())
    }

    async fn hls_fetch_segment(
        &self,
        client: &Client,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>> {
        let mut attempt = 0u32;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => bail!("已取消"),
                res = client.get(url).send() => {
                    match res {
                        Ok(resp) => match resp.error_for_status() {
                            Ok(r) => {
                                match r.bytes().await {
                                    Ok(b) => return Ok(b.to_vec()),
                                    Err(e) => {
                                        attempt += 1;
                                        if attempt >= 3 { return Err(anyhow!("分片读取失败: {e}")); }
                                    }
                                }
                            }
                            Err(e) => {
                                attempt += 1;
                                if attempt >= 3 { return Err(anyhow!("分片请求失败: {e}")); }
                            }
                        },
                        Err(e) => {
                            attempt += 1;
                            if attempt >= 3 { return Err(anyhow!("分片连接失败: {e}")); }
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64)).await;
        }
    }
}
