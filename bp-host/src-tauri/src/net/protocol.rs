//! net/protocol.rs —— 版本化协议契约（交付要求一）。
//!
//! 客户端意图 `ClientIntent`（C→S）与主机事件 `HostEvent`（S→C）共用同一组
//! 共享数据模型（`BpSnapshot` / `BpOperation` / `InterferenceEvent` / `ChartMeta` 等）。
//!
//! 传输层：
//! - 主机 `axum` 同时服务静态前端与 WS 端点；WebSocket 文本帧直接传 JSON 消息体。
//! - 帧内仅消息体；`tag = "t"` 的 internally-tagged 枚举便于排错与日志。
//! - 与既有 `phira-duel` 的 `[u32 len][JSON]` TCP 帧风格不同：本工具走浏览器 WS，
//!   故用 JSON 文本帧，但消息命名 / serde 约定保持一致（camelCase、tag 风格）。
//!
//! 非法意图处理：每个 BP 状态下只允许特定意图（见枚举 doc 与 `bp/state_machine::legal_intent`）。
//! 非法意图**返回错误事件**（`OperationRejected` / `InterferenceDenied` / `Error`），而非静默忽略。

use serde::{Deserialize, Serialize};

use crate::bp::interference::{InterferenceConfig, InterferenceKind, InterferenceParams};
use crate::model::{BpAction, BpStatus, Side};

/// 协议版本（breaking change 时 +1；客户端可在 `Welcome` 校验，不匹配则提示升级）。
pub const PROTOCOL_VERSION: u16 = 1;

/// 玩家会话 ID（连接分配，绑定身份）。
pub type PlayerId = u32;

/// 客户端角色。同一套前端代码以角色参数区分：主机控制台 / 参赛者视角 / 观众视角。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ClientRole {
    /// 主机 / 主办方控制台
    Host,
    /// 参赛者（携带 `Side`）
    Player,
    /// 观众（只读 / 轻互动）
    Spectator,
}

/// 曲库中的一首曲目（主机权威状态，含可用性）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChartMeta {
    pub id: u32,
    pub title: String,
    pub artist: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_sec: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<f32>,
    /// 主机侧权威状态（Obscured 是纯渲染态，不进此枚举）
    pub status: ChartStatus,
}

/// 曲目在 BP 中的权威状态。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ChartStatus {
    /// 可选（曲池剩余）
    Available,
    /// 已被 Ban
    Banned,
    /// 已被 Pick
    Picked,
    /// 被追加 Ban 压力指定，本回合必须 Ban
    ForcedBanCandidate,
}

/// 单个玩家的权威状态（含干扰资源）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PlayerState {
    pub id: PlayerId,
    pub name: String,
    pub role: ClientRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
    #[serde(default)]
    pub ready: bool,
    #[serde(default = "default_true")]
    pub connected: bool,
    #[serde(default)]
    pub interference_points: u8,
    #[serde(default)]
    pub interference_disabled: bool,
}

fn default_true() -> bool {
    true
}

/// 一条 Ban / Pick 操作记录（进入 BP 时间线与导出结果）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[derive(PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BpOperation {
    /// 操作顺序编号（全局自增，BP 结果导出使用）
    pub order_no: u32,
    pub actor_side: Side,
    pub action: BpAction,
    pub chart_id: u32,
    /// 主机时间戳（毫秒）
    pub at_ms: u64,
}

/// 一条干扰事件（与 Ban/Pick 并列进入时间线 + 审计 + 导出）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct InterferenceEvent {
    pub id: u64,
    pub kind: InterferenceKind,
    pub from_side: Side,
    pub target_side: Side,
    pub at_ms: u64,
    pub params: InterferenceParams,
    /// 是否被反制
    #[serde(default)]
    pub countered: bool,
    /// 是否被主办方回滚
    #[serde(default)]
    pub rolled_back: bool,
    /// 是否由主办方裁定产生
    #[serde(default)]
    pub by_host: bool,
}

/// 计时状态：以主机为准，广播截止时间戳由客户端本地平滑渲染，收到事件后校准。
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TimerState {
    /// 绝对截止时间戳（毫秒，主机时钟）
    pub deadline_ts_ms: u64,
    /// 本回合总时长（毫秒）
    pub duration_ms: u64,
}

/// 当前回合视图（客户端用于渲染「轮到谁 / Ban 还是 Pick / 是否 TB」）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurrentTurn {
    pub actor_side: Side,
    pub action: BpAction,
    /// 是否为决胜 / 先手判定位
    pub decider: bool,
    pub is_tb: bool,
}

/// # 完整快照 `BpSnapshot`
///
/// 断线重连 / 初次进入时主机推送，客户端据此*完整恢复*界面（回合序列、当前步骤、
/// 双方操作记录、曲池剩余、干扰资源、倒计时剩余毫秒）。这是唯一事实源。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BpSnapshot {
    pub room_code: String,
    pub status: BpStatus,
    /// 回合序列原文（如 "bppp"）
    pub sequence: String,
    pub first_side: Side,
    /// 当前第几步（b/p/t 计数）1-based；Lobby 时为 0
    pub step_index: u32,
    pub total_steps: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<CurrentTurn>,
    pub players: Vec<PlayerState>,
    pub operations: Vec<BpOperation>,
    pub pool: Vec<ChartMeta>,
    pub interference_log: Vec<InterferenceEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer: Option<TimerState>,
    pub interference: InterferenceConfig,
    #[serde(default)]
    pub locked: bool,
}

/// BP 结束后的结构化结果（可导出图片 / 复制链接）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BpResult {
    pub sequence: String,
    pub players: Vec<PlayerState>,
    pub bans: Vec<BpOperation>,
    pub picks: Vec<BpOperation>,
    /// 最终可用曲目集合（未被 Ban）
    pub available_charts: Vec<u32>,
    pub interference_log: Vec<InterferenceEvent>,
    pub finished_at_ms: u64,
}

// ----------------------------------------------------------------- 客户端 → 主机意图
/// # 客户端意图 `ClientIntent`（C→S）
///
/// **每个状态下允许的意图**（非法返回错误事件，不静默忽略）：
/// - `Lobby`：仅 `Join` / `SetReady` / `SetRole` / `HostCommand { hostCommand: StartBp }` / `RequestSnapshot` / `Ping` / `Leave`。
/// - `InProgress`（轮到某人）：当前行动方允许 `Ban` / `Pick` / `RevealCandidate` / `UseInterference` / `CounterInterference`；
///   非行动方允许 `UseInterference`（针对行动方）、 `CounterInterference` / `SendEmote`；所有人不允许自推回合。
/// - `Paused`：除主办方 `HostCommand { hostCommand: Resume }` 外，冻结所有 BP 推进意图（仍放 `Ping`）。
/// - `Finished`：仅 `RequestSnapshot` / `Leave`（及主办方导出）。
/// 完整判定见 `bp/state_machine::BpState::legal_intent`。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ClientIntent {
    /// 加入房间。`side` 仅参赛者需要；`token` 用于身份确认（防冒名）。
    Join {
        room_code: String,
        player_name: String,
        role: ClientRole,
        side: Option<Side>,
        token: Option<String>,
    },
    /// 准备 / 取消准备（主办方开始 BP 前）。
    SetReady { ready: bool },
    /// 进房后切换角色（主办方裁定用）。
    SetRole { role: ClientRole, side: Option<Side> },
    /// Ban 一首（仅当前行动方在 Ban 回合合法）。
    Ban { chart_id: u32 },
    /// Pick 一首（仅当前行动方在 Pick 回合合法）。
    Pick { chart_id: u32 },
    /// 被「强制公开」后，目标方公开一首候选意向（不可撤回，但可改选其他）。
    RevealCandidate { chart_id: u32 },
    /// 使用干扰。`obscure_chart_ids` 用于遮蔽曲池；`forced_ban_chart_id` 用于追加 Ban 压力。
    UseInterference {
        kind: InterferenceKind,
        target_side: Side,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        obscure_chart_ids: Option<Vec<u32>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forced_ban_chart_id: Option<u32>,
    },
    /// 反制：取消一次针对自己的干扰（消耗 `counter_cost` 点）。
    CounterInterference { interference_id: u64 },
    /// 发送表情 / 弹幕（轻量层；观众与对手皆可，可被静音）。
    SendEmote {
        emote: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// 主办方裁定命令（见 `HostCommand`）。非 Host 发送将被拒绝。
    /// 必须用结构体变体命名承载：嵌套两层同 tag（"t"）的 internally-tagged
    /// 枚举会在 serde 处理时撞字段名（duplicate field `t`），命令就永远进不来。
    /// 线上形状：`{"t":"hostCommand","hostCommand":{"t":"startBp"}}`。
    #[serde(rename_all = "camelCase")]
    HostCommand { host_command: HostCommand },
    /// 请求完整快照（初次进入 / 重连）。
    RequestSnapshot,
    /// 离开房间（WS 关闭亦可，主机标记断线）。
    Leave,
    /// 心跳（携带客户端发送时刻，服务端回 `Pong`）。
    Ping { at: u64 },
}

/// 主办方裁定命令（仅 Host 角色可发，服务端二次校验）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum HostCommand {
    /// 从 Lobby 开始 BP（要求双方已就位 / 准备）。
    StartBp,
    /// 锁定 / 解锁房间（拒绝新连接）。
    LockRoom { locked: bool },
    /// 暂停（停表、冻结推进，不断线）。
    Pause,
    /// 恢复。
    Resume,
    /// 裁定回退：撤销某次 Ban/Pick 操作（记录日志）。
    ArbitrateRollback { operation_order_no: u32 },
    /// 禁用 / 恢复某选手的干扰权。
    DisableInterference { side: Side, disabled: bool },
    /// 回滚最近一次干扰。
    RollbackInterference { interference_id: u64 },
    /// 踢出某玩家。
    Kick { player_id: PlayerId },
    /// 设置 / 替换曲池（仅 Lobby 阶段；曲目状态一律以 Available 进入）。
    /// 由主机控制台从本地曲库导出的 JSON 导入后下发。
    SetPool { pool: Vec<ChartMeta> },
    /// 动态调整干扰配置（下发后所有客户端生效）。
    SetInterferenceConfig(InterferenceConfig),
}

// ----------------------------------------------------------------- 主机 → 客户端事件
/// # 主机事件 `HostEvent`（S→C）
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum HostEvent {
    /// 加入成功：告知你的会话 ID、角色、房间码、协议版本。
    Welcome {
        you: PlayerId,
        role: ClientRole,
        room_code: String,
        protocol_version: u16,
        /// 身份令牌：客户端未携带时由主机签发（原样回给客户端持久化），
        /// 断线重连 Join 带着 token 才能恢复同一 PlayerId（防冒名）。不进广播。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    /// 完整快照（初次进入 / 重连 / 状态大幅变化）。
    Snapshot(BpSnapshot),
    /// 回合切换：当前第几步 / 共几步、轮到谁、Ban 还是 Pick、是否决胜位 / TB。
    PhaseChanged {
        step_index: u32,
        total_steps: u32,
        actor_side: Side,
        action: BpAction,
        decider: bool,
        is_tb: bool,
    },
    /// 曲池变化（Ban/Pick/强制位/遮蔽恢复后）。
    PoolUpdated { pool: Vec<ChartMeta> },
    /// 一次 Ban/Pick 被主机确认（已持久化、不可逆）。
    OperationConfirmed(BpOperation),
    /// Ban/Pick 被拒（非法意图 / 非轮到你 / 曲目不可用）。
    OperationRejected { reason: String, code: String },
    /// 干扰已应用（广播给受影响客户端渲染；`params` 决定各自表现）。
    InterferenceApplied(InterferenceEvent),
    /// 干扰被拒（资源 / 阶段 / 权限不合法），返回原因。
    InterferenceDenied { kind: InterferenceKind, reason: String },
    /// 反制生效：取消了某次干扰。
    CounterApplied { interference_id: u64, by_side: Side },
    /// 计时同步：广播截止时间戳（客户端本地平滑渲染，收到事件后校准）。
    TimerSync { deadline_ts_ms: u64, duration_ms: u64 },
    /// 新玩家连接（含身份）。
    PlayerConnected(PlayerState),
    /// 玩家断线（时间线标记「断线」，不自动暂停）。
    PlayerDisconnected { player_id: PlayerId, at_ms: u64 },
    /// 玩家准备状态变化。
    PlayerReady { player_id: PlayerId, ready: bool },
    /// 主办方命令结果（成功 / 失败原因）。
    HostActionResult { ok: bool, message: String },
    /// 表情 / 弹幕（屏幕边缘淡出，不覆盖底部操作区）。
    Emote {
        from_side: Option<Side>,
        from_player: PlayerId,
        from_name: String,
        emote: String,
        text: Option<String>,
    },
    /// BP 结束：推送结构化结果。
    SessionEnded(BpResult),
    /// 普通提示（谁 Ban 了哪张、谁投了干扰等），非错误。
    Notice { message: String },
    /// 错误（非法意图 / 房间码错误 / 房间已满 / 身份不匹配等）。
    Error { code: String, message: String },
    /// 心跳回包。
    Pong { at: u64 },
}

// ----------------------------------------------------------------- 契约自检
//
// 这些测试锁死「Rust serde 形状 ↔ 前端 net.ts」的线上契约：
// - `tag="t"` 的新类型变体（Snapshot / OperationConfirmed 等包结构体的）会把字段
//   **拍平到顶层**，前端不得再包一层字段名；
// - 嵌套同 tag 枚举必须经命名变体承载（历史教训：`HostCommand(HostCommand)`
//   新类型嵌套同 tag 会 duplicate field `t`，命令永远无法从前端解析进入）。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_command_intent_roundtrip() {
        let intent = ClientIntent::HostCommand {
            host_command: HostCommand::StartBp,
        };
        let json = serde_json::to_string(&intent).unwrap();
        assert_eq!(json, r#"{"t":"hostCommand","hostCommand":{"t":"startBp"}}"#);
        let back: ClientIntent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ClientIntent::HostCommand { host_command: HostCommand::StartBp }
        ));
    }

    #[test]
    fn operation_confirmed_event_is_flat() {
        let op = BpOperation {
            order_no: 1,
            actor_side: Side::A,
            action: BpAction::Ban,
            chart_id: 3,
            at_ms: 42,
        };
        let v = serde_json::to_value(HostEvent::OperationConfirmed(op)).unwrap();
        assert_eq!(v["t"].as_str(), Some("operationConfirmed"));
        // 拍平：BpOperation 字段直接在顶层，不再包一层 "operation"
        assert_eq!(v["orderNo"], 1);
        assert_eq!(v["chartId"], 3);
        assert!(
            v.get("operation").is_none(),
            "newtype 变体必须拍平到顶层（与前端 net.ts 契约一致）"
        );
        let _: HostEvent = serde_json::from_value(v).unwrap();
    }

    #[test]
    fn snapshot_event_is_flat() {
        let snap = BpSnapshot {
            room_code: "131400".into(),
            status: crate::model::BpStatus::Lobby,
            sequence: "bpp".into(),
            first_side: Side::A,
            step_index: 0,
            total_steps: 2,
            current: None,
            players: vec![],
            operations: vec![],
            pool: vec![ChartMeta {
                id: 1,
                title: "曲1".into(),
                artist: "艺术家".into(),
                cover_url: None,
                duration_sec: None,
                difficulty: None,
                status: ChartStatus::Available,
            }],
            interference_log: vec![],
            timer: None,
            interference: InterferenceConfig::default(),
            locked: false,
        };
        let v = serde_json::to_value(HostEvent::Snapshot(snap)).unwrap();
        assert_eq!(v["t"].as_str(), Some("snapshot"));
        assert_eq!(v["roomCode"].as_str(), Some("131400"));
        assert!(
            v.get("snapshot").is_none(),
            "BpSnapshot 字段必须拍平到顶层（与前端 net.ts 契约一致）"
        );
        let _: HostEvent = serde_json::from_value(v).unwrap();
    }
}
