//! net —— 传输与网络层。
//!
//! - `protocol`：版本化协议契约（C→S 意图 / S→C 事件，WS JSON 文本帧）。
//! - `server`：axum HTTP（静态前端 + `/ws` 升级 + `/api/status`）。
//! - `session`：连接级会话管理（身份绑定、心跳记录、表情限频、踢出）。

pub mod protocol;
pub mod server;
pub mod session;

pub use protocol::*;
