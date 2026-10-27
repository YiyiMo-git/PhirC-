//! 共享领域原语（无依赖叶子模块）。
//!
//! 放在独立模块是为了打破 `net` 与 `bp` 之间的循环依赖：
//! `net/protocol` 需要 `BpAction/Side/BpStatus`，`bp/state_machine` 需要 `protocol` 的消息体，
//! 二者都从本模块取基础枚举，形成无环的 `model → {interference, protocol} → state_machine`。

use serde::{Deserialize, Serialize};

/// 选手方：A / B。先手方在 BP 开始前固定。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Side {
    A,
    B,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::A => Side::B,
            Side::B => Side::A,
        }
    }
}

/// BP 单步动作。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum BpAction {
    Ban,
    Pick,
}

impl BpAction {
    pub fn name(self) -> &'static str {
        match self {
            BpAction::Ban => "禁用",
            BpAction::Pick => "选曲",
        }
    }
}

/// BP 整体阶段。状态权威在主机，客户端只渲染广播结果。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BpStatus {
    /// 房间已开，等待选手就位 / 主办方开始
    Lobby,
    /// BP 进行中
    InProgress,
    /// 主办方手动暂停（不断线、不重置，仅停表）
    Paused,
    /// BP 结束，可导出结果
    Finished,
}

/// 回合序列展开后的单个「行动位」。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct BpTurn {
    pub action: BpAction,
    pub side: Side,
    /// 是否为 TB / 加赛回合（阶段门控用）
    pub is_tb: bool,
    /// 所属步骤序号（b/p/t 计数，1-based，用于「当前第几步 / 共几步」）
    pub step: u32,
}

/// 回合序列解析错误（非法字符或空序列）。
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    EmptySequence,
    InvalidChar(char),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::EmptySequence => write!(f, "回合序列为空"),
            ParseError::InvalidChar(c) => write!(f, "回合序列含非法字符: '{c}'（仅允许 b/p/t）"),
        }
    }
}

impl std::error::Error for ParseError {}
