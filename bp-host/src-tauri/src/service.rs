//! service.rs —— 主机服务层：本地 invoke 与远程 WS 的**唯一共享执行体**。
//!
//! 两条入口只是传输不同（Tauri invoke 直调 / axum WS），业务编排只有一份：
//!
//! ```text
//! ClientIntent ──┐
//!                ├─→ BpService::process_intent ─→ (直接回执, 广播事件, SQLite 落库)
//! HostCommand ───┘
//! ```
//!
//! 关键设计（对应 handoff 第 3 节第 4 条）：
//! - 所有状态变更在**同一把锁内**完成：校验 → 变更 → 落库 → 组装事件；
//! - 先落库再广播（store-then-broadcast）：崩溃后事件时间线完整；
//! - 非法意图**一律返回错误事件**（OperationRejected / InterferenceDenied / Error），
//!   绝不静默忽略——硬约束在本文件是主机侧的唯一落点；
//! - 干扰点数 / 回合计时 / 曲池状态全为后端权威，客户端不乐观推进。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::json;
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::bp::interference::InterferenceKind;
use crate::bp::state_machine::{BpState, ResumeTimerMode};
use crate::config::{generate_token, time_seed, xorshift64, BpHostConfig, TimeoutPickBehavior};
use crate::model::{BpAction, BpStatus, Side};
use crate::net::protocol::{
    BpSnapshot, ChartStatus, ClientIntent, ClientRole, HostCommand, HostEvent, PlayerId, PlayerState,
    TimerState, PROTOCOL_VERSION,
};
use crate::persistence::sqlite::SqliteStore;

/// 本地 invoke 使用的固定连接 ID（与 WS 连接共存于同一会话表）。
pub const LOCAL_CONN_ID: u64 = 0;

/// 主机时钟（毫秒）。计时一律以主机为准（硬约束）。
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ------------------------------------------------------------------ 会话表

/// 已认证会话的身份（绑定 player）。WS 与本地 invoke 一致。
#[derive(Clone, Debug)]
pub struct AuthIdentity {
    pub player_id: PlayerId,
    pub token: String,
    pub role: ClientRole,
    pub side: Option<Side>,
    pub name: String,
}

/// 表情限频令牌桶（handoff 第 4 节：`emote_per_sec` 在会话层实现）。
pub struct EmoteBucket {
    tokens: f32,
    last_ms: u64,
    rate_per_sec: f32,
}

impl EmoteBucket {
    pub fn new(rate_per_sec: u8, now: u64) -> Self {
        let rate = rate_per_sec.max(1) as f32;
        Self { tokens: rate, last_ms: now, rate_per_sec: rate }
    }
    /// 消耗 1 个额度；false = 限频拒绝。
    pub fn try_consume(&mut self, now: u64) -> bool {
        let dt = now.saturating_sub(self.last_ms) as f32 / 1000.0;
        self.last_ms = now;
        self.tokens = (self.tokens + dt * self.rate_per_sec).min(self.rate_per_sec);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// 一个连接的登记项（未认证也占位）。
pub struct ConnInfo {
    pub conn_id: u64,
    pub addr: String,
    pub auth: Option<AuthIdentity>,
    pub emote: EmoteBucket,
    pub last_seen_ms: u64,
}

/// 关闭原因（时间线记录用）。
#[derive(Clone, Copy, Debug)]
pub enum CloseReason {
    /// 正常断线 / 主动离开
    Leave,
    /// 主办方踢出
    Kicked,
}

/// 连接句柄：WS 循环以 `kick_rx` 监听踢出信号。
pub struct ConnHandle {
    pub conn_id: u64,
    pub kick_rx: mpsc::UnboundedReceiver<u8>,
}

#[derive(Default)]
struct SessionTable {
    conns: HashMap<u64, ConnInfo>,
    closers: HashMap<u64, mpsc::UnboundedSender<u8>>,
}

// ------------------------------------------------------------------ 服务本体

pub struct BpService {
    pub config: Arc<BpHostConfig>,
    /// 权威状态。一把锁内「校验 → 变更 → 落库 → 组事件」。
    pub state: Mutex<BpState>,
    /// 全局事件广播（WS 订阅 / 本地桥接订阅）。
    pub events: broadcast::Sender<HostEvent>,
    sessions: Mutex<SessionTable>,
    pub store: Arc<SqliteStore>,
}

impl BpService {
    /// 构造：生成 / 恢复房间。同步。
    pub fn new(config: BpHostConfig, store: Arc<SqliteStore>) -> Result<Arc<Self>, String> {
        config.validate()?;
        let room_code = config.resolved_room_code();
        let state = Self::build_initial_state(&config, &room_code, &store)?;
        Ok(Arc::new(Self {
            config: Arc::new(config),
            state: Mutex::new(state),
            events: broadcast::channel(1024).0,
            sessions: Mutex::new(SessionTable::default()),
            store,
        }))
    }

    /// 初始状态 = 崩溃恢复（同房间码未完结）或全新 Lobby。
    fn build_initial_state(
        config: &BpHostConfig,
        room_code: &str,
        store: &SqliteStore,
    ) -> Result<BpState, String> {
        if config.auto_recover {
            if let Some(mut state) = store.load_unfinished(room_code)? {
                // 恢复修补：所有连接标记离线（真实连接随重连恢复）。
                for p in state.players.iter_mut() {
                    p.connected = false;
                }
                // 倒计时恢复策略（配置）：
                // - Continue（默认）：deadline 原样保留（绝对时间戳语义：错过的时间照算，
                //   已过期则 tick 立刻执行超时行为，主办方可用裁定回退/暂停介入）。
                // - Reset：进行中回合重新计时（满时长）。
                if state.status == BpStatus::InProgress && config.resume_timer_reset {
                    state.timer = Some(TimerState {
                        deadline_ts_ms: now_ms() + config.turn_timeout_ms,
                        duration_ms: config.turn_timeout_ms,
                    });
                    state.paused_remaining_ms = None;
                }
                return Ok(state);
            }
        }
        BpState::with_resume_mode(
            room_code.to_string(),
            &config.sequence,
            config.first_side,
            config.interference.clone(),
            Vec::new(), // 曲池由主办方 SetPool 导入
            config.turn_timeout_ms,
            if config.resume_timer_reset {
                ResumeTimerMode::Reset
            } else {
                ResumeTimerMode::Continue
            },
        )
        .map_err(|e| format!("回合序列非法: {e}"))
    }

    pub async fn room_code(&self) -> String {
        self.state.lock().await.room_code.clone()
    }

    /// 为连接开登记（返回 kick 供 WS 循环 select）。
    pub async fn open_conn(&self, addr: String) -> ConnHandle {
        let mut table = self.sessions.lock().await;
        let mut seed = time_seed();
        let conn_id = xorshift64(&mut seed) | 1; // 非 0（0 = 本地 invoke 保留）
        let conn_id = if table.conns.contains_key(&conn_id) {
            (table.conns.len() as u64).max(1) + 1
        } else {
            conn_id
        };
        let now = now_ms();
        let (tx, rx) = mpsc::unbounded_channel();
        table.closers.insert(conn_id, tx);
        table.conns.insert(
            conn_id,
            ConnInfo {
                conn_id,
                addr,
                auth: None,
                emote: EmoteBucket::new(self.config.interference.spectator.emote_per_sec, now),
                last_seen_ms: now,
            },
        );
        ConnHandle { conn_id, kick_rx: rx }
    }

    /// 本地 invoke 连接惰性登记（不占用 open_conn 的随机 id 空间）。
    pub async fn ensure_local_conn(&self) {
        let mut table = self.sessions.lock().await;
        if !table.conns.contains_key(&LOCAL_CONN_ID) {
            let now = now_ms();
            table.conns.insert(
                LOCAL_CONN_ID,
                ConnInfo {
                    conn_id: LOCAL_CONN_ID,
                    addr: "local".into(),
                    auth: None,
                    emote: EmoteBucket::new(self.config.interference.spectator.emote_per_sec, now),
                    last_seen_ms: now,
                },
            );
        }
    }

    /// 关闭连接：标记断线 + 广播 + 时间线。
    ///
    /// 同一身份存在其他存活连接时不误报断线（多设备临时补线场景）。
    pub async fn close_conn(&self, conn_id: u64, reason: CloseReason) {
        let auth = {
            let mut table = self.sessions.lock().await;
            table.closers.remove(&conn_id);
            table.conns.remove(&conn_id).and_then(|c| c.auth)
        };
        let Some(auth) = auth else { return };
        let at = now_ms();

        // 该身份是否仍有别的存活连接
        let still_alive = {
            let table = self.sessions.lock().await;
            table.conns.values().any(|c| {
                c.auth.as_ref().map(|a| a.player_id) == Some(auth.player_id)
            })
        };
        if still_alive {
            return;
        }

        let notice = {
            let mut state = self.state.lock().await;
            let mut notice = None;
            if let Some(p) = state.players.iter_mut().find(|p| p.id == auth.player_id) {
                if p.connected {
                    p.connected = false;
                    notice = Some(match reason {
                        CloseReason::Leave => format!("{} 与主机断开连接", p.name),
                        CloseReason::Kicked => format!("{} 已被主办方移出", p.name),
                    });
                }
            }
            notice
        };
        let Some(notice) = notice else { return };

        let evs = vec![
            HostEvent::PlayerDisconnected { player_id: auth.player_id, at_ms: at },
            HostEvent::Notice { message: notice },
        ];
        let room = self.state.lock().await.room_code.clone();
        if let Err(e) = self.store.save_state(&*self.state.lock().await) {
            eprintln!("[bp-host] save_state(disconnect) 失败: {e}");
        }
        let _ = self.store.append_connection(
            &room,
            auth.player_id,
            &auth.name,
            role_tag(auth.role),
            auth.side.map(side_tag),
            false,
            at,
        );
        self.record_events(&room, &evs, at);
        self.broadcast(evs);
    }

    // ------------------------------------------------------------- 意图处理

    /// 处理一条意图。返回**直接回执**（发给该连接）；广播事件走 `events`。
    /// 非法输入必然得到错误事件（硬约束），绝不静默。
    pub async fn process_intent(
        self: &Arc<Self>,
        conn_id: u64,
        intent: ClientIntent,
    ) -> Vec<HostEvent> {
        if conn_id == LOCAL_CONN_ID {
            self.ensure_local_conn().await;
        }
        self.touch_conn(conn_id).await;
        match intent {
            ClientIntent::Ping { at } => vec![HostEvent::Pong { at }],
            ClientIntent::SendEmote { emote, text } => self.handle_emote(conn_id, emote, text).await,
            ClientIntent::UseInterference { kind: InterferenceKind::Emote, .. } => vec![err(
                "EMOTE_NEEDS_SEND_EMOTE",
                "表情请通过 SendEmote 发送；UseInterference 仅承载策略干扰",
            )],
            ClientIntent::Join { room_code, player_name, role, side, token } => {
                self.handle_join(conn_id, room_code, player_name, role, side, token).await
            }
            other => self.execute(conn_id, other).await,
        }
    }

    async fn touch_conn(&self, conn_id: u64) {
        let now = now_ms();
        if let Some(c) = self.sessions.lock().await.conns.get_mut(&conn_id) {
            c.last_seen_ms = now;
        }
    }

    async fn auth_of(&self, conn_id: u64) -> Option<AuthIdentity> {
        self.sessions
            .lock()
            .await
            .conns
            .get(&conn_id)
            .and_then(|c| c.auth.clone())
    }

    /// 表情（轻量社交层）：限频 → 广播 → 时间线。
    /// **不进 interference_log**：导出/分享卡的「干扰摘要」只统计策略干扰。
    async fn handle_emote(&self, conn_id: u64, emote: String, text: Option<String>) -> Vec<HostEvent> {
        let Some(auth) = self.auth_of(conn_id).await else {
            return vec![err("JOIN_REQUIRED", "请先加入房间")];
        };
        let allow = {
            let mut table = self.sessions.lock().await;
            match table.conns.get_mut(&conn_id) {
                Some(c) => c.emote.try_consume(now_ms()),
                None => false,
            }
        };
        if !allow {
            return vec![err("EMOTE_RATE_LIMITED", "表情发送过快，请稍候")];
        }
        if auth.role == ClientRole::Spectator && !self.config.interference.spectator.can_emote {
            return vec![err("SPECTATOR_EMOTE_DISABLED", "本场已限制观众弹幕")];
        }
        let at = now_ms();
        let ev = HostEvent::Emote {
            from_side: auth.side,
            from_player: auth.player_id,
            from_name: auth.name.clone(),
            emote,
            text,
        };
        self.record_events(&self.state.lock().await.room_code, std::slice::from_ref(&ev), at);
        self.broadcast(vec![ev]);
        Vec::new()
    }

    /// 加入 / 重连。直接回执 = Welcome + Snapshot；广播 PlayerConnected。
    async fn handle_join(
        self: &Arc<Self>,
        conn_id: u64,
        room_code: String,
        player_name: String,
        role: ClientRole,
        side: Option<Side>,
        token: Option<String>,
    ) -> Vec<HostEvent> {
        let at = now_ms();
        let name = player_name.trim().to_string();
        let name = if name.is_empty() {
            format!("选手{}", if role == ClientRole::Player { "" } else { "/访客" })
        } else {
            // 防超长与控制字符
            name.chars().take(16).collect()
        };

        // 1) 房间码核对（不广播，静默方一致：只回错误事件）
        let state_room = self.state.lock().await.room_code.clone();
        if room_code != state_room {
            return vec![err("ROOM_CODE_MISMATCH", "房间码不正确")];
        }

        let mut state = self.state.lock().await;

        // 2) token 识别 → 断线重连恢复同一 player_id（防冒名）
        if let Some(tok) = token.as_deref() {
            if let Some(pid) = state
                .tokens
                .iter()
                .find(|(_, v)| v.as_str() == tok)
                .map(|(k, _)| *k)
            {
                let Some(p) = state.players.iter_mut().find(|p| p.id == pid) else {
                    return vec![err("IDENTITY_LOST", "身份记录失效，请重新加入")];
                };
                p.connected = true;
                let player = p.clone(); // 借用最后一次使用；之后仅走不可变借用
                let welcome = HostEvent::Welcome {
                    you: pid,
                    role: player.role,
                    room_code: state.room_code.clone(),
                    protocol_version: PROTOCOL_VERSION,
                    token: Some(tok.to_string()),
                };
                let connected_ev = HostEvent::PlayerConnected(player.clone());
                let snapshot = state.build_snapshot();
                let auth = AuthIdentity {
                    player_id: pid,
                    token: tok.to_string(),
                    role: player.role,
                    side: player.side,
                    name: player.name.clone(),
                };

                self.bind_auth(conn_id, auth).await;
                return self
                    .finish_join(at, vec![welcome, HostEvent::Snapshot(snapshot)], connected_ev)
                    .await;
            }
        }

        // 3) 全新加入
        if state.locked {
            return vec![err("ROOM_LOCKED", "房间已锁定，暂不接受新成员")];
        }
        let host_taken = state.players.iter().any(|p| p.role == ClientRole::Host);
        let a_taken = state.players.iter().any(|p| p.side == Some(Side::A));
        let b_taken = state.players.iter().any(|p| p.side == Some(Side::B));

        let can_fresh = match state.status {
            BpStatus::Lobby => true,
            // 赛中不接待参赛者新席位（防乱入）；主办方与观众允许
            BpStatus::InProgress | BpStatus::Paused => {
                matches!(role, ClientRole::Spectator | ClientRole::Host)
            }
            BpStatus::Finished => true,
        };
        if !can_fresh {
            return vec![err("BP_ALREADY_RUNNING", "BP 已开始，不再接受参赛者加入")];
        }
        if role == ClientRole::Host && host_taken {
            return vec![err("HOST_SEAT_TAKEN", "主办方席位已存在")];
        }
        // 参赛席位（请求侧被占 → 拒；未指定则自动分配空侧）
        let side = match role {
            ClientRole::Player => match side {
                Some(Side::A) if a_taken => None,
                Some(Side::B) if b_taken => None,
                Some(s) => Some(s),
                None => {
                    if !a_taken {
                        Some(Side::A)
                    } else if !b_taken {
                        Some(Side::B)
                    } else {
                        None
                    }
                }
            },
            _ => None,
        };
        if role == ClientRole::Player && side.is_none() {
            return vec![err("SIDE_OCCUPIED", "A/B 选手席位已满")];
        }

        let pid = state.players.iter().map(|p| p.id).max().unwrap_or(0) + 1;
        let started = state.status != BpStatus::Lobby;
        let player = PlayerState {
            id: pid,
            name: name.clone(),
            role,
            side,
            ready: false,
            connected: true,
            // 赛中补位直接持满初始点数；Lobby 阶段由 start() 统一初始化
            interference_points: if started { state.interference.initial_points } else { 0 },
            interference_disabled: false,
        };
        let token_new = token.clone().unwrap_or_else(generate_token);
        state.tokens.insert(pid, token_new.clone());
        state.players.push(player.clone());

        let welcome = HostEvent::Welcome {
            you: pid,
            role,
            room_code: state.room_code.clone(),
            protocol_version: PROTOCOL_VERSION,
            token: Some(token_new.clone()),
        };
        let connected_ev = HostEvent::PlayerConnected(player.clone());
        let snapshot = state.build_snapshot();
        let auth = AuthIdentity { player_id: pid, token: token_new, role, side, name };

        drop(state);
        self.bind_auth(conn_id, auth).await;
        self.finish_join(at, vec![welcome, HostEvent::Snapshot(snapshot)], connected_ev).await
    }

    async fn bind_auth(&self, conn_id: u64, auth: AuthIdentity) {
        let mut table = self.sessions.lock().await;
        if let Some(c) = table.conns.get_mut(&conn_id) {
            c.auth = Some(auth);
        }
    }

    /// Join 尾部：落库 + 审计 + 广播加人事件。
    async fn finish_join(self: &Arc<Self>, at: u64, direct: Vec<HostEvent>, connected_ev: HostEvent) -> Vec<HostEvent> {
        let state = self.state.lock().await;
        if let Err(e) = self.store.save_state(&state) {
            eprintln!("[bp-host] save_state(join) 失败: {e}");
        }
        drop(state);
        self.record_events(&self.state.lock().await.room_code, std::slice::from_ref(&connected_ev), at);
        self.broadcast(vec![connected_ev]);
        direct
    }

    // ------------------------------------------------------------ 已认证意图

    /// 已认证主体执行除 Join / Ping / Emote 外的意图。
    async fn execute(self: &Arc<Self>, conn_id: u64, intent: ClientIntent) -> Vec<HostEvent> {
        let Some(auth) = self.auth_of(conn_id).await else {
            return vec![err("JOIN_REQUIRED", "请先加入房间（Join）")];
        };
        let at = now_ms();
        let mut state = self.state.lock().await;

        let Some(sender) = state.players.iter().find(|p| p.id == auth.player_id).cloned() else {
            return vec![err("IDENTITY_LOST", "会话与主机状态不同步，请重新加入")];
        };
        // 1) 粗校验（状态机 legal_intent：各状态的允许意图面）
        if let Err(rej) = state.legal_intent(&sender, &intent) {
            return vec![err("INTENT_REJECTED", &rej.0)];
        }

        // 2) 细路径
        match intent {
            ClientIntent::RequestSnapshot => vec![HostEvent::Snapshot(state.build_snapshot())],

            ClientIntent::Leave => {

                self.close_conn(conn_id, CloseReason::Leave).await;
                Vec::new()
            }

            ClientIntent::SetReady { ready } => {
                let mut evs = Vec::new();
                if let Some(p) = state.players.iter_mut().find(|p| p.id == sender.id) {
                    p.ready = ready;
                    evs.push(HostEvent::PlayerReady { player_id: p.id, ready });
                    evs.push(HostEvent::Notice {
                        message: format!("{} {}", p.name, if ready { "已就绪" } else { "取消就绪" }),
                    });
                }
                if let Err(e) = self.store.save_state(&state) {
                    eprintln!("[bp-host] save_state(ready) 失败: {e}");
                }

                self.record_events(&self.state.lock().await.room_code, &evs, at);
                self.broadcast(evs);
                Vec::new()
            }

            ClientIntent::SetRole { role, side } => {
                match set_role_in_state(&mut state, sender.id, role, side) {
                    Ok(msg) => {
                        let snapshot = state.build_snapshot();
                        let evs = vec![
                            HostEvent::Snapshot(snapshot),
                            HostEvent::Notice { message: msg },
                        ];
                        if let Err(e) = self.store.save_state(&state) {
                            eprintln!("[bp-host] save_state(role) 失败: {e}");
                        }

                        self.record_events(&self.state.lock().await.room_code, &evs, at);
                        self.broadcast(evs);
                        Vec::new()
                    }
                    Err(e) => vec![err("ROLE_CHANGE_REJECTED", &e)],
                }
            }

            ClientIntent::Ban { chart_id } => {
                self.confirm_operation(&mut state, &sender, BpAction::Ban, chart_id, at).await
            }
            ClientIntent::Pick { chart_id } => {
                self.confirm_operation(&mut state, &sender, BpAction::Pick, chart_id, at).await
            }

            ClientIntent::RevealCandidate { chart_id } => {
                let Some(side) = sender.side else {
                    return vec![err("NO_SIDE", "参赛者身份缺失侧别")];
                };
                match state.reveal_candidate(side, chart_id) {
                    Ok(ev) => {
                        let evs = vec![
                            HostEvent::InterferenceApplied(ev.clone()),
                            HostEvent::Notice {
                                message: format!("候选意向已公开（事件 #{})", ev.id),
                            },
                        ];
                        if let Err(e) = self.store.save_state(&state) {
                            eprintln!("[bp-host] save_state(reveal) 失败: {e}");
                        }

                        self.record_events(&self.state.lock().await.room_code, &evs, at);
                        self.broadcast(evs);
                        Vec::new()
                    }
                    Err(e) => vec![err("REVEAL_REJECTED", &e.0)],
                }
            }

            ClientIntent::UseInterference { kind, target_side, obscure_chart_ids, forced_ban_chart_id } => {
                if sender.role != ClientRole::Player {
                    return vec![err("NOT_A_PLAYER", "策略干扰仅参赛者可发起")];
                }
                let Some(from_side) = sender.side else {
                    return vec![err("NO_SIDE", "参赛者身份缺失侧别")];
                };
                match state.try_interference(
                    from_side,
                    target_side,
                    kind,
                    obscure_chart_ids.unwrap_or_default(),
                    forced_ban_chart_id,
                    at,
                ) {
                    Ok(ev) => {
                        let mut evs = vec![HostEvent::InterferenceApplied(ev.clone())];
                        // 追加 Ban 压力 / 压缩都会改渲染与计时：一并广播
                        evs.push(HostEvent::PoolUpdated { pool: state.pool.clone() });
                        if let Some(t) = state.timer.as_ref() {
                            evs.push(HostEvent::TimerSync {
                                deadline_ts_ms: t.deadline_ts_ms,
                                duration_ms: t.duration_ms,
                            });
                        }
                        evs.push(HostEvent::Notice {
                            message: format!(
                                "{} 对 {} 使用了「{}」",
                                side_tag(from_side),
                                side_tag(target_side),
                                kind.name()
                            ),
                        });
                        if let Err(e) = self.store.save_state(&state) {
                            eprintln!("[bp-host] save_state(interference) 失败: {e}");
                        }

                        self.record_events(&self.state.lock().await.room_code, &evs, at);
                        self.broadcast(evs);
                        Vec::new()
                    }
                    Err(deny) => vec![HostEvent::InterferenceDenied {
                        kind,
                        reason: deny.message().to_string(),
                    }],
                }
            }

            ClientIntent::CounterInterference { interference_id } => {
                let Some(side) = sender.side else {
                    return vec![err("NO_SIDE", "参赛者身份缺失侧别")];
                };
                match state.counter_interference(side, interference_id) {
                    Ok(ev) => {
                        let evs = vec![
                            HostEvent::CounterApplied { interference_id: ev.id, by_side: side },
                            HostEvent::Notice {
                                message: format!(
                                    "{} 反制了干扰 #{})",
                                    side_tag(side),
                                    ev.id
                                ),
                            },
                        ];
                        if let Err(e) = self.store.save_state(&state) {
                            eprintln!("[bp-host] save_state(counter) 失败: {e}");
                        }

                        self.record_events(&self.state.lock().await.room_code, &evs, at);
                        self.broadcast(evs);
                        Vec::new()
                    }
                    Err(e) => vec![err("COUNTER_REJECTED", &e.0)],
                }
            }

            ClientIntent::HostCommand { host_command: cmd } => {
                self.host_command(&mut state, cmd, at).await
            }

            // 其余意图在上层已截走；到达即不该存在
            ClientIntent::Join { .. } | ClientIntent::Ping { .. } | ClientIntent::SendEmote { .. } => {
                Vec::new()
            }
        }
    }

    // ------------------------------------------------------------ Ban/Pick

    async fn confirm_operation(
        self: &Arc<Self>,
        state: &mut BpState,
        sender: &PlayerState,
        action: BpAction,
        chart_id: u32,
        at: u64,
    ) -> Vec<HostEvent> {
        let Some(side) = sender.side else {
            return vec![err("NO_SIDE", "参赛者身份缺失侧别")];
        };
        match state.apply_operation(side, action, chart_id, at) {
            Ok(op) => {
                let mut evs = vec![HostEvent::OperationConfirmed(op.clone())];
                evs.extend(phase_events(state, at));
                let _ = self.store.save_state(state);
                let room = state.room_code.clone();

                self.record_events(&room, &evs, at);
                self.broadcast(evs);
                Vec::new()
            }
            Err(e) => vec![HostEvent::OperationRejected {
                reason: e.message().to_string(),
                code: e.code().to_string(),
            }],
        }
    }

    // ------------------------------------------------------------ 主办方命令

    async fn host_command(
        self: &Arc<Self>,
        state: &mut BpState,
        cmd: HostCommand,
        at: u64,
    ) -> Vec<HostEvent> {
        // Host 专属已由 legal_intent 兜住，这里只做执行与结果回报
        match cmd {
            HostCommand::StartBp => {
                let a_missing = state.player_by_side(Side::A).is_none();
                let b_missing = state.player_by_side(Side::B).is_none();
                let a_ready = state.player_by_side(Side::A).map(|p| p.ready).unwrap_or(false);
                let b_ready = state.player_by_side(Side::B).map(|p| p.ready).unwrap_or(false);
                if a_missing || b_missing || !a_ready || !b_ready {
                    let mut why = Vec::new();
                    if a_missing { why.push("A 缺席"); } else if !a_ready { why.push("A 未就绪"); }
                    if b_missing { why.push("B 缺席"); } else if !b_ready { why.push("B 未就绪"); }
                    let msg = if why.is_empty() {
                        "无法开始".to_string()
                    } else {
                        format!("双方需就位并就绪：{}", why.join("、"))
                    };
                    return vec![HostEvent::HostActionResult { ok: false, message: msg }];
                }
                // start() 内含已知坑修复：按 initial_points 初始化双方干扰点
                state.start(at);
                if state.status != BpStatus::InProgress {
                    return vec![HostEvent::HostActionResult {
                        ok: false,
                        message: "回合序列为空，无法开始".into(),
                    }];
                }
                let snap = state.build_snapshot();
                let mut evs = vec![
                    HostEvent::Snapshot(snap),
                    HostEvent::Notice { message: "BP 开始".into() },
                ];
                evs.extend(phase_events(state, at));
                evs.push(HostEvent::HostActionResult { ok: true, message: "BP 开始".into() });
                let room = state.room_code.clone();
                let _ = self.store.save_state(state);

                self.record_events(&room, &evs, at);
                self.broadcast(evs);
                Vec::new()
            }

            HostCommand::LockRoom { locked } => {
                state.locked = locked;
                let msg = if locked { "房间已锁定" } else { "房间已解锁" };
                let snap = state.build_snapshot();
                let evs = vec![
                    HostEvent::Snapshot(snap),
                    HostEvent::Notice { message: msg.into() },
                    HostEvent::HostActionResult { ok: true, message: msg.into() },
                ];
                let room = state.room_code.clone();

                self.record_events(&room, &evs, at);
                self.broadcast(evs);
                Vec::new()
            }

            HostCommand::Pause => {
                state.pause(at);
                let snap = state.build_snapshot(); // timer=None、status=Paused
                let evs = vec![
                    HostEvent::Snapshot(snap),
                    HostEvent::Notice { message: "BP 已暂停（停表不断线）".into() },
                    HostEvent::HostActionResult { ok: true, message: "已暂停".into() },
                ];
                let room = state.room_code.clone();
                let _ = self.store.save_state(state);

                self.record_events(&room, &evs, at);
                self.broadcast(evs);
                Vec::new()
            }

            HostCommand::Resume => {
                state.resume(at);
                let mut evs = vec![HostEvent::Notice { message: "BP 已恢复".into() }];
                evs.extend(phase_events(state, at));
                evs.push(HostEvent::HostActionResult { ok: true, message: "已恢复".into() });
                let room = state.room_code.clone();
                let _ = self.store.save_state(state);

                self.record_events(&room, &evs, at);
                self.broadcast(evs);
                Vec::new()
            }

            HostCommand::ArbitrateRollback { operation_order_no } => {
                match state.arbitrate_rollback(operation_order_no, at) {
                    Ok(removed) => {
                        // removed 为时间序、目标在首；级联条数在公告中明示（仅 M>0 时）
                        let target_no = removed[0].order_no;
                        let extra = removed.len() - 1;
                        let notice = if extra > 0 {
                            format!("主办方回退了操作 #{target_no}（含其后 {extra} 条）")
                        } else {
                            format!("主办方回退了操作 #{target_no}")
                        };
                        let snap = state.build_snapshot();
                        let mut evs = vec![
                            HostEvent::Snapshot(snap),
                            HostEvent::Notice {
                                message: notice,
                            },
                        ];
                        evs.extend(phase_events(state, at));
                        evs.push(HostEvent::HostActionResult { ok: true, message: "已回退".into() });
                        let room = state.room_code.clone();
                        let _ = self.store.save_state(state);

                        self.record_events(&room, &evs, at);
                        self.broadcast(evs);
                        Vec::new()
                    }
                    Err(e) => vec![HostEvent::HostActionResult { ok: false, message: e.0 }],
                }
            }

            HostCommand::DisableInterference { side, disabled } => {
                let found = state.players.iter_mut().find(|p| p.side == Some(side));
                match found {
                    Some(p) => {
                        p.interference_disabled = disabled;
                        let snap = state.build_snapshot();
                        let evs = vec![
                            HostEvent::Snapshot(snap),
                            HostEvent::Notice {
                                message: format!(
                                    "{} 干扰权已{}",
                                    side_tag(side),
                                    if disabled { "禁用" } else { "恢复" }
                                ),
                            },
                            HostEvent::HostActionResult { ok: true, message: "已更新".into() },
                        ];
                        let room = state.room_code.clone();
                        let _ = self.store.save_state(state);

                        self.record_events(&room, &evs, at);
                        self.broadcast(evs);
                        Vec::new()
                    }
                    None => vec![HostEvent::HostActionResult {
                        ok: false,
                        message: "该侧未找到选手".into(),
                    }],
                }
            }

            HostCommand::RollbackInterference { interference_id } => {
                match state.rollback_interference(interference_id) {
                    Ok(ev) => {
                        let snap = state.build_snapshot();
                        let mut evs = vec![
                            HostEvent::InterferenceApplied(ev.clone()),
                            HostEvent::Snapshot(snap),
                            HostEvent::Notice {
                                message: format!(
                                    "主办方回滚了干扰 #{}（{}）",
                                    ev.id,
                                    ev.kind.name()
                                ),
                            },
                        ];
                        if let Some(t) = state.timer.as_ref() {
                            evs.push(HostEvent::TimerSync {
                                deadline_ts_ms: t.deadline_ts_ms,
                                duration_ms: t.duration_ms,
                            });
                        }
                        evs.push(HostEvent::HostActionResult { ok: true, message: "已回滚".into() });
                        let room = state.room_code.clone();
                        let _ = self.store.save_state(state);

                        self.record_events(&room, &evs, at);
                        self.broadcast(evs);
                        Vec::new()
                    }
                    Err(e) => vec![HostEvent::HostActionResult { ok: false, message: e.0 }],
                }
            }

            HostCommand::Kick { player_id } => {

                self.kick_player(player_id).await
            }

            HostCommand::SetPool { pool } => match state.status {
                BpStatus::Lobby => {
                    let mut charts = pool;
                    for c in charts.iter_mut() {
                        c.status = ChartStatus::Available;
                    }
                    state.pool = charts;
                    let evs = vec![
                        HostEvent::PoolUpdated { pool: state.pool.clone() },
                        HostEvent::Notice {
                            message: format!("曲池已更新（{} 首）", state.pool.len()),
                        },
                        HostEvent::HostActionResult { ok: true, message: "曲池已更新".into() },
                    ];
                    let room = state.room_code.clone();
                    let _ = self.store.save_state(state);

                    self.record_events(&room, &evs, at);
                    self.broadcast(evs);
                    Vec::new()
                }
                _ => vec![HostEvent::HostActionResult {
                    ok: false,
                    message: "曲池只能在 Lobby 阶段更换".into(),
                }],
            },

            HostCommand::SetInterferenceConfig(cfg) => {
                // 收敛配置边界（下发给全端；各玩家**已持有点数不追改**，
                // initial_points 影响 start() 与赛中补位）
                let mut sanitized = cfg.clone();
                sanitized.initial_points = sanitized.initial_points.clamp(0, 9);
                sanitized.max_strategic_per_round_per_side = sanitized
                    .max_strategic_per_round_per_side
                    .clamp(0, 9);
                sanitized.counter_cost = sanitized.counter_cost.clamp(0, 9);
                sanitized.compress.reduce_secs = sanitized.compress.reduce_secs.clamp(1, 120);
                sanitized.compress.floor_secs = sanitized.compress.floor_secs.clamp(0, 110);
                sanitized.obscure.max_count = sanitized.obscure.max_count.clamp(0, 20);
                sanitized.spectator.emote_per_sec = sanitized.spectator.emote_per_sec.clamp(1, 30);
                let snap = state.build_snapshot();
                let evs = vec![
                    HostEvent::Snapshot(snap),
                    HostEvent::Notice { message: "干扰配置已更新".into() },
                    HostEvent::HostActionResult { ok: true, message: "配置已更新".into() },
                ];
                let room = state.room_code.clone();
                let _ = self.store.save_state(state);

                self.record_events(&room, &evs, at);
                self.broadcast(evs);
                Vec::new()
            }
        }
    }

    async fn kick_player(self: &Arc<Self>, target: PlayerId) -> Vec<HostEvent> {
        let at = now_ms();
        // 1) state：标记离线、清 token（重连凭据作废）
        let name = {
            let mut state = self.state.lock().await;
            match state.players.iter_mut().find(|p| p.id == target) {
                Some(p) if p.role == ClientRole::Host => {
                    return vec![HostEvent::HostActionResult {
                        ok: false,
                        message: "不能踢出主办方".into(),
                    }];
                }
                Some(p) => {
                    let name = p.name.clone();
                    p.connected = false;
                    state.tokens.remove(&target);
                    name
                }
                None => {
                    return vec![HostEvent::HostActionResult {
                        ok: false,
                        message: "未找到该玩家".into(),
                    }]
                }
            }
        };
        // 2) sessions：让目标连接自行退出
        let closer = {
            let table = self.sessions.lock().await;
            table
                .conns
                .iter()
                .find(|(_, c)| c.auth.as_ref().map(|a| a.player_id) == Some(target))
                .map(|(id, _)| *id)
                .and_then(|id| table.closers.get(&id).cloned())
        };
        if let Some(tx) = closer {
            let _ = tx.send(1u8);
        }
        // 3) 广播 + 审计
        let evs = vec![
            HostEvent::PlayerDisconnected { player_id: target, at_ms: at },
            HostEvent::Notice { message: format!("{name} 已被主办方踢出") },
        ];
        let room = self.state.lock().await.room_code.clone();
        let _ = self.store.append_connection(&room, target, &name, "player", None, false, at);
        if let Err(e) = self.store.save_state(&*self.state.lock().await) {
            eprintln!("[bp-host] save_state(kick) 失败: {e}");
        }
        self.record_events(&room, &evs, at);
        self.broadcast(evs);
        vec![HostEvent::HostActionResult { ok: true, message: format!("已踢出 {name}") }]
    }

    // ------------------------------------------------------------ 周期任务

    /// 计时 tick（默认 250ms~1s per config）：广播 TimerSync；封顶即执行超时行为。
    pub async fn tick(&self) -> Vec<HostEvent> {
        let now = now_ms();
        let mut state = self.state.lock().await;
        if state.status != BpStatus::InProgress {
            return Vec::new();
        }
        let Some(timer) = state.timer else { return Vec::new() };
        if now < timer.deadline_ts_ms {
            return vec![HostEvent::TimerSync {
                deadline_ts_ms: timer.deadline_ts_ms,
                duration_ms: timer.duration_ms,
            }];
        }
        // 超时：Ban→空 Ban；Pick→随机 / 判对手（配置）
        let choice = if state.timeout_action() == crate::bp::state_machine::TimeoutAction::RandomPick
            && self.config.timeout_pick == TimeoutPickBehavior::RandomPick
        {
            random_available(&state, now)
        } else {
            None
        };
        let Some(op) = state.execute_timeout(now, choice) else {
            return Vec::new();
        };
        let mut evs = vec![HostEvent::OperationConfirmed(op.clone())];
        evs.push(HostEvent::Notice {
            message: match op.action {
                BpAction::Ban => "超时：记空 Ban".into(),
                BpAction::Pick => "超时：按超时裁决处理 Pick".into(),
            },
        });
        evs.extend(phase_events(&*state, now));
        let room = state.room_code.clone();
        let _ = self.store.save_state(&*state);
        drop(state);
        self.record_events(&room, &evs, now);
        evs
    }

    // ------------------------------------------------------------ 工具

    /// 广播一批事件（顺序保持）。
    pub fn broadcast(&self, evs: Vec<HostEvent>) {
        for ev in evs {
            let _ = self.events.send(ev);
        }
    }

    /// 事件时间线落库（TimerSync / Snapshot / 操作确认之外的噪声不落）。
    fn record_events(&self, room: &str, evs: &[HostEvent], at: u64) {
        for ev in evs {
            let (kind, summary, payload): (&'static str, String, serde_json::Value) = match ev {
                HostEvent::OperationConfirmed(op) => (
                    "operation",
                    format!(
                        "#{} {} {} {}",
                        op.order_no,
                        if op.action == BpAction::Ban { "BAN" } else { "PICK" },
                        op.chart_id,
                        side_tag(op.actor_side)
                    ),
                    json!(op),
                ),
                HostEvent::InterferenceApplied(e) => (
                    "interference",
                    format!("#{} {:?} → {}", e.id, e.kind, side_tag(e.target_side)),
                    json!(e),
                ),
                HostEvent::CounterApplied { interference_id, by_side } => (
                    "counter",
                    format!("反制 #{interference_id} by {}", side_tag(*by_side)),
                    json!(ev),
                ),
                HostEvent::HostActionResult { ok, message } => (
                    "host_command",
                    format!("{} {message}", if *ok { "OK" } else { "FAIL" }),
                    json!(ev),
                ),
                HostEvent::PhaseChanged { step_index, total_steps, .. } => (
                    "phase",
                    format!("step {step_index}/{total_steps}"),
                    json!(ev),
                ),
                HostEvent::PlayerConnected(_) => ("connection", "连接".into(), json!(ev)),
                HostEvent::PlayerDisconnected { player_id, .. } => (
                    "connection",
                    format!("断线 {player_id}"),
                    json!(ev),
                ),
                HostEvent::SessionEnded(_) => ("session_end", "BP 结束".into(), json!(ev)),
                HostEvent::Notice { message } => ("notice", message.clone(), json!(ev)),
                HostEvent::Emote { .. } => ("emote", "弹幕".into(), json!(ev)),
                HostEvent::Error { code, message } => (
                    "error",
                    format!("{code}: {message}"),
                    json!(ev),
                ),
                _ => continue,
            };
            if let Err(e) = self.store.append_event(room, at, kind, &summary, &payload) {
                eprintln!("[bp-host] 时间线落库失败: {e}");
            }
        }
    }
}

// ------------------------------------------------------------------ 事件生成

/// 一次操作确认 / 超时 / 回退之后的共同事件束：
/// Pool + Phase + Timer（+ 结束态）。OperationConfirmed 由调用方按需精确发出
/// （裁定回退不产生新的确认，避免误报）。
pub fn phase_events(state: &BpState, at: u64) -> Vec<HostEvent> {
    let mut evs = Vec::new();
    evs.push(HostEvent::PoolUpdated { pool: state.pool.clone() });
    let (step, total) = state.step_info();
    if let Some(t) = state.current_turn() {
        evs.push(HostEvent::PhaseChanged {
            step_index: step,
            total_steps: total,
            actor_side: t.side,
            action: t.action,
            decider: t.is_tb,
            is_tb: t.is_tb,
        });
    }
    if let Some(timer) = state.timer.as_ref() {
        evs.push(HostEvent::TimerSync {
            deadline_ts_ms: timer.deadline_ts_ms,
            duration_ms: timer.duration_ms,
        });
    }
    if state.status == BpStatus::Finished {
        evs.push(HostEvent::SessionEnded(state.build_result(at)));
    }
    evs
}

/// 角色变更（自由函数避免与字段互借冲突）。
fn set_role_in_state(
    state: &mut BpState,
    player_id: PlayerId,
    role: ClientRole,
    side: Option<Side>,
) -> Result<String, String> {
    // 先做全部只读检查，再修改
    let other_host = state
        .players
        .iter()
        .any(|p| p.role == ClientRole::Host && p.id != player_id);
    let side_taken = |s: Side| {
        state
            .players
            .iter()
            .any(|p| p.side == Some(s) && p.id != player_id)
    };
    match role {
        ClientRole::Host => {
            if other_host {
                return Err("已有另一位主办方".into());
            }
        }
        ClientRole::Player => {
            let Some(s) = side else {
                return Err("参赛者必须持有侧别 A/B".into());
            };
            if side_taken(s) {
                return Err(format!("{} 侧席位已被占用", side_tag(s)));
            }
        }
        ClientRole::Spectator => {}
    }
    let Some(p) = state.players.iter_mut().find(|p| p.id == player_id) else {
        return Err("未找到该会话".into());
    };
    p.side = match role {
        ClientRole::Player => side,
        _ => None,
    };
    p.role = role;
    Ok(format!("角色已切换（当前：{:?}）", p.role))
}

/// 从曲池随机挑一首可用曲目（曲池空 → None）。
fn random_available(state: &BpState, seed_extra: u64) -> Option<u32> {
    let ids = state.available_chart_ids();
    if ids.is_empty() {
        return None;
    }
    let mut seed = crc_like_seed(seed_extra);
    let idx = (xorshift64(&mut seed) % ids.len() as u64) as usize;
    Some(ids[idx])
}

fn crc_like_seed(mut s: u64) -> u64 {
    if s == 0 {
        s = 0x2545_F491_4F6C_DD1D;
    }
    s
}

fn side_tag(s: Side) -> &'static str {
    match s {
        Side::A => "A",
        Side::B => "B",
    }
}

fn role_tag(r: ClientRole) -> &'static str {
    match r {
        ClientRole::Host => "host",
        ClientRole::Player => "player",
        ClientRole::Spectator => "spectator",
    }
}

pub fn err(code: &str, message: &str) -> HostEvent {
    HostEvent::Error { code: code.to_string(), message: message.to_string() }
}

/// 快照类型显式化（供外部引用；内部直调 build_snapshot）。
pub type ServiceSnapshot = BpSnapshot;
