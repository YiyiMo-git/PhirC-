#![allow(unused)]

prpr_l10n::tl_file!("game");

use super::{
    draw_background,
    ending::RecordUpdateState,
    loading::{BasicPlayer, SaveFn, UpdateFn, UploadFn},
    request_input, return_input, show_message, take_input, EndingScene, NextScene, Scene,
};
use crate::{
    bin::BinaryReader,
    config::{
        Config, Mods, NOTE_FLOW_SPEED_MAX, NOTE_FLOW_SPEED_MIN, NOTE_FLOW_SPEED_STEP, PLAYBACK_SPEED_MAX, PLAYBACK_SPEED_MIN, PLAYBACK_SPEED_STEP,
    },
    core::{copy_fbo, BadNote, Chart, ChartExtra, Effect, JudgeLineKind, Matrix, NoteKind, Point, Resource, UIElement, Vector, GAME_FONT, PGR_FONT},
    ext::{draw_text_aligned_opt_width_using, draw_text_aligned_using, parse_time, screen_aspect, semi_white, RectExt, SafeTexture, ScaleType},
    fs::FileSystem,
    info::{ChartFormat, ChartInfo},
    judge::{Judge, JudgeReportEvent, Judgement},
    parse::{parse_extra, parse_pec, parse_phigros, parse_rpe},
    task::Task,
    time::TimeManager,
    timing_bar::{self, TimingBar, TimingWindows},
    ui::{RectButton, TextPainter, Ui},
};
use anyhow::{bail, Context, Result};
use concat_string::concat_string;
use inputbox::InputBox;
use lyon::path::Path;
use macroquad::{prelude::*, window::InternalGlContext};
use sasa::{Music, MusicParams};
use serde::{Deserialize, Serialize};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    fs::File,
    io::{Cursor, ErrorKind},
    ops::{Deref, DerefMut, Range},
    path::PathBuf,
    process::{Command, Stdio},
    rc::Rc,
    sync::{Arc, Mutex},
    thread::LocalKey,
    time::Duration,
};
use tracing::{debug, warn};

const PAUSE_CLICK_INTERVAL: f32 = 0.7;

/// 打歌界面里 `size()` 和实际字高的换算。
///
/// 文字渲染的缩放是 `0.04 * size * 视口宽(px)`，画到世界坐标里又被 `2 / 视口宽`
/// 除回来，所以一行字的**字高（世界坐标）≈ 0.08 × 字体 cap 高比例 × size**。
/// 打歌界面用的 `assets/phigros.ttf` cap 高是 0.688 em，于是：
///
/// * `size 1.0` → 字高 ≈ 0.055（世界坐标）；
/// * 16:9 时屏幕高 = `2 / 1.7778` = 1.125，所以 `size 1.0` ≈ **屏幕高的 4.9%**。
///
/// 换到「打歌界面字体 = `assets/font.ttf`」（默认，见 [`text_font`]）时不用改这个常量：
/// 两套字体 `unitsPerEm` 都是 1000，cap 高实测 phigros.ttf **0.688 em**、
/// font.ttf **0.693 em**，只差 0.7%（`0.055` 换算回去是 0.688，用 0.693 应是 0.0554）。
/// 所以字高/行距的尺度两套字体通用。
///
/// 对照 Phi-Recorder（`phire`）和原生 phira 的打歌界面（**基准值**，实际还会乘
/// `Config::in_game_text_scale`，见下）：
/// COMBO 数字 0.98、分数 0.71、偏移面板标题 0.7 / 数值 0.6、曲名难度 0.505、
/// 按钮文字 0.42（`Ui::button`）、ACC 与连击文字 0.34 / 0.4。
/// 自己加的 UI 一律按这套尺度写，不要再凭感觉填「零点几」。
const IN_GAME_TEXT_H: f32 = 0.055;

/// Phi-Recorder（phire）里那个硬编码的 `scale_ratio`，**仅作换算基准留档**。
///
/// phire 的打歌界面把所有 `size` 都写成「基准值 × scale_ratio」，而**两边的
/// `size → px` 公式是完全相同的**（`0.04 * size * 视口宽`）。所以 phire 真正画出来的
/// 字 = 基准值 × 1.7778，不是基准值本身。
///
/// > 上一轮记录的"字号两边完全一致"就是这么错的：只比了基准值（phire 0.71 / phira 0.8），
/// > 漏了这个 1.7778，实际像素差 1.58 倍。实测佐证：1600 宽下 phira 的分数 bbox 高 32px、
/// > phire 换算后应为 56px。（截图已存 `rev/s3_play.png`）
///
/// 实际倍率是**可调设置** `Config::in_game_text_scale`（默认 1.4），不再是编译期常量 ——
/// 在设置 → 谱面UI → 「打歌字号」里调。想完全按 phire 就调到 1.78。
///
/// 位置则相反：phire 的世界是 `±宽高比 × ±1`，phira 是 `±1 × ±1/宽高比`，
/// 两者差一个宽高比 —— 所以 phire 的**裸长度要 / 宽高比**，带 `* scale_ratio` 的乘完正好相消。
/// **这个倍率只作用于字号，位置不受影响。**
pub const PHIRE_SCALE_RATIO: f32 = 1.777777;

/// 打歌界面文字用的字体。
///
/// * 默认（`phi_recorder_font = true`）走 `assets/font.ttf`（[`GAME_FONT`]，
///   Source Han Sans + Saira + Noto）—— 也就是 Phi-Recorder 渲染视频时唯一的那套字体，
///   分数 / 连击 / 准度的字形和它一致；
/// * 关掉后退回原生 phira 的行为：打歌文字用 `assets/phigros.ttf`（[`PGR_FONT`]）。
fn text_font(config: &Config) -> &'static LocalKey<RefCell<Option<TextPainter>>> {
    if config.phi_recorder_font {
        &GAME_FONT
    } else {
        &PGR_FONT
    }
}

/// 曲名 / 难度这类原生用界面字体画的文字，开了「Phi-Recorder 字体」后一并统一。
fn name_font(config: &Config) -> Option<&'static LocalKey<RefCell<Option<TextPainter>>>> {
    config.phi_recorder_font.then_some(&GAME_FONT)
}

thread_local! {
    /// 暂停界面上调过的判定偏移（秒），客户端每帧取走并写回配置。
    static PENDING_OFFSET: Cell<Option<f32>> = const { Cell::new(None) };
    /// 暂停界面上调过的音量（音乐, 音效），客户端每帧取走并写回配置。
    static PENDING_VOLUME: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    /// 设备当前的摇动强度（线性加速度，单位 g）。平台层（Android 的 SensorManager /
    /// iOS 的 CoreMotion）负责写进来，引擎只关心数值；负数表示没有传感器。
    static MOTION_LEVEL: Cell<f32> = const { Cell::new(-1.) };
    /// 设备姿态倾斜（相对启动时的「中位」），归一化到 [-1, 1]。
    /// `(NaN, NaN)` 表示没有姿态传感器 —— 桌面端就是这种，调用方要回退到指针。
    static MOTION_TILT: Cell<(f32, f32)> = const { Cell::new((f32::NAN, f32::NAN)) };
}

/// 平台层写入当前摇动强度（单位 g，≥ 0）。负数表示这台设备没有传感器（模拟器等），
/// 此时「摇一摇再玩」不会自动暂停/恢复。
pub fn set_motion_level(level: f32) {
    MOTION_LEVEL.with(|it| it.set(if level.is_finite() { level } else { -1. }));
}

/// 当前摇动强度（单位 g）；负数表示传感器不可用。
pub fn motion_level() -> f32 {
    MOTION_LEVEL.with(|it| it.get())
}

/// 平台层写入设备姿态倾斜（已归一化到 [-1, 1]）。传 NaN 表示传感器不可用。
///
/// 平台层要报的是**相对启动时中位的偏移**，不是原始重力方向 ——
/// 否则「横屏握持」这个静态姿势本身就会把偏移顶到 ±1，画面永远歪着。
pub fn set_motion_tilt(x: f32, y: f32) {
    MOTION_TILT.with(|it| it.set(if x.is_finite() && y.is_finite() { (x, y) } else { (f32::NAN, f32::NAN) }));
}

/// 设备姿态倾斜；`None` 表示这台设备没有姿态传感器（桌面端）。
pub fn motion_tilt() -> Option<(f32, f32)> {
    MOTION_TILT.with(|it| {
        let (x, y) = it.get();
        (x.is_finite() && y.is_finite()).then_some((x, y))
    })
}

/// 取走暂停界面上调整过的判定偏移（秒）；没有调整过时返回 `None`。
pub fn take_pending_offset() -> Option<f32> {
    PENDING_OFFSET.with(|it| it.take())
}

/// 取走暂停界面上调整过的音量 `(音乐, 音效)`；没有调整过时返回 `None`。
pub fn take_pending_volume() -> Option<(f32, f32)> {
    PENDING_VOLUME.with(|it| it.take())
}

#[rustfmt::skip]
#[cfg(closed)]
mod inner;
#[cfg(closed)]
use inner::*;

const WAIT_TIME: f64 = 0.5;
const AFTER_TIME: f64 = 0.7;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SimpleRecord {
    pub score: i32,
    pub accuracy: f32,
    pub full_combo: bool,
    /// 下面这些是给「成绩历史」用的：服务端的最好成绩接口只返回上面三个，
    /// 所以它们必须能缺省（`#[serde(default)]`），缺省就是 0 / 空。
    pub counts: [u32; 4],
    pub max_combo: u32,
    pub num_of_notes: u32,
    /// 判定误差分布（早 ← → 晚），和结算图的 `hist` 同源
    pub hist: Vec<u32>,
}

impl SimpleRecord {
    pub fn update(&mut self, other: &SimpleRecord) -> bool {
        let mut changed = false;
        if other.score > self.score {
            self.score = other.score;
            changed = true;
        }
        if other.accuracy > self.accuracy {
            self.accuracy = other.accuracy;
            changed = true;
        }
        if other.full_combo & !self.full_combo {
            self.full_combo = other.full_combo;
            changed = true;
        }
        changed
    }
}

fn fmt_time(t: f32) -> String {
    let f = t < 0.;
    let t = t.abs();
    let secs = t % 60.;
    let mut t = (t / 60.) as u64;
    let mins = t % 60;
    t /= 60;
    let hrs = t % 100;
    format!("{}{hrs:02}:{mins:02}:{secs:05.2}", if f { "-" } else { "" })
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    fn on_game_start();
}

#[derive(PartialEq, Eq)]
pub enum GameMode {
    Normal,
    TweakOffset,
    Exercise,
    NoRetry,
    View,
}

#[derive(Clone)]
enum State {
    Starting,
    BeforeMusic,
    Playing,
    Ending,
}

/// 课题模式不再走单谱结算画面，而是把结果回传给课题场景。
///
/// 由 [`crate::config::Config::challenge_mode`] 打开，只有课题场景会用到。
pub enum ChallengeEvent {
    /// 一门打完了，把 `PlayResult` 交回课题场景。
    Completed(crate::judge::PlayResult),
    /// 玩家在暂停菜单点了重开 —— 整个课题从头再来。
    Restart,
    /// 玩家中途退出 —— 整个课题作废。
    Aborted,
}

pub struct GameScene {
    should_exit: bool,
    next_scene: Option<NextScene>,

    pub mode: GameMode,
    pub res: Resource,
    pub chart: Chart,
    pub judge: Judge,
    pub gl: InternalGlContext<'static>,
    player: Option<BasicPlayer>,
    chart_bytes: Vec<u8>,
    chart_format: ChartFormat,
    info_offset: f32,
    effects: Vec<Effect>,

    first_in: bool,
    exercise_range: Range<f64>,
    exercise_press: Option<(i8, u64)>,
    exercise_btns: (RectButton, RectButton),
    /// 练习模式手动指定的视觉流速（`exercise_note_flow_locked` 打开时改用倒打速度的倒数）。
    exercise_note_flow_speed: f32,
    /// 「跟随播放速度」：流速 = 1 / 播放倍速，锁定时忽略手动值。
    exercise_note_flow_locked: bool,
    /// 「额外功能」面板是否展开（练习镜头 / 流速）。
    exercise_extra_open: bool,

    pub music: Music,

    state: State,
    pub last_update_time: f64,
    pause_rewind: Option<f64>,
    pause_first_time: f32,
    /// 摇一摇再玩：是否为「因为静止而自动暂停」（只自动恢复自己暂停的）
    paused_by_motion: bool,
    /// 持续静止的累计秒数
    still_time: f32,
    /// 上一次采样摇动强度的真实时间
    motion_last: f64,

    pub bad_notes: Vec<BadNote>,

    upload_fn: Option<UploadFn>,
    update_fn: Option<UpdateFn>,
    save_fn: Option<SaveFn>,

    best_record: Option<SimpleRecord>,

    pub touch_points: Vec<(f32, f32)>,
    /// 噪区触摸时的低通滤波（官方 JudgeControl/ProgressControl 手势过渡）。
    block_audio: crate::core::BlockAudio,
    /// 装饰性准度条（Timing Bar）。
    timing_bar: TimingBar,
    /// 已经喂给准度条的最后一个判定时间（谱面时间）。
    timing_bar_seen: f64,
    fps_frame_count: u32,
    fps_total_time: f64,
    fps_last_frame_time: f64,

    dead: bool,
}

macro_rules! reset {
    ($self:ident, $res:expr, $tm:ident) => {{
        $self.bad_notes.clear();
        $self.judge.reset();
        $self.chart.reset();
        $self.block_audio.reset();
        $self.timing_bar.clear();
        $self.timing_bar_seen = f64::NEG_INFINITY;
        $res.health.reset();
        $res.judge_line_color = $res.res_pack.info.color_perfect();
        $self.music.pause()?;
        $self.music.seek_to(0.)?;
        $tm.speed = $res.config.speed as _;
        $tm.reset();
        $self.last_update_time = $tm.now();
        $self.state = State::Starting;
        $self.fps_frame_count = 0;
        $self.fps_total_time = 0.0;
        $self.fps_last_frame_time = $tm.real_time();
        $self.dead = false;
    }};
}

impl GameScene {
    pub const BEFORE_TIME: f64 = 0.7;
    pub const FADEOUT_TIME: f64 = WAIT_TIME + AFTER_TIME + 0.3;

    pub async fn load_chart_bytes(fs: &mut dyn FileSystem, info: &ChartInfo) -> Result<Vec<u8>> {
        if let Ok(bytes) = fs.load_file(&info.chart).await {
            return Ok(bytes);
        }
        if let Some(name) = info.chart.strip_suffix(".pec") {
            if let Ok(bytes) = fs.load_file(&concat_string!(name, ".json")).await {
                return Ok(bytes);
            }
        }
        bail!("Cannot find chart file")
    }

    pub fn infer_chart_format(info: &ChartInfo, bytes: &[u8]) -> ChartFormat {
        info.format.clone().unwrap_or_else(|| {
            if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                if text.starts_with('{') {
                    if text.contains("\"META\"") {
                        ChartFormat::Rpe
                    } else {
                        ChartFormat::Pgr
                    }
                } else {
                    ChartFormat::Pec
                }
            } else {
                ChartFormat::Pbc
            }
        })
    }

    pub async fn load_chart(fs: &mut dyn FileSystem, info: &ChartInfo) -> Result<(Chart, Vec<u8>, ChartFormat)> {
        let extra = fs.load_file("extra.json").await.ok().map(String::from_utf8).transpose()?;
        let extra = if let Some(extra) = extra {
            parse_extra(&extra, fs).await.context("Failed to parse extra")?
        } else {
            ChartExtra::default()
        };
        let bytes = Self::load_chart_bytes(fs, info).await.context("Failed to load chart")?;
        let format = Self::infer_chart_format(info, &bytes);
        let mut chart = match format {
            ChartFormat::Rpe => parse_rpe(&String::from_utf8_lossy(&bytes), fs, extra, info.use_rpe_170_speed.unwrap_or_default()).await,
            ChartFormat::Pgr => parse_phigros(&String::from_utf8_lossy(&bytes), extra),
            ChartFormat::Pec => parse_pec(&String::from_utf8_lossy(&bytes), extra),
            ChartFormat::Pbc => {
                let mut r = BinaryReader::new(Cursor::new(&bytes));
                r.read()
            }
        }?;
        chart.load_textures(fs).await?;
        chart.settings.hold_partial_cover = info.hold_partial_cover;
        Ok((chart, bytes, format))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        mode: GameMode,
        info: ChartInfo,
        mut config: Config,
        mut fs: Box<dyn FileSystem>,
        player: Option<BasicPlayer>,
        background: SafeTexture,
        illustration: SafeTexture,
        upload_fn: Option<UploadFn>,
        update_fn: Option<UpdateFn>,
        save_fn: Option<SaveFn>,
    ) -> Result<Self> {
        match mode {
            GameMode::TweakOffset => {
                config.mods.insert(Mods::AUTOPLAY);
            }
            GameMode::Exercise => {
                config.mods.remove(Mods::AUTOPLAY);
            }
            _ => {}
        }

        let (mut chart, chart_bytes, chart_format) = Self::load_chart(fs.deref_mut(), &info).await?;
        if config.mods.contains(Mods::NO_SHADER) {
            chart.extra.effects.clear();
            chart.extra.global_effects.clear();
        }
        let effects = std::mem::take(&mut chart.extra.global_effects);
        if config.fxaa {
            chart
                .extra
                .effects
                .push(Effect::new(0.0..f64::INFINITY, include_str!("fxaa.glsl"), Vec::new(), false).unwrap());
        }

        if config.has_mod(Mods::NIGHTCORE) {
            config.speed *= 1.5;
        }

        if config.has_mod(Mods::RAINBOW) {
            chart
                .extra
                .effects
                .push(Effect::new(0.0..f64::INFINITY, include_str!("rainbow.glsl"), Vec::new(), false).unwrap());
        }

        let info_offset = info.offset;
        let mut res = Resource::new(
            config,
            info,
            fs,
            player.as_ref().and_then(|it| it.avatar.clone()),
            background,
            illustration,
            chart.extra.effects.is_empty() && effects.is_empty(),
        )
        .await
        .context("Failed to load resources")?;

        // Prepare extra sfx from chart.hitsounds
        chart.hitsounds.drain().for_each(|(name, clip)| {
            if let Ok(clip) = res.create_sfx(clip) {
                res.extra_sfxs.insert(name, clip);
            }
        });

        let exercise_range = (chart.offset + info_offset + res.config.offset) as f64..res.track_length;

        let judge = Judge::new(&chart);

        let music = Self::new_music(&mut res)?;
        crate::core::reset_block_effects();
        // 预渲染：在**加载阶段**把噪域的着色器编译 + 显存分配全部做掉。
        // 原来这些都堆在「第一次真的出现噪域」那一帧，表现就是进图卡一下。
        // 这里只做分配与编译，不产生任何画面输出，观赏度不变。
        let prewarm_aspect = res.aspect_ratio;
        crate::core::prewarm_block(&mut res, prewarm_aspect);
        Ok(Self {
            should_exit: false,
            next_scene: None,

            mode,
            res,
            chart,
            judge,
            gl: unsafe { get_internal_gl() },
            player,
            chart_bytes,
            chart_format,
            effects,
            info_offset,

            first_in: false,
            exercise_range,
            exercise_press: None,
            exercise_btns: (RectButton::new(), RectButton::new()),
            exercise_note_flow_speed: 1.,
            exercise_note_flow_locked: false,
            exercise_extra_open: false,

            music,

            state: State::Starting,
            last_update_time: 0.,
            pause_rewind: None,
            pause_first_time: f32::NEG_INFINITY,
            paused_by_motion: false,
            still_time: 0.,
            motion_last: 0.,

            bad_notes: Vec::new(),

            upload_fn,
            update_fn,
            save_fn,

            best_record: None,

            touch_points: Vec::new(),
            block_audio: Default::default(),
            timing_bar: TimingBar::default(),
            timing_bar_seen: f64::NEG_INFINITY,

            fps_frame_count: 0,
            fps_total_time: 0.0,
            fps_last_frame_time: 0.0,

            dead: false,
        })
    }

    fn new_music(res: &mut Resource) -> Result<Music> {
        res.audio.create_music(
            res.music.clone(),
            MusicParams {
                amplifier: res.config.volume_music as _,
                playback_rate: res.config.speed as _,
                ..Default::default()
            },
        )
    }

    fn touch_scale(&self) -> f32 {
        (screen_width() / screen_height()) / self.res.aspect_ratio
    }

    /// 锁定流速：跟随播放速度取倒数（保持音符视觉速度与时间轴同步）。照抄 PhiraiAd。
    fn locked_note_flow_speed(playback_speed: f32) -> f32 {
        ((1. / playback_speed.max(PLAYBACK_SPEED_MIN) * 1000.).round() / 1000.).clamp(NOTE_FLOW_SPEED_MIN, NOTE_FLOW_SPEED_MAX)
    }

    /// 练习模式自己的流速倍率；非练习模式恒为 1。照抄 PhiraiAd。
    fn practice_note_flow_speed(&self) -> f32 {
        if self.mode == GameMode::Exercise {
            if self.exercise_note_flow_locked {
                Self::locked_note_flow_speed(self.res.config.speed)
            } else {
                self.exercise_note_flow_speed
            }
        } else {
            1.
        }
    }

    /// 全局流速 × 练习倍率。照抄 PhiraiAd。
    fn effective_note_flow_speed(&self) -> f32 {
        self.res.config.effective_note_flow_speed(self.practice_note_flow_speed())
    }

    /// 把算好的流速写进 `Resource`，渲染层（note / line）每帧读它。
    fn sync_note_flow_speed(&mut self) {
        self.res.note_flow_speed = self.effective_note_flow_speed();
    }

    fn ui(&mut self, ui: &mut Ui, tm: &mut TimeManager) -> Result<()> {
        let time = tm.now();
        let p = match self.state {
            State::Starting => {
                if time <= Self::BEFORE_TIME {
                    1. - (1. - time / Self::BEFORE_TIME).powi(3)
                } else {
                    1.
                }
            }
            State::BeforeMusic => 1.,
            State::Playing => 1.,
            State::Ending => {
                let t = time - self.res.track_length - WAIT_TIME;
                1. - (t / (AFTER_TIME + 0.3)).min(1.).powi(2)
            }
        } as f32;
        let res = &mut self.res;
        let eps = 2e-2 / res.aspect_ratio;
        let top = -1. / res.aspect_ratio;
        let pause_w = 0.011; // phire: `0.011 * scale_ratio` → 除以宽高比后就是 0.011
        let pause_h = pause_w * 3.5;
        // phire: `-aspect_ratio + 0.0525 * scale_ratio` → `-1. + 0.0525`
        let pause_center = Point::new(-1. + 0.0525, top + eps * 3.6454 - (1. - p) * 0.4 + pause_h / 2.);
        if res.config.interactive
            && !tm.paused()
            && self.pause_rewind.is_none()
            && Judge::get_touches().iter().any(|touch| {
                touch.phase == TouchPhase::Started && {
                    let p = touch.position;
                    let p = Point::new(p.x, p.y);
                    (pause_center - p).norm() < 0.05
                }
            })
        {
            let t = tm.now() as f32;
            if t - self.pause_first_time > PAUSE_CLICK_INTERVAL && res.config.double_click_to_pause {
                self.pause_first_time = t;
            } else {
                self.pause_first_time = f32::NEG_INFINITY;
                if !self.music.paused() {
                    self.music.pause()?;
                }
                tm.pause();
                #[cfg(target_env = "ohos")]
                miniquad::native::set_interceptor_state(false);
            }
        }
        ui.alpha(res.alpha, |ui| {
            ui.text("MAGIC BUGFIX TEXT").color(Color::new(0., 0., 0., 0.)).draw();
            if tm.now() as f32 - self.pause_first_time <= PAUSE_CLICK_INTERVAL {
                ui.fill_circle(pause_center.x, pause_center.y, 0.05, Color::new(1., 1., 1., 0.5));
            }

            // phire: `0.0425 * scale_ratio` → 除以宽高比后就是 0.0425
            let margin = 0.0425;
            let font = text_font(&res.config);
            // 打歌界面字号倍率（设置 -> 谱面UI -> 打歌字号），只作用于字号
            let text_scale = res.config.in_game_text_scale_value();

            let legacy_aui = !res.info.use_attach_ui_fix.unwrap_or_default();
            let unit_h = if legacy_aui { ui.text("0").measure_using(font).h } else { 0. };

            // phire 的 ACC 是独立定位的（`top + eps*2.2 + 0.07 + 0.05`），不是"分数下方 h"。
            //
            // ⚠️ 但那套固定偏移会**压到分数上**：`.anchor(1., 0.)` 的锚点是文本
            // **ascent 盒的顶**，字形顶还要再往下 `(ascent - cap) ≈ 0.236 em`。
            // 分数放大到 phire 的尺度之后，这 0.236 em 就是十几像素，
            // 于是 ACC 的盒顶落到了分数字形底之上 → 压字。
            // （phire 自己也压：两边 anchor 实现逐行相同。只是 `show_acc` 在两边都默认关，
            // 平时看不到，一开才露馅。）
            //
            // 所以取「phire 的偏移」与「分数基线下方一点」中**更靠下的**那个：
            // 字号没放大时等价于 phire 原值，放大后自动错开，不会再压。
            let acc_phire_y = top + eps * 2.2 - (1. - p) * 0.4 + (0.07 + 0.05) / res.aspect_ratio;
            let score_top = top + eps * 2.8125 - (1. - p) * 0.4;
            let score_right = 1. - margin;
            let score = format!("{:07}", self.judge.score(res.config.has_mod(Mods::NO_COMBO_SCORE)));
            // phire 的宽度上限：`0.55 * aspect_ratio`（phire 世界）→ 除以宽高比 = 0.55
            let max_width = 0.55;
            // phire: `0.71 * scale_ratio`
            let mut text_size = 0.71 * text_scale;
            let w = ui.text(&score).size(text_size).measure_using(font).w;
            if w > max_width {
                text_size *= max_width / w;
            }
            // 分数那一行的盒子高（baseline 模式下 = 字体的 ascent），用来把 ACC 顶下去
            let score_box_h = ui.text(&score).size(text_size).measure_using(font).h;
            let acc_y = acc_phire_y.max(score_top + score_box_h + 0.008);
            let scale_point = legacy_aui.then(|| {
                let ct = ui.text(&score).size(text_size).measure_using(font).center();
                (score_right - ct.x, score_top + ct.y)
            });
            self.chart
                .with_element(ui, res, UIElement::Score, scale_point, (score_right, score_top), |ui, c| {
                    ui.text(&score)
                        .pos(score_right, score_top)
                        .anchor(1., 0.)
                        .size(text_size)
                        .color(c)
                        .draw_using(font);
                    if res.config.show_acc {
                        // phire: `.size(0.4 * scale_ratio)`
                        ui.text(format!("{:05.2}%", self.judge.real_time_accuracy() * 100.))
                            .pos(1. - margin, acc_y)
                            .anchor(1., 0.)
                            .size(0.4 * text_scale)
                            .color(Color { a: c.a * 0.7, ..c })
                            .draw_using(font);
                    }
                });

            self.chart.with_element(
                ui,
                res,
                UIElement::Pause,
                legacy_aui.then(|| (pause_center.x, pause_center.y)),
                (pause_center.x - pause_w * 1.5, pause_center.y - pause_h / 2.),
                |ui, c| {
                    let mut r = Rect::new(pause_center.x - pause_w * 1.5, pause_center.y - pause_h / 2., pause_w, pause_h);
                    ui.fill_rect(r, c);
                    r.x += pause_w * 2.;
                    ui.fill_rect(r, c);
                },
            );
            if self.judge.combo() >= 3 {
                let combo = self.judge.combo().to_string();
                let combo_label = res.config.combo_label();
                // 连击数字同样留宽度上限；连击文字（可以自定义成任意长）超了自动缩字号，
                // 不然会直接画到屏幕外面去。
                // phire: `0.98 * scale_ratio`
                let mut combo_size = 0.98 * text_scale;
                let w = ui.text(&combo).size(combo_size).measure_using(font).w;
                if w > max_width {
                    combo_size *= max_width / w;
                }
                // phire: `0.34 * scale_ratio`（连击文字）
                let label_size = 0.34 * text_scale;
                // phire 的连击文字与数字之间的间隙：裸长度 0.015 → 除以宽高比
                let label_gap = 0.015 / res.aspect_ratio;
                if legacy_aui {
                    let combo_top = top + eps * 1.55 - (1. - p) * 0.4;
                    let btm = self
                        .chart
                        .with_element(ui, res, UIElement::ComboNumber, None, (0., combo_top + unit_h / 2.), |ui, c| {
                            ui.text(&combo)
                                .pos(0., combo_top)
                                .anchor(0.5, 0.)
                                .size(combo_size)
                                .color(c)
                                .draw_using(font)
                                .bottom()
                        });
                    let combo_top = btm + label_gap;
                    self.chart
                        .with_element(ui, res, UIElement::Combo, None, (0., combo_top + unit_h * 0.2), |ui, c| {
                            draw_text_aligned_opt_width_using(ui, font, &combo_label, 0., combo_top, (0.5, 0.), label_size, c, max_width);
                        });
                } else {
                    let ct = ui.text(&combo).size(combo_size).measure_using(font).center();
                    let combo_y = top + eps * 1.55 - (1. - p) * 0.4 + ct.y;
                    let btm = self.chart.with_element(ui, res, UIElement::ComboNumber, None, (0., combo_y), |ui, c| {
                        ui.text(&combo)
                            .pos(0., combo_y)
                            .anchor(0.5, 0.5)
                            .size(combo_size)
                            .color(c)
                            .draw_using(font)
                            .bottom()
                    });
                    let ct = ui.text(&*combo_label).size(label_size).measure_using(font).center();
                    let combo_top = btm + label_gap + ct.y;
                    self.chart.with_element(ui, res, UIElement::Combo, None, (0., combo_top), |ui, c| {
                        draw_text_aligned_opt_width_using(ui, font, &combo_label, 0., combo_top, (0.5, 0.5), label_size, c, max_width);
                    });
                }
            }
            // 血条模式：左侧竖直血条 + 血量数字（颜色与长度可在设置里调）
            if res.config.health_mode() && matches!(self.mode, GameMode::Normal | GameMode::NoRetry | GameMode::View) {
                let (cr, cg, cb) = res.config.health_bar_color.rgb();
                let x = -1. + margin * 0.8;
                let w = 0.028;
                let full = 1. / res.aspect_ratio - eps * 3.5 - (top + eps * 3.5);
                let h = full * res.config.health_bar_len_ratio();
                // 竖直居中
                let y0 = top + eps * 3.5 + (full - h) / 2.;
                let y1 = y0 + h;
                let filled = h * res.health.ratio();
                ui.fill_rect(Rect::new(x, y0, w, h), Color::new(0.28, 0.28, 0.28, 0.7));
                ui.fill_rect(Rect::new(x, y1 - filled, w, filled), Color::new(cr, cg, cb, 0.95));
                draw_text_aligned_opt_width_using(
                    ui,
                    font,
                    &format!("{:.0}", res.health.hp),
                    x + w / 2.,
                    y1 - filled - 0.012,
                    (0.5, 1.),
                    // Phi-Recorder 的血量数字是 0.4（`0.4 * scale_ratio`）对应到我们的
                    // 「字宽归一化」坐标系就是 0.4，别自己另取一个值。
                    0.4,
                    semi_white(0.9),
                    0.9,
                );
            }
            // 摇一摇再玩：左上角实时显示当前摇动强度，方便确认传感器有没有在报数
            // （负数 = 这台设备没有传感器/没读到，此时不会自动暂停）
            if res.config.motion_pause {
                let level = motion_level();
                let text = if level < 0. {
                    "MOTION n/a".to_owned()
                } else {
                    format!("MOTION {level:.2}g / {:.2}g", res.config.motion_threshold_g())
                };
                ui.text(text)
                    .pos(-1. + margin, top + eps * 1.2)
                    .anchor(0., 0.)
                    .size(0.4)
                    .color(if level >= 0. && level >= res.config.motion_threshold_g() {
                        Color::new(0.5, 1., 0.6, 0.9)
                    } else {
                        semi_white(0.7)
                    })
                    .draw_using(font);
            }
            // Early / Late 实时提示：最近 0.8 秒内的判定按时间误差淡出。
            // 字号对齐 ACC（0.4），行距按字高（0.055 * 0.4 ≈ 0.022）的 2 倍留，最多 5 行。
            if res.config.early_late_hint {
                let now = res.time;
                let hint_size = 0.4;
                let hint_line = IN_GAME_TEXT_H * hint_size * 2.;
                let mut y = top + eps * 9.5;
                for (what, diff, at) in self.judge.feedback.iter().rev().take(5) {
                    let age = now - at;
                    if !(0. ..0.8).contains(&age) {
                        continue;
                    }
                    let alpha = (1. - age / 0.8) as f32;
                    let ms = diff * 1000.;
                    let color = match what {
                        Judgement::Perfect => Color::new(1., 1., 1., alpha),
                        Judgement::Good => Color::new(0.62, 0.85, 1., alpha),
                        _ => Color::new(1., 0.5, 0.5, alpha),
                    };
                    let text = if ms < 0. {
                        format!("EARLY {ms:.0}ms")
                    } else {
                        format!("LATE +{ms:.0}ms")
                    };
                    ui.text(text).pos(0., y).anchor(0.5, 0.).size(hint_size).color(color).draw_using(font);
                    y += hint_line;
                }
            }
            // magic to make score visible, refer to phira/src/rate.rs#L219
            ui.text("").draw_using(font);
            let lf = -1. + margin;
            // phire: `-top - eps * 3.5 + (1. - p) * 0.4`
            let bt = -top - eps * 3.5 + (1. - p) * 0.4;
            // phire: `.size(0.505 * scale_ratio)` / 宽度上限 `0.9 * aspect_ratio` → 0.9
            let name_size = 0.505 * text_scale;
            let name_max_w = 0.9;
            // 曲名 / 难度原生用界面字体画（中文字形靠它）。开了 Phi-Recorder 字体后
            // 一并换成 assets/font.ttf，和 Phi-Recorder 里「整块打歌文字只有一套字体」一致。
            let name_painter = name_font(&res.config);
            let scale_point = legacy_aui.then(|| {
                let mut text = ui.text(&res.info.name).size(name_size);
                let ct = match name_painter {
                    Some(font) => text.measure_using(font).center(),
                    None => text.measure().center(),
                };
                (lf + ct.x, bt - ct.y)
            });
            self.chart.with_element(ui, res, UIElement::Name, scale_point, (lf, bt), |ui, c| {
                let mut text = ui.text(&res.info.name).pos(lf, bt).anchor(0., 1.).size(name_size).color(c).max_width(name_max_w);
                match name_painter {
                    Some(font) => text.draw_using(font),
                    None => text.draw(),
                }
            });

            let scale_point = legacy_aui.then(|| {
                let mut text = ui.text(&res.info.level).size(name_size);
                let ct = match name_painter {
                    Some(font) => text.measure_using(font).center(),
                    None => text.measure().center(),
                };
                (-lf - ct.x, bt - ct.y)
            });
            self.chart.with_element(ui, res, UIElement::Level, scale_point, (-lf, bt), |ui, c| {
                let mut text = ui.text(&res.info.level).pos(-lf, bt).anchor(1., 1.).size(name_size).color(c);
                match name_painter {
                    Some(font) => text.draw_using(font),
                    None => text.draw(),
                }
            });

            let hw = 0.003;
            let height = eps * 1.0;
            let dest = (2. * res.time / res.track_length).clamp(0., 2.) as f32;
            self.chart
                .with_element(ui, res, UIElement::Bar, Some((-1., top + height / 2.)), (-1., top + height / 2.), |ui, color| {
                    ui.fill_rect(Rect::new(-1., top, dest, height), semi_white(0.6));
                    ui.fill_rect(Rect::new(-1. + dest - hw, top, hw * 2., height), WHITE);
                });
        });
        Ok(())
    }

    fn overlay_ui(&mut self, ui: &mut Ui, tm: &mut TimeManager) -> Result<()> {
        // 离开「练习模式 + 暂停」状态时，清掉练习滑条的手指捕获，避免残留的手指点到别处。
        if !tm.paused() || self.mode != GameMode::Exercise {
            Ui::clear_practice_speed_drag();
        }
        let c = semi_white(self.res.alpha);
        let res = &mut self.res;
        // 覆盖层上的文字（练习模式时间轴、倒计时、暂停微调面板）也按打歌界面字体走。
        // phire 的 game.rs 里这些位置同样是没有额外指定字体的 `ui.text(...).draw()`，
        // 而它全局只有 font.ttf 一套字体 —— 所以这边要对齐的也是 `text_font`。
        let font = text_font(&res.config);
        if tm.paused() {
            let h = 1. / res.aspect_ratio;
            draw_rectangle(-1., -h, 2., h * 2., Color::new(0., 0., 0., 0.6));
            let o = if self.mode == GameMode::Exercise { -0.3 } else { 0. };
            let s = 0.06;
            let w = 0.05;
            let no_retry = self.mode == GameMode::NoRetry;
            draw_texture_ex(
                *res.icon_back,
                -s * 3. - w,
                -s + o,
                c,
                DrawTextureParams {
                    dest_size: Some(vec2(s * 2., s * 2.)),
                    ..Default::default()
                },
            );
            let r = Rect::new(0., o, 0., 0.).feather(s);
            let disabled_color = semi_white(res.alpha * 0.4);
            ui.fill_rect(r, (*res.icon_retry, r.feather(0.02), ScaleType::Fit, if no_retry { disabled_color } else { c }));
            draw_texture_ex(
                *res.icon_resume,
                s + w,
                -s + o,
                if self.dead { disabled_color } else { c },
                DrawTextureParams {
                    dest_size: Some(vec2(s * 2., s * 2.)),
                    ..Default::default()
                },
            );
            if res.config.interactive {
                let mut clicked = None;
                for touch in Judge::get_touches() {
                    if touch.phase != TouchPhase::Started {
                        continue;
                    }
                    let p = touch.position;
                    let p = Point::new(p.x, p.y);
                    for i in -1..=1 {
                        let ct = Point::new((s * 2. + w) * i as f32, o);
                        let d = p - ct;
                        if d.x.abs() <= s && d.y.abs() <= s {
                            clicked = Some(i);
                            break;
                        }
                    }
                }
                if no_retry && clicked == Some(0) || self.dead && clicked == Some(1) {
                    clicked = None;
                }
                let mut pos = self.music.position();
                if self.mode == GameMode::Exercise {
                    pos = tm.now();
                }
                if clicked.is_some_and(|it| it != -1) && (tm.speed - res.config.speed as f64).abs() > 0.01 {
                    debug!("recreating music");
                    self.music = res.audio.create_music(
                        res.music.clone(),
                        MusicParams {
                            amplifier: res.config.volume_music as _,
                            playback_rate: res.config.speed as _,
                            ..Default::default()
                        },
                    )?;
                    self.block_audio.reset();
                }
                match clicked {
                    Some(-1) => {
                        self.should_exit = true;
                        #[cfg(target_env = "ohos")]
                        miniquad::native::set_interceptor_state(false);
                    }
                    Some(0) => {
                        // 课题模式里「重开」= 整个课题重新开始，交给课题场景处理。
                        if res.config.challenge_mode {
                            self.music.pause()?;
                            self.next_scene = Some(NextScene::PopWithResult(Box::new(ChallengeEvent::Restart)));
                            return Ok(());
                        }
                        reset!(self, res, tm);
                        if self.mode == GameMode::Exercise {
                            self.judge.advance_to(&mut self.chart, self.exercise_range.start);
                        }
                        #[cfg(target_env = "ohos")]
                        miniquad::native::set_interceptor_state(true);
                    }
                    Some(1) => {
                        if self.mode == GameMode::Exercise && (tm.now() > self.exercise_range.end || tm.now() < self.exercise_range.start) {
                            tm.seek_to(self.exercise_range.start);
                            self.music.seek_to(self.exercise_range.start)?;
                            pos = self.exercise_range.start;
                        }
                        self.music.play()?;
                        res.time -= 3.;
                        let dst = pos - 3.;
                        if dst < 0. {
                            self.music.pause()?;
                            self.state = State::BeforeMusic;
                        } else {
                            self.music.seek_to(dst)?;
                        }
                        let now = tm.now();
                        tm.speed = res.config.speed as _;
                        tm.resume();
                        tm.seek_to(now - 3.);
                        self.pause_rewind = Some(tm.now() - 0.2);
                        #[cfg(target_env = "ohos")]
                        miniquad::native::set_interceptor_state(true);
                    }
                    _ => {}
                }
            }
            if self.mode == GameMode::Exercise {
                let asp = self.touch_scale();
                for touch in ui.ensure_touches() {
                    touch.position *= asp;
                }
                // 额外功能面板（练习镜头 + 音符流速）：默认收起，点标题展开。照抄 PhiraiAd。
                ui.scope(|ui| {
                    ui.dx(-0.9);
                    ui.dy(-0.48);
                    ui.with(Matrix::new_scaling(crate::practice_view::EXTRA_PANEL_SCALE), |ui| {
                        let label = if self.exercise_extra_open {
                            "▾ 额外功能"
                        } else {
                            "▸ 额外功能"
                        };
                        if ui.button("exercise_extra", Rect::new(0., -0.01, 0.38, 0.065), label) {
                            self.exercise_extra_open ^= true;
                            Ui::clear_practice_speed_drag();
                        }
                        if self.exercise_extra_open {
                            ui.dy(crate::practice_view::extra_panel_offset(0.08));
                            ui.text("改变视角").size(0.4).draw();
                            ui.dy(crate::practice_view::extra_panel_offset(0.055));
                            ui.practice_value_slider(
                                "exercise_view_scale",
                                "大小",
                                crate::practice_view::SCALE,
                                &mut self.res.practice_view.scale_percent,
                                Some(0.40),
                            );
                            ui.dy(crate::practice_view::extra_panel_offset(0.175));
                            ui.practice_value_slider(
                                "exercise_view_x",
                                "中心 X",
                                crate::practice_view::CENTER,
                                &mut self.res.practice_view.center_x,
                                Some(0.40),
                            );
                            ui.dy(crate::practice_view::extra_panel_offset(0.175));
                            ui.practice_value_slider(
                                "exercise_view_y",
                                "中心 Y",
                                crate::practice_view::CENTER,
                                &mut self.res.practice_view.center_y,
                                Some(0.40),
                            );
                        }
                    });
                });
                // 播放速度 + 音符流速 + 「锁定为播放速度的倒数」。
                ui.scope(|ui| {
                    ui.dx(0.3);
                    ui.dy(-0.48);
                    ui.practice_speed_slider(
                        "exercise_speed",
                        tl!("speed"),
                        PLAYBACK_SPEED_MIN..PLAYBACK_SPEED_MAX,
                        PLAYBACK_SPEED_STEP,
                        &mut self.res.config.speed,
                        Some(0.46),
                    );
                    ui.dy(0.19);
                    let mut displayed_note_flow_speed = self.practice_note_flow_speed();
                    let previous_note_flow_speed = displayed_note_flow_speed;
                    ui.practice_speed_slider(
                        "exercise_note_flow",
                        tl!("note-flow-speed"),
                        NOTE_FLOW_SPEED_MIN..NOTE_FLOW_SPEED_MAX,
                        NOTE_FLOW_SPEED_STEP,
                        &mut displayed_note_flow_speed,
                        Some(0.46),
                    );
                    if (displayed_note_flow_speed - previous_note_flow_speed).abs() > f32::EPSILON {
                        self.exercise_note_flow_speed = displayed_note_flow_speed;
                        self.exercise_note_flow_locked = false;
                    }
                    ui.dy(0.19);
                    ui.checkbox(tl!("lock-note-flow-speed"), &mut self.exercise_note_flow_locked);
                });
                self.sync_note_flow_speed();
                // 时间轴整体下移，给上面的面板让位（照抄 PhiraiAd 的 timeline_position）。
                let viewport_width = self.res.camera.viewport.map_or(screen_width(), |vp| vp.2 as f32);
                let timeline_y = crate::practice_view::timeline_position(screen_height(), viewport_width);
                ui.dy(timeline_y);
                let hw = 0.7;
                let h = 0.06;
                let rad = 0.03;
                let eh = 0.12_f32.min((1. / self.res.aspect_ratio - timeline_y - rad - 0.01).max(0.));
                let sp = self.offset().min(0.) as f64;
                ui.fill_rect(Rect::new(-hw, -h, hw * 2., h * 2.), GRAY);
                let st = -hw + ((self.exercise_range.start - sp) / (self.res.track_length - sp)) as f32 * hw * 2.;
                let en = -hw + ((self.exercise_range.end - sp) / (self.res.track_length - sp)) as f32 * hw * 2.;
                let t = tm.now();
                let cur = -hw + ((t - sp) / (self.res.track_length - sp)) as f32 * hw * 2.;
                ui.fill_rect(Rect::new(st, -h, en - st, h * 2.), WHITE);
                ui.fill_rect(Rect::new(st, -eh, 0., eh + h).feather(0.005), BLUE);
                ui.fill_circle(st, -eh, rad, BLUE);
                if self.exercise_press.is_none() {
                    let r = ui.rect_to_global(Rect::new(st, -eh, 0., 0.).feather(rad));
                    self.exercise_press = Judge::get_touches()
                        .iter()
                        .find(|it| it.phase == TouchPhase::Started && r.contains(it.position))
                        .map(|it| (-1, it.id));
                }
                ui.fill_rect(Rect::new(en, -h, 0., eh + h).feather(0.005), RED);
                ui.fill_circle(en, eh, rad, RED);
                if self.exercise_press.is_none() {
                    let r = ui.rect_to_global(Rect::new(en, eh, 0., 0.).feather(rad));
                    self.exercise_press = Judge::get_touches()
                        .iter()
                        .find(|it| it.phase == TouchPhase::Started && r.contains(it.position))
                        .map(|it| (1, it.id));
                }
                ui.fill_rect(Rect::new(cur, -h, 0., h * 2.).feather(0.005), GREEN);
                ui.fill_circle(cur, 0., rad, GREEN);
                if self.exercise_press.is_none() {
                    let r = ui.rect_to_global(Rect::new(cur, 0., 0., 0.).feather(rad));
                    self.exercise_press = Judge::get_touches()
                        .iter()
                        .find(|it| it.phase == TouchPhase::Started && r.contains(it.position))
                        .map(|it| (0, it.id));
                }
                ui.text(fmt_time(t as f32)).pos(0., -0.23).anchor(0.5, 0.).size(0.8).draw_using(font);
                if let Some((ctrl, id)) = &self.exercise_press {
                    if let Some(touch) = Judge::get_touches().iter().rfind(|it| it.id == *id) {
                        let x = touch.position.x;
                        let p = (x + hw) as f64 / (hw * 2.) as f64 * (self.res.track_length - sp) + sp;
                        let p = if self.res.track_length - sp <= 3. || *ctrl == 0 {
                            p.clamp(sp, self.res.track_length)
                        } else {
                            p.clamp(
                                if *ctrl == -1 { sp } else { self.exercise_range.start + 3. },
                                if *ctrl == -1 {
                                    self.exercise_range.end - 3.
                                } else {
                                    self.res.track_length
                                },
                            )
                        };
                        if *ctrl == 0 {
                            tm.seek_to(p);
                            self.music.seek_to(p)?;
                            self.bad_notes.clear();
                            self.judge.reset();
                            self.chart.reset();
                            self.res.judge_line_color = self.res.res_pack.info.color_perfect();
                        } else {
                            *(if *ctrl == -1 {
                                &mut self.exercise_range.start
                            } else {
                                &mut self.exercise_range.end
                            }) = p;
                        }
                        if matches!(touch.phase, TouchPhase::Cancelled | TouchPhase::Ended) {
                            self.exercise_press = None;
                        }
                    }
                }
                ui.dy(0.2);
                let r = ui.text(tl!("to")).size(0.8).anchor(0.5, 0.).draw_using(font);
                let mut tx = ui
                    .text(fmt_time(self.exercise_range.start as f32))
                    .pos(r.x - 0.02, 0.)
                    .anchor(1., 0.)
                    .size(0.8)
                    .color(BLACK);
                let re = tx.measure_using(font);
                self.exercise_btns.0.set(tx.ui, re);
                tx.ui
                    .fill_rect(re.feather(0.01), Color::new(1., 1., 1., if self.exercise_btns.0.touching() { 0.5 } else { 1. }));
                tx.draw_using(font);

                let mut tx = ui
                    .text(fmt_time(self.exercise_range.end as f32))
                    .pos(r.right() + 0.02, 0.)
                    .size(0.8)
                    .color(BLACK);
                let re = tx.measure_using(font);
                self.exercise_btns.1.set(tx.ui, re);
                tx.ui
                    .fill_rect(re.feather(0.01), Color::new(1., 1., 1., if self.exercise_btns.1.touching() { 0.5 } else { 1. }));
                tx.draw_using(font);
                for touch in ui.ensure_touches() {
                    touch.position /= asp;
                }
            }
        }
        if let Some(time) = self.pause_rewind {
            let dt = tm.now() - time;
            let t = 3 - dt.floor() as i32;
            if t <= 0 {
                self.pause_rewind = None;
            } else {
                let a = (1. - dt as f32 / 3.) * 1.;
                let h = 1. / self.res.aspect_ratio;
                draw_rectangle(-1., -h, 2., h * 2., Color::new(0., 0., 0., a));
                ui.text(t.to_string()).anchor(0.5, 0.5).size(1.).color(c).draw_using(font);
            }
        }
        if self.res.config.chart_debug_line || self.res.config.chart_debug_note {
            self.debug_overlay(ui);
        }
        if self.res.config.pause_offset_adjust && self.mode != GameMode::TweakOffset && tm.paused() && self.pause_rewind.is_none() {
            self.pause_offset_ui(ui);
        }
        if self.res.config.touch_debug {
            // 触点标记：样式（圆点 / 命中特效贴图 / 头像）、颜色、不透明度都能配
            let alpha = self.res.config.touch_marker_alpha_value();
            let (r, g, b) = self.res.config.touch_marker_color.rgb();
            let color = Color::new(r, g, b, alpha);
            let style = self.res.config.touch_marker_style;
            for touch in Judge::get_touches() {
                self.draw_touch_marker(ui, touch.position, style, color);
            }
        }
        for pos in &self.touch_points {
            let alpha = self.res.config.touch_marker_alpha_value();
            let (r, g, b) = self.res.config.touch_marker_color.rgb();
            self.draw_touch_marker(ui, Vec2::new(pos.0, pos.1), self.res.config.touch_marker_style, Color::new(r, g, b, alpha));
        }
        Ok(())
    }

    /// 画一个触点标记。
    fn draw_touch_marker(&self, ui: &mut Ui, pos: Vec2, style: crate::config::TouchMarkerStyle, color: Color) {
        use crate::config::TouchMarkerStyle::*;
        const R: f32 = 0.04;
        match style {
            Circle => ui.fill_circle(pos.x, pos.y, R, color),
            HitFx => {
                let r = Rect::new(pos.x - R, pos.y - R, R * 2., R * 2.);
                ui.fill_rect(r, (*self.res.res_pack.hit_fx, r, ScaleType::Fit, color));
            }
            Avatar => {
                let r = Rect::new(pos.x - R, pos.y - R, R * 2., R * 2.);
                ui.fill_rect(r, (*self.res.player, r, ScaleType::Fit, color));
            }
        }
    }

    fn interactive(res: &Resource, state: &State) -> bool {
        res.config.interactive && matches!(state, State::Playing)
    }

    /// 调试显示：判定线编号 / 线高 / z-index，以及音符时间与横向判定范围。
    fn debug_overlay(&self, ui: &mut Ui) {
        let res = &self.res;
        let lines = &self.chart.lines;
        let font = text_font(&res.config);
        if res.config.chart_debug_line {
            for (id, line) in lines.iter().enumerate() {
                let tr = line.now_transform(res, lines);
                let pos = tr.transform_point(&Point::new(0., 0.));
                let h = line.height.now();
                // f32 的 ULP：越大说明这个线高在浮点上越不可靠（速度快到丢精度）
                let ulp = if h == 0. { 0. } else { f32::from_bits(h.to_bits() + 1) - h };
                let color = if ulp > 0.018518519 {
                    RED
                } else if ulp > 0.0018518519 {
                    YELLOW
                } else {
                    WHITE
                };
                let kind = match &line.kind {
                    JudgeLineKind::Normal => "",
                    JudgeLineKind::Texture(..) => " img",
                    JudgeLineKind::TextureGif(..) => " gif",
                    JudgeLineKind::Text(..) => " text",
                    JudgeLineKind::Paint(..) => " paint",
                };
                let z = line.z_index;
                let attach = if line.attach_ui.is_some() { " +ui" } else { "" };
                ui.text(format!("[{id}] h:{h:.2} z:{z}{attach}{kind}"))
                    .pos(pos.x, pos.y - 0.012)
                    .anchor(0.5, 1.)
                    .size(0.045)
                    .color(color)
                    .draw_using(font);
            }
        }
        if res.config.chart_debug_note {
            let x_diff = res.config.x_diff_max() as f32;
            for (id, line) in lines.iter().enumerate() {
                let tr = line.now_transform(res, lines);
                for note in &line.notes {
                    if (note.time - res.time).abs() > 0.35 {
                        continue;
                    }
                    let mat = tr * note.object.now(res);
                    let pos = mat.transform_point(&Point::new(0., 0.));
                    let half = x_diff * note.judge_area;
                    let a = mat.transform_point(&Point::new(-half, 0.));
                    let b = mat.transform_point(&Point::new(half, 0.));
                    ui.fill_rect(Rect::new(a.x, pos.y - 0.005, b.x - a.x, 0.01), Color::new(1., 0.6, 0.2, 0.3));
                    let kind = match &note.kind {
                        NoteKind::Click => "click",
                        NoteKind::Hold { .. } => "hold",
                        NoteKind::Flick => "flick",
                        NoteKind::Drag => "drag",
                    };
                    ui.text(format!("[{id}] t:{:.2} h:{:.0} {kind}", note.time, note.height))
                        .pos(pos.x, pos.y - 0.008)
                        .anchor(0.5, 1.)
                        .size(0.04)
                        .color(WHITE)
                        .draw_using(font);
                }
            }
        }
    }

    /// 点击后立刻改变 `config.offset`（判定立刻跟着变），并把新值交给客户端保存。
    /// 暂停时的微调面板：判定偏移 ±1ms / ±5ms / 重置，外加音乐 / 音效音量。
    ///
    /// 位置放在**屏幕下方**：暂停菜单的返回 / 重试 / 继续三个图标是屏幕正中的，
    /// 之前这块面板画在正中，直接和它们糊在一起了。
    /// 音量改完立刻生效（`Music::set_amplifier` + `UI_SFX_VOLUME`），
    /// 新值通过 `PENDING_VOLUME` 交给客户端写回配置保存。
    fn pause_offset_ui(&mut self, ui: &mut Ui) {
        // 字号按打歌界面的尺度来（见 `IN_GAME_TEXT_H`）：
        // 标题 0.6（≈ 屏幕高的 2.9%，和原生偏移面板的数值同档）、
        // 按钮 0.42（= 原生 `Ui::button` 的字号）、行文字 0.4（= ACC / 滑条标签）。
        // 之前这里写的是 0.1 / 0.05，字高只有屏幕高的 0.3%~0.5%（1080p 上 3~5 像素），
        // 整个面板等于一片没有字的黑块。
        // 字体同样跟着打歌界面的字体走：phire 的偏移面板就是没有额外指定字体的
        // `ui.text(...).draw()`，它全局只有 font.ttf 一套字体。
        let font = text_font(&self.res.config);
        let w = 0.94;
        let h = 0.38;
        let x = -w / 2.;
        // 贴着屏幕下边缘（y 正方向朝下），并且保证不侵入中间那三个暂停图标
        // （它们以屏幕中心为圆心、半高 0.06）。
        let bottom = 1. / self.res.aspect_ratio;
        let y = (bottom - h - 0.02).max(0.075);
        let touches = Judge::get_touches();
        let clicked = |rect: Rect| -> bool { touches.iter().any(|it| it.phase == TouchPhase::Started && rect.contains(it.position)) };
        let mut button = |ui: &mut Ui, rect: Rect, label: &str, size: f32| -> bool {
            let hit = clicked(rect);
            ui.fill_rect(
                rect,
                if hit {
                    Color::new(1., 1., 1., 0.85)
                } else {
                    Color::new(1., 1., 1., 0.22)
                },
            );
            ui.text(label)
                .pos(rect.center().x, rect.center().y)
                .anchor(0.5, 0.5)
                .max_width(rect.w * 0.86)
                .size(size)
                .color(if hit { BLACK } else { WHITE })
                .no_baseline()
                .draw_using(font);
            hit
        };
        ui.fill_rect(Rect::new(x, y, w, h), Color::new(0., 0., 0., 0.72));

        // ---- 判定偏移 ----
        ui.text(format!("{} {:+.0}ms", tl!("adjust-offset"), self.res.config.offset * 1000.))
            .pos(0., y + 0.03)
            .anchor(0.5, 0.)
            .size(0.6)
            .color(WHITE)
            .draw_using(font);
        let bw = 0.13;
        let bh = 0.075;
        let gap = 0.02;
        let total = bw * 5. + gap * 4.;
        let mut bx = x + (w - total) / 2.;
        let by = y + 0.115;
        let mut delta = 0.;
        let mut reset = false;
        for (label, d) in [("-5", -0.005f32), ("-1", -0.001), ("+1", 0.001), ("+5", 0.005)] {
            if button(ui, Rect::new(bx, by, bw, bh), label, 0.42) {
                delta += d;
            }
            bx += bw + gap;
        }
        if button(ui, Rect::new(bx, by, bw, bh), &tl!("offset-reset"), 0.36) {
            reset = true;
        }

        // ---- 音量（游戏内实时可调）----
        // 一行一个：左边文字标签，右边 [-] 数值 [+]
        let mut music_delta = 0.;
        let mut sfx_delta = 0.;
        let vh = 0.075;
        let mut row = |ui: &mut Ui, cy: f32, label: String, value: f32| -> f32 {
            ui.text(label)
                .pos(x + 0.045, cy)
                .anchor(0., 0.5)
                .size(0.4)
                .color(semi_white(0.9))
                .no_baseline()
                .draw_using(font);
            ui.text(format!("{:.0}%", value * 100.))
                .pos(x + 0.60, cy)
                .anchor(0.5, 0.5)
                .size(0.4)
                .color(WHITE)
                .no_baseline()
                .draw_using(font);
            let mut d = 0.;
            if button(ui, Rect::new(x + 0.46, cy - vh / 2., 0.10, vh), "-", 0.42) {
                d = -0.05;
            }
            if button(ui, Rect::new(x + 0.74, cy - vh / 2., 0.10, vh), "+", 0.42) {
                d = 0.05;
            }
            d
        };
        let music = self.res.config.volume_music.clamp(0., 1.);
        let sfx = self.res.config.volume_sfx.clamp(0., 1.);
        music_delta += row(ui, y + 0.255, tl!("volume-music").into_owned(), music);
        sfx_delta += row(ui, y + 0.33, tl!("volume-sfx").into_owned(), sfx);

        let mut volume_changed = false;
        if music_delta != 0. {
            let v = (self.res.config.volume_music + music_delta).clamp(0., 1.);
            self.res.config.volume_music = v;
            // 立刻作用到正在播放的曲子（不然得等这首打完才生效）
            let _ = self.music.set_amplifier(v);
            volume_changed = true;
        }
        if sfx_delta != 0. {
            let v = (self.res.config.volume_sfx + sfx_delta).clamp(0., 1.);
            self.res.config.volume_sfx = v;
            crate::ui::UI_SFX_VOLUME.store(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
            // 顺手播一下按钮音，直接听效果
            crate::ui::button_hit();
            volume_changed = true;
        }
        if volume_changed {
            PENDING_VOLUME.with(|it| it.set(Some((self.res.config.volume_music, self.res.config.volume_sfx))));
        }

        if reset {
            self.res.config.offset = 0.;
            PENDING_OFFSET.with(|it| it.set(Some(0.)));
        } else if delta != 0. {
            self.res.config.offset = (self.res.config.offset + delta).clamp(-0.5, 0.5);
            PENDING_OFFSET.with(|it| it.set(Some(self.res.config.offset)));
        }
    }

    fn offset(&self) -> f32 {
        self.chart.offset + self.res.config.offset + self.info_offset
    }

    fn tweak_offset(&mut self, ui: &mut Ui, ita: bool) {
        // 暂停微调面板（TweakOffset 模式）的字号同样是打歌界面的尺度，
        // 字体也跟打歌界面走（phire 里这个面板是 `ui.text(...).draw()` = font.ttf）。
        let font = text_font(&self.res.config);
        ui.scope(|ui| {
            let width = 0.55;
            let height = 0.4;
            ui.dx(1. - width - 0.02);
            ui.dy(ui.top - height - 0.02);
            ui.fill_rect(Rect::new(0., 0., width, height), GRAY);
            ui.dy(0.02);
            ui.text(tl!("adjust-offset")).pos(width / 2., 0.).anchor(0.5, 0.).size(0.7).draw_using(font);
            ui.dy(0.16);
            let r = ui
                .text(format!("{}ms", (self.info_offset * 1000.).round() as i32))
                .pos(width / 2., 0.)
                .anchor(0.5, 0.)
                .size(0.6)
                .no_baseline()
                .draw_using(font);
            let d = 0.14;
            if ui.button("lg_sub", Rect::new(d, r.center().y, 0., 0.).feather(0.026), "-") && ita {
                self.info_offset -= 0.05;
            }
            if ui.button("lg_add", Rect::new(width - d, r.center().y, 0., 0.).feather(0.026), "+") && ita {
                self.info_offset += 0.05;
            }
            let d = 0.08;
            if ui.button("sm_sub", Rect::new(d, r.center().y, 0., 0.).feather(0.022), "-") && ita {
                self.info_offset -= 0.005;
            }
            if ui.button("sm_add", Rect::new(width - d, r.center().y, 0., 0.).feather(0.022), "+") && ita {
                self.info_offset += 0.005;
            }
            let d = 0.03;
            if ui.button("ti_sub", Rect::new(d, r.center().y, 0., 0.).feather(0.017), "-") && ita {
                self.info_offset -= 0.001;
            }
            if ui.button("ti_add", Rect::new(width - d, r.center().y, 0., 0.).feather(0.017), "+") && ita {
                self.info_offset += 0.001;
            }
            ui.dy(0.14);
            let pad = 0.02;
            let spacing = 0.01;
            let mut r = Rect::new(pad, 0., (width - pad * 2. - spacing * 2.) / 3., 0.06);
            if ui.button("cancel", r, tl!("offset-cancel")) {
                self.next_scene = Some(NextScene::PopWithResult(Box::new(None::<f32>)));
            }
            r.x += r.w + spacing;
            if ui.button("reset", r, tl!("offset-reset")) {
                self.info_offset = 0.;
            }
            r.x += r.w + spacing;
            if ui.button("save", r, tl!("offset-save")) {
                self.next_scene = Some(NextScene::PopWithResult(Box::new(Some(self.info_offset))));
            }
        });
    }
    pub fn get_avg_fps(&self) -> Option<f32> {
        if self.fps_frame_count > 0 && self.fps_total_time > 0.0 {
            Some(self.fps_frame_count as f32 / self.fps_total_time as f32)
        } else {
            None
        }
    }

    /// 实时 HUD（H1~H8）。
    ///
    /// 用自带的九宫格定位，和谱面的 attach-ui 无关；描边用「先画一层暗色偏移再画亮色」
    /// 装饰性准度条（Timing Bar）：真实判定事件驱动，不影响判定结果。
    fn timing_bar_ui(&self, ui: &mut Ui) {
        let config = &self.res.config;
        if !config.timing_bar.enabled {
            return;
        }
        let windows = TimingWindows::from_config(config);
        timing_bar::render(ui, &config.timing_bar, &self.timing_bar, &windows);
    }

    /// 冒充（文本 builder 没有真正的 stroke）。
    fn hud_ui(&self, ui: &mut Ui) {
        let res = &self.res;
        let config = &res.config;
        if !config.hud {
            return;
        }
        let alpha = config.hud_alpha_value() * res.alpha;
        if alpha <= 0.01 {
            return;
        }
        let judge = &self.judge;
        let font = text_font(config);
        // HUD 的字号：原生 ACC 是 0.4、连击文字也是 0.4，HUD 比它们再小一档取 0.35
        // （≈ 屏幕高的 1.7%，1080p 上约 18 像素），设置里的倍率在 0.5x~2x 之间调。
        //
        // ⚠️ **HUD 不跟打歌界面的字号一起放大**。它是自建的调试叠加层，不是 phire 的东西；
        // 跟着 `PHIRE_SCALE_RATIO` 放大之后（0.35 * 1.7778）实测又大又散，
        // 所以基准保持 0.35，行距从 1.9 收到 **1.45** —— HUD 要的是"一眼扫完"。
        // 行距按**字高**算（`IN_GAME_TEXT_H * size`），不是按 size 算：
        // 字高只有 0.055 * size，行距写 `size * 1.45` 会得到比字高大 26 倍的空档。
        let size = 0.35 * config.hud_size_scale();
        let text_h = IN_GAME_TEXT_H * size;
        let line_h = text_h * 1.45;
        let dim = semi_white(alpha * 0.92);
        let dim2 = semi_white(alpha * 0.72);
        let mut items: Vec<(String, Color)> = Vec::new();
        if config.hud_acc {
            items.push((format!("ACC {:.2}%", judge.real_time_accuracy() * 100.), dim));
        }
        if config.hud_counts {
            let c = judge.counts();
            items.push((format!("P{} G{} B{} M{}", c[0], c[1], c[2], c[3]), dim));
        }
        if config.hud_max_combo {
            items.push((format!("MAX {} ↑{}", judge.max_combo(), judge.combo()), dim));
        }
        if config.hud_delta {
            let text = match judge.feedback.last() {
                Some((_, diff, _)) => format!("{:+05.0}ms", diff * 1000.),
                None => "  ±--ms".to_owned(),
            };
            items.push((text, dim));
        }
        if config.hud_rks {
            let score = judge.score(config.has_mod(Mods::NO_COMBO_SCORE));
            items.push((format!("RKS~{:.2}", config.estimate_rks(res.info.difficulty, score)), dim));
        }
        if config.hud_time {
            items.push((format!("{:.1}s/{:.1}s", res.time.max(0.), res.track_length), dim));
        }
        if config.hud_fps {
            let fps = self.get_avg_fps().map(|it| format!("{it:.0} FPS")).unwrap_or_else(|| "-- FPS".to_owned());
            items.push((fps, dim2));
        }
        if items.is_empty() {
            return;
        }

        let (ax, ay) = config.hud_corner.anchor();
        let margin = 0.03 + text_h;
        let n = items.len() as f32;
        // 注意：这个坐标系的 y 向下为正，top 是屏幕上边缘（负值）
        let top = -1. / res.aspect_ratio;
        let x = ax * (1. - margin);
        let anchor_x = if ax > 0. {
            1.
        } else if ax < 0. {
            0.
        } else {
            0.5
        };
        let mut y = if ay > 0. {
            top + margin
        } else if ay < 0. {
            -top - margin - (n - 1.) * line_h
        } else {
            -(n - 1.) * line_h / 2.
        };
        let outline = config.hud_outline;
        // 描边是「同一行暗色偏移一遍」，偏移量按字高取，不能按 size 取
        let outline_off = text_h * 0.09;
        for (text, color) in &items {
            if outline {
                let mut shadow = *color;
                shadow.a *= 0.85;
                shadow.r = 0.;
                shadow.g = 0.;
                shadow.b = 0.;
                ui.text(text)
                    .pos(x + outline_off, y + outline_off)
                    .anchor(anchor_x, 0.)
                    .size(size)
                    .color(shadow)
                    .draw_using(font);
            }
            ui.text(text)
                .pos(x, y)
                .anchor(anchor_x, 0.)
                .size(size)
                .color(*color)
                .draw_using(font);
            y += line_h;
        }
        // 进度条跟着 HUD 一起走（hud_time 打开时才有）
        if config.hud_time {
            let w = 0.34;
            let h = 0.008;
            let bx = match anchor_x {
                1. => x - w,
                0. => x,
                _ => x - w / 2.,
            };
            let by = y - line_h + line_h * 0.45;
            ui.fill_rect(Rect::new(bx, by, w, h), semi_white(alpha * 0.25));
            let prog = (res.time / res.track_length.max(0.001)).clamp(0., 1.) as f32;
            ui.fill_rect(Rect::new(bx, by, w * prog, h), semi_white(alpha * 0.85));
        }
    }

    /// 判定文字（V12）：只显示最近一次判定，按配置的样式（淡出 / 上浮 / 弹出）出现。
    fn judge_text_ui(&self, ui: &mut Ui) {
        let res = &self.res;
        let config = &res.config;
        if !config.judge_text {
            return;
        }
        const LIFE: f64 = 0.75;
        let Some(&(what, at)) = self.judge.recent_judgements.last() else {
            return;
        };
        let age = res.time - at;
        if !(0. ..LIFE).contains(&age) {
            return;
        }
        let k = (age / LIFE) as f32;
        let alpha = ((1. - k) * 1.6).min(1.).max(0.) * res.alpha;
        if alpha <= 0.01 {
            return;
        }
        let style = config.judge_text_style;
        // 判定文字配色（P 金 / G 蓝 / Bad 红 / Miss 白），每种都配一层同色外发光
        let color = match what {
            Judgement::Perfect => Color::new(1.00, 0.85, 0.36, alpha),
            Judgement::Good => Color::new(0.42, 0.74, 1.00, alpha),
            Judgement::Bad => Color::new(1.00, 0.38, 0.38, alpha),
            Judgement::Miss => Color::new(1.00, 1.00, 1.00, alpha),
        };
        let text = match what {
            Judgement::Perfect => "PERFECT",
            Judgement::Good => "GOOD",
            Judgement::Bad => "BAD",
            Judgement::Miss => "MISS",
        };
        // 弹出：刚判定的 0.1 秒内从 1.5 倍缩回正常
        let pop = style.pop_ratio();
        let pop_scale = if pop > 0. {
            1. + (1. - (age / 0.1).min(1.)) as f32 * 0.5 * pop
        } else {
            1.
        };
        let rise = style.rise_ratio();
        let y = -0.04 - rise * k * 0.20;
        let size = config.judge_text_size_value() * pop_scale;
        let font = text_font(config);
        // 外发光：没有 shader / 后处理，只能拿「同色偏移副本」冒充。
        // 半径必须是**字高的百分之几**（字高 ≈ IN_GAME_TEXT_H * size）：
        // size 0.62 时字高 ≈ 0.034 世界坐标，1080p（1 世界单位 = 960px）上约 33px，
        // 于是三圈 ≈ 1.8 / 3.9 / 7.2 px —— 才是"糊出去一圈光"的观感。
        //
        // ⚠️ 这里曾经写成 `0.8` / `1.7`（= 字高的 0.8 / 1.7 倍，1080p 上 26 / 56 px），
        // 副本比字还大，画出来是**围着本体摆了一圈能看清的 PERFECT**，不是发光。
        // 改这里的半径前先按上面的换算在脑子里过一遍像素数。
        const GLOW_RINGS: [(f32, f32); 3] = [
            (0.055, 0.55), // 内圈：贴着一圈实光，负责"描边感"
            (0.12, 0.30),  // 中圈
            (0.22, 0.14),  // 外圈：散得最开、最淡，负责"发出去"
        ];
        let glow_unit = IN_GAME_TEXT_H * size;
        // 由外向内画，中心自然叠得最亮
        for (radius, strength) in GLOW_RINGS.iter().rev() {
            let mut glow = color;
            glow.a = alpha * strength;
            for (dx, dy) in [
                (-1., 0.),
                (1., 0.),
                (0., -1.),
                (0., 1.),
                (-0.707, -0.707),
                (0.707, -0.707),
                (-0.707, 0.707),
                (0.707, 0.707),
            ] {
                ui.text(text)
                    .pos(dx * glow_unit * radius, y + dy * glow_unit * radius)
                    .anchor(0.5, 0.5)
                    .size(size)
                    .color(glow)
                    .draw_using(font);
            }
        }
        ui.text(text).pos(0., y).anchor(0.5, 0.5).size(size).color(color).draw_using(font);
    }

    /// 漏键标记（V11）：Bad / Miss 的位置在判定线上留一个淡出的叉，方便复盘漏在哪。
    fn miss_marker_ui(&mut self, ui: &mut Ui) {
        let life = self.res.config.miss_marker_secs() as f64;
        if !self.res.config.miss_marker {
            return;
        }
        let res = &mut self.res;
        let chart = &self.chart;
        let base_alpha = res.alpha;
        let now = res.time;
        for mark in &self.judge.miss_marks {
            let age = now - mark.time;
            if !(0. ..life).contains(&age) {
                continue;
            }
            let k = (age / life) as f32;
            let mut color = Color::new(1., 0.32, 0.32, (1. - k) * (1. - k) * 0.95 * base_alpha);
            if color.a <= 0.01 {
                continue;
            }
            let Some(line) = chart.lines.get(mark.line as usize) else {
                continue;
            };
            let tr = line.now_transform(res, &chart.lines);
            let x = mark.x;
            color.a *= 1.;
            res.with_model(tr, |_res| {
                let s = 0.02;
                let w = 0.006;
                draw_line(x - s, -s, x + s, s, w, color);
                draw_line(x - s, s, x + s, -s, w, color);
            });
        }
        let _ = ui;
    }
}

impl Scene for GameScene {
    fn enter(&mut self, tm: &mut TimeManager, target: Option<RenderTarget>) -> Result<()> {
        Ui::clear_practice_speed_drag();
        #[cfg(target_arch = "wasm32")]
        on_game_start();
        #[cfg(target_env = "ohos")]
        miniquad::native::set_interceptor_state(true);
        self.music = Self::new_music(&mut self.res)?;
        self.res.camera.render_target = target;
        tm.speed = self.res.config.speed as _;
        tm.adjust_time = self.res.config.adjust_time;
        reset!(self, self.res, tm);
        set_camera(&self.res.camera);
        self.first_in = true;
        Ok(())
    }

    fn pause(&mut self, tm: &mut TimeManager) -> Result<()> {
        Ui::clear_practice_speed_drag();
        if !tm.paused() {
            self.pause_rewind = None;
            self.music.pause()?;
            tm.pause();
        }
        #[cfg(target_env = "ohos")]
        miniquad::native::set_interceptor_state(false);
        Ok(())
    }

    fn resume(&mut self, tm: &mut TimeManager) -> Result<()> {
        if !matches!(self.state, State::Playing) {
            tm.resume();
        }
        Ok(())
    }

    fn update(&mut self, tm: &mut TimeManager) -> Result<()> {
        self.res.audio.recover_if_needed()?;
        // 摇一摇再玩：设备静止一段时间就自动暂停，摇动超过阈值就自动继续
        // （只恢复自己暂停的，不会打断玩家手动暂停）
        if self.res.config.motion_pause
            && !self.dead
            && matches!(self.mode, GameMode::Normal | GameMode::NoRetry | GameMode::View)
            && motion_level() >= 0.
        {
            let now = tm.real_time();
            let dt = (now - self.motion_last).clamp(0., 0.5) as f32;
            self.motion_last = now;
            let level = motion_level();
            if level >= self.res.config.motion_threshold_g() {
                self.still_time = 0.;
                if self.paused_by_motion {
                    self.paused_by_motion = false;
                    if tm.paused() {
                        self.music.play()?;
                        tm.resume();
                        show_message(tl!("shake-resumed")).ok();
                    }
                }
            } else if !tm.paused() && matches!(self.state, State::Playing) {
                self.still_time += dt;
                if self.still_time >= self.res.config.motion_still_secs() {
                    self.paused_by_motion = true;
                    self.still_time = 0.;
                    if !self.music.paused() {
                        self.music.pause()?;
                    }
                    tm.pause();
                    show_message(tl!("still-paused")).ok();
                }
            } else if !self.paused_by_motion {
                self.still_time = 0.;
            }
        }
        if matches!(self.state, State::Playing) {
            tm.update(self.music.position());
        }
        if self.mode == GameMode::Exercise && tm.now() > self.exercise_range.end && !tm.paused() {
            let state = self.state.clone();
            reset!(self, self.res, tm);
            self.state = state;
            tm.seek_to(self.exercise_range.start);
            tm.pause();
            self.music.pause()?;
            #[cfg(target_env = "ohos")]
            miniquad::native::set_interceptor_state(false);
        }
        let offset = self.offset();
        let time = tm.now();
        let time = match self.state {
            State::Starting => {
                if time >= Self::BEFORE_TIME {
                    self.res.alpha = 1.;
                    self.state = State::BeforeMusic;
                    tm.reset();
                    tm.seek_to(if self.mode == GameMode::Exercise {
                        self.exercise_range.start
                    } else {
                        offset.min(0.) as f64
                    });
                    self.last_update_time = tm.real_time();
                    if self.first_in && self.mode == GameMode::Exercise {
                        tm.pause();
                        self.first_in = false;
                    }
                    tm.now()
                } else {
                    #[cfg(target_os = "windows")]
                    {
                        // wtf bro. why must particles exist on Windows?
                        let emitter_config = self.res.emitter.emitter.config.clone();
                        let emitter_square_config = self.res.emitter.emitter_square.config.clone();
                        self.res.emitter.emitter.config.size = 0.0;
                        self.res.emitter.emitter_square.config.size = 0.0;
                        self.res.emitter.emitter.emit(vec2(0.0, 0.0), 1);
                        self.res.emitter.emitter_square.emit(vec2(0.0, 0.0), 1);
                        self.res.emitter.emitter.config = emitter_config;
                        self.res.emitter.emitter_square.config = emitter_square_config;
                    }
                    self.res.alpha = (1. - (1. - time / Self::BEFORE_TIME).powi(3)) as f32;
                    if self.mode == GameMode::Exercise {
                        self.exercise_range.start
                    } else {
                        offset as f64
                    }
                }
            }
            State::BeforeMusic => {
                if time >= 0.0 {
                    self.music.seek_to(time)?;
                    if !tm.paused() {
                        self.music.play()?;
                    }
                    self.state = State::Playing;
                }
                time
            }
            State::Playing => {
                if time > self.res.track_length + WAIT_TIME {
                    self.state = State::Ending;
                    #[cfg(target_env = "ohos")]
                    miniquad::native::set_interceptor_state(false);
                }
                time
            }
            State::Ending => {
                let t = time - self.res.track_length - WAIT_TIME;
                if t >= AFTER_TIME + 0.3 {
                    let no_combo_score = self.res.config.has_mod(Mods::NO_COMBO_SCORE);
                    let result = self.judge.result(no_combo_score);
                    // 课题模式：不落盘、不进结算画面，直接把成绩回传给课题场景。
                    if self.res.config.challenge_mode {
                        self.music.pause()?;
                        self.next_scene = Some(NextScene::PopWithResult(Box::new(ChallengeEvent::Completed(result))));
                        return Ok(());
                    }
                    // 成绩上传：默认关（`Config::upload_record` 默认 false），只有用户在
                    // 设置里打开「上传成绩」后才会构造记录。条件与官方一致：
                    // 非离线模式、没有 UNRATED mod、不是键盘游玩、速度 ≥ 1.0。
                    // 这段原来在 `#[cfg(closed)]` 里，是开源构建传不了成绩的直接原因。
                    let can_upload = self.upload_fn.is_some()
                        && !self.res.config.offline_mode
                        && !self.res.config.mods.intersects(Mods::UNRATED)
                        && !self.res.config.use_keyboard
                        && self.res.config.speed >= 1.0 - 1e-3;
                    let record_data = match (can_upload, &self.player, &self.res.info.id) {
                        (true, Some(player), Some(chart)) => Some(crate::record::encode_record(
                            player.id,
                            *chart,
                            self.res.config.speed,
                            &result,
                            &self.chart_bytes,
                        )),
                        _ => None,
                    };
                    let record = if self.res.config.mods.intersects(Mods::UNRATED) || self.res.config.speed < 1.0 - 1e-3 {
                        None
                    } else {
                        Some(SimpleRecord {
                            score: result.score as _,
                            accuracy: result.accuracy as _,
                            full_combo: result.max_combo == result.num_of_notes,
                            counts: result.counts,
                            max_combo: result.max_combo,
                            num_of_notes: result.num_of_notes,
                            hist: result.hist.to_vec(),
                        })
                    };
                    self.next_scene = match self.mode {
                        GameMode::Normal | GameMode::NoRetry | GameMode::View => {
                            let historic_best = self.player.as_ref().map_or(0, |it| it.historic_best);
                            if let Some(new_rec) = &record {
                                if let Some(f) = &self.save_fn {
                                    f(new_rec.clone())?;
                                }
                                if let Some(best) = &mut self.best_record {
                                    best.update(new_rec);
                                } else {
                                    self.best_record = record.clone();
                                }
                                if let Some(best) = &self.best_record {
                                    if let Some(player) = &mut self.player {
                                        player.historic_best = player.historic_best.max(best.score as _);
                                    }
                                }
                            }
                            Some(NextScene::Overlay(Box::new(EndingScene::new(
                                self.res.background.clone(),
                                self.res.illustration.clone(),
                                self.res.player.clone(),
                                self.res.icons.clone(),
                                self.res.icon_retry.clone(),
                                self.res.icon_proceed.clone(),
                                self.res.mod_icons.clone(),
                                self.res.challenge_icons[self.res.config.challenge_color as usize].clone(),
                                self.res.config.challenge_rank,
                                self.res.info.clone(),
                                self.judge.result(no_combo_score),
                                &self.res.config,
                                self.res.res_pack.ending.clone(),
                                self.upload_fn.as_ref().map(Arc::clone),
                                self.player.as_ref().map(|it| it.rks),
                                historic_best,
                                record_data,
                                self.best_record.clone(),
                                if self.res.config.show_avg_fps { self.get_avg_fps() } else { None },
                            )?)))
                        }
                        GameMode::TweakOffset => Some(NextScene::PopWithResult(Box::new(None::<f32>))),
                        GameMode::Exercise => None,
                    };
                }
                self.res.alpha = (1. - (t / AFTER_TIME).min(1.).powi(2)) as f32;
                self.res.track_length
            }
        };
        let time = (time - offset as f64).max(0.);
        self.res.time = time;
        if !tm.paused() && self.pause_rewind.is_none() && self.mode != GameMode::View {
            self.gl.quad_gl.viewport(self.res.camera.viewport);
            self.judge.update(&mut self.res, &mut self.chart, &mut self.bad_notes);
            self.gl.quad_gl.viewport(None);
        } else {
            // 暂停 / 查看模式不应保留上一帧的手指 ID、悬停或音乐滤波。
            self.chart.blocked_touches.clear();
        }
        if tm.paused() || self.pause_rewind.is_some() {
            self.block_audio.suspend(&mut self.music);
        } else {
            self.block_audio.sync(&mut self.music, matches!(self.state, State::Playing) && !self.chart.blocked_touches.is_empty());
        }
        // 装饰性准度条：消费本帧新增的判定（`feedback` 只保留最近 2 秒，故按时间戳增量喂）。
        if self.res.config.timing_bar.enabled {
            let cfg = self.res.config.timing_bar.clone();
            let rt = tm.real_time() as f64;
            for (what, diff, at) in &self.judge.feedback {
                if *at > self.timing_bar_seen {
                    self.timing_bar.record(
                        &JudgeReportEvent {
                            time: *at,
                            line_id: 0,
                            note_id: 0,
                            judgement: Ok(*what),
                            difference: *diff,
                        },
                        &NoteKind::Click,
                        &cfg,
                        rt,
                    );
                    self.timing_bar_seen = self.timing_bar_seen.max(*at);
                }
            }
            self.timing_bar.animate(rt, &cfg);
        }
        if let Some(update) = &mut self.update_fn {
            update(self.res.time, &mut self.res, &mut self.judge);
        }
        let counts = self.judge.counts();
        self.res.judge_line_color = if counts[2] + counts[3] == 0 && self.res.config.ap_fc_indicator {
            if counts[1] == 0 {
                self.res.res_pack.info.color_perfect()
            } else {
                self.res.res_pack.info.color_good()
            }
        } else {
            WHITE
        };
        if !self.dead
            && matches!(self.state, State::Playing)
            && (self.res.config.mods.contains(Mods::INSTANT_DEATH_AP) && counts[1] + counts[2] + counts[3] > 0
                || self.res.config.mods.contains(Mods::INSTANT_DEATH_FC) && counts[2] + counts[3] > 0
                // 血条模式：只在会显示血条的模式下判死（练习 / 调偏移模式不判，与血条显示保持一致）
                || (self.res.config.health_mode()
                    && self.res.health.failed
                    && matches!(self.mode, GameMode::Normal | GameMode::NoRetry | GameMode::View)))
        {
            if !self.music.paused() {
                self.music.pause()?;
            }
            tm.pause();
            self.dead = true;
            #[cfg(target_env = "ohos")]
            miniquad::native::set_interceptor_state(false);
            show_message(tl!("game-over")).error();
        }
        self.res.judge_line_color.a *= self.res.alpha;
        self.chart.update(&mut self.res);
        let res = &mut self.res;
        if res.config.interactive && is_key_pressed(KeyCode::Space) {
            if tm.paused() {
                if matches!(self.state, State::Playing) {
                    self.music.play()?;
                    tm.resume();
                }
            } else if matches!(self.state, State::Playing | State::BeforeMusic) {
                if !self.music.paused() {
                    self.music.pause()?;
                }
                tm.pause();
            }
        }
        if Self::interactive(res, &self.state) {
            if is_key_pressed(KeyCode::Left) && res.config.use_keyboard {
                res.time -= 1.;
                let dst = (self.music.position() - 1.).max(0.);
                self.music.seek_to(dst)?;
                tm.seek_to(dst);
            }
            if is_key_pressed(KeyCode::Right) && res.config.use_keyboard {
                res.time += 5.;
                let dst = (self.music.position() + 5.).min(res.track_length);
                self.music.seek_to(dst)?;
                tm.seek_to(dst);
            }
            if is_key_pressed(KeyCode::Q) {
                self.should_exit = true;
            }
        }
        for e in &mut self.effects {
            e.update(&self.res);
        }
        if let Some((id, text)) = take_input() {
            let offset = self.offset().min(0.);
            match id.as_str() {
                // 练习面板的数值输入框：速度 / 音符流速 / 视角三项。照抄 PhiraiAd。
                "exercise_speed" | "exercise_note_flow" => {
                    if self.mode == GameMode::Exercise && tm.paused() {
                        let playback = id == "exercise_speed";
                        let range = if playback {
                            PLAYBACK_SPEED_MIN..PLAYBACK_SPEED_MAX
                        } else {
                            NOTE_FLOW_SPEED_MIN..NOTE_FLOW_SPEED_MAX
                        };
                        if let Some(value) = crate::practice_speed::parse(&text, &range) {
                            if playback {
                                self.res.config.speed = value;
                            } else {
                                self.exercise_note_flow_speed = value;
                                self.exercise_note_flow_locked = false;
                            }
                            self.sync_note_flow_speed();
                        } else {
                            show_message(tl!("ex-speed-invalid", "min" => range.start, "max" => range.end)).error();
                        }
                    }
                }
                "exercise_view_scale" | "exercise_view_x" | "exercise_view_y" => {
                    if self.mode == GameMode::Exercise && tm.paused() {
                        if let Some(value) = crate::practice_view::parse(&text) {
                            match id.as_str() {
                                "exercise_view_scale" => self.res.practice_view.scale_percent = value,
                                "exercise_view_x" => self.res.practice_view.center_x = value,
                                _ => self.res.practice_view.center_y = value,
                            }
                        }
                    }
                }
                "exercise_start" => {
                    if let Some(t) = parse_time(&text) {
                        if !(offset as f64..self.res.track_length.min(self.exercise_range.end - 3.).max(offset as f64)).contains(&t) {
                            show_message(tl!("ex-time-out-of-range")).error();
                        } else {
                            self.exercise_range.start = t;
                            show_message(tl!("ex-time-set")).ok();
                        }
                    } else {
                        show_message(tl!("ex-invalid-format")).error();
                    }
                }
                "exercise_end" => {
                    if let Some(t) = parse_time(&text) {
                        if !((self.exercise_range.start + 3.).max(offset as f64).min(self.res.track_length)..self.res.track_length).contains(&t) {
                            show_message(tl!("ex-time-out-of-range")).error();
                        } else {
                            self.exercise_range.end = t;
                            show_message(tl!("ex-time-set")).ok();
                        }
                    } else {
                        show_message(tl!("ex-invalid-format")).error();
                    }
                }
                _ => return_input(id, text),
            }
        }
        Ok(())
    }

    fn touch(&mut self, tm: &mut TimeManager, touch: &Touch) -> Result<bool> {
        if self.mode == GameMode::Exercise && tm.paused() {
            let touch = Touch {
                position: touch.position * self.touch_scale(),
                ..touch.clone()
            };
            if self.exercise_btns.0.touch(&touch) {
                request_input("exercise_start", InputBox::new().default_text(fmt_time(self.exercise_range.start as f32)));
                return Ok(true);
            }
            if self.exercise_btns.1.touch(&touch) {
                request_input("exercise_end", InputBox::new().default_text(fmt_time(self.exercise_range.end as f32)));
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn render(&mut self, tm: &mut TimeManager, ui: &mut Ui) -> Result<()> {
        // 打歌 HUD 的配色是照着深底设计的：UI 用浅色主题时这里强制回深色，
        // 否则 `semi_white` / 默认文字色会翻成深色，压在谱面画面上根本看不见。
        let was_light = crate::ui::theme::is_light();
        crate::ui::theme::set_light(false);
        let _guard = LightGuard(was_light);

        if self.res.config.show_avg_fps || self.res.config.hud_fps {
            let current_time = tm.real_time();
            if matches!(self.state, State::Playing) && !tm.paused() {
                let frame_delta = current_time - self.fps_last_frame_time;
                self.fps_total_time += frame_delta;
                self.fps_frame_count += 1;
            }
            self.fps_last_frame_time = current_time;
        }

        self.sync_note_flow_speed();
        let res = &mut self.res;
        let asp = ui.viewport.2 as f32 / ui.viewport.3 as f32;
        if res.update_size(ui.viewport) || self.mode == GameMode::View {
            set_camera(&res.camera);
        }

        let msaa = res.config.sample_count > 1;

        let chart_onto = res
            .chart_target
            .as_ref()
            .map(|it| if msaa { it.input() } else { it.output() })
            .or(res.camera.render_target);
        push_camera_state();
        set_camera(&Camera2D {
            zoom: vec2(1., -asp),
            viewport: if res.chart_target.is_some() { None } else { Some(ui.viewport) },
            render_target: chart_onto,
            ..Default::default()
        });
        clear_background(BLACK);
        draw_background(*res.background);
        pop_camera_state();

        let chart_target_vp = if res.chart_target.is_some() {
            let vp = res.camera.viewport.unwrap();
            Some((vp.0 - ui.viewport.0, vp.1 - ui.viewport.1, vp.2, vp.3))
        } else {
            res.camera.viewport
        };
        self.gl.quad_gl.render_pass(chart_onto.map(|it| it.render_pass));
        self.gl.quad_gl.viewport(chart_target_vp);

        let h = 1. / res.aspect_ratio;
        draw_rectangle(-1., -h, 2., h * 2., Color::new(0., 0., 0., res.alpha * res.info.background_dim));

        // 音乐可视化：每帧算一次频谱（判定线渲染时读它画条）
        if res.config.music_spectrum {
            crate::spectrum::update(res.music.frames(), res.music.sample_rate(), res.time, res.config.music_spectrum_gain_value());
        } else {
            crate::spectrum::silence();
        }
        // 练习镜头：把谱面层（音符 / 判定线 / 特效 / 粒子 / 调试叠加层）整体套进变换后的相机，
        // 但 HUD / 暂停面板 / 练习面板等 UI 层**不转**。照抄 PhiraiAd 的 transformed_view 分支。
        let transformed_view = self.mode == GameMode::Exercise && res.practice_view != Default::default();
        if transformed_view {
            push_camera_state();
            let (cx, cy) = res
                .practice_view
                .center(res.camera.viewport.map_or(ui.viewport.2 as f32, |vp| vp.2 as f32));
            set_camera(&Camera2D {
                target: vec2(cx, cy),
                zoom: res.camera.zoom * res.practice_view.scale(),
                viewport: chart_target_vp,
                render_target: chart_onto,
                ..Default::default()
            });
            self.gl.quad_gl.render_pass(chart_onto.map(|it| it.render_pass));
            self.gl.quad_gl.viewport(chart_target_vp);
        }
        self.chart.render(ui, res);

        self.gl.quad_gl.render_pass(
            res.chart_target
                .as_ref()
                .map(|it| it.output().render_pass)
                .or_else(|| res.camera.render_pass()),
        );

        self.bad_notes.retain(|dummy| dummy.render(res));
        // 判定范围可视化（调试叠加层）：画在音符之上、HUD 之下。
        // 只读 `judge.notes` 的稳定索引，不碰任何判定状态。
        if res.config.judgement_range_debug.enabled {
            let profile = self.judge.judgement_range_profile(&res.config);
            crate::judgement_range::render_judgement_ranges(&self.chart.lines, &self.judge.notes, res, &profile);
        }
        let t = tm.real_time();
        let dt = (t - std::mem::replace(&mut self.last_update_time, t)) as f32;
        if res.config.particle {
            res.emitter.draw(dt);
        }
        if transformed_view {
            pop_camera_state();
            self.gl.quad_gl.render_pass(
                res.chart_target
                    .as_ref()
                    .map(|it| it.output().render_pass)
                    .or_else(|| res.camera.render_pass()),
            );
            self.gl.quad_gl.viewport(chart_target_vp);
        }
        self.ui(ui, tm)?;
        // 实时 HUD / 判定文字 / 漏键标记：都画在暂停遮罩（overlay_ui）下面
        self.hud_ui(ui);
        // 装饰性准度条（独立于 config.hud 的 HUD 开关）
        self.timing_bar_ui(ui);
        self.judge_text_ui(ui);
        self.miss_marker_ui(ui);
        self.overlay_ui(ui, tm)?;
        // 官方 ActiveBlock 在 AfterForwardAlpha，即音符与 HUD 之后合成。
        // 练习镜头下必须把同样的相机再套一遍：活动噪域层画在谱面层之后，
        // 不套的话被缩放 / 平移过的谱面会和留在原位的噪域对不上，
        // 表现为噪区矩形整体偏移 / 看起来被拉伸。
        if transformed_view {
            push_camera_state();
            let (cx, cy) = self
                .res
                .practice_view
                .center(self.res.camera.viewport.map_or(ui.viewport.2 as f32, |vp| vp.2 as f32));
            set_camera(&Camera2D {
                target: vec2(cx, cy),
                zoom: self.res.camera.zoom * self.res.practice_view.scale(),
                viewport: chart_target_vp,
                render_target: chart_onto,
                ..Default::default()
            });
            self.gl.quad_gl.render_pass(chart_onto.map(|it| it.render_pass));
            self.gl.quad_gl.viewport(chart_target_vp);
        }
        self.chart.render_block_overlay(&mut self.res);
        if transformed_view {
            pop_camera_state();
            self.gl.quad_gl.render_pass(
                self.res
                    .chart_target
                    .as_ref()
                    .map(|it| it.output().render_pass)
                    .or_else(|| self.res.camera.render_pass()),
            );
            self.gl.quad_gl.viewport(chart_target_vp);
        }

        if self.mode == GameMode::TweakOffset {
            push_camera_state();
            self.gl.quad_gl.viewport(None);
            set_camera(&Camera2D {
                zoom: vec2(1., -screen_aspect()),
                render_target: self.res.chart_target.as_ref().map(|it| it.output()).or(self.res.camera.render_target),
                ..Default::default()
            });
            self.tweak_offset(ui, Self::interactive(&self.res, &self.state));
            pop_camera_state();
        }

        if !self.res.no_effect && !self.effects.is_empty() {
            push_camera_state();
            set_camera(&Camera2D {
                zoom: vec2(1., asp),
                ..Default::default()
            });
            for e in &self.effects {
                e.render(&mut self.res);
            }
            pop_camera_state();
        }
        if msaa || !self.res.no_effect {
            // render the texture onto screen
            if let Some(target) = &self.res.chart_target {
                self.gl.flush();
                push_camera_state();
                self.gl.quad_gl.viewport(None);
                set_camera(&Camera2D {
                    zoom: vec2(1., asp),
                    render_target: self.res.camera.render_target,
                    viewport: Some(ui.viewport),
                    ..Default::default()
                });
                draw_texture_ex(
                    target.output().texture,
                    -1.,
                    -ui.top,
                    WHITE,
                    DrawTextureParams {
                        dest_size: Some(vec2(2., ui.top * 2.)),
                        ..Default::default()
                    },
                );
                pop_camera_state();
            }
        }
        Ok(())
    }

    fn next_scene(&mut self, tm: &mut TimeManager) -> NextScene {
        if self.should_exit {
            if tm.paused() {
                tm.resume();
            }
            tm.speed = 1.0;
            tm.adjust_time = false;
            // 课题模式中途退出 = 整个课题作废。
            if self.res.config.challenge_mode {
                return NextScene::PopWithResult(Box::new(ChallengeEvent::Aborted));
            }
            match self.mode {
                // return result to update score and refresh
                GameMode::Normal => {
                    if let Some(rec) = &self.best_record {
                        NextScene::PopWithResult(Box::new(rec.clone()))
                    } else {
                        NextScene::Pop
                    }
                }
                // not sure if they need result. just keep it
                GameMode::Exercise | GameMode::NoRetry | GameMode::View => NextScene::Pop,
                GameMode::TweakOffset => NextScene::PopWithResult(Box::new(None::<f32>)),
            }
        } else if let Some(next_scene) = self.next_scene.take() {
            if !matches!(next_scene, NextScene::None) && tm.paused() {
                tm.resume();
            }
            tm.speed = 1.0;
            tm.adjust_time = false;
            next_scene
        } else {
            NextScene::None
        }
    }
}


/// 保证 [`crate::ui::theme::set_light`] 的临时覆盖在函数任何出口都被还原。
struct LightGuard(bool);
impl Drop for LightGuard {
    fn drop(&mut self) {
        crate::ui::theme::set_light(self.0);
    }
}
