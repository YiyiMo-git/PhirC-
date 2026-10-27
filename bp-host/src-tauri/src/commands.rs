//! commands.rs —— Tauri 命令契约（**本地 invoke 与远程 WS 共用同一 service 层**，
//! 本文件只做薄封装：拿 `Arc<BpService>`、起服务器、桥接事件到前端）。
//!
//! 本地主控台的语义 = 「第 0 号连接」：它同样先走 `Join`（拿 token / 控制台身份），
//! 之后每次 `bp_send_intent` 的返回值就是该意图的直接回执；广播事件经
//! `bp-event` Tauri 事件推送（与 WS 客户端的 `svc.events` 订阅同源同序）。
//! 这样前端 `net.ts` 的两种传输实现（invoke / WS）可以共享同一份 UI 状态机。

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};
use tokio::sync::RwLock;

use crate::config::{self, BpHostConfig};
use crate::net::protocol::{BpResult, BpSnapshot, ClientIntent, HostEvent};
use crate::net::{protocol::PROTOCOL_VERSION, server as net_server};
use crate::persistence::sqlite::SqliteStore;
use crate::service::{LOCAL_CONN_ID, BpService};

/// 服务器进程内状态（bp_server_status 查询用）。
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub running: bool,
    pub bind: String,
    pub room_code: String,
    pub error: Option<String>,
}

/// 挂在 Tauri manage 上的应用上下文。
pub struct AppCtx {
    pub svc: Arc<BpService>,
    pub config_path: PathBuf,
    pub config_lock: RwLock<Arc<BpHostConfig>>,
    pub server_status: std::sync::Mutex<ServerStatus>,
    /// 当前监听任务句柄（Some = 正在监听）。停机信号 + 任务 join 双持。
    server_handle: std::sync::Mutex<Option<ServerHandle>>,
}

/// 运行中的服务器句柄：停机信号发送端 + 服务任务的 join 手柄。
struct ServerHandle {
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

// ---------------------------------------------------------------- 命令

/// 发送一条意图。返回该意图的**直接回执**；广播事件经 `bp-event` 推送。
#[tauri::command]
pub async fn bp_send_intent(
    ctx: State<'_, AppCtx>,
    intent: ClientIntent,
) -> Result<Vec<HostEvent>, String> {
    Ok(ctx.svc.process_intent(LOCAL_CONN_ID, intent).await)
}

/// 拉取当前权威快照（免 round-trip Join；仅供控制台渲染兜底）。
#[tauri::command]
pub async fn bp_get_snapshot(ctx: State<'_, AppCtx>) -> Result<BpSnapshot, String> {
    let state = ctx.svc.state.lock().await;
    Ok(state.build_snapshot())
}

/// 读取配置（含端口 / 房间码 / 干扰开关；UI「端口转发/中继说明」入口的数据源）。
#[tauri::command]
pub async fn bp_get_config(ctx: State<'_, AppCtx>) -> Result<BpHostConfig, String> {
    Ok(ctx.config_lock.read().await.as_ref().clone())
}

/// 保存配置：校验后写回配置文件；**监听地址/端口变更需重启进程**（返回提示）。
#[tauri::command]
pub async fn bp_save_config(
    ctx: State<'_, AppCtx>,
    cfg: BpHostConfig,
) -> Result<ServerStatus, String> {
    cfg.validate()?;
    config::save(&ctx.config_path, &cfg)?;
    *ctx.config_lock.write().await = Arc::new(cfg);
    let status = ctx.server_status.lock().unwrap().clone();
    Ok(status)
}

/// 启动 HTTP + WS 服务器（地址/端口来自配置）。
#[tauri::command]
pub async fn bp_server_start(ctx: State<'_, AppCtx>) -> Result<ServerStatus, String> {
    {
        let st = ctx.server_status.lock().unwrap();
        if st.running {
            return Ok(st.clone());
        }
    }
    let cfg = ctx.config_lock.read().await.as_ref().clone();
    let bind = cfg.bind_target();
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("监听 {bind} 失败: {e}"))?;
    let svc = ctx.svc.clone();
    let cfg_for_server = Arc::new(cfg.clone());
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = net_server::serve_listener(listener, cfg_for_server, svc, shutdown_rx).await;
    });
    net_server::spawn_timer_ticker(ctx.svc.clone());
    *ctx.server_handle.lock().unwrap() = Some(ServerHandle { shutdown_tx, task });
    let status = ServerStatus {
        running: true,
        bind: cfg.bind_target(),
        room_code: cfg.resolved_room_code(),
        error: None,
    };
    *ctx.server_status.lock().unwrap() = status.clone();
    Ok(status)
}

/// 停止服务器：发优雅停机信号 → 等任务收尾（守 5s 超时，超时则任务与
/// 进程退出兜底，不永久卡住 UI）；状态按停机结果收口。
#[tauri::command]
pub async fn bp_server_stop(ctx: State<'_, AppCtx>) -> Result<ServerStatus, String> {
    let (prev_bind, prev_room) = {
        let st = ctx.server_status.lock().unwrap();
        (st.bind.clone(), st.room_code.clone())
    };
    let handle = ctx.server_handle.lock().unwrap().take();
    let status = match handle {
        None => ServerStatus {
            running: false,
            bind: prev_bind,
            room_code: prev_room,
            error: Some("服务器当前未在监听".into()),
        },
        Some(h) => {
            // 接收端一定存活（serve_listener 持有 rx 至退出）——send 结果忽略即可
            let _ = h.shutdown_tx.send(());
            match tokio::time::timeout(std::time::Duration::from_secs(5), h.task).await {
                Ok(Ok(_)) => ServerStatus {
                    running: false,
                    bind: prev_bind,
                    room_code: prev_room,
                    error: None,
                },
                Ok(Err(_)) => ServerStatus {
                    running: false,
                    bind: prev_bind,
                    room_code: prev_room,
                    error: Some("服务任务提前退出（视为已停止）".into()),
                },
                Err(_) => ServerStatus {
                    running: false,
                    bind: prev_bind,
                    room_code: prev_room,
                    error: Some("停机超时：任务已分离，重启进程可彻底回收".into()),
                },
            }
        }
    };
    *ctx.server_status.lock().unwrap() = status.clone();
    Ok(status)
}

/// BP 结束后的结构化结果（分享卡导出 / 复制链接 数据源）。
#[tauri::command]
pub async fn bp_get_result(ctx: State<'_, AppCtx>) -> Result<BpResult, String> {
    let state = ctx.svc.state.lock().await;
    if state.status == crate::model::BpStatus::Finished {
        Ok(state.build_result(crate::service::now_ms()))
    } else {
        Err("BP 尚未结束".into())
    }
}

/// 供二维码/帮助面板使用的提示（房间码 + 建议的局域网访问方式）。
#[tauri::command]
pub async fn bp_join_hint(ctx: State<'_, AppCtx>) -> Result<serde_json::Value, String> {
    let cfg = (*ctx.config_lock.read().await).clone();
    let room = cfg.resolved_room_code();
    let svc_room = ctx.svc.room_code().await;
    Ok(serde_json::json!({
        "roomCode": svc_room,
        "configRoomCode": room,
        "protocolVersion": PROTOCOL_VERSION,
        "publicHint": cfg.public_hint_url,
        "howTo": "客户端连入同一 Wi-Fi 后，浏览器访问 http://主机IP:端口，输入房间码加入。",
    }))
}

/// 处理中被控制台触发的本地裁定也走同一意图通道；这里提供幂等的「当前操作序号」辅助。
#[tauri::command]
pub async fn bp_last_order_no(ctx: State<'_, AppCtx>) -> Result<u32, String> {
    let state = ctx.svc.state.lock().await;
    Ok(state.order_counter)
}

/// 事件桥：把 service 广播的所有事件推给前端（本地控制台的主接收通道）。
/// 客户端出现重复事件时由前端以 `bp-event` 为唯一广播通道去重设计即可。
pub fn spawn_event_bridge(app: AppHandle, ctx_svc: Arc<BpService>) {
    tauri::async_runtime::spawn(async move {
        let mut rx = ctx_svc.events.subscribe();
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if app.emit("bp-event", &ev).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

// ---------------------------------------------------------------- 启动

/// Tauri 应用入口（main.rs 调用）。
pub fn run_tauri() {
    let config_path = config::default_config_path();
    let (cfg, warn) = config::load_or_default(&config_path);
    if let Some(w) = warn {
        eprintln!("[bp-host] 配置读取提示：{w}（已用内置默认值；启动后可在 UI 保存）");
    }
    let db_path = resolve_db_path(&config_path, &cfg);
    let store = match SqliteStore::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[bp-host] SQLite 初始化失败：{e}");
            return;
        }
    };
    let svc = match BpService::new(cfg.clone(), Arc::new(store)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[bp-host] 服务初始化失败：{e}");
            return;
        }
    };
    let room = cfg.resolved_room_code();
    tauri::Builder::default()
        .manage(AppCtx {
            svc: svc.clone(),
            config_path,
            config_lock: RwLock::new(Arc::new(cfg)),
            server_status: std::sync::Mutex::new(ServerStatus {
                running: false,
                bind: String::new(),
                room_code: room,
                error: None,
            }),
            server_handle: std::sync::Mutex::new(None),
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            {
                let svc = svc.clone();
                tauri::async_runtime::spawn(async move {
                    // 本地控制台连接惰性创建发生在首个意图；此处预先占位以便心跳可见
                    svc.ensure_local_conn().await;
                });
            }
            spawn_event_bridge(handle, svc.clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bp_send_intent,
            bp_get_snapshot,
            bp_get_config,
            bp_save_config,
            bp_server_start,
            bp_server_stop,
            bp_get_result,
            bp_join_hint,
            bp_last_order_no,
        ])
        .run(tauri::generate_context!())
        .expect("error while running bp-host tauri app");
}

fn resolve_db_path(config_path: &std::path::Path, cfg: &BpHostConfig) -> PathBuf {
    if let Some(p) = cfg.db_path.as_ref() {
        return PathBuf::from(p);
    }
    if let Some(dir) = cfg.data_dir.as_ref() {
        return PathBuf::from(dir).join("bp-host.db");
    }
    config_path
        .parent()
        .map(|p| p.join("bp-host.db"))
        .unwrap_or_else(|| PathBuf::from("bp-host.db"))
}
