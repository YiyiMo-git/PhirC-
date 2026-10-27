//! net/session.rs —— 连接级会话：WS 套接字生命周期。
//!
//! 职责：
//! - 读写分离：写半边由 mpsc 消费（直接回执 + 广播桥接统一走文本帧）；
//! - 广播桥：`svc.events` broadcast 订阅 → 每个连接一份（社会事件不丢）；
//! - 踢出信号：`svc` 侧 Kick → `kick_rx` select 退出读循环 → 会话关闭；
//! - JSON 解析失败也回 `Error` 事件（硬约束：不静默忽略）；
//! - 心跳：`Ping → Pong`；页面隐藏的降频心跳只是客户端少发，服务端不主动
//!   连接清洗（断线标记依赖 WS 真关闭事件，不断线不自动暂停）。

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::net::protocol::{ClientIntent, HostEvent};
use crate::service::{err, BpService, CloseReason, ConnHandle};

/// WS 升级入口（由 server.rs 调用）。
pub async fn ws_upgrade(svc: Arc<BpService>, ws: WebSocketUpgrade, addr: String) -> Response {
    let handle = svc.open_conn(addr.clone()).await;
    ws.on_upgrade(move |socket| run_socket(socket, svc, handle, addr))
}

async fn run_socket(socket: WebSocket, svc: Arc<BpService>, handle: ConnHandle, _addr: String) {
    let (mut ws_sink, mut ws_src) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<HostEvent>();
    let _conn_id = handle.conn_id;

    // 写半边：宿主消息队列 → 文本帧
    let writer = tokio::spawn(async move {
        while let Some(ev) = out_rx.recv().await {
            let text = serde_json::to_string(&ev).unwrap_or_default();
            if ws_sink.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
    });

    // 广播桥：svc.events → 本连接（DoS 友好；Lagged 丢帧只影响过期事件）
    let mut bridge_src = svc.events.subscribe();
    let bridge_tx = out_tx.clone();
    let bridge = tokio::spawn(async move {
        loop {
            match bridge_src.recv().await {
                Ok(ev) => {
                    if bridge_tx.send(ev).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // 读循环：意图 → 服务层；同时监听踢出信号。
    let mut closed = false;
    let mut kick = handle.kick_rx;
    loop {
        tokio::select! {
            m = ws_src.next() => {
                let Some(msg) = m else { closed = true; break; };
                match msg {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<ClientIntent>(&text) {
                            Ok(intent) => {
                                for ev in svc.process_intent(handle.conn_id, intent).await {
                                    if out_tx.send(ev).is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                // 不可解析的帧：回错误事件而非静默丢弃
                                let _ = out_tx.send(err(
                                    "BAD_JSON",
                                    &format!("意图解析失败：{e}；帧协议见 net/protocol.rs"),
                                ));
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        closed = true;
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        closed = true;
                        break;
                    }
                }
            }
            _ = kick.recv() => {
                out_tx.send(err("KICKED", "你已被主办方移出会话")).ok();
                break;
            }
        }
    }

    out_tx.send(HostEvent::Notice { message: "连接已关闭".into() }).ok();
    let reason = if closed { CloseReason::Leave } else { CloseReason::Leave };
    svc.close_conn(handle.conn_id, reason).await;
    bridge.abort();
    writer.abort();
}

/// 404 兜底（带提示，避免静默空响应）。
pub fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "bp-host: 资源未找到（静态前端目录见 config.frontend_dir）").into_response()
}
