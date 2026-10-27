//! net/server.rs —— axum HTTP + WebSocket（主机即服务器）。
//!
//! 路由：
//! - `/` 与静态文件：`config.frontend_dir`（禁止目录穿越；缺省提供占位帮助页）；
//! - `/ws`：WebSocket 升级，进入 `net::session::run_socket`；
//! - `/api/status`：房间码 / 状态 / 人数 / 协议版本（二维码页与排障用）。
//!
//! 硬约束：
//! - 监听地址与端口 100% 来自 `config.bind_target()`，本文件无任何字面地址；
//! - 静态目录来自配置，未配置时给出明确提示页而非硬编码页面。

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::config::BpHostConfig;
use crate::net::session;
use crate::net::protocol::PROTOCOL_VERSION;
use crate::service::{now_ms, BpService};

#[derive(Clone)]
pub struct WebState {
    pub svc: Arc<BpService>,
    pub cfg: Arc<BpHostConfig>,
}

/// 组装路由（供命令层复用同一定义）。
pub fn build_router(cfg: Arc<BpHostConfig>, svc: Arc<BpService>) -> Router {
    let state = WebState { svc, cfg };
    Router::new()
        .route("/ws", get(ws_route))
        .route("/api/status", get(status_route))
        .fallback(static_route)
        .with_state(state)
}

/// 阻塞到底地 serve（崩溃恢复后的服务入口；standalone / 测试共用）。
pub async fn serve(cfg: BpHostConfig, svc: Arc<BpService>) -> Result<(), String> {
    let cfg = Arc::new(cfg);
    let bind = cfg.bind_target();
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("监听 {bind} 失败: {e}"))?;
    let app = build_router(cfg.clone(), svc.clone());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .map_err(|e| format!("HTTP 服务异常退出: {e}"))
}

/// 绑定成功后交由调用方 spawn 的启动路径（Tauri 命令用）。
///
/// `shutdown` 触发后 axum 停止接受新连接、等已建立的连接收尾再返回；
/// 发送端被直接 drop 同样触发停机（防御性：接收失败不 panic）。
pub async fn serve_listener(
    listener: tokio::net::TcpListener,
    cfg: Arc<BpHostConfig>,
    svc: Arc<BpService>,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), String> {
    let app = build_router(cfg, svc);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = shutdown.await;
    })
    .await
    .map_err(|e| format!("HTTP 服务异常退出: {e}"))
}

/// 计时 tick 后台任务（TimerSync 广播 + 超时裁决 + SessionEnded）。
pub fn spawn_timer_ticker(svc: Arc<BpService>) {
    let interval = svc.config.timer_sync_interval_ms.max(100) as u64;
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(interval));
        loop {
            tick.tick().await;
            let evs = svc.tick().await;
            if !evs.is_empty() {
                svc.broadcast(evs);
            }
            let _ = now_ms(); // 占位：避免 unused 警告噪声（时钟在 tick 内读取）
        }
    });
}

async fn ws_route(
    State(st): State<WebState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> Response {
    session::ws_upgrade(st.svc.clone(), ws, addr.to_string()).await
}

async fn status_route(State(st): State<WebState>) -> Response {
    let svc = st.svc.clone();
    let state = svc.state.lock().await;
    let payload = json!({
        "roomCode": state.room_code,
        "status": state.status,
        "players": state.players.iter().filter(|p| p.connected).count(),
        "totalSteps": state.step_info().1,
        "protocolVersion": PROTOCOL_VERSION,
        "locked": state.locked,
    });
    drop(state);
    Json(payload).into_response()
}

// ------------------------------------------------------------------ 静态前端

async fn static_route(State(st): State<WebState>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    // 目录穿越防护
    if path.split('/').any(|seg| seg == ".." || seg.is_empty() && !path.is_empty()) {
        return (axum::http::StatusCode::BAD_REQUEST, "非法路径").into_response();
    }
    let rel = if path.is_empty() { "index.html" } else { path };
    let local = std::path::Path::new(&st.cfg.frontend_dir).join(rel);
    match tokio::fs::read(&local).await {
        Ok(bytes) => {
            let mime = mime_of(rel);
            ([(header::CONTENT_TYPE, mime)], bytes).into_response()
        }
        Err(_) => {
            // 帮助页（避免裸 404）；说明如何落地前端目录
            let tip = "<h1>bp-host</h1><p>静态前端目录尚未就绪。\
                       在配置文件的 frontend_dir 指到包含 index.html 的目录，\
                       或先用 wireframe-*.html 做视觉走查。</p>\
                       <pre>GET /ws   WebSocket 端点</pre>".to_string();
            (
                axum::http::StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                tip,
            )
                .into_response()
        }
    }
}

fn mime_of(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BpHostConfig;
    use crate::persistence::sqlite::SqliteStore;
    use crate::service::BpService;
    use std::sync::Arc;

    fn temp_db() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "bp-host-sdapi-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ))
    }

    /// 优雅停机 smoke：shutdown 信号发出后 listener 必须关闭、拒绝新 TCP 连接，
    /// 且 `serve_listener` 正常返回（axum graceful 关闭语义）。
    #[tokio::test]
    async fn graceful_shutdown_stops_accepting() {
        let path = temp_db();
        let store = Arc::new(SqliteStore::open(&path).unwrap());
        let cfg = BpHostConfig::default();
        let cfg_arc = Arc::new(cfg.clone());
        let svc = BpService::new(cfg, store).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(serve_listener(listener, cfg_arc, svc, rx));
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        // 停机前：可建立 TCP 连接（服务器活着）
        let pre = tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
        assert!(pre.is_ok(), "停机前应可建立 TCP 连接");

        tx.send(()).unwrap();
        // 停机在下次事件循环 tick 后生效：轮询至 connect 被拒（上限 2s）
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
                break;
            }
        }
        let after = tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
        assert!(after.is_err(), "停机后 listener 应已关闭、拒绝新连接");

        // task await 已返回 = 优雅关闭走完；内部 Result 无错误
        let res = task.await.unwrap();
        assert!(res.is_ok(), "serve_listener 应正常退出");
        let _ = std::fs::remove_file(&path);
    }
}
