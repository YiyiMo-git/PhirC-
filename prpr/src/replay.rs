//! 回放系统：录制 / 播放。
//!
//! 磁带 = 逐帧的**屏幕空间触摸** + 判定事件 + 元数据。设计要点：
//!
//! * **录制不碰判定**：游戏循环每帧把 `Judge` 的原始触摸（未经谱面坐标变换）
//!   抄一份进来；判定仍由 `judge.update` 正常产出。
//! * **播放不碰输入**：`set_playback_touches` 把合成触摸塞进 `Judge` 的
//!   thread-local，`on_new_frame` 优先用它、**跳过 macroquad 采集**，
//!   于是 flick 追踪 / 判定 / 计分整条管线原样复用，无需任何引擎改动。
//! * 格式自包含：`PHIRAREC` + u32 版本号 + zstd(JSON)，最大 128 MiB。
//!   读取时也认 `.phirar` / `.phirarec` 扩展名（导入兼容），但只解析本格式。
//!
//! 播放驱动在 `phira/src/scene/replay.rs`（经 `update_fn` 钩子每帧推进）。

use crate::judge::Judge;
use macroquad::prelude::{Touch, TouchPhase, *};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::PathBuf,
    sync::Mutex,
};

const MAX_BYTES: u64 = 128 << 20;
const MAX_EVENTS: usize = 2_000_000;
const FORMAT_VERSION: u32 = 1;

pub const PHASE_DOWN: u8 = 0;
pub const PHASE_MOVE: u8 = 1;
pub const PHASE_UP: u8 = 2;

/// 判定种类编码（与 `JudgeEvent::kind` 一致）。
pub const KIND_PERFECT: u8 = 0;
pub const KIND_GOOD: u8 = 1;
pub const KIND_BAD: u8 = 2;
pub const KIND_MISS: u8 = 3;

/// 一条触摸帧（屏幕空间，与 `Judge::on_new_frame` 里采集的 `Touch` 同基准）。
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct TouchEvent {
    pub phase: u8,
    pub id: u64,
    pub x: f32,
    pub y: f32,
    /// 谱面时间（秒，0 = 谱面起始，与 `Resource::time` 同基准）。
    pub t: f32,
}

/// 一条判定事件（`kind` 见 `KIND_*`）。
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct JudgeEvent {
    pub t: f64,
    pub line: u32,
    pub note: u32,
    pub kind: u8,
    /// 判定误差（秒）：正 = 早按，负 = 晚按。
    pub diff: f32,
}

/// 谱面引用：在线谱用 id，本地谱用路径。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ChartRef {
    Id(i32),
    Local(String),
}

/// 影响判定 / 观感的玩法参数快照（播放时套回，保证观感一致）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Gameplay {
    pub hp_mode: bool,
    pub health_max: f32,
    pub health_scale: f32,
    pub use_keyboard: bool,
    pub note_scale: f32,
    pub note_flow_speed: f32,
}

impl Gameplay {
    pub fn capture(c: &crate::config::Config) -> Self {
        Self {
            hp_mode: c.health_mode(),
            health_max: c.health_max,
            health_scale: c.health_scale,
            use_keyboard: c.use_keyboard,
            note_scale: c.note_scale,
            note_flow_speed: c.note_flow_speed,
        }
    }
    pub fn apply(&self, c: &mut crate::config::Config) {
        // 血条模式在本工程不是独立 Config 字段（由 health_max 是否 <100 推导），
        // 所以这里只回套数值部分。
        c.health_max = self.health_max;
        c.health_scale = self.health_scale;
        c.use_keyboard = self.use_keyboard;
        c.note_scale = self.note_scale;
        c.note_flow_speed = self.note_flow_speed;
    }
}

/// 成绩快照（原样存进元数据；播放结束只是展示，**不写历史、不上传**）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayResult {
    pub score: u32,
    pub accuracy: f64,
    pub max_combo: u32,
    pub num_of_notes: u32,
    /// Perfect / Good / Bad / Miss
    pub counts: [u32; 4],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    pub name: String,
    pub level: String,
    pub difficulty: f32,
    /// 结束时的 Unix 毫秒时间戳。
    pub recorded_at: Option<i64>,
    pub player: Option<String>,
    pub result: Option<ReplayResult>,
}

/// 回放文件。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Replay {
    pub meta: Metadata,
    /// 每帧的谱面时间（真实帧步进 / 时长 / 拖动定位用）。
    pub frames: Vec<f64>,
    pub chart: Option<ChartRef>,
    /// 录制时的全局偏移（秒）。
    pub offset: f32,
    pub speed: f32,
    pub mods: u32,
    pub gameplay: Gameplay,
    pub touches: Vec<TouchEvent>,
    pub judges: Vec<JudgeEvent>,
}

impl Replay {
    /// 没有逐音符判定事件时无法还原打击特效与实时分数（旧版故障数据）。
    pub fn ensure_playable(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.judges.is_empty() || self.touches.is_empty(),
            "该回放缺少逐音符判定事件，无法还原打击和实时分数。"
        );
        Ok(())
    }

    /// 磁带时长（秒）。
    pub fn duration(&self) -> f64 {
        self.frames.last().copied().unwrap_or(0.)
    }
}

fn read_with_limit(reader: impl Read, limit: u64) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    reader.take(limit + 1).read_to_end(&mut out)?;
    anyhow::ensure!(out.len() as u64 <= limit, "回放文件超过大小上限");
    Ok(out)
}

pub fn bytes(replay: &Replay) -> anyhow::Result<Vec<u8>> {
    validate(replay)?;
    let mut out = b"PHIRAREC".to_vec();
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    let json = serde_json::to_vec(replay)?;
    anyhow::ensure!(json.len() as u64 <= MAX_BYTES, "回放数据过大");
    out.extend(zstd::stream::encode_all(json.as_slice(), 3)?);
    Ok(out)
}

pub fn save(replay: &Replay, path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("ppr.tmp");
    std::fs::write(&tmp, bytes(replay)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load(path: &std::path::Path) -> anyhow::Result<Replay> {
    let raw = read_with_limit(std::fs::File::open(path)?, MAX_BYTES)?;
    anyhow::ensure!(raw.len() >= 12 && &raw[..8] == b"PHIRAREC", "不是有效的回放文件（缺少 PHIRAREC 头）");
    let version = u32::from_le_bytes(raw[8..12].try_into().unwrap());
    anyhow::ensure!(version == FORMAT_VERSION, "回放格式版本 {version} 不受支持（当前 {FORMAT_VERSION}）");
    let json = zstd::stream::decode_all(&raw[12..])?;
    let replay: Replay = serde_json::from_slice(&json)?;
    validate(&replay)?;
    replay.ensure_playable()?;
    Ok(replay)
}

fn validate(replay: &Replay) -> anyhow::Result<()> {
    anyhow::ensure!(
        replay.touches.len() <= MAX_EVENTS && replay.judges.len() <= MAX_EVENTS && replay.frames.len() <= MAX_EVENTS,
        "回放事件过多"
    );
    anyhow::ensure!(
        replay.offset.is_finite() && replay.speed.is_finite() && replay.speed > 0. && replay.speed <= 16.,
        "回放速度或偏移无效"
    );
    anyhow::ensure!(
        replay.frames.windows(2).all(|w| w[0] <= w[1])
            && replay.judges.windows(2).all(|w| w[0].t <= w[1].t)
            && replay.touches.windows(2).all(|w| w[0].t <= w[1].t),
        "回放时间顺序无效"
    );
    let time = |t: f64| t.is_finite() && (-3600. ..=86400.).contains(&t);
    anyhow::ensure!(
        replay
            .touches
            .iter()
            .all(|e| time(e.t as f64) && e.x.is_finite() && e.y.is_finite() && e.phase <= 2)
            && replay.judges.iter().all(|e| time(e.t) && e.diff.is_finite() && e.kind <= 3)
            && replay.frames.iter().all(|t| time(*t)),
        "回放含无效事件"
    );
    Ok(())
}

// ============================================================
//  录制
// ============================================================

struct Recorder {
    replay: Replay,
    last_feedback_t: f64,
}

static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

/// 开始录制一局。`chart` 传 `None` 表示不记录谱面引用（仍可当纯触摸回放）。
pub fn start_recording(chart: Option<ChartRef>, offset: f32, mods: u32, gameplay: Gameplay, player: Option<String>) {
    let replay = Replay {
        chart,
        offset,
        speed: 1.,
        mods,
        gameplay,
        meta: Metadata {
            player,
            ..Default::default()
        },
        ..Default::default()
    };
    *RECORDER.lock().unwrap() = Some(Recorder { replay, last_feedback_t: f64::NEG_INFINITY });
}

pub fn is_recording() -> bool {
    RECORDER.lock().unwrap().is_some()
}

/// 每帧调用：抄一份本帧触摸 + 新增判定事件。
///
/// `judge` 只是为了读 `feedback`（`(判定, 误差, 时刻)`，只保留最近 2 秒、
/// 按时间戳增量取，不会漏也不会重复）。
pub fn record_frame(chart_time: f64, judge: &Judge) {
    let mut guard = RECORDER.lock().unwrap();
    let Some(rec) = guard.as_mut() else { return };
    if chart_time < 0. {
        return; // 前奏 / 黑场不录
    }
    rec.replay.frames.push(chart_time);
    for touch in Judge::raw_touches() {
        rec.replay.touches.push(TouchEvent {
            phase: match touch.phase {
                TouchPhase::Started => PHASE_DOWN,
                TouchPhase::Moved | TouchPhase::Stationary => PHASE_MOVE,
                TouchPhase::Ended | TouchPhase::Cancelled => PHASE_UP,
            },
            id: touch.id,
            x: touch.position.x,
            y: touch.position.y,
            t: chart_time as f32,
        });
    }
    for (what, diff, at) in &judge.feedback {
        if *at <= rec.last_feedback_t {
            continue;
        }
        rec.last_feedback_t = *at;
        rec.replay.judges.push(JudgeEvent {
            t: *at,
            line: 0,
            note: 0,
            kind: match what {
                crate::judge::Judgement::Perfect => KIND_PERFECT,
                crate::judge::Judgement::Good => KIND_GOOD,
                crate::judge::Judgement::Bad => KIND_BAD,
                crate::judge::Judgement::Miss => KIND_MISS,
            },
            diff: *diff as f32,
        });
    }
}

/// 结束录制并取出磁带（没在录制时返回 `None`）。`result` 为成绩快照。
pub fn finish_recording(result: Option<ReplayResult>, name: String, level: String, difficulty: f32) -> Option<Replay> {
    let mut guard = RECORDER.lock().unwrap();
    let mut rec = guard.take()?;
    rec.replay.meta.name = name;
    rec.replay.meta.level = level;
    rec.replay.meta.difficulty = difficulty;
    rec.replay.meta.result = result;
    rec.replay.meta.recorded_at = Some(current_time_millis());
    Some(rec.replay)
}

fn current_time_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

// ============================================================
//  播放（触摸注入）
// ============================================================

static PLAYBACK_TOUCHES: Mutex<Option<Vec<Touch>>> = Mutex::new(None);

/// 设置下一帧要注入的合成触摸（由播放驱动每帧调用）。
pub fn set_playback_touches(touches: Vec<Touch>) {
    *PLAYBACK_TOUCHES.lock().unwrap() = Some(touches);
}

/// 清空注入，恢复真实输入。
pub fn clear_playback_touches() {
    *PLAYBACK_TOUCHES.lock().unwrap() = None;
}

/// `judge::on_new_frame` 用：有注入就取走（一次性）。
pub(crate) fn take_playback_touches() -> Option<Vec<Touch>> {
    PLAYBACK_TOUCHES.lock().unwrap().take()
}

/// 播放游标：增量推进，输出「本帧要注入的触摸」与上一帧的差集。
///
/// 磁带每帧都记了**全部**活跃触点（含 Stationary），所以按 id 去重后：
/// 本帧有事件的发 Started / Moved / Ended，仍按住但本帧无事件的发 Stationary
/// （判定里 Stationary 会继续喂 flick tracker，按住不放的行为才能还原）。
pub struct TouchPlayer {
    idx: usize,
    active: std::collections::HashMap<u64, (f32, f32)>,
}

impl Default for TouchPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl TouchPlayer {
    pub fn new() -> Self {
        Self {
            idx: 0,
            active: std::collections::HashMap::new(),
        }
    }

    /// 重置到磁带开头。
    pub fn reset(&mut self) {
        self.idx = 0;
        self.active.clear();
    }

    /// 推进到谱面时间 `t`，返回本帧应注入的触摸。
    pub fn advance(&mut self, replay: &Replay, t: f64) -> Vec<Touch> {
        let mut frame: std::collections::HashMap<u64, &TouchEvent> = std::collections::HashMap::new();
        while self.idx < replay.touches.len() && (replay.touches[self.idx].t as f64) <= t {
            let e = &replay.touches[self.idx];
            frame.insert(e.id, e);
            self.idx += 1;
        }
        let mut out: Vec<Touch> = Vec::new();
        for (id, e) in &frame {
            match e.phase {
                PHASE_DOWN => {
                    self.active.insert(*id, (e.x, e.y));
                    out.push(Touch {
                        id: *id,
                        phase: TouchPhase::Started,
                        position: vec2(e.x, e.y),
                        time: e.t as f64,
                    });
                }
                PHASE_UP => {
                    self.active.remove(id);
                    out.push(Touch {
                        id: *id,
                        phase: TouchPhase::Ended,
                        position: vec2(e.x, e.y),
                        time: e.t as f64,
                    });
                }
                _ => {
                    self.active.insert(*id, (e.x, e.y));
                    out.push(Touch {
                        id: *id,
                        phase: TouchPhase::Moved,
                        position: vec2(e.x, e.y),
                        time: e.t as f64,
                    });
                }
            }
        }
        // 仍按住、本帧没有新事件的 → Stationary，保证长按不断
        for (id, (x, y)) in &self.active {
            if !frame.contains_key(id) {
                out.push(Touch {
                    id: *id,
                    phase: TouchPhase::Stationary,
                    position: vec2(*x, *y),
                    time: t,
                });
            }
        }
        out
    }
}

/// 播放驱动的游标（当前播到第几帧），供 UI 显示进度。
static PLAY_CURSOR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn set_play_cursor(i: usize) {
    PLAY_CURSOR.store(i, std::sync::atomic::Ordering::Relaxed);
}

pub fn play_cursor() -> usize {
    PLAY_CURSOR.load(std::sync::atomic::Ordering::Relaxed)
}

/// 录制文件的默认落点：`<data>/replays/<sanitized>_<时间戳>.ppr`
pub fn default_path(root: &str, chart_key: &str, time_ms: i64) -> PathBuf {
    let safe: String = chart_key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .take(64)
        .collect();
    PathBuf::from(root).join("replays").join(format!("{safe}_{time_ms}.ppr"))
}
