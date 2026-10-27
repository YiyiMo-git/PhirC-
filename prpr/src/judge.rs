//! Judgement system

use crate::{
    config::Config,
    core::{block_touch_blocked, BadNote, Chart, NoteKind, Point, Resource, Vector, NOTE_WIDTH_RATIO_BASE},
    ext::{get_viewport, NotNanExt},
};
use macroquad::prelude::{
    utils::{register_input_subscriber, repeat_all_miniquad_input},
    *,
};
use miniquad::{EventHandler, MouseButton};
use once_cell::sync::Lazy;
use sasa::{PlaySfxParams, Sfx};
use serde::Serialize;
use std::{cell::RefCell, collections::HashMap, mem, num::FpCategory};
use tracing::debug;

pub const FLICK_SPEED_THRESHOLD: f32 = 0.8;
pub const UP_TOLERANCE: f64 = 0.05;
pub const DIST_FACTOR: f64 = 0.2;

// 关于「晚按补偿」：上游 prpr 在这里写死了 `const EARLY_OFFSET: f64 = 0.07;`，
// 而且只作用在晚按一侧 —— 判定距离 `dt = (note.time - t) / spd` 在晚按时（dt < 0）
// 会先加 0.07 再取绝对值，等于把晚按误差整体抹掉 70ms。于是实际窗口变成
// Perfect ≤150ms / Good ≤230ms / Bad ≤290ms，而早按一侧还是 80/160/220，
// 也就是「late 判定太松」。现在改成可配置项 `Config::late_leniency_ms`（默认 0 = 对称）。

#[derive(Debug, Clone)]
pub enum HitSound {
    None,
    Click,
    Flick,
    Drag,
    Custom(String),
}

impl HitSound {
    pub fn play(&self, res: &mut Resource) {
        match self {
            HitSound::None => {}
            HitSound::Click => play_sfx(&mut res.sfx_click, &res.config),
            HitSound::Flick => play_sfx(&mut res.sfx_flick, &res.config),
            HitSound::Drag => play_sfx(&mut res.sfx_drag, &res.config),
            HitSound::Custom(s) => {
                if let Some(sfx) = res.extra_sfxs.get_mut(s) {
                    play_sfx(sfx, &res.config);
                }
            }
        }
    }

    pub fn default_from_kind(kind: &NoteKind) -> Self {
        match kind {
            NoteKind::Click => HitSound::Click,
            NoteKind::Flick => HitSound::Flick,
            NoteKind::Drag => HitSound::Drag,
            NoteKind::Hold { .. } => HitSound::Click,
        }
    }
}

pub fn play_sfx(sfx: &mut Sfx, config: &Config) {
    if config.volume_sfx <= 1e-2 {
        return;
    }
    let _ = sfx.play(PlaySfxParams {
        amplifier: config.volume_sfx,
    });
}

#[cfg(all(not(target_os = "windows"), not(target_os = "ios")))]
fn get_uptime() -> f64 {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    let ret = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    assert!(ret == 0);
    time.tv_sec as f64 + time.tv_nsec as f64 * 1e-9
}

#[cfg(target_os = "ios")]
fn get_uptime() -> f64 {
    objc2_foundation::NSProcessInfo::processInfo().systemUptime()
}

#[cfg(target_os = "windows")]
fn get_uptime() -> f64 {
    miniquad::native::windows::get_uptime()
}

#[derive(Clone)]
pub struct FlickTracker {
    threshold: f32,
    last_point: Point,
    last_delta: Option<Vector>,
    last_time: f32,
    flicked: bool,
    stopped: bool,
}

impl FlickTracker {
    pub fn new(_dpi: u32, time: f32, point: Point) -> Self {
        // TODO maybe a better approach?
        let dpi = 275;
        Self {
            threshold: FLICK_SPEED_THRESHOLD * dpi as f32 / 386.,
            last_point: point,
            last_delta: None,
            last_time: time,
            flicked: false,
            stopped: true,
        }
    }

    pub fn push(&mut self, time: f32, position: Point) {
        let delta = position - self.last_point;
        self.last_point = position;
        if let Some(last_delta) = &self.last_delta {
            let dt = time - self.last_time;
            let speed = delta.dot(last_delta) / dt;
            if speed < self.threshold {
                self.stopped = true;
            }
            if self.stopped && !self.flicked {
                self.flicked = delta.magnitude() / dt >= self.threshold * 2.;
            }
            // if speed < self.threshold || self.stopped {
            // self.stopped = delta.magnitude() / dt < self.threshold * 5.;
            // self.flicked = self.threshold <= speed;
            // if self.flicked {
            // warn!("new flick!");
            // }
            // }
        }
        self.last_delta = Some(delta.normalize());
        self.last_time = time;
    }
}

#[derive(Debug, Clone, Copy)]
pub enum JudgeStatus {
    NotJudged,
    PreJudge,
    Judged,
    Hold(bool, f64, f64, bool, f64), // perfect, at, diff, pre-judge, up-time
}

// ============================================================
// 判定范围可视化用的窗口快照（从 PhiraiAd 移植）
// ============================================================
//
// PhiraiAd 的 `judgement_range.rs` 依赖 iAd 独有的 `JudgementRangeProfile` /
// `JudgementTimeWindow`，本分支原本没有这套。这里补上，但**数值来源换成
// 本分支自己的可配置窗口**（[`Config::limit_perfect`] / `limit_good` /
// `limit_bad` + 晚按补偿），与 [`crate::timing_bar::TimingWindows`] 完全同源 ——
// 不引入 iAd 的 Phigros 模式常量，避免两套窗口各说各话。

/// 一个判定等级在「早 / 晚」两侧的实时窗口（秒）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JudgementTimeWindow {
    /// 早于音符时间的窗口宽度（秒）。
    pub early: f64,
    /// 晚于音符时间的窗口宽度（秒）。
    pub late: f64,
}

impl JudgementTimeWindow {
    /// 早 / 晚对称的窗口。
    pub const fn symmetric(value: f64) -> Self {
        Self { early: value, late: value }
    }

    /// 从 `(early, late)` 构造。
    pub const fn new(early: f64, late: f64) -> Self {
        Self { early, late }
    }
}

/// 当前判定引擎实际使用的全部窗口快照，供调试叠加层绘制。
///
/// `tap_*` / `hold_*` / `drag_*` / `flick_*` 是**时间**窗口（秒）；
/// 带 `_x` 后缀的是**位置**容差（世界坐标）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JudgementRangeProfile {
    pub tap_perfect: JudgementTimeWindow,
    pub tap_good: JudgementTimeWindow,
    /// 最外层（Bad）时间窗：早侧 = bad，晚侧 = good（与判定逻辑一致）。
    pub tap_outer: JudgementTimeWindow,
    pub hold_perfect: JudgementTimeWindow,
    pub hold_outer: JudgementTimeWindow,
    pub drag_outer: JudgementTimeWindow,
    pub flick_outer: JudgementTimeWindow,
    /// Hold 尾判额外容忍（秒）。
    pub hold_tail: f64,
    pub tap_x: f64,
    pub hold_x: f64,
    pub drag_x: f64,
    pub flick_x: f64,
}

/// 位置容差上限：原版 `PHIRA_X_MAX = 0.21 / (16/9) * 2`。
pub const PHIRA_X_MAX: f64 = 0.21 / (16. / 9.) * 2.;

#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize)]
pub enum Judgement {
    Perfect,
    Good,
    Bad,
    Miss,
}

/// 一次判定事件的快照（供装饰性准度条等 HUD 消费，不参与判定结果）。
#[derive(Debug, Clone, Copy)]
pub struct JudgeReportEvent {
    pub time: f64,
    pub line_id: u32,
    pub note_id: u32,
    /// `Ok(判定)`；`Err(perfect)` 是长按的中间报告（`perfect` 表示头判定是否 Perfect）。
    pub judgement: Result<Judgement, bool>,
    /// 判定误差（秒）：正 = 早按，负 = 晚按。
    pub difference: f64,
}

#[cfg(not(closed))]
#[derive(Default, Clone)]
pub(crate) struct JudgeInner {
    diffs: Vec<f64>,

    combo: u32,
    max_combo: u32,
    counts: [u32; 4],
    num_of_notes: u32,
    early_kind: [u32; 4],
    late_kind: [u32; 4],
    /// 判定时间误差分布（-HIST_MAX_MS .. +HIST_MAX_MS，共 HIST_BUCKETS 个桶）。
    hist: [u32; HIST_BUCKETS],

    /// 判定误差平方和（含合成值），成绩上传记录里 variance 的分子。
    diff_sum_sq: f64,
    /// 参与 `diff_sum_sq` 的判定次数（完整打完一局时等于 `num_of_notes`）。
    diff_count: u32,
}

#[cfg(not(closed))]
impl JudgeInner {
    pub fn new(num_of_notes: u32) -> Self {
        Self {
            diffs: Vec::new(),

            combo: 0,
            max_combo: 0,
            counts: [0; 4],
            num_of_notes,
            early_kind: [0; 4],
            late_kind: [0; 4],
            hist: [0; HIST_BUCKETS],
            diff_sum_sq: 0.,
            diff_count: 0,
        }
    }

    /// 记录一次真实计时的判定误差（秒），用于结算画面的判定分布图。
    pub fn push_hist(&mut self, diff: f64) {
        let width = HIST_MAX_MS * 2. / HIST_BUCKETS as f64;
        let idx = ((diff * 1000. + HIST_MAX_MS) / width).clamp(0., HIST_BUCKETS as f64 - 1.) as usize;
        self.hist[idx] += 1;
    }

    pub fn commit(&mut self, what: Judgement, diff: f64) {
        use Judgement::*;
        // 成绩上传的 variance：官方对**每一次**判定都累加 diff²，包括合成值
        // （Miss = 0.25、Drag/Flick 与 Hold 的自动判定 = 0.0），见 judge.rs:898-908。
        // 全 Miss 的一局方差正好是 0.25² = 0.0625，与真实样本完全吻合。
        self.diff_sum_sq += diff * diff;
        self.diff_count += 1;
        if matches!(what, Judgement::Good) {
            self.diffs.push(diff);
        }
        if diff < 0. {
            self.early_kind[what as usize] += 1;
        } else if diff > 0. {
            self.late_kind[what as usize] += 1;
        }
        self.counts[what as usize] += 1;
        match what {
            Perfect | Good => {
                self.combo += 1;
                if self.combo > self.max_combo {
                    self.max_combo = self.combo;
                }
            }
            _ => {
                self.combo = 0;
            }
        }
    }

    pub fn reset(&mut self) {
        self.combo = 0;
        self.max_combo = 0;
        self.counts = [0; 4];
        self.diffs.clear();
        self.early_kind = [0; 4];
        self.late_kind = [0; 4];
        self.hist = [0; HIST_BUCKETS];
        self.diff_sum_sq = 0.;
        self.diff_count = 0;
    }

    pub fn accuracy(&self) -> f64 {
        (self.counts[0] as f64 + self.counts[1] as f64 * 0.65) / self.num_of_notes as f64
    }

    pub fn real_time_accuracy(&self) -> f64 {
        let cnt = self.counts.iter().sum::<u32>();
        if cnt == 0 {
            return 1.;
        }
        (self.counts[0] as f64 + self.counts[1] as f64 * 0.65) / cnt as f64
    }

    /// 判定时间误差的方差（`Σdiff² / 判定次数`）。
    ///
    /// 这就是成绩上传记录 offset 9..16 的那个 f64。注意分母是**判定次数**而不是
    /// `num_of_notes`：完整打完一局时两者相等，练歌模式（`advance_to` 跳过音符）会不同。
    pub fn variance(&self) -> f64 {
        if self.diff_count == 0 {
            0.
        } else {
            self.diff_sum_sq / self.diff_count as f64
        }
    }

    /// `no_combo_score` 为真时不把最大连击计入分数（去掉连击分，分数 = 准确率 * 1000000）。
    pub fn score(&self, no_combo_score: bool) -> u32 {
        const TOTAL: u32 = 1000000;
        if self.counts[0] == self.num_of_notes {
            TOTAL
        } else if no_combo_score {
            (self.accuracy() * TOTAL as f64).round() as u32
        } else {
            let score = (0.9 * self.accuracy() + self.max_combo as f64 / self.num_of_notes as f64 * 0.1) * TOTAL as f64;
            score.round() as u32
        }
    }

    pub fn result(&self, no_combo_score: bool) -> PlayResult {
        let early = self.diffs.iter().filter(|it| **it < 0.).count() as u32;
        PlayResult {
            score: self.score(no_combo_score),
            accuracy: self.accuracy(),
            max_combo: self.max_combo,
            num_of_notes: self.num_of_notes,
            counts: self.counts,
            early,
            late: self.diffs.len() as u32 - early,
            std: 0.,
            variance: self.variance(),
            early_kind: self.early_kind,
            late_kind: self.late_kind,
            hist: self.hist,
        }
    }

    pub fn combo(&self) -> u32 {
        self.combo
    }

    pub fn counts(&self) -> [u32; 4] {
        self.counts
    }
}

#[rustfmt::skip]
#[cfg(closed)]
pub mod inner;
#[cfg(closed)]
use inner::*;

type Judgements = Vec<(f64, u32, u32, Result<Judgement, bool>)>;

/// 判定分布图的桶数（覆盖 ±[`HIST_MAX_MS`] 毫秒）。
pub const HIST_BUCKETS: usize = 21;
/// 判定分布图的半宽（毫秒）。
pub const HIST_MAX_MS: f64 = 200.;

#[repr(C)]
pub struct Judge {
    // notes of each line in order
    // LinkedList::drain_filter is unstable...
    pub notes: Vec<(Vec<u32>, usize)>,
    pub trackers: HashMap<u64, FlickTracker>,
    pub last_time: f64,

    key_down_count: u32,

    pub(crate) inner: JudgeInner,
    pub judgements: RefCell<Judgements>,
    /// 最近几次真实计时的判定：(判定种类, 时间误差秒, 判定时刻)，供 Early/Late 实时提示使用。
    pub feedback: Vec<(Judgement, f64, f64)>,
    /// 最近几次判定的种类与时刻（**包含 Miss**，与 `feedback` 不同：feedback 只收真实计时的判定）。
    /// 判定文字动画用它，所以这里不关心时间误差。
    pub recent_judgements: Vec<(Judgement, f64)>,
    /// 漏键标记：被判为 Bad / Miss 的音符在判定线上的位置，供「漏键标记」画出来。
    pub miss_marks: Vec<MissMark>,
}

/// 时间倒流的判定状态快照（[`Judge::snapshot`] / [`Judge::restore`]）。
#[derive(Clone)]
pub struct JudgeSnapshot {
    notes: Vec<(Vec<u32>, usize)>,
    trackers: Vec<(u64, FlickTracker)>,
    last_time: f64,
    key_down_count: u32,
    inner: JudgeInner,
    recent: Vec<(Judgement, f64)>,
}

/// 一次漏键（Bad / Miss）在判定线上的落点。
#[derive(Clone, Copy, Debug)]
pub struct MissMark {
    /// 判定线编号。
    pub line: u32,
    /// 音符在判定线上的横向位置（判定线局部坐标）。
    pub x: f32,
    /// 判定发生的时刻（chart time），用来淡出。
    pub time: f64,
}

#[derive(Default)]
struct TouchStatus {
    touches: Vec<Touch>,
    key_delta: i32,
    keys_down: u32,
}

static SUBSCRIBER_ID: Lazy<usize> = Lazy::new(register_input_subscriber);
thread_local! {
    static TOUCHES: RefCell<TouchStatus> = RefCell::default();
    static WHEEL: RefCell<(f32, f32)> = RefCell::default();
}

pub fn take_wheel() -> (f32, f32) {
    WHEEL.with(|it| mem::take(&mut *it.borrow_mut()))
}

impl Judge {
    pub fn new(chart: &Chart) -> Self {
        let notes = chart
            .lines
            .iter()
            .map(|line| {
                let mut idx: Vec<u32> = (0..(line.notes.len() as u32)).filter(|it| !line.notes[*it as usize].fake).collect();
                idx.sort_by_key(|id| line.notes[*id as usize].time.not_nan());
                (idx, 0)
            })
            .collect();
        Self {
            notes,
            trackers: HashMap::new(),
            last_time: 0.,

            key_down_count: 0,

            inner: JudgeInner::new(chart.lines.iter().map(|it| it.notes.iter().filter(|it| !it.fake).count() as u32).sum()),
            judgements: RefCell::new(Vec::new()),
            feedback: Vec::new(),
            recent_judgements: Vec::new(),
            miss_marks: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.notes.iter_mut().for_each(|it| it.1 = 0);
        self.trackers.clear();
        self.inner.reset();
        self.judgements.borrow_mut().clear();
        self.feedback.clear();
        self.recent_judgements.clear();
        self.miss_marks.clear();
    }

    /// 时间倒流：抓一份完整判定状态（音符游标 / flick 追踪 / 计分内核 / 判定文字）。
    /// 纯数据拷贝，开销与已判定音符数线性；game.rs 每 0.5s 存一份、保留约 6 秒。
    pub fn snapshot(&self) -> JudgeSnapshot {
        JudgeSnapshot {
            notes: self.notes.clone(),
            trackers: self.trackers.iter().map(|(k, v)| (*k, v.clone())).collect(),
            last_time: self.last_time,
            key_down_count: self.key_down_count,
            inner: self.inner.clone(),
            recent: self.recent_judgements.clone(),
        }
    }

    /// 恢复到快照时刻：判定文字保留到当时点，feedback / 漏键标记清掉。
    /// 注意：**音符的 `judge` 状态由 game.rs 一并恢复**（要和这里的游标对齐）。
    pub fn restore(&mut self, s: JudgeSnapshot) {
        self.notes = s.notes;
        self.trackers = s.trackers.into_iter().collect();
        self.last_time = s.last_time;
        self.key_down_count = s.key_down_count;
        self.inner = s.inner;
        self.judgements.borrow_mut().clear();
        self.feedback.clear();
        self.recent_judgements = s.recent;
        self.miss_marks.clear();
    }

    /// Advance note pointers past notes before time `t`, marking them as judged.
    /// Used in exercise mode to skip notes before the exercise range start.
    pub fn advance_to(&mut self, chart: &mut Chart, t: f64) {
        for (line, (idx, st)) in chart.lines.iter_mut().zip(self.notes.iter_mut()) {
            while *st < idx.len() {
                let note = &mut line.notes[idx[*st] as usize];
                if note.time >= t {
                    break;
                }
                note.judge = JudgeStatus::Judged;
                *st += 1;
            }
        }
        self.last_time = t;
    }

    pub fn commit(&mut self, t: f64, what: Judgement, line_id: u32, note_id: u32, diff: f64, timing: Option<f64>) {
        self.judgements.borrow_mut().push((t, line_id, note_id, Ok(what)));
        self.inner.commit(what, diff);
        // 判定文字动画要看全部判定（含 Miss），所以单独记一份
        self.recent_judgements.push((what, t));
        if self.recent_judgements.len() > 32 {
            let extra = self.recent_judgements.len() - 32;
            self.recent_judgements.drain(..extra);
        }
        // timing 只对「真实计时」的判定有值（Miss 与 Drag/Flick 的 diff 是合成值，不参与）
        if let Some(timing) = timing {
            self.feedback.push((what, timing, t));
            self.inner.push_hist(timing);
        }
    }

    /// 记录一次漏键（Bad / Miss）在判定线上的位置，供「漏键标记」画出来。
    pub fn push_miss_mark(&mut self, t: f64, line: u32, x: f32) {
        self.miss_marks.push(MissMark { line, x, time: t });
        // 只留最近一小段，避免长谱面无限增长
        if self.miss_marks.len() > 256 {
            let extra = self.miss_marks.len() - 256;
            self.miss_marks.drain(..extra);
        }
    }

    /// 当前最大连击（实时 HUD 用）。
    #[inline]
    pub fn max_combo(&self) -> u32 {
        self.inner.max_combo
    }

    #[inline]
    pub fn accuracy(&self) -> f64 {
        self.inner.accuracy()
    }

    #[inline]
    pub fn real_time_accuracy(&self) -> f64 {
        self.inner.real_time_accuracy()
    }

    #[inline]
    pub fn score(&self, no_combo_score: bool) -> u32 {
        self.inner.score(no_combo_score)
    }

    /// 当前判定引擎实际使用的窗口快照，供「判定范围可视化」叠加层绘制。
    ///
    /// 数值**完全来自本分支的 [`Config`]**（Perfect / Good / Bad 可分别配置，
    /// 晚按一侧再加 `late_leniency`），与 [`crate::timing_bar::TimingWindows`]
    /// 同源，因此叠加层画的框与真实判定永远一致，不会各说各话。
    pub fn judgement_range_profile(&self, config: &Config) -> JudgementRangeProfile {
        let leniency = config.late_leniency();
        let perfect = config.limit_perfect();
        let good = config.limit_good();
        let outer = config.limit_bad();

        // 晚按一侧额外加补偿，与 `timing_bar` 的算法保持一致
        let late_add = |v: f64| JudgementTimeWindow::new(v, v + leniency);

        JudgementRangeProfile {
            tap_perfect: late_add(perfect),
            tap_good: late_add(good),
            // 最外层：早侧用 bad，晚侧用 good —— 与判定里 `dt` 的处理一致
            tap_outer: JudgementTimeWindow::new(outer, good + leniency),
            hold_perfect: late_add(perfect),
            hold_outer: JudgementTimeWindow::new(outer, good + leniency),
            drag_outer: late_add(outer),
            flick_outer: late_add(outer),
            hold_tail: outer,
            tap_x: PHIRA_X_MAX,
            hold_x: PHIRA_X_MAX,
            drag_x: PHIRA_X_MAX,
            flick_x: PHIRA_X_MAX,
        }
    }

    pub(crate) fn on_new_frame() {
        // 回放播放：有注入的合成触摸就直接用，**跳过 macroquad 采集**
        // （整条判定 / flick 管线因此原样复用，无需任何分支）。
        if let Some(touches) = crate::replay::take_playback_touches() {
            TOUCHES.with(|it| {
                *it.borrow_mut() = TouchStatus {
                    touches,
                    ..Default::default()
                };
            });
            return;
        }
        let mut handler = Handler {
            status: TouchStatus::default(),
            wheel: (0., 0.),
        };
        repeat_all_miniquad_input(&mut handler, *SUBSCRIBER_ID);
        handler.finalize();
        TOUCHES.with(|it| {
            *it.borrow_mut() = handler.status;
        });
        WHEEL.with(|it| {
            *it.borrow_mut() = handler.wheel;
        });
    }

    fn touch_transform(flip_x: bool) -> impl Fn(&mut Touch) {
        let vp = get_viewport();
        move |touch| {
            let p = touch.position;
            touch.position = vec2(
                (p.x - vp.0 as f32) / vp.2 as f32 * 2. - 1.,
                ((p.y - (screen_height() - (vp.1 + vp.3) as f32)) / vp.3 as f32 * 2. - 1.) / (vp.2 as f32 / vp.3 as f32),
            );
            if flip_x {
                touch.position.x *= -1.;
            }
        }
    }

    /// 练习模式：触摸坐标必须跟着镜头一起反变换，否则缩放 / 平移之后点不准。
    ///
    /// 顺序照抄 PhiraiAd：先做屏幕→谱面归一化，再过练习镜头反变换，最后施加左右翻转。
    /// 翻转放在镜头之后是因为本分支的翻转是**模型矩阵**，位于相机内侧。
    /// （本分支暂无自动倒打，故不含 PhiraiAd 的 `flip_y` 参数。）
    fn chart_touch_transform(flip_x: bool, view: crate::practice_view::PracticeView) -> impl Fn(&mut Touch) {
        let transform = Self::touch_transform(false);
        let width = get_viewport().2 as f32;
        let chart_flip = if flip_x { -1. } else { 1. };
        move |touch| {
            transform(touch);
            let (x, y) = view.screen_to_chart(touch.position.x, touch.position.y, width);
            touch.position = vec2(x * chart_flip, y);
        }
    }

    pub fn get_touches() -> Vec<Touch> {
        TOUCHES.with(|it| {
            let guard = it.borrow();
            let tr = Self::touch_transform(false);
            guard
                .touches
                .iter()
                .cloned()
                .map(|mut it| {
                    tr(&mut it);
                    it
                })
                .collect()
        })
    }

    /// 录制的原始触摸：**不做任何坐标变换**（与 `on_new_frame` 采集时同基准）。
    /// 回放注入时也按这个基准原样送回，保证「录什么播什么」。
    pub(crate) fn raw_touches() -> Vec<Touch> {
        TOUCHES.with(|it| it.borrow().touches.clone())
    }

    pub fn update(&mut self, res: &mut Resource, chart: &mut Chart, bad_notes: &mut Vec<BadNote>) {
        chart.blocked_touches.clear();
        if res.config.autoplay() {
            self.auto_play_update(res, chart);
            return;
        }
        // 全屏判定：横向不再限制（整个屏幕宽度），
        // 并且距离对判定优先级的影响从 DIST_FACTOR 降到 0.01，让判定以时间为主。
        let full_screen_judge = res.config.full_screen_judge();
        let x_diff_max = res.config.x_diff_max();
        let dist_factor: f64 = if full_screen_judge { 0.01 } else { DIST_FACTOR };
        let spd = res.config.speed as f64;
        // The judgement windows are configurable (see `Config::judge_window`).
        // These are the half-widths in effect for this run, in seconds of chart time.
        let limit_perfect = res.config.limit_perfect();
        let limit_good = res.config.limit_good();
        let limit_bad = res.config.limit_bad();
        let late_leniency = res.config.late_leniency();
        // 黄键 / 红键保护
        let drag_protect = res.config.drag_protect;
        let flick_protect = res.config.flick_protect;

        let uptime = get_uptime();

        let t = res.time;
        // Early/Late 提示只保留最近两秒内的记录，避免一场下来堆上千条
        self.feedback.retain(|it| t - it.2 < 2.);
        // TODO optimize
        let mut touches: HashMap<u64, Touch> = {
            let mut touches = touches();
            let btn = MouseButton::Left;
            let id = button_to_id(btn);
            if is_mouse_button_pressed(btn) {
                let p = mouse_position();
                touches.push(Touch {
                    id,
                    phase: TouchPhase::Started,
                    position: vec2(p.0, p.1),
                    time: f64::NEG_INFINITY,
                });
            } else if is_mouse_button_down(btn) {
                let p = mouse_position();
                touches.push(Touch {
                    id,
                    phase: TouchPhase::Moved,
                    position: vec2(p.0, p.1),
                    time: f64::NEG_INFINITY,
                });
            } else if is_mouse_button_released(btn) {
                let p = mouse_position();
                touches.push(Touch {
                    id,
                    phase: TouchPhase::Ended,
                    position: vec2(p.0, p.1),
                    time: f64::NEG_INFINITY,
                });
            }
            let tr = Self::chart_touch_transform(res.config.flip_x(), res.practice_view);
            touches
                .into_iter()
                .map(|mut it| {
                    tr(&mut it);
                    (it.id, it)
                })
                .collect()
        };
        let (events, keys_down, key_delta) = TOUCHES.with(|it| {
            let guard = it.borrow();
            let events = guard.touches.clone();
            if res.config.use_keyboard {
                (events, guard.keys_down, guard.key_delta)
            } else {
                (events, 0, 0)
            }
        });
        self.key_down_count = self.key_down_count.saturating_add_signed(key_delta);
        {
            fn to_local(Vec2 { x, y }: Vec2) -> Point {
                Point::new(x / screen_width() * 2. - 1., y / screen_height() * 2. - 1.)
            }
            let delta = (t / spd - self.last_time) / (events.len() + 1) as f64;
            let mut t = self.last_time;
            for Touch {
                id,
                phase,
                position: p,
                time,
            } in events.into_iter()
            {
                t += delta;
                let t = t as f32;
                let p = to_local(p);
                match phase {
                    TouchPhase::Started => {
                        self.trackers.insert(id, FlickTracker::new(res.dpi, t, p));
                        touches
                            .entry(id)
                            .or_insert_with(|| Touch {
                                id,
                                phase: TouchPhase::Started,
                                position: vec2(p.x, p.y),
                                time,
                            })
                            .phase = TouchPhase::Started;
                    }
                    TouchPhase::Moved | TouchPhase::Stationary => {
                        if let Some(tracker) = self.trackers.get_mut(&id) {
                            tracker.push(t, p);
                        }
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        self.trackers.remove(&id);
                    }
                }
            }
        }
        let mut touches: Vec<Touch> = touches
            .into_values()
            .map(|mut it| {
                it.time = if it.time.is_infinite() {
                    f64::NEG_INFINITY
                } else {
                    t - (uptime - it.time) * spd
                };
                it
            })
            .collect();
        // 噪区：落在生效阻挡区（enableTime <= t < disableTime）内的触摸从触摸列表里移除，
        // 下面的音符因此判定不到，自然成为 Miss。
        if !chart.block_areas.is_empty() || !chart.next_block_areas.is_empty() {
            let aspect = res.aspect_ratio;
            let areas = &chart.block_areas;
            let single = chart.single_inversion;
            let projected = chart.projected_chart();
            // 新格式噪区按 `Area::sample` 的时间轴取采样；层分配 / 反转都在
            // `rpe_next_block::blocked` 里按层独立统计。
            let next = chart.next_block_samples(t, aspect);
            // 有相机时触点要先从屏幕空间反投影回谱面空间；
            // 反投影失败（在可视深度之外）→ 触点不在谱面内，直接忽略。
            let mut blocked = Vec::new();
            touches.retain(|touch| {
                let raw = Vector::new(touch.position.x, -touch.position.y);
                let p = if projected {
                    chart.unproject_touch(raw, t, aspect).unwrap_or(raw)
                } else {
                    raw
                };
                // 旧噪区走旧规则；RPEN 新格式区域走分层 + single 反转规则。
                let hit = block_touch_blocked(areas, p, t, aspect)
                    || crate::core::rpe_next_block::blocked(&next, p, aspect, single);
                if hit {
                    blocked.push((touch.id, p));
                    false
                } else {
                    true
                }
            });
            drop(next);
            chart.blocked_touches = blocked;
        }
        // pos[line][touch]
        let mut pos = Vec::<Vec<Option<Point>>>::with_capacity(chart.lines.len());
        for id in 0..chart.lines.len() {
            chart.lines[id].object.set_time(t);
            let inv = chart.lines[id].now_transform(res, &chart.lines).try_inverse().unwrap();
            pos.push(
                touches
                    .iter()
                    .map(|touch| {
                        let p = touch.position;
                        let p = inv.transform_point(&Point::new(p.x, -p.y));
                        fn ok(f: f32) -> bool {
                            matches!(f.classify(), FpCategory::Zero | FpCategory::Subnormal | FpCategory::Normal)
                        }
                        if ok(p.x) && ok(p.y) {
                            Some(p)
                        } else {
                            None
                        }
                    })
                    .collect(),
            );
        }
        let time_of = |touch: &Touch| {
            if touch.time.is_infinite() {
                t
            } else {
                touch.time
            }
        };
        let mut judgements = Vec::new();
        // clicks & flicks
        for (id, touch) in touches.iter().enumerate() {
            let click = touch.phase == TouchPhase::Started;
            let flick =
                matches!(touch.phase, TouchPhase::Moved | TouchPhase::Stationary) && self.trackers.get_mut(&touch.id).is_some_and(|it| it.flicked);
            if !(click || flick) {
                continue;
            }
            let t = time_of(touch);
            // 第 5 槽 = 已选候选的**原始 dt**（未经红黄键偏移），保护窗比较要用；
            // 第 6 槽 = 已选候选是否为 Tap/Hold（蓝键），避免再借 chart 造成借用冲突。
            let mut closest: (Option<(usize, u32)>, f64, f64, f64, f64, bool) = (
                None,
                x_diff_max,
                limit_bad,
                limit_bad + (x_diff_max / NOTE_WIDTH_RATIO_BASE - 1.).max(0.) * dist_factor,
                f64::INFINITY,
                false,
            );
            for (line_id, ((line, pos), (idx, st))) in chart.lines.iter_mut().zip(pos.iter()).zip(self.notes.iter_mut()).enumerate() {
                let Some(pos) = pos[id] else {
                    continue;
                };
                for id in &idx[*st..] {
                    let note = &mut line.notes[*id as usize];
                    if !matches!(note.judge, JudgeStatus::NotJudged | JudgeStatus::PreJudge) {
                        continue;
                    }
                    if !click && matches!(note.kind, NoteKind::Click | NoteKind::Hold { .. }) {
                        continue;
                    }
                    // 保护机制：点击（蓝键）不被叠在附近的 Drag（黄键）/ Flick（红键）
                    // 抢走，直接跳过它们去找真正要判定的 Tap / Hold。
                    if click {
                        if drag_protect && matches!(note.kind, NoteKind::Drag) {
                            continue;
                        }
                        if flick_protect && matches!(note.kind, NoteKind::Flick) {
                            continue;
                        }
                    }
                    let dt = (note.time - t) / spd;
                    if dt >= closest.3 {
                        break;
                    }
                    // 晚按（dt < 0）时按设置的补偿量放宽；默认 0 → 和早按完全对称
                    let dt = if dt < 0. { (dt + late_leniency).min(0.).abs() } else { dt };
                    let x = &mut note.object.translation.0;
                    x.set_time(t);
                    let dist = (x.now() - pos.x).abs() as f64 / note.judge_area as f64;
                    if dist > x_diff_max {
                        continue;
                    }
                    if dt
                        > if matches!(note.kind, NoteKind::Click) {
                            limit_bad - limit_perfect * (dist - 0.9).max(0.)
                        } else {
                            limit_good
                        }
                    {
                        continue;
                    }
                    let raw_dt = dt;
                    let dt = if matches!(note.kind, NoteKind::Flick | NoteKind::Drag) {
                        dt + limit_good
                    } else {
                        dt
                    };
                    let key = dt + (dist / NOTE_WIDTH_RATIO_BASE - 1.).max(0.) * dist_factor;
                    if key < closest.3 {
                        // —— 本家保护窗（对齐 Phira-Pro 的 `rules.protection_ms`）——
                        // 蓝键（Tap/Hold）与红黄键（Flick/Drag）之间设 10ms 保护窗：
                        // 窗内**不让「谁时间更近」直接决定归属**，优先保蓝键 ——
                        // 红黄键是「手指顺带碰到」的，不该把同一时刻要打的蓝键抢走。
                        //   * 已选蓝键、新来红黄键 → 必须明显更早（超出保护窗）才能取代；
                        //   * 已选红黄键、新来蓝键 → 只要不比它晚超过保护窗就抢回来。
                        // 红键（Flick）的保护可重复触发；黄键（Drag）被跳过时不 arm 判定，
                        // 所以「单次保护」语义天然成立。
                        const PROTECTION_SEC: f64 = 0.010;
                        let mut accept = true;
                        if closest.0.is_some() {
                            let new_is_tap = matches!(note.kind, NoteKind::Click | NoteKind::Hold { .. });
                            if closest.5 != new_is_tap {
                                let delta = raw_dt - closest.4;
                                accept = if closest.5 {
                                    // 已选蓝键：红黄键要明显更早才抢
                                    delta < -PROTECTION_SEC
                                } else {
                                    // 已选红黄键：蓝键不比它晚太多就抢回来
                                    delta <= PROTECTION_SEC
                                };
                            }
                        }
                        if accept {
                            closest = (
                                Some((line_id, *id)),
                                dist,
                                dt,
                                key,
                                raw_dt,
                                matches!(note.kind, NoteKind::Click | NoteKind::Hold { .. }),
                            );
                        }
                    }
                }
            }
            if let (Some((line_id, id)), _, dt, _, _, _) = closest {
                let line = &mut chart.lines[line_id];
                if matches!(line.notes[id as usize].kind, NoteKind::Drag) {
                    debug!("reject by drag");
                    continue;
                }
                if click {
                    // click & hold
                    let note = &mut line.notes[id as usize];
                    if matches!(note.kind, NoteKind::Flick) {
                        continue; // to next loop
                    }
                    if dt <= limit_good || matches!(note.kind, NoteKind::Hold { .. }) {
                        match note.kind {
                            NoteKind::Click => {
                                note.judge = JudgeStatus::Judged;
                                judgements.push((if dt <= limit_perfect { Judgement::Perfect } else { Judgement::Good }, line_id, id, Some(t)));
                            }
                            NoteKind::Hold { .. } => {
                                note.hitsound.play(res);
                                self.judgements.borrow_mut().push((t, line_id as _, id, Err(dt <= limit_perfect)));
                                note.judge = JudgeStatus::Hold(dt <= limit_perfect, t, t, false, f64::INFINITY);
                            }
                            _ => unreachable!(),
                        };
                    } else {
                        // prevent extra judgements
                        if matches!(note.judge, JudgeStatus::NotJudged) {
                            // keep the note after bad judgement
                            line.notes[id as usize].judge = JudgeStatus::PreJudge;
                            judgements.push((Judgement::Bad, line_id, id, None));
                        }
                    }
                } else {
                    // flick
                    line.notes[id as usize].judge = JudgeStatus::PreJudge;
                    if let Some(tracker) = self.trackers.get_mut(&touch.id) {
                        tracker.flicked = false;
                    }
                }
            }
        }
        for _ in 0..keys_down {
            // find the earliest not judged click / hold note
            if let Some((line_id, id)) = chart
                .lines
                .iter()
                .zip(self.notes.iter())
                .enumerate()
                .filter_map(|(line_id, (line, (idx, st)))| {
                    idx[*st..]
                        .iter()
                        .cloned()
                        .find(|id| {
                            let note = &line.notes[*id as usize];
                            matches!(note.judge, JudgeStatus::NotJudged) && matches!(note.kind, NoteKind::Click | NoteKind::Hold { .. })
                        })
                        .map(|id| (line_id, id))
                })
                .min_by_key(|(line_id, id)| chart.lines[*line_id].notes[*id as usize].time.not_nan())
            {
                let note = &mut chart.lines[line_id].notes[id as usize];
                // 键盘/外接按键走的是另一条路，这里也用同一套「晚按补偿」，
                // 免得触屏和键盘的判定松紧不一致。
                let raw = (t - note.time) / spd;
                let dt = if raw > 0. { (raw - late_leniency).max(0.) } else { -raw };
                if dt <= if matches!(note.kind, NoteKind::Click) { limit_bad } else { limit_good } {
                    match note.kind {
                        NoteKind::Click => {
                            note.judge = JudgeStatus::Judged;
                            judgements.push((
                                if dt <= limit_perfect {
                                    Judgement::Perfect
                                } else if dt <= limit_good {
                                    Judgement::Good
                                } else {
                                    Judgement::Bad
                                },
                                line_id,
                                id,
                                None,
                            ));
                        }
                        NoteKind::Hold { .. } => {
                            note.hitsound.play(res);
                            self.judgements.borrow_mut().push((t, line_id as _, id, Err(dt <= limit_perfect)));
                            note.judge = JudgeStatus::Hold(dt <= limit_perfect, t, (t - note.time) / spd, false, f64::INFINITY);
                        }
                        _ => unreachable!(),
                    };
                }
            } else {
                break;
            }
        }
        for (line_id, ((line, pos), (idx, st))) in chart.lines.iter_mut().zip(pos.iter()).zip(self.notes.iter()).enumerate() {
            line.object.set_time(t);
            for id in &idx[*st..] {
                let note = &mut line.notes[*id as usize];
                if let NoteKind::Hold { end_time, .. } = &note.kind {
                    if let JudgeStatus::Hold(.., ref mut pre_judge, ref mut up_time) = note.judge {
                        if (*end_time - t) / spd <= limit_bad {
                            *pre_judge = true;
                            continue;
                        }
                        let x = &mut note.object.translation.0;
                        x.set_time(t);
                        let x = x.now();
                        if self.key_down_count == 0
                            && !pos
                                .iter()
                                .any(|it| it.is_some_and(|it| (it.x - x).abs() as f64 / note.judge_area as f64 <= x_diff_max))
                        {
                            if t > *up_time + UP_TOLERANCE {
                                note.judge = JudgeStatus::Judged;
                                judgements.push((Judgement::Miss, line_id, *id, None));
                            } else if up_time.is_infinite() {
                                *up_time = t;
                            }
                        } else {
                            *up_time = f64::INFINITY;
                        }
                        continue;
                    }
                }
                if !matches!(note.judge, JudgeStatus::NotJudged) {
                    continue;
                }
                // process miss
                let dt = (t - note.time) / spd;
                if dt > limit_bad {
                    note.judge = JudgeStatus::Judged;
                    judgements.push((Judgement::Miss, line_id, *id, None));
                    continue;
                }
                if -dt > limit_bad {
                    break;
                }
                if !matches!(note.kind, NoteKind::Drag) && (self.key_down_count == 0 || !matches!(note.kind, NoteKind::Flick)) {
                    continue;
                }
                let dt = dt.abs();
                let x = &mut note.object.translation.0;
                x.set_time(t);
                let x = x.now();
                if self.key_down_count != 0
                    || pos.iter().any(|it| {
                        it.is_some_and(|it| {
                            let dx = (it.x - x).abs() as f64 / note.judge_area as f64;
                            dx <= x_diff_max && dt <= (limit_bad - limit_perfect * (dx - 0.9).max(0.))
                        })
                    })
                {
                    note.judge = JudgeStatus::PreJudge;
                }
            }
        }
        // process pre-judge
        for (line_id, (line, (idx, st))) in chart.lines.iter_mut().zip(self.notes.iter()).enumerate() {
            line.object.set_time(t);
            for id in &idx[*st..] {
                let note = &mut line.notes[*id as usize];
                if let JudgeStatus::Hold(perfect, .., diff, true, _) = note.judge {
                    if let NoteKind::Hold { end_time, .. } = &note.kind {
                        if *end_time <= t {
                            note.judge = JudgeStatus::Judged;
                            judgements.push((if perfect { Judgement::Perfect } else { Judgement::Good }, line_id, *id, Some(diff)));
                            continue;
                        }
                    }
                }
                // TODO adjust
                let ghost_t = t + limit_good;
                if matches!(note.kind, NoteKind::Click) {
                    if ghost_t < note.time {
                        break;
                    }
                } else if t < note.time {
                    continue;
                }
                if matches!(note.judge, JudgeStatus::PreJudge) {
                    let diff = if let JudgeStatus::Hold(.., diff, _, _) = note.judge {
                        Some(diff)
                    } else {
                        None
                    };
                    note.judge = JudgeStatus::Judged;
                    if !matches!(note.kind, NoteKind::Click) {
                        judgements.push((Judgement::Perfect, line_id, *id, diff));
                    }
                }
            }
        }
        for (judgement, line_id, id, diff) in judgements {
            let line = &mut chart.lines[line_id];
            let note = &mut line.notes[id as usize];
            line.object.set_time(t);
            note.object.set_time(t);
            let line = &chart.lines[line_id];
            let note = &line.notes[id as usize];
            let line_tr = line.now_transform(res, &chart.lines);
            let synthetic = matches!(judgement, Judgement::Miss) || matches!(note.kind, NoteKind::Drag | NoteKind::Flick);
            let timing = (!synthetic).then(|| (diff.unwrap_or(t) - note.time) / spd);
            self.commit(t, judgement, line_id as _, id, timing.unwrap_or(if matches!(judgement, Judgement::Miss) { 0.25 } else { 0. }), timing);
            // 漏键标记：Bad / Miss 在判定线上的落点留个痕（Hold 除外，它本来就占一整条）
            if matches!(judgement, Judgement::Bad | Judgement::Miss) && !matches!(note.kind, NoteKind::Hold { .. }) {
                self.push_miss_mark(t, line_id as _, note.object.now_translation(res).x);
            }
            if res.config.health_mode() {
                // 血条模式：按真实判定加减血（Miss 与 Bad 同样处理，与参考一致）
                let combo = self.combo();
                res.health.on_judge(judgement, combo);
            }
            if matches!(note.kind, NoteKind::Hold { .. }) {
                continue;
            }
            if match judgement {
                Judgement::Perfect => {
                    res.with_model(line_tr * note.object.now(res), |res| {
                        res.emit_at_origin(note.rotation(line), note.fx_color.unwrap_or_else(|| res.res_pack.info.fx_perfect()))
                    });
                    true
                }
                Judgement::Good => {
                    res.with_model(line_tr * note.object.now(res), |res| {
                        res.emit_at_origin(note.rotation(line), note.fx_color.unwrap_or_else(|| res.res_pack.info.fx_good()))
                    });
                    true
                }
                Judgement::Bad => {
                    if !matches!(note.kind, NoteKind::Hold { .. }) {
                        bad_notes.push(BadNote {
                            time: t,
                            kind: note.kind.clone(),
                            matrix: {
                                let mut mat = line_tr;
                                if !note.above {
                                    mat.append_nonuniform_scaling_mut(&Vector::new(1., -1.));
                                }
                                let incline_sin = line.incline.now_opt().map(|it| it.to_radians().sin()).unwrap_or_default();
                                mat *= note.now_transform(
                                    res,
                                    &line.ctrl_obj.borrow_mut(),
                                    ((note.height - line.height.now() as f64) / res.aspect_ratio as f64 * note.speed) as f32,
                                    incline_sin,
                                );
                                mat
                            },
                        });
                    }
                    false
                }
                _ => false,
            } {
                note.hitsound.play(res);
            }
        }
        for (line, (idx, st)) in chart.lines.iter().zip(self.notes.iter_mut()) {
            while idx
                .get(*st)
                .is_some_and(|id| matches!(line.notes[*id as usize].judge, JudgeStatus::Judged))
            {
                *st += 1;
            }
        }
        self.last_time = t / spd;
    }

    fn auto_play_update(&mut self, res: &mut Resource, chart: &mut Chart) {
        let t = res.time;
        let spd = res.config.speed as f64;
        let mut judgements = Vec::new();
        for (line_id, (line, (idx, st))) in chart.lines.iter_mut().zip(self.notes.iter_mut()).enumerate() {
            for id in &idx[*st..] {
                let note = &mut line.notes[*id as usize];
                if let JudgeStatus::Hold(..) = note.judge {
                    if let NoteKind::Hold { end_time, .. } = note.kind {
                        if t >= end_time {
                            note.judge = JudgeStatus::Judged;
                            judgements.push((line_id, *id));
                            continue;
                        }
                    }
                }
                if !matches!(note.judge, JudgeStatus::NotJudged) {
                    continue;
                }
                if note.time > t {
                    break;
                }
                note.judge = if matches!(note.kind, NoteKind::Hold { .. }) {
                    note.hitsound.play(res);
                    self.judgements.borrow_mut().push((t, line_id as _, *id, Err(true)));
                    JudgeStatus::Hold(true, t, (t - note.time) / spd, false, f64::INFINITY)
                } else {
                    judgements.push((line_id, *id));
                    JudgeStatus::Judged
                };
            }
            while idx
                .get(*st)
                .is_some_and(|id| matches!(line.notes[*id as usize].judge, JudgeStatus::Judged))
            {
                *st += 1;
            }
        }
        for (line_id, id) in judgements.into_iter() {
            self.commit(t, Judgement::Perfect, line_id as _, id, 0., None);
            let (note_transform, note_hitsound) = {
                let line = &mut chart.lines[line_id];
                let note = &mut line.notes[id as usize];
                let nt = if matches!(note.kind, NoteKind::Hold { .. }) { t } else { note.time };
                line.object.set_time(nt);
                note.object.set_time(nt);
                (note.object.now(res), note.hitsound.clone())
            };
            let line = &chart.lines[line_id];
            res.with_model(line.now_transform(res, &chart.lines) * note_transform, |res| {
                res.emit_at_origin(line.notes[id as usize].rotation(line), res.res_pack.info.fx_perfect())
            });
            if !matches!(chart.lines[line_id].notes[id as usize].kind, NoteKind::Hold { .. }) {
                note_hitsound.play(res);
            }
        }
    }

    #[inline]
    pub fn result(&self, no_combo_score: bool) -> PlayResult {
        self.inner.result(no_combo_score)
    }

    #[inline]
    pub fn combo(&self) -> u32 {
        self.inner.combo()
    }

    #[inline]
    pub fn counts(&self) -> [u32; 4] {
        self.inner.counts()
    }

    /// 判定时间误差的方差，成绩上传记录 offset 9..16 用。
    #[inline]
    pub fn variance(&self) -> f64 {
        self.inner.variance()
    }
}

struct Handler {
    status: TouchStatus,
    wheel: (f32, f32),
}
impl Handler {
    fn finalize(&mut self) {
        if is_mouse_button_down(MouseButton::Left) {
            self.status.touches.push(Touch {
                id: button_to_id(MouseButton::Left),
                phase: TouchPhase::Moved,
                position: mouse_position().into(),
                time: f64::NEG_INFINITY,
            });
        }
    }
}

fn button_to_id(button: MouseButton) -> u64 {
    u64::MAX
        - match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::Unknown => 3,
        }
}

impl EventHandler for Handler {
    fn update(&mut self, _: &mut miniquad::Context) {}
    fn draw(&mut self, _: &mut miniquad::Context) {}
    fn touch_event(&mut self, _: &mut miniquad::Context, phase: miniquad::TouchPhase, id: u64, x: f32, y: f32, time: f64) {
        self.status.touches.push(Touch {
            id,
            phase: phase.into(),
            position: vec2(x, y),
            time,
        });
    }

    fn mouse_wheel_event(&mut self, _ctx: &mut miniquad::Context, x: f32, y: f32) {
        self.wheel.0 += x;
        self.wheel.1 += y;
    }

    fn mouse_button_down_event(&mut self, _ctx: &mut miniquad::Context, button: MouseButton, x: f32, y: f32) {
        self.status.touches.push(Touch {
            id: button_to_id(button),
            phase: TouchPhase::Started,
            position: vec2(x, y),
            time: f64::NEG_INFINITY,
        });
    }

    fn mouse_button_up_event(&mut self, _ctx: &mut miniquad::Context, button: MouseButton, x: f32, y: f32) {
        self.status.touches.push(Touch {
            id: button_to_id(button),
            phase: TouchPhase::Ended,
            position: vec2(x, y),
            time: f64::NEG_INFINITY,
        });
    }

    fn key_down_event(&mut self, _ctx: &mut miniquad::Context, _keycode: KeyCode, _keymods: miniquad::KeyMods, repeat: bool) {
        if !repeat {
            self.status.key_delta += 1;
            self.status.keys_down += 1;
        }
    }

    fn key_up_event(&mut self, _ctx: &mut miniquad::Context, _keycode: KeyCode, _keymods: miniquad::KeyMods) {
        self.status.key_delta -= 1;
    }
}

#[derive(Default)]
pub struct PlayResult {
    pub score: u32,
    pub accuracy: f64,
    pub max_combo: u32,
    pub num_of_notes: u32,
    pub counts: [u32; 4],
    pub early: u32,
    pub late: u32,
    pub std: f32,
    /// 判定时间误差的方差（`Σdiff² / 判定次数`），成绩上传用。
    pub variance: f64,
    pub early_kind: [u32; 4],
    pub late_kind: [u32; 4],
    /// 判定时间误差分布（早 ← → 晚），用于结算画面的分布图。
    pub hist: [u32; HIST_BUCKETS],
}

pub fn icon_index(score: u32, full_combo: bool) -> usize {
    match (score, full_combo) {
        (x, _) if x < 700000 => 0,
        (x, _) if x < 820000 => 1,
        (x, _) if x < 880000 => 2,
        (x, _) if x < 920000 => 3,
        (x, _) if x < 960000 => 4,
        (1000000, _) => 7,
        (_, false) => 5,
        (_, true) => 6,
    }
}
