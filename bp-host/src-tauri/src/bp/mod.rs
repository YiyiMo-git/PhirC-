//! bp —— BP 核心域（回合状态机 + 干扰系统）。
//!
//! 依赖链无环：`model`（叶子）→ `interference` / `net::protocol` → `state_machine`。
//! 状态权威全在主机侧；客户端只渲染广播结果。

pub mod interference;
pub mod state_machine;

pub use interference::{InterferenceConfig, InterferenceDeny, InterferenceKind, InterferenceParams};
pub use state_machine::{
    BpState, IntentRejected, OpError, ResumeTimerMode, TimeoutAction, DEFAULT_TURN_TIMEOUT_MS,
};
