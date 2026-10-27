//! bp/state_machine.rs —— 可配置回合状态机（交付要求二核心）。
//!
//! BP 本质是被严格时序约束的决策协议，在主机侧建模为状态机，而非前端拼接：
//! - 回合序列由事件字符串定义（`bppp` / `bpbpp` / `bpt` …），解析为有序 `BpTurn` 列表。
//! - 状态权威在主机；客户端不乐观推进、不自行扣减资源。
//! - 操作不可逆（除非主办方裁定回退）。
//! - 每回合可配置倒计时；超时由主机执行预设行为。
//! - 非法意图返回错误（见 `legal_intent`），不静默忽略。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::bp::interference::{
    apply as apply_interference, validate as validate_interference, InterferenceConfig,
    InterferenceDeny, InterferenceFacts, InterferenceKind, InterferenceParams,
};
use crate::model::{BpAction, BpStatus, BpTurn, ParseError, Side};
use crate::net::protocol::{
    BpOperation, BpResult, BpSnapshot, ChartMeta, ChartStatus, ClientIntent, ClientRole, CurrentTurn,
    InterferenceEvent, PlayerId, PlayerState, TimerState,
};

/// 单回合默认超时（毫秒）。实际值从赛事配置读取，此处为兜底默认。
pub const DEFAULT_TURN_TIMEOUT_MS: u64 = 30_000;

/// 非法 Ban / Pick 的原因码与文案。
#[derive(Debug, PartialEq, Eq)]
pub enum OpError {
    NotInProgress,
    NotYourTurn,
    WrongAction,
    ChartUnavailable,
    MustIncludeForcedBan,
    AlreadyFinished,
}

impl OpError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotInProgress => "NOT_IN_PROGRESS",
            Self::NotYourTurn => "NOT_YOUR_TURN",
            Self::WrongAction => "WRONG_ACTION",
            Self::ChartUnavailable => "CHART_UNAVAILABLE",
            Self::MustIncludeForcedBan => "MUST_INCLUDE_FORCED_BAN",
            Self::AlreadyFinished => "ALREADY_FINISHED",
        }
    }
    pub fn message(&self) -> &'static str {
        match self {
            Self::NotInProgress => "BP 未在进行中",
            Self::NotYourTurn => "当前不是你的回合",
            Self::WrongAction => "当前不是该动作回合",
            Self::ChartUnavailable => "该曲目不可选（已被 Ban / 已选 / 不可用）",
            Self::MustIncludeForcedBan => "本回合必须先 Ban 强制位曲目",
            Self::AlreadyFinished => "BP 已结束",
        }
    }
}

/// 非法意图的拒绝原因。
#[derive(Debug, PartialEq, Eq)]
pub struct IntentRejected(pub String);

/// 超时后主机的预设行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutAction {
    /// Ban 回合：空 Ban（记录超时事件，不消耗曲池）。
    EmptyBan,
    /// Pick 回合：随机选择一首可用曲目归当前行动方。
    RandomPick,
    /// Pick 回合：判对手获得选择权（交由服务端切换行动方）。
    DeferToOpponent,
}

/// BP 权威状态（主机持有，唯一事实源）。
///
/// 整体 `Serialize / Deserialize`：persistence/sqlite 直接以 JSON 行落库，
/// 崩溃恢复时反序列化回来再加少量修补（connected=false 等）即可复原会话。
/// 注意 `tokens` 只存在于权威状态，**不进 `BpSnapshot`**（协议层不下发身份令牌反向表）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BpState {
    pub room_code: String,
    pub sequence: String,
    pub first_side: Side,
    pub turns: Vec<BpTurn>,
    pub current_index: usize,
    pub operations: Vec<BpOperation>,
    pub order_counter: u32,
    pub pool: Vec<ChartMeta>,
    pub players: Vec<PlayerState>,
    pub interference: InterferenceConfig,
    pub interference_log: Vec<InterferenceEvent>,
    pub interference_id_counter: u64,
    pub status: BpStatus,
    pub timer: Option<TimerState>,
    pub locked: bool,
    pub turn_timeout_ms: u64,

    // ---- 安全 / 会话（不进快照） ----
    /// player_id → 身份令牌（Join 防冒名与重连识别）
    #[serde(default)]
    pub tokens: HashMap<PlayerId, String>,

    // ---- 暂停 / 恢复 ----
    /// 暂停→恢复的计时策略（续算 / 重置），由配置下发
    pub resume_mode: ResumeTimerMode,
    /// 暂停时冻结的剩余毫秒（非暂停期为 None）
    #[serde(default)]
    pub paused_remaining_ms: Option<u64>,

    // ---- 每回合（step）计数，进入新 step 时重置 ----
    /// 各方本回合已承受的策略干扰次数
    #[serde(default)]
    strategic_received: HashMap<Side, u8>,
    /// 同回合是否已压缩过当前行动方倒计时
    #[serde(default)]
    compress_used_this_round: bool,
    /// 本回合强制 Ban 候选（追加 Ban 压力产生）
    #[serde(default)]
    current_forced_ban: Option<u32>,
    /// 本回合「强制公开」待补全：目标方 + 暂存事件 id（目标公开后补全 candidate）
    #[serde(default)]
    pending_force_reveal: Option<(Side, u64)>,
}

/// 暂停→恢复的计时策略（需求允许配置决定：续算或重置）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ResumeTimerMode {
    /// 恢复时按暂停时刻剩余时间续算（默认）
    #[default]
    Continue,
    /// 恢复时本回合重新计时（满时长）
    Reset,
}

impl BpState {
    /// 构造：解析回合序列；`pool_charts` 为初始曲池（状态全 Available）。
    pub fn new(
        room_code: String,
        sequence: &str,
        first_side: Side,
        interference: InterferenceConfig,
        pool_charts: Vec<ChartMeta>,
        turn_timeout_ms: u64,
    ) -> Result<Self, ParseError> {
        Self::with_resume_mode(
            room_code,
            sequence,
            first_side,
            interference,
            pool_charts,
            turn_timeout_ms,
            ResumeTimerMode::Continue,
        )
    }

    /// 同 `new`，但显式指定「暂停→恢复」的计时策略（来自 config）。
    pub fn with_resume_mode(
        room_code: String,
        sequence: &str,
        first_side: Side,
        interference: InterferenceConfig,
        pool_charts: Vec<ChartMeta>,
        turn_timeout_ms: u64,
        resume_mode: ResumeTimerMode,
    ) -> Result<Self, ParseError> {
        let turns = Self::parse_sequence(sequence, first_side)?;
        Ok(Self {
            room_code,
            sequence: sequence.to_string(),
            first_side,
            turns,
            current_index: 0,
            operations: Vec::new(),
            order_counter: 0,
            pool: pool_charts,
            players: Vec::new(),
            interference,
            interference_log: Vec::new(),
            interference_id_counter: 0,
            status: BpStatus::Lobby,
            timer: None,
            locked: false,
            turn_timeout_ms,
            tokens: HashMap::new(),
            resume_mode,
            paused_remaining_ms: None,
            strategic_received: HashMap::new(),
            compress_used_this_round: false,
            current_forced_ban: None,
            pending_force_reveal: None,
        })
    }

    /// 解析回合序列（交付验收语义）：
    /// - `b` = 双方各 Ban 一张（一个 step，两个 turn：先手方优先）；
    /// - `p` = Pick 阶段：**每 2 个连续 `p` 合并为一个 step**（双方各 Pick 一张），
    ///   奇数末尾的 `p` 并入最后一个 pick step——"bppp" 即 3 step / 6 turn；
    /// - `t` = TB：先手方单 Pick（一个 turn，is_tb=true）。
    /// 其余字符 / 非法字符 → `ParseError`。
    pub fn parse_sequence(seq: &str, first: Side) -> Result<Vec<BpTurn>, ParseError> {
        let seq = seq.trim();
        if seq.is_empty() {
            return Err(ParseError::EmptySequence);
        }
        let chars: Vec<char> = seq.chars().collect();
        let mut turns = Vec::new();
        let mut step: u32 = 0;
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                'b' => {
                    step += 1;
                    turns.push(BpTurn { action: BpAction::Ban, side: first, is_tb: false, step });
                    turns.push(BpTurn { action: BpAction::Ban, side: first.other(), is_tb: false, step });
                    i += 1;
                }
                't' => {
                    step += 1;
                    turns.push(BpTurn { action: BpAction::Pick, side: first, is_tb: true, step });
                    i += 1;
                }
                'p' => {
                    // 收集连续 p 段：每 2 个 p 合并为一个 pick step（双方各选一张）
                    let mut run = 0usize;
                    while i < chars.len() && chars[i] == 'p' {
                        run += 1;
                        i += 1;
                    }
                    for _ in 0..run.div_ceil(2) {
                        step += 1;
                        turns.push(BpTurn { action: BpAction::Pick, side: first, is_tb: false, step });
                        turns.push(BpTurn { action: BpAction::Pick, side: first.other(), is_tb: false, step });
                    }
                }
                other => return Err(ParseError::InvalidChar(other)),
            }
        }
        Ok(turns)
    }

    /// 当前行动位（无则返回 None：已结束 / Lobby / Paused）。
    pub fn current_turn(&self) -> Option<&BpTurn> {
        if self.status != BpStatus::InProgress {
            return None;
        }
        self.turns.get(self.current_index)
    }

    /// 当前步骤序号与总步骤数（b/p/t 计数）。
    pub fn step_info(&self) -> (u32, u32) {
        let total = self.turns.last().map(|t| t.step).unwrap_or(0);
        let cur = match self.status {
            BpStatus::InProgress => self.current_turn().map(|t| t.step).unwrap_or(0),
            BpStatus::Finished => total,
            _ => 0,
        };
        (cur, total)
    }

    /// 按 Side 取玩家。
    pub fn player_by_side(&self, side: Side) -> Option<&PlayerState> {
        self.players.iter().find(|p| p.side == Some(side))
    }

    // ---------------------------------------------------------------- 生命周期
    /// 从 Lobby 开始 BP：双方就位后由主办方触发。初始化计时。
    ///
    /// **已知坑修复（handoff 第 4 节）**：干扰点必须在 start 时按
    /// `interference.initial_points` 给双方赋值 — 此前 players 点数恒为 0，
    /// 任何策略干扰都会被 `InsufficientPoints` 拒绝。
    /// 只初始化 role=Player 的参赛者（Host / Spectator 不持干扰资源）。
    pub fn start(&mut self, now_ms: u64) {
        if self.status == BpStatus::Lobby && !self.turns.is_empty() {
            let init_points = self.interference.initial_points;
            for p in self.players.iter_mut() {
                if p.role == ClientRole::Player {
                    p.interference_points = init_points;
                }
            }
            self.status = BpStatus::InProgress;
            self.current_index = 0;
            self.reset_round_counters();
            self.arm_timer(now_ms);
        }
    }

    /// 暂停：把剩余时间冻结进 `paused_remaining_ms`，`timer` 置 None
    /// （否则快照里还挂着旧 deadline，客户端会继续倒计时——暂停必须是真停表）。
    pub fn pause(&mut self, now_ms: u64) {
        if self.status == BpStatus::InProgress {
            if let Some(t) = self.timer.take() {
                self.paused_remaining_ms = Some(t.deadline_ts_ms.saturating_sub(now_ms));
            }
            self.status = BpStatus::Paused;
        }
    }

    /// 恢复：策略由 `resume_mode` 决定（配置项：续算 / 重置）。
    pub fn resume(&mut self, now_ms: u64) {
        if self.status != BpStatus::Paused {
            return;
        }
        let duration_ms = match self.resume_mode {
            ResumeTimerMode::Reset => self.turn_timeout_ms,
            // 续算：按暂停时刻冻结的剩余时间；记录丢失则回退满时长
            ResumeTimerMode::Continue => self.paused_remaining_ms.take().unwrap_or(self.turn_timeout_ms),
        };
        self.paused_remaining_ms = None;
        self.status = BpStatus::InProgress;
        self.timer = Some(TimerState {
            deadline_ts_ms: now_ms.saturating_add(duration_ms),
            duration_ms,
        });
    }

    fn arm_timer(&mut self, now_ms: u64) {
        self.timer = Some(TimerState {
            deadline_ts_ms: now_ms + self.turn_timeout_ms,
            duration_ms: self.turn_timeout_ms,
        });
    }

    /// 进入新 step / 回退时重置回合计数。
    /// 强制 Ban 位随回合失效，其挂「待 Ban」状态的曲目恢复为 Available。
    fn reset_round_counters(&mut self) {
        if let Some(id) = self.current_forced_ban.take() {
            if let Some(c) = self.pool.iter_mut().find(|c| c.id == id) {
                if c.status == ChartStatus::ForcedBanCandidate {
                    c.status = ChartStatus::Available;
                }
            }
        }
        self.strategic_received.clear();
        self.compress_used_this_round = false;
        self.pending_force_reveal = None;
    }

    // ---------------------------------------------------------------- 操作
    /// 确认一次 Ban / Pick。返回被确认的操作（已持久化、不可逆）。
    pub fn apply_operation(
        &mut self,
        side: Side,
        action: BpAction,
        chart_id: u32,
        now_ms: u64,
    ) -> Result<BpOperation, OpError> {
        if self.status == BpStatus::Finished {
            return Err(OpError::AlreadyFinished);
        }
        let (turn_side, turn_action, step) = {
            let turn = self.current_turn().ok_or(OpError::NotInProgress)?;
            (turn.side, turn.action, turn.step)
        };
        if turn_side != side {
            return Err(OpError::NotYourTurn);
        }
        if turn_action != action {
            return Err(OpError::WrongAction);
        }
        // 强制 Ban 位必须被包含
        if action == BpAction::Ban && self.current_forced_ban.is_some() {
            if self.current_forced_ban != Some(chart_id) {
                return Err(OpError::MustIncludeForcedBan);
            }
        }
        let forced_matched = self.current_forced_ban == Some(chart_id);
        let chart = self
            .pool
            .iter_mut()
            .find(|c| c.id == chart_id)
            .ok_or(OpError::ChartUnavailable)?;
        match chart.status {
            ChartStatus::Available => {
                chart.status = if action == BpAction::Ban {
                    ChartStatus::Banned
                } else {
                    ChartStatus::Picked
                };
            }
            // 追加 Ban 压力挂的强制位：本回合被要求 Ban 它本身就是合法操作
            // （MustIncludeForcedBan 已保证 chart_id 与强制位一致）。
            ChartStatus::ForcedBanCandidate if forced_matched && action == BpAction::Ban => {
                chart.status = ChartStatus::Banned;
            }
            _ => return Err(OpError::ChartUnavailable),
        }
        Ok(self.push_op(side, action, chart_id, step, now_ms))
    }

    /// 落一条操作并推进回合（apply_operation / execute_timeout / 随机 Pick 共用尾部）。
    fn push_op(
        &mut self,
        side: Side,
        action: BpAction,
        chart_id: u32,
        prev_step: u32,
        now_ms: u64,
    ) -> BpOperation {
        self.order_counter += 1;
        let op = BpOperation {
            order_no: self.order_counter,
            actor_side: side,
            action,
            chart_id,
            at_ms: now_ms,
        };
        self.operations.push(op.clone());
        // 强制 Ban 已执行 → 清除（空 Ban 也要清，防跨回合残留）。
        // 正常 Ban 流程中该曲目此刻已是 Banned，不会被恢复；空 Ban 时则把它还原 Available。
        if action == BpAction::Ban {
            self.current_forced_ban = None;
            for c in self.pool.iter_mut() {
                if c.status == ChartStatus::ForcedBanCandidate {
                    c.status = ChartStatus::Available;
                }
            }
        }
        self.advance_after_op(prev_step, now_ms);
        op
    }

    /// 推进索引 → 跨 step 重置 → 动计时 / 收尾。
    fn advance_after_op(&mut self, prev_step: u32, now_ms: u64) {
        self.current_index += 1;
        if self.current_index >= self.turns.len() {
            self.status = BpStatus::Finished;
            self.timer = None;
            self.paused_remaining_ms = None;
        } else if self.turns[self.current_index].step != prev_step {
            // 进入新 step：重置回合计数与挂起干扰
            self.reset_round_counters();
        }
        if self.status == BpStatus::InProgress {
            self.arm_timer(now_ms);
        }
    }

    /// 曲池中仍为 Available 的曲目 id。
    pub fn available_chart_ids(&self) -> Vec<u32> {
        self.pool
            .iter()
            .filter(|c| c.status == ChartStatus::Available)
            .map(|c| c.id)
            .collect()
    }

    /// 超时预设行为（由主机定时器在 deadline 到达时调用）。
    ///
    /// - Ban 回合 → 空 Ban：合成 `chart_id = 0` 的占位操作（导出/时间线显示「空 Ban」），
    ///   同时释放本回合强制 Ban 位。
    /// - Pick 回合 → 使用调用方选好的曲目（默认随机、可配置判给对手→服务层传 None）；
    ///   曲池已空时同样落 `chart_id = 0` 的空 Pick。
    pub fn execute_timeout(&mut self, now_ms: u64, chosen_chart: Option<u32>) -> Option<BpOperation> {
        let turn = *self.current_turn()?;
        match turn.action {
            BpAction::Ban => {
                let op = self.push_op(turn.side, BpAction::Ban, 0, turn.step, now_ms);
                Some(op)
            }
            BpAction::Pick => {
                let valid = chosen_chart
                    .filter(|id| self.available_chart_ids().contains(id))
                    .unwrap_or(0);
                // 落空 Pick（chart_id = 0 哨兵）不消耗曲池；有效选择则先落状态
                if valid != 0 {
                    if let Some(c) = self.pool.iter_mut().find(|c| c.id == valid) {
                        c.status = ChartStatus::Picked;
                    }
                }
                Some(self.push_op(turn.side, BpAction::Pick, valid, turn.step, now_ms))
            }
        }
    }

    /// 超时预设行为（由主机定时器在 deadline 到达时调用）。
    pub fn timeout_action(&self) -> TimeoutAction {
        match self.current_turn() {
            Some(t) if t.action == BpAction::Ban => TimeoutAction::EmptyBan,
            Some(_) => TimeoutAction::RandomPick,
            None => TimeoutAction::DeferToOpponent,
        }
    }

    // ---------------------------------------------------------------- 干扰
    /// 校验并应用一次干扰，原子化：扣减资源 + 写日志 + 返回事件供广播。
    /// 返回被应用（或待补全）的 `InterferenceEvent`。
    pub fn try_interference(
        &mut self,
        from_side: Side,
        target_side: Side,
        kind: InterferenceKind,
        obscure_ids: Vec<u32>,
        forced_ban_id: Option<u32>,
        now_ms: u64,
    ) -> Result<InterferenceEvent, InterferenceDeny> {
        let turn = self.current_turn().ok_or(InterferenceDeny::WrongPhase)?;

        // 参数预检（依赖曲池这一权威状态，故在状态机而非纯函数 validate 里做）：
        let obscure_ids = match kind {
            InterferenceKind::ObscurePool => {
                let valid: Vec<u32> = obscure_ids
                    .into_iter()
                    .filter(|id| {
                        self.pool
                            .iter()
                            .any(|c| c.id == *id && c.status == ChartStatus::Available)
                    })
                    .collect();
                if valid.is_empty() {
                    return Err(InterferenceDeny::ObscureChartsUnavailable);
                }
                valid
            }
            _ => Vec::new(),
        };
        let forced_ban_id = match kind {
            InterferenceKind::ExtraBanPressure => {
                let id = forced_ban_id.ok_or(InterferenceDeny::ExtraBanNotSpecified)?;
                if !self
                    .pool
                    .iter()
                    .any(|c| c.id == id && c.status == ChartStatus::Available)
                {
                    return Err(InterferenceDeny::ExtraBanChartUnavailable);
                }
                Some(id)
            }
            _ => None,
        };

        let from_pts = self
            .player_by_side(from_side)
            .map(|p| p.interference_points)
            .unwrap_or(0);
        let sender_disabled = self
            .player_by_side(from_side)
            .map(|p| p.interference_disabled)
            .unwrap_or(false);
        // 回合饱和取各方承受值的最大值（交付用例语义：本回合一旦有方承受过
        // 策略干扰、达到上限，回合即封闭，不再接受新的策略干扰）。
        let received = self
            .strategic_received
            .values()
            .copied()
            .max()
            .unwrap_or(0);
        let remaining_secs = self
            .timer
            .map(|t| t.deadline_ts_ms.saturating_sub(now_ms) / 1000)
            .unwrap_or(0);

        let facts = InterferenceFacts {
            kind,
            from_side,
            target_side,
            actor_action: turn.action,
            is_tb: turn.is_tb,
            from_points: from_pts,
            target_received_this_round: received,
            same_round_compress_used: self.compress_used_this_round,
            sender_interference_disabled: sender_disabled,
            actor_remaining_secs: remaining_secs,
            obscure_requested: obscure_ids,
            forced_ban_chart_id: forced_ban_id,
            emote: None,
        };

        validate_interference(&facts, &self.interference)?;

        // 原子变更：先算出效果与消耗，再扣点
        let applied = apply_interference(&facts, &self.interference);
        if let Some(p) = self.players.iter_mut().find(|p| p.side == Some(from_side)) {
            p.interference_points = p.interference_points.saturating_sub(applied.consumed_points);
        }

        let mut event = self.make_event(kind, from_side, target_side, now_ms);
        event.params = applied.params;

        // 维持每回合计数 / 阶段副作用
        if kind.is_strategic() {
            *self.strategic_received.entry(target_side).or_insert(0) += 1;
        }
        match event.params.clone() {
            InterferenceParams::CompressTimer { reduce_secs } => {
                self.compress_used_this_round = true;
                if let Some(t) = self.timer.as_mut() {
                    t.deadline_ts_ms =
                        t.deadline_ts_ms.saturating_sub((reduce_secs as u64) * 1000);
                }
            }
            InterferenceParams::ExtraBanPressure { forced_chart_id } => {
                self.current_forced_ban = Some(forced_chart_id);
                if let Some(c) = self.pool.iter_mut().find(|c| c.id == forced_chart_id) {
                    c.status = ChartStatus::ForcedBanCandidate;
                }
            }
            InterferenceParams::ForceReveal { .. } => {
                // 候选待目标方 RevealCandidate 补全
                self.pending_force_reveal = Some((target_side, event.id));
            }
            _ => {}
        }

        self.interference_log.push(event.clone());
        Ok(event)
    }

    /// 主办方「回滚最近干扰」：撤销副作用、退还点数、恢复曲池状态，留审计标记。
    ///
    /// 简化约定（对应 handoff 已知坑条目，与实现共同演进）：
    /// - 反制之后仍可回滚：回滚会同时退还发起方干扰点与被反制方反制点。
    /// - 压缩回滚把被压缩的秒数加回 deadline（错过的时间不补偿）。
    /// - 追加 Ban 压力回滚恢复指定曲目状态并清空强制位。
    /// - 强制公开回滚仅清挂起（候选意向本身不是曲目状态）。
    pub fn rollback_interference(
        &mut self,
        interference_id: u64,
    ) -> Result<InterferenceEvent, IntentRejected> {
        let idx = self
            .interference_log
            .iter()
            .position(|e| e.id == interference_id)
            .ok_or_else(|| IntentRejected("未找到该干扰事件".into()))?;

        // 事件本身无副作用可回退的轻量层直接拒绝
        if !self.interference_log[idx].kind.is_strategic() {
            return Err(IntentRejected("表情弹幕无副作用，无需回滚".into()));
        }
        if self.interference_log[idx].rolled_back {
            return Err(IntentRejected("该干扰已被回滚".into()));
        }
        let was_countered = self.interference_log[idx].countered;
        let kind = self.interference_log[idx].kind;
        let from_side = self.interference_log[idx].from_side;
        let target_side = self.interference_log[idx].target_side;

        // 退点：发起方拿回干扰消耗；若已被反制，被反制方也拿回反制消耗
        let cost = kind.cost();
        if let Some(p) = self.players.iter_mut().find(|p| p.side == Some(from_side)) {
            p.interference_points = p.interference_points.saturating_add(cost);
        }
        if was_countered {
            if let Some(p) = self.players.iter_mut().find(|p| p.side == Some(target_side)) {
                p.interference_points = p
                    .interference_points
                    .saturating_add(self.interference.counter_cost);
            }
        }

        // 撤销具体效果
        match &self.interference_log[idx].params {
            InterferenceParams::CompressTimer { reduce_secs } => {
                if let Some(t) = self.timer.as_mut() {
                    t.deadline_ts_ms = t.deadline_ts_ms.saturating_add((*reduce_secs as u64) * 1000);
                }
                self.compress_used_this_round = false;
            }
            InterferenceParams::ExtraBanPressure { forced_chart_id } => {
                if self.current_forced_ban == Some(*forced_chart_id) {
                    self.current_forced_ban = None;
                }
                if let Some(c) = self
                    .pool
                    .iter_mut()
                    .find(|c| c.id == *forced_chart_id && c.status == ChartStatus::ForcedBanCandidate)
                {
                    c.status = ChartStatus::Available;
                }
            }
            InterferenceParams::ForceReveal { .. } => {
                if self
                    .pending_force_reveal
                    .map(|(_, id)| id == interference_id)
                    .unwrap_or(false)
                {
                    self.pending_force_reveal = None;
                }
            }
            // 遮蔽是目标客户端的渲染态，主机不落曲目状态：客户端按 rolled_back 标记自恢复
            _ => {}
        }

        // 本回合承受计数回退一格（策略干扰）
        if let Some(v) = self.strategic_received.get_mut(&target_side) {
            *v = v.saturating_sub(1);
        }

        self.interference_log[idx].rolled_back = true;
        self.interference_log[idx].by_host = true;
        Ok(self.interference_log[idx].clone())
    }

    /// 被「强制公开」的目标方公开候选意向：补全挂起事件的 `candidate_chart_id`。
    pub fn reveal_candidate(
        &mut self,
        side: Side,
        chart_id: u32,
    ) -> Result<InterferenceEvent, IntentRejected> {
        let (target, id) = self
            .pending_force_reveal
            .ok_or_else(|| IntentRejected("当前没有待公开的强制公开干扰".into()))?;
        if target != side {
            return Err(IntentRejected("你不是被要求公开的一方".into()));
        }
        if let Some(ev) = self.interference_log.iter_mut().find(|e| e.id == id) {
            ev.params = crate::bp::interference::InterferenceParams::ForceReveal {
                candidate_chart_id: chart_id,
            };
            self.pending_force_reveal = None;
            return Ok(ev.clone());
        }
        Err(IntentRejected("未找到对应干扰事件".into()))
    }

    /// 反制：取消一次针对自己的干扰，消耗 `counter_cost` 点。
    pub fn counter_interference(
        &mut self,
        by_side: Side,
        interference_id: u64,
    ) -> Result<InterferenceEvent, IntentRejected> {
        let (idx, target) = self
            .interference_log
            .iter()
            .enumerate()
            .find(|(_, e)| e.id == interference_id)
            .map(|(i, e)| (i, e.target_side))
            .ok_or_else(|| IntentRejected("未找到该干扰".into()))?;
        if target != by_side {
            return Err(IntentRejected("只能反制针对自己的干扰".into()));
        }
        if self.interference_log[idx].countered {
            return Err(IntentRejected("该干扰已被反制".into()));
        }
        // 扣反制成本
        if let Some(p) = self.players.iter_mut().find(|p| p.side == Some(by_side)) {
            p.interference_points = p
                .interference_points
                .saturating_sub(self.interference.counter_cost);
        }
        self.interference_log[idx].countered = true;
        Ok(self.interference_log[idx].clone())
    }

    fn make_event(
        &mut self,
        kind: InterferenceKind,
        from_side: Side,
        target_side: Side,
        now_ms: u64,
    ) -> InterferenceEvent {
        self.interference_id_counter += 1;
        InterferenceEvent {
            id: self.interference_id_counter,
            kind,
            from_side,
            target_side,
            at_ms: now_ms,
            params: crate::bp::interference::InterferenceParams::Emote {
                emote: String::new(),
                text: None,
            },
            countered: false,
            rolled_back: false,
            by_host: false,
        }
    }

    // ---------------------------------------------------------------- 合法性
    /// 校验某意图在当前状态下是否被允许。非法返回 `IntentRejected`（→ 错误事件）。
    pub fn legal_intent(&self, sender: &PlayerState, intent: &ClientIntent) -> Result<(), IntentRejected> {
        match intent {
            ClientIntent::Ping { .. }
            | ClientIntent::RequestSnapshot
            | ClientIntent::Leave => Ok(()),

            ClientIntent::Join { .. }
            | ClientIntent::SetReady { .. }
            | ClientIntent::SetRole { .. } => {
                if self.status != BpStatus::Lobby {
                    return Err(IntentRejected("BP 已开始，无法变更准备 / 角色".into()));
                }
                Ok(())
            }

            ClientIntent::HostCommand { .. } => {
                if sender.role != crate::net::protocol::ClientRole::Host {
                    return Err(IntentRejected("非主办方不能发送裁定命令".into()));
                }
                Ok(())
            }

            ClientIntent::Ban { chart_id } | ClientIntent::Pick { chart_id } => {
                let action = if matches!(intent, ClientIntent::Ban { .. }) {
                    BpAction::Ban
                } else {
                    BpAction::Pick
                };
                let turn = self.current_turn().ok_or(IntentRejected("当前无行动方".into()))?;
                let side = sender
                    .side
                    .ok_or_else(|| IntentRejected("观众不能 Ban/Pick".into()))?;
                if turn.side != side {
                    return Err(IntentRejected("当前不是你的回合".into()));
                }
                if turn.action != action {
                    return Err(IntentRejected("当前不是该动作回合".into()));
                }
                let _ = chart_id;
                Ok(())
            }

            ClientIntent::RevealCandidate { .. } => {
                // 仅当存在针对自己的强制公开挂起时合法
                if self.pending_force_reveal.map(|(s, _)| s) != sender.side {
                    return Err(IntentRejected("当前没有要求你公开候选".into()));
                }
                Ok(())
            }

            ClientIntent::UseInterference { kind, target_side, .. } => {
                if !kind.is_strategic() {
                    // Emote：任何在线玩家（含观众）均可
                    return Ok(());
                }
                let side = sender
                    .side
                    .ok_or_else(|| IntentRejected("观众不能使用策略干扰".into()))?;
                if side == *target_side {
                    return Err(IntentRejected("不能对自己使用干扰".into()));
                }
                Ok(())
            }

            ClientIntent::CounterInterference { .. } => {
                // 任何被干扰方均可尝试；具体是否被反制由 try_interference 校验
                if sender.side.is_none() {
                    return Err(IntentRejected("观众不能反制".into()));
                }
                Ok(())
            }

            ClientIntent::SendEmote { .. } => Ok(()),
        }
    }

    // ---------------------------------------------------------------- 快照
    /// 构建完整快照（重连 / 初次进入 / 状态大幅变化时使用）。
    pub fn build_snapshot(&self) -> BpSnapshot {
        let (step, total) = self.step_info();
        let current = self.current_turn().map(|t| CurrentTurn {
            actor_side: t.side,
            action: t.action,
            decider: t.is_tb,
            is_tb: t.is_tb,
        });
        BpSnapshot {
            room_code: self.room_code.clone(),
            status: self.status,
            sequence: self.sequence.clone(),
            first_side: self.first_side,
            step_index: step,
            total_steps: total,
            current,
            players: self.players.clone(),
            operations: self.operations.clone(),
            pool: self.pool.clone(),
            interference_log: self.interference_log.clone(),
            timer: self.timer,
            interference: self.interference.clone(),
            locked: self.locked,
        }
    }

    /// 构建结束结果（导出 / 分享卡）。
    pub fn build_result(&self, now_ms: u64) -> BpResult {
        let bans: Vec<BpOperation> = self
            .operations
            .iter()
            .filter(|o| o.action == BpAction::Ban)
            .cloned()
            .collect();
        let picks: Vec<BpOperation> = self
            .operations
            .iter()
            .filter(|o| o.action == BpAction::Pick)
            .cloned()
            .collect();
        let available: Vec<u32> = self
            .pool
            .iter()
            .filter(|c| c.status == ChartStatus::Available)
            .map(|c| c.id)
            .collect();
        BpResult {
            sequence: self.sequence.clone(),
            players: self.players.clone(),
            bans,
            picks,
            available_charts: available,
            interference_log: self.interference_log.clone(),
            finished_at_ms: now_ms,
        }
    }

    /// 主办方裁定回退：撤销指定操作**及其之后的全部操作**（级联）。
    ///
    /// 语义（对齐 operations 与 turns 严格 1:1 递增的结构，本函数即 handoff §4
    /// 「精确 turn 对位」的落地）：operations 数组每次确认操作 push 一条并使
    /// `current_index +1`，因此「撤销下标 pos → 数组截断回 pos」即把回合对位
    /// 回被撤操作所在的 turn；这就是 §3.2 简化实现「任意点回退会与后续操作错位」
    /// 的正解——级联撤销 pos..len 全部，而不是只允许最近一条。
    ///
    /// 副作用：被撤曲目全部回到 Available；强制 Ban 位 / 挂起公开 / 回合计数
    /// 随回合重开失效（`reset_round_counters`）；Finished 状态回升为 InProgress
    /// 并按裁定时刻重新计时。
    /// 返回被撤的操作（时间序，目标在首）；order_no 全局自增不回收（审计可溯）。
    pub fn arbitrate_rollback(&mut self, order_no: u32, now_ms: u64) -> Result<Vec<BpOperation>, IntentRejected> {
        let pos = self
            .operations
            .iter()
            .position(|o| o.order_no == order_no)
            .ok_or_else(|| IntentRejected("未找到该操作".into()))?;
        let removed: Vec<BpOperation> = self.operations.split_off(pos);
        for op in &removed {
            // 空 Ban/Pick 的哨兵（chart_id = 0）在线上真实曲库（id 从 1 起）找不到
            // 对应曲目，find 不命中即天然跳过——无需额外守卫。
            if let Some(c) = self.pool.iter_mut().find(|c| c.id == op.chart_id) {
                if c.status == ChartStatus::Banned || c.status == ChartStatus::Picked {
                    c.status = ChartStatus::Available;
                }
            }
        }
        // 精确对回被撤操作所在的 turn（InProgress 下 current_index == len == pos，
        // min 是对 Paused / 恢复途中异常序列的防御）
        self.current_index = self.current_index.min(pos);
        // 回退跨 step 边界：回合计数、强制 Ban 位、挂起公开全部随之失效
        self.reset_round_counters();
        self.paused_remaining_ms = None;
        if self.status == BpStatus::Finished {
            self.status = BpStatus::InProgress;
        }
        // 被撤回合重新走一遍 → 重新计时（裁定视为重开该回合）
        if self.status == BpStatus::InProgress {
            self.arm_timer(now_ms);
        }
        Ok(removed)
    }
}

// ------------------------------------------------------------------ 单测

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bp::interference::{InterferenceConfig, InterferenceFacts, InterferenceParams};

    fn chart(id: u32) -> ChartMeta {
        ChartMeta {
            id,
            title: format!("曲{id}"),
            artist: "艺术家".into(),
            cover_url: None,
            duration_sec: None,
            difficulty: None,
            status: ChartStatus::Available,
        }
    }
    fn charts(n: u32) -> Vec<ChartMeta> {
        (0..n).map(chart).collect()
    }
    fn player(id: u32, name: &str, side: Side) -> PlayerState {
        PlayerState {
            id,
            name: name.into(),
            role: ClientRole::Player,
            side: Some(side),
            ready: true,
            connected: true,
            interference_points: 0,
            interference_disabled: false,
        }
    }
    fn cfg() -> InterferenceConfig {
        InterferenceConfig::default()
    }
    fn mk_state(seq: &str, n: u32) -> BpState {
        BpState::with_resume_mode(
            "131400".into(),
            seq,
            Side::A,
            cfg(),
            charts(n),
            30_000,
            ResumeTimerMode::Continue,
        )
        .unwrap()
    }
    fn with_sides(seq: &str, n: u32) -> BpState {
        let mut s = mk_state(seq, n);
        s.players.push(player(1, "A哥", Side::A));
        s.players.push(player(2, "B酱", Side::B));
        s
    }

    // ---------------- parse_sequence ----------------

    #[test]
    fn parse_bppp_yields_6_turns_first_side_first() {
        let turns = BpState::parse_sequence("bppp", Side::A).unwrap();
        assert_eq!(turns.len(), 6);
        assert_eq!(turns[0].side, Side::A);
        assert_eq!(turns[0].action, BpAction::Ban);
        assert_eq!(turns[1].side, Side::B);
        assert_eq!(turns[1].action, BpAction::Ban);
        assert_eq!(turns[2].side, Side::A);
        assert_eq!(turns[2].action, BpAction::Pick);
        assert_eq!(turns[5].action, BpAction::Pick);
        // step：1,1,2,2,3,3
        let steps: Vec<u32> = turns.iter().map(|t| t.step).collect();
        assert_eq!(steps, vec![1, 1, 2, 2, 3, 3]);
        assert!(turns.iter().all(|t| !t.is_tb));
    }

    #[test]
    fn parse_bpbpp_and_tb() {
        let turns = BpState::parse_sequence("bpbpp", Side::B).unwrap();
        assert_eq!(turns.len(), 8);
        assert_eq!(turns[0].side, Side::B); // 先手固定
        let t = BpState::parse_sequence("t", Side::A).unwrap();
        assert_eq!(t.len(), 1);
        assert!(t[0].is_tb);
        assert_eq!(t[0].action, BpAction::Pick);
        assert_eq!(t[0].side, Side::A);
    }

    #[test]
    fn parse_rejects_empty_and_bad_char() {
        assert_eq!(BpState::parse_sequence("   ", Side::A), Err(ParseError::EmptySequence));
        assert_eq!(BpState::parse_sequence("bpx", Side::A), Err(ParseError::InvalidChar('x')));
    }

    // ---------------- start：已知坑修复回归 ----------------

    #[test]
    fn start_initializes_interference_points() {
        let mut s = with_sides("bppp", 4);
        assert!(s.players.iter().all(|p| p.interference_points == 0));
        s.start(1000);
        assert_eq!(s.status, BpStatus::InProgress);
        let points = cfg().initial_points;
        assert!(s.players.iter().all(|p| p.interference_points == points));
        assert!(s.timer.is_some());

        // 观众点数不受影响（只有 role=Player 被初始化）
        let mut s2 = with_sides("bppp", 4);
        s2.players.push(PlayerState {
            id: 3,
            name: "吃瓜".into(),
            role: ClientRole::Spectator,
            side: None,
            ready: false,
            connected: true,
            interference_points: 77,
            interference_disabled: false,
        });
        s2.start(1000);
        assert_eq!(s2.players.last().unwrap().interference_points, 77);
        assert_eq!(s2.players[0].interference_points, points);
    }

    // ---------------- apply_operation ----------------

    #[test]
    fn op_requires_turn_side_action_and_available_chart() {
        let mut s = with_sides("bppp", 4);
        assert_eq!(
            s.apply_operation(Side::A, BpAction::Ban, 0, 0),
            Err(OpError::NotInProgress)
        ); // Lobby 不可动
        s.start(1000);
        assert_eq!(s.apply_operation(Side::B, BpAction::Ban, 0, 1000), Err(OpError::NotYourTurn));
        assert_eq!(s.apply_operation(Side::A, BpAction::Pick, 0, 1000), Err(OpError::WrongAction));
        assert_eq!(
            s.apply_operation(Side::A, BpAction::Ban, 999, 1000),
            Err(OpError::ChartUnavailable)
        );
        let op = s.apply_operation(Side::A, BpAction::Ban, 0, 1000).unwrap();
        assert_eq!(op.order_no, 1);
        assert_eq!(s.pool.iter().find(|c| c.id == 0).unwrap().status, ChartStatus::Banned);
    }

    #[test]
    fn op_advances_in_order_and_finishes() {
        let mut s = with_sides("bpb", 8); // 6 turns
        s.start(0);
        let plan = [
            (Side::A, BpAction::Ban),
            (Side::B, BpAction::Ban),
            (Side::A, BpAction::Pick),
            (Side::B, BpAction::Pick),
            (Side::A, BpAction::Ban),
            (Side::B, BpAction::Ban),
        ];
        for (i, (side, action)) in plan.iter().enumerate() {
            let op = s.apply_operation(*side, *action, i as u32, 100 * (i as u64 + 1)).unwrap();
            assert_eq!(op.order_no, (i + 1) as u32);
        }
        assert_eq!(s.status, BpStatus::Finished);
        assert!(s.timer.is_none());
        assert_eq!(
            s.apply_operation(Side::A, BpAction::Ban, 6, 9999),
            Err(OpError::AlreadyFinished)
        );
    }

    // ---------------- validate：全拒绝分支（纯函数） ----------------

    fn facts_with(kind: InterferenceKind, from: Side, to: Side) -> InterferenceFacts {
        InterferenceFacts {
            kind,
            from_side: from,
            target_side: to,
            actor_action: BpAction::Ban,
            is_tb: false,
            from_points: 3,
            target_received_this_round: 0,
            same_round_compress_used: false,
            sender_interference_disabled: false,
            actor_remaining_secs: 30,
            obscure_requested: vec![],
            forced_ban_chart_id: None,
            emote: None,
        }
    }

    #[test]
    fn validate_deny_branches() {
        let c = cfg();
        assert_eq!(
            validate_interference(
                &facts_with(InterferenceKind::CompressTimer, Side::A, Side::B),
                &c
            ),
            Ok(())
        );

        // 全局开关
        let mut c_off = c.clone();
        c_off.enabled = false;
        assert_eq!(
            validate_interference(
                &facts_with(InterferenceKind::CompressTimer, Side::A, Side::B),
                &c_off
            ),
            Err(InterferenceDeny::DisabledGlobally)
        );
        // 种类白名单
        let mut c_none = c.clone();
        c_none.enabled_kinds = vec![];
        assert_eq!(
            validate_interference(
                &facts_with(InterferenceKind::CompressTimer, Side::A, Side::B),
                &c_none
            ),
            Err(InterferenceDeny::KindNotEnabled)
        );
        // 不能打自己
        assert_eq!(
            validate_interference(&facts_with(InterferenceKind::CompressTimer, Side::A, Side::A), &c),
            Err(InterferenceDeny::CannotTargetSelf)
        );
        // 发起方干扰权被禁用
        let f = InterferenceFacts {
            sender_interference_disabled: true,
            ..facts_with(InterferenceKind::CompressTimer, Side::A, Side::B)
        };
        assert_eq!(validate_interference(&f, &c), Err(InterferenceDeny::SideInterferenceDisabled));
        // TB 门控
        let f = InterferenceFacts {
            is_tb: true,
            ..facts_with(InterferenceKind::CompressTimer, Side::A, Side::B)
        };
        assert_eq!(validate_interference(&f, &c), Err(InterferenceDeny::NotAllowedOnTb));
        // 阶段不匹配（ForceReveal 仅 Pick 阶段）
        let f = InterferenceFacts {
            actor_action: BpAction::Ban,
            ..facts_with(InterferenceKind::ForceReveal, Side::A, Side::B)
        };
        assert_eq!(validate_interference(&f, &c), Err(InterferenceDeny::WrongPhase));
        // 点数不足
        let f = InterferenceFacts {
            from_points: 0,
            ..facts_with(InterferenceKind::CompressTimer, Side::A, Side::B)
        };
        assert_eq!(validate_interference(&f, &c), Err(InterferenceDeny::InsufficientPoints));
        // 同回合压缩叠加
        let f = InterferenceFacts {
            same_round_compress_used: true,
            ..facts_with(InterferenceKind::CompressTimer, Side::A, Side::B)
        };
        assert_eq!(validate_interference(&f, &c), Err(InterferenceDeny::CompressAlreadyApplied));
        // 目标承受上限
        let f = InterferenceFacts {
            target_received_this_round: 1,
            ..facts_with(InterferenceKind::CompressTimer, Side::A, Side::B)
        };
        assert_eq!(validate_interference(&f, &c), Err(InterferenceDeny::TargetAlreadySaturated));
        // Emote 轻量层：点数 0 也放行（不受策略约束）
        let f = InterferenceFacts {
            from_points: 0,
            ..facts_with(InterferenceKind::Emote, Side::A, Side::B)
        };
        assert_eq!(validate_interference(&f, &c), Ok(()));
    }

    // ---------------- try_interference ----------------

    #[test]
    fn compress_deducts_points_and_shifts_deadline() {
        let mut s = with_sides("bppp", 4);
        s.start(1000); // deadline = 1000 + 30_000
        let ev = s
            .try_interference(Side::B, Side::A, InterferenceKind::CompressTimer, vec![], None, 2000)
            .unwrap();
        assert!(matches!(ev.params, InterferenceParams::CompressTimer { reduce_secs: 8 }));
        assert_eq!(s.timer.unwrap().deadline_ts_ms, 1000 + 30_000 - 8_000);
        assert_eq!(
            s.player_by_side(Side::B).unwrap().interference_points,
            cfg().initial_points - 1
        );
        // 同回合再压缩 → 叠加拒绝
        assert_eq!(
            s.try_interference(Side::A, Side::B, InterferenceKind::CompressTimer, vec![], None, 2000)
                .unwrap_err(),
            InterferenceDeny::CompressAlreadyApplied
        );
        // 目标（A）已承受 1 次 → 再来一个策略干扰饱和拒绝
        assert_eq!(
            s.try_interference(Side::A, Side::B, InterferenceKind::ObscurePool, vec![1], None, 2100)
                .unwrap_err(),
            InterferenceDeny::TargetAlreadySaturated
        );
    }

    #[test]
    fn compress_respects_floor() {
        let mut s = with_sides("bppp", 4);
        s.start(1000);
        // 剩余仅 6 秒：reduce = min(8, 6-5) = 1
        s.timer.as_mut().unwrap().deadline_ts_ms = 2000 + 6000;
        let ev = s
            .try_interference(Side::B, Side::A, InterferenceKind::CompressTimer, vec![], None, 2000)
            .unwrap();
        assert!(matches!(ev.params, InterferenceParams::CompressTimer { reduce_secs: 1 }));
    }

    #[test]
    fn obscure_truncates_to_max_count() {
        let mut s = with_sides("bppp", 16);
        s.start(1000);
        let ev = s
            .try_interference(
                Side::B,
                Side::A,
                InterferenceKind::ObscurePool,
                vec![0, 2, 4, 6, 8, 10],
                None,
                2000,
            )
            .unwrap();
        match ev.params {
            InterferenceParams::ObscurePool { chart_ids, count } => {
                assert_eq!(count, 3);
                assert_eq!(chart_ids, vec![0, 2, 4]);
            }
            other => panic!("不是遮蔽结果: {other:?}"),
        }
        // 遮蔽也是策略干扰：目标本回合饱和 → B→A 再遮蔽拒绝
        assert_eq!(
            s.try_interference(
                Side::B,
                Side::A,
                InterferenceKind::ObscurePool,
                vec![1],
                None,
                2100
            )
            .unwrap_err(),
            InterferenceDeny::TargetAlreadySaturated
        );
    }

    #[test]
    fn extra_ban_sets_forced_and_must_include() {
        let mut s = with_sides("bppp", 4);
        s.start(1000);
        // step1 turn1：A 先 Ban（推进到 B 的 Ban 回合）
        s.apply_operation(Side::A, BpAction::Ban, 0, 1100).unwrap();
        // A 对当前 Ban 行动方 B 施加追加 Ban 压力（指定曲 1）
        let ev = s
            .try_interference(Side::A, Side::B, InterferenceKind::ExtraBanPressure, vec![], Some(1), 1200)
            .unwrap();
        assert!(matches!(
            ev.params,
            InterferenceParams::ExtraBanPressure { forced_chart_id: 1 }
        ));
        assert_eq!(s.pool.iter().find(|c| c.id == 1).unwrap().status, ChartStatus::ForcedBanCandidate);
        // B 的 Ban 必须包含强制位
        assert_eq!(
            s.apply_operation(Side::B, BpAction::Ban, 2, 1300),
            Err(OpError::MustIncludeForcedBan)
        );
        let op = s.apply_operation(Side::B, BpAction::Ban, 1, 1400).unwrap();
        assert_eq!(op.chart_id, 1);
        assert_eq!(s.pool.iter().find(|c| c.id == 1).unwrap().status, ChartStatus::Banned);
        // 之后不残留：下一 Ban 正常
    }

    #[test]
    fn extra_ban_rejects_bad_chart() {
        let mut s = with_sides("bppp", 4);
        s.start(1000);
        // 未指定 → 拒
        assert_eq!(
            s.try_interference(Side::B, Side::A, InterferenceKind::ExtraBanPressure, vec![], None, 2000)
                .unwrap_err(),
            InterferenceDeny::ExtraBanNotSpecified
        );
        // 指定已 Ban 曲目 → 拒
        s.pool.iter_mut().find(|c| c.id == 1).unwrap().status = ChartStatus::Banned;
        assert_eq!(
            s.try_interference(
                Side::B,
                Side::A,
                InterferenceKind::ExtraBanPressure,
                vec![],
                Some(1),
                2000
            )
            .unwrap_err(),
            InterferenceDeny::ExtraBanChartUnavailable
        );
    }

    #[test]
    fn force_reveal_phase_and_reveal_flow() {
        let mut s = with_sides("bppp", 4);
        s.start(1000);
        // 当前为 Ban 回合 → 门控拒绝
        assert_eq!(
            s.try_interference(Side::A, Side::B, InterferenceKind::ForceReveal, vec![], None, 2000)
                .unwrap_err(),
            InterferenceDeny::WrongPhase
        );

        // Pick 回合内生效 → 挂起 → 目标公开候选
        let mut s2 = with_sides("p", 4);
        s2.start(1000);
        let ev = s2
            .try_interference(Side::A, Side::B, InterferenceKind::ForceReveal, vec![], None, 2000)
            .unwrap();
        // 非目标方不能补全
        assert!(s2.reveal_candidate(Side::A, 3).is_err());
        let revealed = s2.reveal_candidate(Side::B, 3).unwrap();
        assert_eq!(revealed.id, ev.id);
        assert!(matches!(
            revealed.params,
            InterferenceParams::ForceReveal { candidate_chart_id: 3 }
        ));
        // 已无挂起 → 再公开拒绝
        assert!(s2.reveal_candidate(Side::B, 2).is_err());
    }

    // ---------------- counter ----------------

    #[test]
    fn counter_only_target_and_only_once() {
        let mut s = with_sides("bppp", 4);
        s.start(1000);
        let ev = s
            .try_interference(Side::B, Side::A, InterferenceKind::CompressTimer, vec![], None, 2000)
            .unwrap();
        // 攻击方不能反制自己的干扰
        assert!(s.counter_interference(Side::B, ev.id).is_err());
        // 目标反制：扣 1 点
        let pts0 = s.player_by_side(Side::A).unwrap().interference_points;
        let ev2 = s.counter_interference(Side::A, ev.id).unwrap();
        assert!(ev2.countered);
        let pts1 = s.player_by_side(Side::A).unwrap().interference_points;
        assert_eq!(pts1, pts0 - cfg().counter_cost);
        // 已反制过 → 拒
        assert!(s.counter_interference(Side::A, ev.id).is_err());
        // 不存在的干扰 → 拒
        assert!(s.counter_interference(Side::A, 999).is_err());
    }

    // ---------------- 回合状态跨 step 重置 ----------------

    #[test]
    fn round_counters_reset_across_steps() {
        let mut s = with_sides("bpbp", 8); // steps: b b p p
        s.start(1000);
        s.apply_operation(Side::A, BpAction::Ban, 0, 1100).unwrap();
        s.apply_operation(Side::B, BpAction::Ban, 1, 1200).unwrap();
        // step2（A pick 行动）：B 压缩 A
        s.try_interference(Side::B, Side::A, InterferenceKind::CompressTimer, vec![], None, 1300)
            .unwrap();
        s.apply_operation(Side::A, BpAction::Pick, 2, 1400).unwrap();
        s.apply_operation(Side::B, BpAction::Pick, 3, 1500).unwrap();
        // step3（b）：承受计数已重置 → 可再次生效（若未重置将 TargetAlreadySaturated）
        s.try_interference(Side::B, Side::A, InterferenceKind::CompressTimer, vec![], None, 1600)
            .unwrap();
    }

    // ---------------- execute_timeout ----------------

    #[test]
    fn timeout_ban_is_empty_ban() {
        let mut s = with_sides("b", 4);
        s.start(1000);
        let op = s.execute_timeout(10500, None).unwrap();
        assert_eq!(op.action, BpAction::Ban);
        assert_eq!(op.chart_id, 0); // 空 Ban 占位
        assert_eq!(s.current_turn().unwrap().side, Side::B); // 推进到 B 的 Ban
        assert_ne!(s.status, BpStatus::Finished);
    }

    #[test]
    fn timeout_pick_uses_provided_choice() {
        let mut s = with_sides("p", 4);
        s.start(1000);
        let op = s.execute_timeout(10500, Some(2)).unwrap();
        assert_eq!(op.action, BpAction::Pick);
        assert_eq!(op.chart_id, 2);
        assert_eq!(s.pool.iter().find(|c| c.id == 2).unwrap().status, ChartStatus::Picked);

        // 指定曲目不可用 → 落空 Pick（chart_id = 0）
        let mut s2 = with_sides("p", 4);
        s2.start(1000);
        s2.pool.iter_mut().find(|c| c.id == 2).unwrap().status = ChartStatus::Banned;
        let op = s2.execute_timeout(10500, Some(2)).unwrap();
        assert_eq!(op.chart_id, 0);
    }

    // ---------------- 裁定回退 ----------------

    #[test]
    fn rollback_last_operation_realigned() {
        let mut s = with_sides("bp", 4);
        s.start(1000);
        s.apply_operation(Side::A, BpAction::Ban, 0, 1100).unwrap();
        let last = s.operations.last().unwrap().clone();
        let removed = s.arbitrate_rollback(last.order_no, 1200).unwrap();
        assert_eq!(removed.len(), 1); // 回退最近一条 → 只撤 1 条
        assert_eq!(removed[0].order_no, last.order_no);
        assert_eq!(s.pool.iter().find(|c| c.id == 0).unwrap().status, ChartStatus::Available);
        assert_eq!(s.current_index, 0);
        assert!(s.operations.is_empty());
        // 不存在的操作 → 拒
        assert!(s.arbitrate_rollback(99, 1250).is_err());
        // 回退后重新 Ban 依旧合法
        s.apply_operation(Side::A, BpAction::Ban, 1, 1300).unwrap();
    }

    #[test]
    fn rollback_cascades_from_earlier() {
        let mut s = with_sides("bpb", 4); // 6 turns：b(2)+p(2)+b(2)
        s.start(1000);
        s.apply_operation(Side::A, BpAction::Ban, 0, 1100).unwrap();
        s.apply_operation(Side::B, BpAction::Ban, 1, 1200).unwrap();
        s.apply_operation(Side::A, BpAction::Pick, 2, 1300).unwrap();
        assert_eq!(s.current_index, 3);
        let first = s.operations.first().unwrap().clone();
        let removed = s.arbitrate_rollback(first.order_no, 1400).unwrap();
        // 级联：目标（首条）连同其后全部共 3 条；时间序目标在首
        assert_eq!(removed.len(), 3);
        assert_eq!(removed[0].order_no, first.order_no);
        assert_eq!(removed[2].order_no, 3);
        // 全部曲目回到 Available；索引精确对回 0；操作记录清空
        assert!(s.pool.iter().all(|c| c.status == ChartStatus::Available));
        assert_eq!(s.current_index, 0);
        assert!(s.operations.is_empty());
        // 状态回 InProgress 且已按裁定时刻重新计时（定时器重新武装）
        assert_eq!(s.status, BpStatus::InProgress);
        assert!(s.timer.unwrap().deadline_ts_ms >= 1400);
        // 回退后从对位回合重新走依旧合法（turn 0 = A ban）
        s.apply_operation(Side::A, BpAction::Ban, 3, 1500).unwrap();
    }

    #[test]
    fn rollback_then_replay_finishes_consistently() {
        let mut s = with_sides("bppp", 6); // 2 bans + 4 picks 恰耗尽 6 曲
        s.start(1000);
        s.apply_operation(Side::A, BpAction::Ban, 0, 1100).unwrap();
        s.apply_operation(Side::B, BpAction::Ban, 1, 1200).unwrap();
        s.apply_operation(Side::A, BpAction::Pick, 2, 1300).unwrap();
        s.apply_operation(Side::B, BpAction::Pick, 3, 1400).unwrap();
        s.apply_operation(Side::A, BpAction::Pick, 4, 1500).unwrap();
        assert_eq!(s.current_index, 5);
        // 回退第 2 条（B 的 ban）：其后 4 条操作级联撤销
        let target = s.operations[1].order_no;
        let removed = s.arbitrate_rollback(target, 1600).unwrap();
        assert_eq!(removed.len(), 4);
        assert_eq!(s.operations.len(), 1);
        assert_eq!(s.current_index, 1);
        // operations 与 turns 严格 1:1：留存的 op0 对位 turns[0]，turns[1..] 共
        // 5 个回合（bppp = 6 turns）需原样重放
        let replay: Vec<(Side, BpAction)> =
            s.turns[1..].iter().map(|t| (t.side, t.action)).collect();
        for (i, (side, action)) in replay.into_iter().enumerate() {
            let chart = s.available_chart_ids()[0];
            s.apply_operation(side, action, chart, 1700 + i as u64).unwrap();
        }
        // 状态自洽收口：Finished、记录对位完整（1 条留存 + 5 条重放）
        assert_eq!(s.status, BpStatus::Finished);
        assert_eq!(s.current_index, 6);
        assert_eq!(s.operations.len(), 6);
        // order_no 全局自增不回收：留下的首条仍是 #1，重放段从 #6 起（审计可溯）
        assert_eq!(s.operations[0].order_no, 1);
        assert_eq!(s.operations[1].order_no, 6);
    }

    // ---------------- 干扰回滚（主办方） ----------------

    #[test]
    fn interference_rollback_refunds_and_restores() {
        let mut s = with_sides("bppp", 4);
        s.start(1000);
        let ev = s
            .try_interference(Side::B, Side::A, InterferenceKind::CompressTimer, vec![], None, 2000)
            .unwrap();
        let before = s.timer.unwrap().deadline_ts_ms;
        let rolled = s.rollback_interference(ev.id).unwrap();
        assert!(rolled.rolled_back && rolled.by_host);
        assert_eq!(s.timer.unwrap().deadline_ts_ms, before + 8_000);
        assert_eq!(s.player_by_side(Side::B).unwrap().interference_points, cfg().initial_points);
        // 再次回滚 → 拒
        assert!(s.rollback_interference(ev.id).is_err());
    }

    // ---------------- legal_intent 粗校验冒烟 ----------------

    #[test]
    fn legal_intent_smoke() {
        let mut s = with_sides("bppp", 4);
        s.start(1000);
        let a = s.player_by_side(Side::A).unwrap().clone();
        let b = s.player_by_side(Side::B).unwrap().clone();
        // 非行动方 Ban → 拒
        assert!(s.legal_intent(&b, &ClientIntent::Ban { chart_id: 0 }).is_err());
        // 行动方 Ban → 粗校验放行（细校验在 apply_operation）
        assert!(s.legal_intent(&a, &ClientIntent::Ban { chart_id: 0 }).is_ok());
        // 自指干扰 → 拒
        assert!(s
            .legal_intent(
                &a,
                &ClientIntent::UseInterference {
                    kind: InterferenceKind::CompressTimer,
                    target_side: Side::A,
                    obscure_chart_ids: None,
                    forced_ban_chart_id: None,
                }
            )
            .is_err());
        // BP 开始后 SetReady 拒（Lobby 专属）
        assert!(s.legal_intent(&a, &ClientIntent::SetReady { ready: true }).is_err());
        // 观众 Ban → 拒
        let mut s2 = with_sides("bppp", 4);
        s2.start(1000);
        let spect = PlayerState {
            id: 9,
            name: "观众".into(),
            role: ClientRole::Spectator,
            side: None,
            ready: false,
            connected: true,
            interference_points: 0,
            interference_disabled: false,
        };
        assert!(s2.legal_intent(&spect, &ClientIntent::Ban { chart_id: 0 }).is_err());
        // 观众弹幕允许
        assert!(s2.legal_intent(&spect, &ClientIntent::SendEmote { emote: "👍".into(), text: None }).is_ok());
    }
}
