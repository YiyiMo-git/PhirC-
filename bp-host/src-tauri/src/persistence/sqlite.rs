//! persistence/sqlite.rs —— SQLite 持久化（交付要求三）。
//!
//! 表设计与用途（对应需求「时间线与审计」「崩溃恢复」）：
//! - `sessions`   一行 = 一次会话（room_code 主键）。`state_json` 直接存完整
//!   序列化的 `BpState`：崩溃恢复 = 反序列化 + 少量修补（connected=false 等），
//!   避免散表重建中间态的复杂度。
//! - `events`     bp 操作 / 干扰 / 主办方裁定 / 连接 / 错误的**统一时间线**
//!   （审计与结果导出的数据源；payload 即各协议事件的 JSON）。
//! - `connections` 连接审计单独成表（进房 / 离开 / 踢出，按人可查）。
//!
//! 写入策略：主机侧每次原子变更（操作确认 / 干扰应用 / 裁定命令 / 连接变化）
//! **先落库再广播**（store-then-broadcast）。WAL + NORMAL 同步 + 单文件短事务；
//! 局域网赛事工具的写入频度（每次 BP 数十起）无需异步队列。
//!
//! rusqlite 是同步驱动：在 tokio 任务里同步调用（持有 `std::Mutex<Connection>`）。
//! 事务耗时为微秒级，不构成 runtime 卡顿；如未来量级变化再迁 `spawn_blocking`。

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::bp::state_machine::BpState;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    room_code     TEXT    PRIMARY KEY,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    status        TEXT    NOT NULL,
    sequence      TEXT    NOT NULL,
    state_json    TEXT    NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    room_code TEXT    NOT NULL,
    at_ms     INTEGER NOT NULL,
    kind      TEXT    NOT NULL,
    summary   TEXT    NOT NULL DEFAULT '',
    payload   TEXT    NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_events_room_time ON events(room_code, at_ms);
CREATE TABLE IF NOT EXISTS connections (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    room_code          TEXT    NOT NULL,
    player_id          INTEGER NOT NULL,
    name               TEXT    NOT NULL,
    role               TEXT    NOT NULL,
    side               TEXT,
    connected          INTEGER NOT NULL,
    connected_at_ms    INTEGER NOT NULL,
    disconnected_at_ms INTEGER
);
"#;

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("创建数据目录 {}: {e}", dir.display()))?;
            }
        }
        let conn = Connection::open(path).map_err(|e| format!("打开 SQLite {}: {e}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| format!("设置 WAL 失败: {e}"))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| format!("设置 synchronous 失败: {e}"))?;
        conn.execute_batch(SCHEMA).map_err(|e| format!("建表失败: {e}"))?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    // ------------------------------------------------------------ 会话状态

    /// 保存当前会话（同一 room_code upsert 一行）。
    pub fn save_state(&self, state: &BpState) -> Result<(), String> {
        let json = serde_json::to_string(state).map_err(|e| e.to_string())?;
        let status = serde_json::to_string(&state.status).unwrap_or_else(|_| "\"lobby\"".into());
        let status = status.trim_matches('"').to_string();
        let now = crate::service::now_ms();
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO sessions(room_code, created_at_ms, updated_at_ms, status, sequence, state_json)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(room_code) DO UPDATE SET
                updated_at_ms = excluded.updated_at_ms,
                status        = excluded.status,
                sequence      = excluded.sequence,
                state_json    = excluded.state_json",
            params![state.room_code, now, now, status, state.sequence, json],
        )
        .map_err(|e| format!("保存会话失败: {e}"))?;
        Ok(())
    }

    /// 崩溃恢复入口：同房间码未完结的会话。
    pub fn load_unfinished(&self, room_code: &str) -> Result<Option<BpState>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT state_json FROM sessions WHERE room_code = ?1 AND status != 'finished'")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map(params![room_code], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        match rows.next() {
            None => Ok(None),
            Some(row) => {
                let json = row.map_err(|e| e.to_string())?;
                let state: BpState = serde_json::from_str(&json)
                    .map_err(|e| format!("会话状态损坏: {e}"))?;
                Ok(Some(state))
            }
        }
    }

    // ------------------------------------------------------------ 时间线

    /// 统一时间线落行。`kind` 约定：
    /// operation / interference / counter / host_command / phase / connection /
    /// emote / timeout / session_end / notice / error。
    pub fn append_event(
        &self,
        room_code: &str,
        at_ms: u64,
        kind: &str,
        summary: &str,
        payload: &Value,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO events(room_code, at_ms, kind, summary, payload) VALUES(?1,?2,?3,?4,?5)",
            params![room_code, at_ms, kind, summary, payload.to_string()],
        )
        .map_err(|e| format!("落事件失败: {e}"))?;
        Ok(())
    }

    // ------------------------------------------------------------ 连接审计

    /// 记录一次「进入连接 / 断开」。
    #[allow(clippy::too_many_arguments)]
    pub fn append_connection(
        &self,
        room_code: &str,
        player_id: u32,
        name: &str,
        role: &str,
        side: Option<&str>,
        connected: bool,
        at_ms: u64,
    ) -> Result<(), String> {
        let dir: i64 = connected.into();
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO connections(room_code, player_id, name, role, side, connected, connected_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![room_code, player_id, name, role, side, dir, at_ms],
        )
        .map_err(|e| format!("落连接审计失败: {e}"))?;
        // 断开则回填最近一行 connected=1 的 disconnected_at_ms
        if !connected {
            conn.execute(
                "UPDATE connections
                 SET disconnected_at_ms = ?3
                 WHERE id = (
                    SELECT id FROM connections
                    WHERE room_code = ?1 AND player_id = ?2 AND connected = 1 AND disconnected_at_ms IS NULL
                    ORDER BY id DESC LIMIT 1)",
                params![room_code, player_id, at_ms],
            )
            .map_err(|e| format!("回填断开时间失败: {e}"))?;
            conn.execute(
                "UPDATE connections SET connected = 0
                 WHERE room_code = ?1 AND player_id = ?2 AND connected = 1 AND disconnected_at_ms IS NOT NULL",
                params![room_code, player_id],
            )
            .map_err(|e| format!("回填连接态失败: {e}"))?;
        }
        Ok(())
    }
}
