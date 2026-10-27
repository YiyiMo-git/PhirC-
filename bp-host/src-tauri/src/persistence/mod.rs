//! persistence —— SQLite 持久化（会话状态 + 统一事件时间线 + 连接审计）。

pub mod sqlite;

pub use sqlite::SqliteStore;
