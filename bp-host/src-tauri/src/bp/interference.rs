//! bp/interference.rs —— 干扰系统：独立资源与效果层。
//!
//! 设计要点（对应需求「干扰机制」一节）：
//! - 干扰建模为独立资源（干扰点）与效果系统，与 BP 状态机解耦，但受**阶段门控**；
//! - 所有干扰在主机侧原子化校验 + 应用 + 广播；客户端不自行模拟结果；
//! - 干扰点不回复，用尽即无；每回合每方最多承受一次策略干扰；可被反制；
//! - `Emote`（表情弹幕）是轻量社交层，不消耗干扰点，贯穿全程但不受策略约束。

use serde::{Deserialize, Serialize};

use crate::model::{BpAction, Side};

/// 干扰类型（可配置启用）。序列化用 camelCase，与协议其它枚举一致。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum InterferenceKind {
    /// 压缩倒计时：减少当前行动方本回合剩余时间（不得低于安全下限）。同回合不可叠加。
    CompressTimer,
    /// 遮蔽曲池：在对方视角中暂时隐藏若干曲目（逻辑上仍可选，只影响渲染）。
    ObscurePool,
    /// 强制公开：要求对方在本回合 Pick 确认前先公开一首候选意向（不可撤回，但可改选）。
    ForceReveal,
    /// 追加 Ban 压力：在对方 Ban 回合追加一个「必须 Ban 的强制位」（由干扰方指定一首）。
    ExtraBanPressure,
    /// 表情 / 弹幕：轻量社交层，不消耗干扰点。
    Emote,
}

impl InterferenceKind {
    pub const ALL: [InterferenceKind; 5] = [
        Self::CompressTimer,
        Self::ObscurePool,
        Self::ForceReveal,
        Self::ExtraBanPressure,
        Self::Emote,
    ];

    /// 中文名，用于 UI 标签、日志、弹幕提示。
    pub fn name(self) -> &'static str {
        match self {
            Self::CompressTimer => "压缩倒计时",
            Self::ObscurePool => "遮蔽曲池",
            Self::ForceReveal => "强制公开",
            Self::ExtraBanPressure => "追加 Ban 压力",
            Self::Emote => "表情弹幕",
        }
    }

    /// 是否为「策略干扰」：受点数 / 承受上限 / 反制约束。`Emote` 是轻量层，不计入。
    pub fn is_strategic(self) -> bool {
        self != Self::Emote
    }

    /// 消耗的干扰点。`Emote` 默认 0。
    pub fn cost(self) -> u8 {
        match self {
            Self::Emote => 0,
            _ => 1,
        }
    }

    /// 阶段门控默认值：该干扰可在哪些 BP 动作阶段使用。
    /// 返回空切片表示该类型默认不允许（需配置显式打开，或本工具策略禁止）。
    pub fn default_allowed_actions(self) -> &'static [BpAction] {
        match self {
            Self::CompressTimer => &[BpAction::Ban, BpAction::Pick],
            Self::ObscurePool => &[BpAction::Ban, BpAction::Pick],
            Self::ForceReveal => &[BpAction::Pick],
            Self::ExtraBanPressure => &[BpAction::Ban],
            Self::Emote => &[BpAction::Ban, BpAction::Pick],
        }
    }

    /// 是否默认允许在 TB（加赛）回合使用。默认仅 `Emote` 允许。
    pub fn allowed_on_tb_by_default(self) -> bool {
        matches!(self, Self::Emote)
    }
}

/// 压缩倒计时参数。
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CompressTimerConfig {
    /// 每次减少的秒数
    pub reduce_secs: u8,
    /// 倒计时安全下限（秒）：压缩后剩余不得低于此值
    pub floor_secs: u8,
}

/// 遮蔽曲池参数。
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ObscurePoolConfig {
    /// 单次最多遮蔽曲目数
    pub max_count: u8,
}

/// 观众层权限（轻量层）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SpectatorConfig {
    /// 观众能否发送表情弹幕
    pub can_emote: bool,
    /// 观众表情频率上限（条 / 秒）
    pub emote_per_sec: u8,
}

/// # 干扰系统可配置清单（交付要求二）
///
/// 全部由主机持有并下发给客户端，*禁止客户端自行决定*。主办方可 在赛事配置中
/// 禁用某类干扰或整体关闭。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct InterferenceConfig {
    /// 总开关
    pub enabled: bool,
    /// 每位参赛者初始干扰点（默认 2-3）
    pub initial_points: u8,
    /// 每回合每方最多承受几次「策略干扰」（默认 1）
    pub max_strategic_per_round_per_side: u8,
    /// 反制成本：反制方消耗点数（默认 1）
    pub counter_cost: u8,
    /// 是否允许在 TB 回合使用策略干扰（默认 false）
    pub allow_on_tb: bool,
    /// 启用的干扰种类白名单
    pub enabled_kinds: Vec<InterferenceKind>,
    /// 压缩倒计时参数
    pub compress: CompressTimerConfig,
    /// 遮蔽曲池参数
    pub obscure: ObscurePoolConfig,
    /// 观众层权限
    pub spectator: SpectatorConfig,
}

impl Default for InterferenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            initial_points: 3,
            max_strategic_per_round_per_side: 1,
            counter_cost: 1,
            allow_on_tb: false,
            enabled_kinds: vec![
                InterferenceKind::CompressTimer,
                InterferenceKind::ObscurePool,
                InterferenceKind::ForceReveal,
                InterferenceKind::ExtraBanPressure,
                InterferenceKind::Emote,
            ],
            compress: CompressTimerConfig {
                reduce_secs: 8,
                floor_secs: 5,
            },
            obscure: ObscurePoolConfig { max_count: 3 },
            spectator: SpectatorConfig {
                can_emote: true,
                emote_per_sec: 2,
            },
        }
    }
}

/// 干扰效果的具体参数（随类型不同）。随 `InterferenceEvent` 广播给受影响客户端渲染。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "k", rename_all = "camelCase")]
#[derive(PartialEq)]
pub enum InterferenceParams {
    CompressTimer { reduce_secs: u8 },
    ObscurePool { chart_ids: Vec<u32>, count: u8 },
    ForceReveal { candidate_chart_id: u32 },
    ExtraBanPressure { forced_chart_id: u32 },
    Emote { emote: String, text: Option<String> },
}

/// 一次干扰请求的「事实输入」：由状态机在仲裁时从权威状态组装。
#[derive(Clone, Debug)]
pub struct InterferenceFacts {
    pub kind: InterferenceKind,
    pub from_side: Side,
    pub target_side: Side,
    /// 当前回合动作（阶段门控用）
    pub actor_action: BpAction,
    /// 当前是否为 TB 回合
    pub is_tb: bool,
    /// 发起方剩余干扰点
    pub from_points: u8,
    /// 目标方本回合已承受策略干扰次数
    pub target_received_this_round: u8,
    /// 同回合是否已对同一行动方压缩过倒计时
    pub same_round_compress_used: bool,
    /// 发起方干扰权是否被主办方禁用
    pub sender_interference_disabled: bool,
    /// 压缩时行动方当前剩余秒数（用于尊重 floor；u64 防止长回合截断）
    pub actor_remaining_secs: u64,
    /// 遮蔽请求的具体曲目（干扰方指定对方看不到的曲目）
    pub obscure_requested: Vec<u32>,
    /// 追加 Ban 压力：干扰方指定的强制 Ban 曲目
    pub forced_ban_chart_id: Option<u32>,
    /// 表情弹幕内容
    pub emote: Option<(String, Option<String>)>,
}

/// 拒绝原因：非法意图返回*错误事件*而非静默忽略（交付要求一）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterferenceDeny {
    DisabledGlobally,
    KindNotEnabled,
    CannotTargetSelf,
    WrongPhase,
    NotAllowedOnTb,
    InsufficientPoints,
    TargetAlreadySaturated,
    CompressAlreadyApplied,
    SideInterferenceDisabled,
    // ---- 以下三项依赖曲池（主机权威状态），故在 `try_interference` 校验而非纯函数 `validate` ----
    /// 追加 Ban 压力未指定强制 Ban 曲目
    ExtraBanNotSpecified,
    /// 追加 Ban 压力指定的曲目不可用（不存在 / 已 Ban / 已 Pick）
    ExtraBanChartUnavailable,
    /// 遮蔽请求中的曲目全部不可用（不存在或状态非 Available）
    ObscureChartsUnavailable,
}

impl InterferenceDeny {
    pub fn message(&self) -> &'static str {
        match self {
            Self::DisabledGlobally => "干扰系统已关闭",
            Self::KindNotEnabled => "该干扰类型未启用",
            Self::CannotTargetSelf => "不能对自己使用干扰",
            Self::WrongPhase => "当前回合阶段不可用该干扰",
            Self::NotAllowedOnTb => "TB 回合不允许使用该干扰",
            Self::InsufficientPoints => "干扰点不足",
            Self::TargetAlreadySaturated => "目标本回合承受干扰已达上限",
            Self::CompressAlreadyApplied => "同回合不可叠加压缩倒计时",
            Self::SideInterferenceDisabled => "你的干扰权已被主办方禁用",
            Self::ExtraBanNotSpecified => "追加 Ban 压力必须指定一首强制 Ban 曲目",
            Self::ExtraBanChartUnavailable => "指定的强制 Ban 曲目不可用",
            Self::ObscureChartsUnavailable => "遮蔽请求中没有任何可用的曲目",
        }
    }
}

/// 阶段门控 + 资源 + 反制前提的原子化校验。纯函数，便于单测。
///
/// 规则（对应需求「反制与公平约束」「干扰的时间线与审计」）：
/// 1. 全局开关 / 种类白名单；
/// 2. 不能对自己使用；
/// 3. `Emote` 轻量层：不受点数 / 承受上限 / 反制约束（仍受全局与种类开关）；
/// 4. 策略干扰：发起方干扰权未被禁用；TB 回合需 `allow_on_tb`；阶段需匹配；
///    点数充足；同回合压缩不叠加；目标本回合承受未达上限。
pub fn validate(f: &InterferenceFacts, cfg: &InterferenceConfig) -> Result<(), InterferenceDeny> {
    if !cfg.enabled {
        return Err(InterferenceDeny::DisabledGlobally);
    }
    if !cfg.enabled_kinds.contains(&f.kind) {
        return Err(InterferenceDeny::KindNotEnabled);
    }
    if f.from_side == f.target_side {
        return Err(InterferenceDeny::CannotTargetSelf);
    }

    // 轻量层：不受策略约束
    if f.kind == InterferenceKind::Emote {
        return Ok(());
    }

    if f.sender_interference_disabled {
        return Err(InterferenceDeny::SideInterferenceDisabled);
    }
    if f.is_tb && !cfg.allow_on_tb {
        return Err(InterferenceDeny::NotAllowedOnTb);
    }
    if !f.kind.default_allowed_actions().contains(&f.actor_action) {
        return Err(InterferenceDeny::WrongPhase);
    }
    if f.from_points < f.kind.cost() {
        return Err(InterferenceDeny::InsufficientPoints);
    }
    if f.kind == InterferenceKind::CompressTimer && f.same_round_compress_used {
        return Err(InterferenceDeny::CompressAlreadyApplied);
    }
    if f.target_received_this_round >= cfg.max_strategic_per_round_per_side {
        return Err(InterferenceDeny::TargetAlreadySaturated);
    }
    Ok(())
}

/// 校验通过后，计算效果参数并扣减资源。
///
/// 原子化：调用方（状态机 / 主机服务）应在同一把锁内完成「状态变更 + 日志 + 广播」，
/// 客户端不能自行模拟结果。
pub fn apply(f: &InterferenceFacts, cfg: &InterferenceConfig) -> AppliedInterference {
    let (params, consumed) = match f.kind {
        InterferenceKind::CompressTimer => {
            // 尊重安全下限：实际减少 = min(配置减少, 剩余 - floor)
            let max_reduce = f
                .actor_remaining_secs
                .saturating_sub(u64::from(cfg.compress.floor_secs));
            let reduce_secs = max_reduce.min(u64::from(cfg.compress.reduce_secs)) as u8;
            (InterferenceParams::CompressTimer { reduce_secs }, 1)
        }
        InterferenceKind::ObscurePool => {
            let count = (f.obscure_requested.len() as u8).min(cfg.obscure.max_count);
            let chart_ids: Vec<u32> = f
                .obscure_requested
                .iter()
                .take(count as usize)
                .copied()
                .collect();
            (InterferenceParams::ObscurePool { chart_ids, count }, 1)
        }
        InterferenceKind::ForceReveal => {
            // 候选由目标方稍后通过 RevealCandidate 上报；此处仅占位，待目标公开后补全。
            (InterferenceParams::ForceReveal { candidate_chart_id: 0 }, 1)
        }
        InterferenceKind::ExtraBanPressure => {
            let forced = f.forced_ban_chart_id.unwrap_or(0);
            (
                InterferenceParams::ExtraBanPressure {
                    forced_chart_id: forced,
                },
                1,
            )
        }
        InterferenceKind::Emote => {
            let (emote, text) = f
                .emote
                .clone()
                .unwrap_or_else(|| ("👍".to_string(), None));
            (InterferenceParams::Emote { emote, text }, 0)
        }
    };
    AppliedInterference {
        params,
        consumed_points: consumed,
    }
}

/// 校验 + 应用的结果。
#[derive(Clone, Debug)]
pub struct AppliedInterference {
    pub params: InterferenceParams,
    pub consumed_points: u8,
}
