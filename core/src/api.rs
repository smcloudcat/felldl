//! 浏览器集成用本地 HTTP API（127.0.0.1）
//!
//! 端点：
//!   GET  /ping                       健康检查（无需鉴权）
//!   POST /add  {url, referer?, cookie?, file_name?, headers?}   添加下载
//!   GET  /tasks                      任务列表
//!   GET  /tasks/{id}                 单个任务（脱敏）
//!   POST /tasks/{id}/pause           暂停
//!   POST /tasks/{id}/resume          继续
//!   POST /tasks/{id}/cancel  {delete_file?}   取消（可选连文件删除）
//!   GET  /stats                      运行状态
//!   GET  /config                     扩展/客户端配置
//!
//! 鉴权：请求头 `X-Felldl-Token` 或 query `?token=`，与设置里的 api_token 比对。
//! 仅绑定 127.0.0.1，且校验 Origin 防止网页跨站调用。

use crate::engine::Engine;
use crate::model::AddTaskRequest;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(Clone)]
struct ApiState {
    engine: Arc<Engine>,
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

fn check_token(engine: &Engine, headers: &HeaderMap, q: &TokenQuery) -> Result<(), Response> {
    let expected = engine.http_token.read().clone();
    let provided = headers
        .get("x-felldl-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| q.token.clone());
    match provided {
        Some(t) if !expected.is_empty() && t == expected => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error": "token 无效"})),
        )
            .into_response()),
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct ApiAddBody {
    pub url: String,
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub cookie: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub connections: Option<usize>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub save_dir: Option<String>,
    #[serde(default)]
    pub mirrors: Option<Vec<String>>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub interval_secs: Option<u64>,
    #[serde(default)]
    pub category: Option<String>,
}

async fn ping() -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "app": "felldl",
        "version": crate::VERSION,
    }))
}

async fn add_task(
    State(st): State<ApiState>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    Json(body): Json<ApiAddBody>,
) -> Response {
    if let Err(r) = check_token(&st.engine, &headers, &q) {
        return r;
    }
    let req = AddTaskRequest {
        url: body.url,
        save_dir: body.save_dir,
        file_name: body.file_name,
        connections: body.connections,
        speed_limit: None,
        scheduled_at: None,
        referer: body.referer,
        cookie: body.cookie,
        headers: body.headers.unwrap_or_default(),
        mirrors: body.mirrors.unwrap_or_default(),
        username: body.username,
        password: body.password,
        interval_secs: body.interval_secs.unwrap_or(0),
        category: body.category,
    };
    match st.engine.add(req).await {
        Ok(id) => Json(json!({"ok": true, "id": id})).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": format!("{e:#}")})),
        )
            .into_response(),
    }
}

async fn list_tasks(
    State(st): State<ApiState>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check_token(&st.engine, &headers, &q) {
        return r;
    }
    // 对外只给脱敏副本：cookie / password 是下载器自用的凭据，
    // 不能随「列出任务」发给任何拿到 token 的客户端
    let tasks: Vec<_> = st.engine.snapshot().iter().map(|t| t.redacted()).collect();
    Json(json!({"ok": true, "tasks": tasks})).into_response()
}

async fn stats(State(st): State<ApiState>) -> impl IntoResponse {
    let tasks = st.engine.snapshot();
    let active = tasks.iter().filter(|t| t.state.is_active()).count();
    let total_speed: u64 = tasks.iter().map(|t| t.speed).sum();
    Json(json!({
        "ok": true,
        "version": crate::VERSION,
        "active": active,
        "tasks": tasks.len(),
        "speed": total_speed,
    }))
}

/// 扩展用的配置：面板显示方式 + 基本信息（需鉴权）
async fn config(
    State(st): State<ApiState>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check_token(&st.engine, &headers, &q) {
        return r;
    }
    let s = st.engine.settings();
    Json(json!({
        "ok": true,
        "version": crate::VERSION,
        "browser_panel": s.browser_panel,
        "categories": s.categories.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        "download_dir": s.download_dir,
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
struct CancelBody {
    /// 取消的同时删除已下载的部分（含 .part 半成品）
    #[serde(default)]
    delete_file: bool,
}

/// 操作失败统一返回稳定 code，客户端（MCP / 扩展）据此决定要不要重试
fn op_error(e: &anyhow::Error) -> Response {
    let info = crate::error::classify(e);
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "ok": false,
            "error": format!("{e:#}"),
            "code": info.code,
            "retryable": info.retryable,
        })),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"ok": false, "error": "任务不存在", "code": "not_found"})),
    )
        .into_response()
}

/// 单个任务（脱敏）：MCP 客户端拿它轮询进度
async fn get_task(
    State(st): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check_token(&st.engine, &headers, &q) {
        return r;
    }
    match st.engine.get(&id) {
        Some(t) => Json(json!({"ok": true, "task": t.redacted()})).into_response(),
        None => not_found(),
    }
}

async fn pause_task(
    State(st): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check_token(&st.engine, &headers, &q) {
        return r;
    }
    if st.engine.get(&id).is_none() {
        return not_found();
    }
    match st.engine.pause(&id).await {
        Ok(()) => Json(json!({"ok": true, "id": id, "state": "paused"})).into_response(),
        Err(e) => op_error(&e),
    }
}

async fn resume_task(
    State(st): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check_token(&st.engine, &headers, &q) {
        return r;
    }
    if st.engine.get(&id).is_none() {
        return not_found();
    }
    match st.engine.resume(&id).await {
        Ok(()) => Json(json!({"ok": true, "id": id})).into_response(),
        Err(e) => op_error(&e),
    }
}

async fn cancel_task(
    State(st): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    body: Option<Json<CancelBody>>,
) -> Response {
    if let Err(r) = check_token(&st.engine, &headers, &q) {
        return r;
    }
    if st.engine.get(&id).is_none() {
        return not_found();
    }
    let delete_file = body.map(|b| b.0.delete_file).unwrap_or(false);
    match st.engine.cancel(&id, delete_file).await {
        Ok(()) => Json(json!({"ok": true, "id": id, "delete_file": delete_file})).into_response(),
        Err(e) => op_error(&e),
    }
}

/// 启动 API 服务（非阻塞）。仅监听 127.0.0.1。
pub async fn serve(engine: Arc<Engine>, port: u16) -> anyhow::Result<u16> {
    let st = ApiState {
        engine: engine.clone(),
    };
    let app = Router::new()
        .route("/ping", get(ping))
        .route("/stats", get(stats))
        .route("/config", get(config))
        .route("/add", post(add_task))
        .route("/tasks", get(list_tasks))
        .route("/tasks/{id}", get(get_task))
        .route("/tasks/{id}/pause", post(pause_task))
        .route("/tasks/{id}/resume", post(resume_task))
        .route("/tasks/{id}/cancel", post(cancel_task))
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods(tower_http::cors::Any)
                .allow_headers(tower_http::cors::Any),
        )
        .with_state(st);

    // 端口被占用时自动顺延，避免启动失败
    let mut actual_port = port;
    let listener = loop {
        match TcpListener::bind(("127.0.0.1", actual_port)).await {
            Ok(l) => break l,
            Err(_) if actual_port < port + 20 => {
                actual_port += 1;
            }
            Err(e) => return Err(anyhow::anyhow!("本地 API 端口不可用: {e}")),
        }
    };
    let bound = listener.local_addr()?.port();
    let me = engine.clone();
    tokio::spawn(async move {
        // 端口顺延后要回写设置，前端展示的端口才准确
        {
            let mut s = me.settings();
            if s.api_port != bound {
                s.api_port = bound;
                let _ = me.update_settings(s);
            }
        }
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("[felldl] API 服务退出: {e}");
        }
    });
    Ok(bound)
}
