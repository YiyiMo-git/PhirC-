//! config.rs —— 全部运行参数的唯一来源。
//!
//! 硬约束（handoff 第 6 节）：所有网络地址 / 端口从配置读取，**禁止硬编码**；
//! 远程公网非默认能力，UI 提供「端口转发 / 中继说明」入口。
//!
//! 读取顺序：
//! 1. 环境变量 `BP_HOST_CONFIG` 指向的 JSON 文件；
//! 2. 可执行文件同目录 `bp-host.config.json`；
//! 3. 拷贝 `config.example.json` 作为起点。
//!
//! standalone（无 Tauri 外壳）模式下，配置文件缺失即退出并给出明确指引；
//! Tauri 模式下允许用内置默认值启动，再由主机 UI 编辑后写回同一份文件。
//!
//! 字段级 serde default 保证「缺字段可容忍」，但**不降低硬约束**：端口非 0、
//! 监听地址必须是合法 IP、房间码必须是 4–6 位数字。

use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::bp::interference::InterferenceConfig;
use crate::model::Side;

/// Pick 回合超时后的裁决行为。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TimeoutPickBehavior {
    /// 随机选一首可用曲目（默认）。曲池已空时落「空 Pick」。
    #[default]
    RandomPick,
    /// 判对方获得选择权：本位落空（空 Pick + 时间线提示）。
    /// v1 简化：不做「跳过本 turn 交给对方行动」的推进魔法，落空即由对方继续；
    /// 完整 turn-skip 语义列为后续任务。
    DeferToOpponent,
}

/// # 主机配置。（交付要求五：全部从配置读取，禁止硬编码）
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct BpHostConfig {
    // ---- 网络（监听地址/端口必须来自配置或 UI 编辑写回，禁止硬编码） ----
    /// 监听 IP（如 0.0.0.0）；端口单列，便于 UI 明示「端口转发」话术。
    pub http_bind_addr: String,
    /// HTTP + WebSocket 端口（1–65535）
    pub http_port: u16,
    /// 公网暴露时给客户端展示的提示地址（可为空；UI 只展示，不做自动内网穿透魔法）
    pub public_hint_url: Option<String>,

    // ---- 房间 ----
    /// 房间码（4–6 位数字 PIN）。空串 = 按位数随机生成。
    pub room_code: String,
    /// 自动生成房间码的位数（4–6）
    pub room_code_digits: u8,

    // ---- BP ----
    /// 回合序列（b/p/t）
    pub sequence: String,
    /// 先手方（固定，不随意切换）
    pub first_side: Side,
    /// 单回合倒计时（毫秒）
    pub turn_timeout_ms: u64,
    /// 计时广播间隔（需求建议 250ms~1s）
    pub timer_sync_interval_ms: u64,
    /// 暂停→恢复：true = 本回合重新计时；false = 按剩余时间续算（默认）
    pub resume_timer_reset: bool,
    /// Pick 超时行为（随机 / 判对手）
    pub timeout_pick: TimeoutPickBehavior,

    // ---- 持久化 / 恢复 ----
    /// 崩溃恢复开关：同房间码未完结会话重启后恢复 BP（倒计时按 resume 策略处理）
    pub auto_recover: bool,
    /// SQLite 路径；None → <data_dir>/bp-host.db
    pub db_path: Option<String>,
    /// 数据目录；None → 可执行文件同目录
    pub data_dir: Option<String>,

    // ---- 前端 ----
    /// 静态前端目录（相对路径按工作目录解析）
    pub frontend_dir: String,

    // ---- 干扰系统（主机持有并下发；客户端禁止自行决定） ----
    pub interference: InterferenceConfig,
}

impl Default for BpHostConfig {
    fn default() -> Self {
        Self {
            // 空 IP 不可用 —— 默认仍取标准全零监听，正式部署必须由配置/UI 显式给出。
            // 这里仅作为「拷贝 example 后一定能起来」的兜底，standalone 缺文件会直接退出。
            http_bind_addr: "0.0.0.0".to_string(),
            http_port: 7878,
            public_hint_url: None,
            room_code: String::new(),
            room_code_digits: 6,
            sequence: "bppppt".to_string(),
            first_side: Side::A,
            turn_timeout_ms: 30_000,
            timer_sync_interval_ms: 500,
            resume_timer_reset: false,
            timeout_pick: TimeoutPickBehavior::RandomPick,
            auto_recover: true,
            db_path: None,
            data_dir: None,
            frontend_dir: "frontend".to_string(),
            interference: InterferenceConfig::default(),
        }
    }
}

impl BpHostConfig {
    /// 校验（硬约束落地点）。返回错误消息供 UI / 启动日志直接展示。
    /// 注意：本函数**只读**；若需由显式 room_code 反推 digits，请先调用 [`Self::normalize`]。
    pub fn validate(&self) -> Result<(), String> {
        let addr: IpAddr = self
            .http_bind_addr
            .parse()
            .map_err(|_| format!("http_bind_addr 非法 IP：{}", self.http_bind_addr))?;
        if self.http_port == 0 {
            return Err("http_port 不能为 0".into());
        }
        let _ = addr; // 合法性已确认
        if self.room_code_digits < 4 || self.room_code_digits > 6 {
            return Err("room_code_digits 必须在 4–6 之间（PIN 位数）".into());
        }
        if !self.room_code.is_empty() {
            let digits = self.room_code.chars().count();
            if digits < 4
                || digits > 6
                || !self.room_code.chars().all(|c| c.is_ascii_digit())
            {
                return Err(format!(
                    "room_code 必须是 4–6 位数字 PIN，当前：{:?}",
                    self.room_code
                ));
            }
            if self.room_code_digits != digits as u8 {
                return Err(format!(
                    "room_code_digits（{}）与 room_code 位数（{digits}）不一致，请任选其一填写",
                    self.room_code_digits
                ));
            }
        }
        if self.turn_timeout_ms < 1_000 {
            return Err("turn_timeout_ms 至少 1000ms".into());
        }
        if self.timer_sync_interval_ms < 100 || self.timer_sync_interval_ms > 2_000 {
            return Err("timer_sync_interval_ms 需在 100–2000ms（需求建议 250ms~1s）".into());
        }
        if self.sequence.is_empty() {
            return Err("sequence 不能为空（b/p/t）".into());
        }
        Ok(())
    }

    /// 由显式 room_code 反推 room_code_digits（加载阶段调用一次，prior to validate）。
    pub fn normalize(&mut self) {
        if !self.room_code.is_empty() {
            self.room_code_digits = self.room_code.chars().count() as u8;
        }
    }

    /// 解析后的监听地址形如 "0.0.0.0:7878"。
    pub fn bind_target(&self) -> String {
        // validate() 已保证 addr 与 port 合法；此处仅拼装，不做硬编码。
        format!("{}:{}", self.http_bind_addr, self.http_port)
    }

    /// 实际使用的房间码（配置非空则原样；否则按位数生成）。
    pub fn resolved_room_code(&self) -> String {
        if self.room_code.is_empty() {
            generate_room_code(self.room_code_digits)
        } else {
            self.room_code.clone()
        }
    }

    /// 恢复模式下重算截止时间用：resume 策略转状态机枚举。
    pub fn resume_mode(&self) -> crate::bp::state_machine::ResumeTimerMode {
        if self.resume_timer_reset {
            crate::bp::state_machine::ResumeTimerMode::Reset
        } else {
            crate::bp::state_machine::ResumeTimerMode::Continue
        }
    }
}

// ------------------------------------------------------------------ 加载

/// 配置文件查找顺序：`BP_HOST_CONFIG` 环境变量 → 可执行文件同目录。
pub fn default_config_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("BP_HOST_CONFIG") {
        if !p.is_empty() {
            return std::path::PathBuf::from(p);
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("bp-host.config.json")))
        .unwrap_or_else(|| std::path::PathBuf::from("bp-host.config.json"))
}

/// 严格加载：standalone 模式使用（缺文件 = 明确报错退出，禁止静默硬编码兜底）。
pub fn load_exact(path: &std::path::Path) -> Result<BpHostConfig, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读取配置失败 {}: {e}（请从 config.example.json 拷贝一份）", path.display()))?;
    let mut cfg: BpHostConfig =
        serde_json::from_str(&text).map_err(|e| format!("解析配置失败 {}: {e}", path.display()))?;
    cfg.normalize();
    cfg.validate()?;
    Ok(cfg)
}

/// 宽松加载：Tauri 模式使用（缺文件用内置 default，之后由 UI 写回）。
pub fn load_or_default(path: &std::path::Path) -> (BpHostConfig, Option<String>) {
    match load_exact(path) {
        Ok(cfg) => (cfg, None),
        Err(e) => (BpHostConfig::default(), Some(e)),
    }
}

/// 把配置写回磁盘（UI「保存设置」按钮）。
pub fn save(path: &std::path::Path, cfg: &BpHostConfig) -> Result<(), String> {
    cfg.validate()?;
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("写配置失败 {}: {e}", path.display()))
}

// ------------------------------------------------------------------ 轻量随机
// 房间码 / 会话令牌的自实现生成（handoff：房间码自实现，不引 uuid）。
// 用途仅限于局域网赛事工具：不可预测性要求为「防手滑撞号」级，不是密码学级。

/// 时间 + 进程 ID 混合种子。
pub fn time_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    nanos ^ ((std::process::id() as u64) << 32)
}

pub fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// 生成 4–6 位数字 PIN。
pub fn generate_room_code(digits: u8) -> String {
    let digits = digits.clamp(4, 6);
    let mut seed = time_seed();
    loop {
        let v = xorshift64(&mut seed);
        let code = (v % 10u64.pow(digits as u32) as u64).to_string();
        if code.len() == digits as usize {
            return code;
        }
    }
}

/// 生成连接令牌（16 位 hex）。
pub fn generate_token() -> String {
    let mut seed = time_seed();
    format!("{:016x}", xorshift64(&mut seed))
}
