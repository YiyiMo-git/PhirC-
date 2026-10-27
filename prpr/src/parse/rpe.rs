use anyhow::{Context, Result};
use image::{codecs::gif, AnimationDecoder, DynamicImage, ImageError};
use macroquad::prelude::{Color, WHITE};
use sasa::AudioClip;
use serde::{Deserialize, Deserializer};
use std::{any::Any, cell::RefCell, collections::HashMap, future::IntoFuture, io::Cursor, rc::Rc, str::FromStr, time::Duration};
use tracing::{debug, warn};

use super::{process_lines, L10N_LOCAL, RPE_TWEEN_MAP};
use crate::{
    core::{
        Anim, AnimFloat, AnimVector, BezierTween, BlockArea, BlockMoveEvent, BlockPhase, BlockRotateEvent, BlockScaleEvent, BpmList, Chart, ChartExtra,
        ChartSettings, ClampedTween, CtrlObject, GeneralIntTween, GifFrames, HitSoundMap, IntClampedTween, IntStaticTween, JudgeLine,
        JudgeLineCache, JudgeLineKind, Keyframe, Note, NoteKind, Object, StaticTween, Triple, TweenFunction, Tweenable, UIElement, Vector, EPS,
        HEIGHT_RATIO,
    },
    ext::{NotNanExt, SafeTexture},
    fs::FileSystem,
    judge::{HitSound, JudgeStatus},
    parse::ParseWarnings,
};

pub const RPE_WIDTH: f32 = 1350.;
pub const RPE_HEIGHT: f32 = 900.;
const SPEED_RATIO: f64 = 10. / 45. / HEIGHT_RATIO;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RPEBpmItem {
    bpm: f64,
    start_time: Triple,
}

// serde is weird...
fn f32_zero() -> f32 {
    0.
}

fn f32_one() -> f32 {
    1.
}

fn i32_one() -> i32 {
    1
}

fn rpe_version_default() -> i32 {
    160
}

fn deserialize_rpe_version<'de, D>(deserializer: D) -> std::result::Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    let parsed = match value {
        Some(serde_json::Value::Number(v)) => v.as_i64().map(|it| it as i32),
        Some(serde_json::Value::String(s)) => s.parse::<i32>().ok(),
        _ => None,
    };
    Ok(parsed.unwrap_or(rpe_version_default()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RPEEvent<T = f32> {
    #[serde(default = "f32_zero")]
    easing_left: f32,
    #[serde(default = "f32_one")]
    easing_right: f32,
    #[serde(default)]
    bezier: u8,
    #[serde(default)]
    bezier_points: [f32; 4],
    #[serde(default = "i32_one")]
    easing_type: i32,
    start: T,
    end: T,
    start_time: Triple,
    end_time: Triple,
}

impl<T> RPEEvent<T> {
    fn bezier_key(&self) -> (u16, i16, i16) {
        let p = &self.bezier_points;
        let int = |p: f32| (p * 100.).round() as i16;
        ((int(p[0]) * 100 + int(p[1])) as u16, int(p[2]), int(p[3]))
    }

    pub fn tween(&self, bezier_map: &BezierMap) -> Rc<dyn TweenFunction> {
        let tween = RPE_TWEEN_MAP.get(self.easing_type.max(1) as usize).copied().unwrap_or(RPE_TWEEN_MAP[0]);
        let left = self.easing_left.clamp(0., 1.);
        let right = self.easing_right.clamp(0., 1.);
        if self.bezier != 0 {
            Rc::clone(&bezier_map[&self.bezier_key()])
        } else if tween <= 2 || (left.abs() < EPS as f32 && (right - 1.0).abs() < EPS as f32) || left >= right {
            StaticTween::get_rc(tween)
        } else {
            Rc::new(ClampedTween::new(tween, left..right))
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RPECtrlEvent {
    easing: u8,
    x: f64,
    #[serde(flatten)]
    value: HashMap<String, f32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RPEEventLayer {
    alpha_events: Option<Vec<RPEEvent>>,
    move_x_events: Option<Vec<RPEEvent>>,
    move_y_events: Option<Vec<RPEEvent>>,
    rotate_events: Option<Vec<RPEEvent>>,
    speed_events: Option<Vec<RPEEvent>>,
}

#[derive(Clone, Deserialize)]
struct RGBColor(u8, u8, u8);
impl From<RGBColor> for Color {
    fn from(RGBColor(r, g, b): RGBColor) -> Self {
        Self::from_rgba(r, g, b, 255)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RPEExtendedEvents {
    color_events: Option<Vec<RPEEvent<RGBColor>>>,
    text_events: Option<Vec<RPEEvent<String>>>,
    scale_x_events: Option<Vec<RPEEvent>>,
    scale_y_events: Option<Vec<RPEEvent>>,
    incline_events: Option<Vec<RPEEvent>>,
    paint_events: Option<Vec<RPEEvent>>,
    gif_events: Option<Vec<RPEEvent>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RPENote {
    // TODO above == 0? what does that even mean?
    #[serde(rename = "type")]
    kind: u8,
    above: u8,
    start_time: Triple,
    end_time: Triple,
    position_x: f32,
    y_offset: f32,
    alpha: u16,               // some alpha has 256...
    hitsound: Option<String>, // TODO implement this feature
    size: f32,
    speed: f32,
    is_fake: u8,
    visible_time: f64,
    #[serde(default)]
    tint: Option<[u8; 3]>,
    #[serde(default)]
    tint_hit_effects: Option<[u8; 3]>,
    #[serde(default)]
    judge_area: Option<f32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RPEJudgeLine {
    // TODO group
    // TODO bpmfactor
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Texture")]
    texture: String,
    #[serde(rename = "father")]
    parent: Option<isize>,
    rotate_with_father: Option<bool>,
    event_layers: Vec<Option<RPEEventLayer>>,
    extended: Option<RPEExtendedEvents>,
    notes: Option<Vec<RPENote>>,
    is_cover: u8,
    #[serde(default)]
    z_order: i32,
    #[serde(rename = "attachUI")]
    attach_ui: Option<UIElement>,

    #[serde(default)]
    pos_control: Vec<RPECtrlEvent>,
    #[serde(default)]
    size_control: Vec<RPECtrlEvent>,
    #[serde(default)]
    alpha_control: Vec<RPECtrlEvent>,
    #[serde(default)]
    y_control: Vec<RPECtrlEvent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RPEMetadata {
    offset: i32,
    #[serde(rename = "RPEVersion", default = "rpe_version_default", deserialize_with = "deserialize_rpe_version")]
    rpe_version: i32,
}

#[derive(Copy, Clone)]
enum SpeedEasingMode {
    Legacy,
    Modern,
}

struct SpeedIntegralTween {
    tween: Rc<dyn TweenFunction>,
    k: f32,
    b: f32,
    total: f32,
}

impl SpeedIntegralTween {
    fn try_create(tween: Rc<dyn TweenFunction>, k: f32, b: f32) -> Option<(Rc<dyn TweenFunction>, f32)> {
        let mut result = Self { tween, k, b, total: 0. };
        let total = result.partial(1.);
        if !total.is_finite() || total.abs() < EPS as f32 {
            return None;
        }
        result.total = total;
        Some((Rc::new(result), total))
    }

    fn partial(&self, x: f32) -> f32 {
        self.tween.y(x) * self.k + self.b * x
    }
}

impl TweenFunction for SpeedIntegralTween {
    fn y(&self, x: f32) -> f32 {
        if x <= 0. {
            return 0.;
        }
        if x >= 1. {
            return 1.;
        }
        let y = self.partial(x) / self.total;
        if y.is_finite() {
            y
        } else {
            x
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn speed_linear_tween(start_speed: f32, end_speed: f32) -> Rc<dyn TweenFunction> {
    if (start_speed - end_speed).abs() < EPS as f32 {
        StaticTween::get_rc(2)
    } else if start_speed.abs() > end_speed.abs() {
        Rc::new(ClampedTween::new(7 /*quadOut*/, 0.0..(1. - end_speed / start_speed)))
    } else {
        Rc::new(ClampedTween::new(6 /*quadIn*/, (start_speed / end_speed)..1.))
    }
}

fn speed_segment_tween(mode: SpeedEasingMode, start_speed: f32, end_speed: f32, tween: Rc<dyn TweenFunction>) -> (Rc<dyn TweenFunction>, f32) {
    let (tween, total) = match mode {
        SpeedEasingMode::Legacy => {
            let df0 = tween.derivative(0.);
            let df1 = tween.derivative(1.);
            let denom = df1 - df0;
            if !denom.is_finite() || denom.abs() < 1e-8 {
                return (speed_linear_tween(start_speed, end_speed), (start_speed + end_speed) / 2.);
            }
            let k = (end_speed - start_speed) / denom;
            let b = start_speed - k * df0;
            SpeedIntegralTween::try_create(tween, k, b)
        }
        SpeedEasingMode::Modern => {
            let int_tween: Rc<dyn TweenFunction> = if let Some(s) = tween.as_any().downcast_ref::<StaticTween>() {
                IntStaticTween::get_rc(s.0)
            } else if let Some(s) = tween.as_any().downcast_ref::<ClampedTween>() {
                Rc::new(IntClampedTween::new(s.0, s.1.clone()))
            } else {
                Rc::new(GeneralIntTween::new(tween))
            };
            SpeedIntegralTween::try_create(int_tween, end_speed - start_speed, start_speed)
        }
    }
    .unwrap_or_else(|| (speed_linear_tween(start_speed, end_speed), (start_speed + end_speed) / 2.));
    (tween, total)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RPEChart {
    #[serde(rename = "META")]
    meta: RPEMetadata,
    #[serde(rename = "BPMList")]
    bpm_list: Vec<RPEBpmItem>,
    judge_line_list: Vec<RPEJudgeLine>,
}

type BezierMap = HashMap<(u16, i16, i16), Rc<dyn TweenFunction>>;

fn parse_events<T: Tweenable, V: Clone + Into<T>>(
    r: &mut BpmList,
    rpe: &[RPEEvent<V>],
    default: Option<T>,
    bezier_map: &BezierMap,
) -> Result<Anim<T>> {
    if rpe.is_empty() {
        return Ok(Anim::default());
    }
    let mut kfs = Vec::new();
    if let Some(default) = default {
        if rpe[0].start_time.beats() != 0.0 {
            kfs.push(Keyframe::new(0.0, default, 0));
        }
    }
    for e in rpe {
        kfs.push(Keyframe {
            time: r.time(&e.start_time),
            value: e.start.clone().into(),
            tween: e.tween(bezier_map),
        });
        kfs.push(Keyframe::new(r.time(&e.end_time), e.end.clone().into(), 0));
    }
    Ok(Anim::new(kfs))
}

fn parse_speed_events(r: &mut BpmList, rpe: &[RPEEventLayer], bezier_map: &BezierMap, max_time: f64, mode: SpeedEasingMode) -> Result<AnimFloat> {
    let layers: Vec<_> = rpe.iter().filter_map(|it| it.speed_events.as_ref()).collect();
    if layers.is_empty() {
        return Ok(AnimFloat::default());
    }
    let mut anis = Vec::new();
    for layer in layers {
        if layer.is_empty() {
            continue;
        }
        let mut events = layer.iter().collect::<Vec<_>>();
        events.sort_by_key(|it| it.start_time.beats().not_nan());

        let mut kfs = vec![Keyframe::new(0.0, 0.0, 2)];
        let mut height = 0f64;
        let mut push_kf = |start_time: f64, end_time: f64, tween: Rc<dyn TweenFunction>, factor: f32| {
            if end_time - start_time <= EPS {
                return;
            }
            if let Some(last) = kfs.last_mut() {
                if (last.time - start_time).abs() < EPS {
                    last.value = height as f32;
                    last.tween = tween;
                } else {
                    kfs.push(Keyframe {
                        time: start_time,
                        value: height as f32,
                        tween,
                    });
                }
            }
            height += factor as f64 * (end_time - start_time);
        };

        let mut cursor = 0.0;
        let mut last_speed = 0.0;
        for event in events {
            let start_time = r.time(&event.start_time).max(cursor);
            let end_time = r.time(&event.end_time).max(start_time);
            let start_speed = event.start * SPEED_RATIO as f32;
            let end_speed = event.end * SPEED_RATIO as f32;

            push_kf(cursor, start_time, StaticTween::get_rc(2), last_speed);
            if end_time > start_time + EPS {
                if event.easing_type == 0 {
                    push_kf(start_time, end_time, StaticTween::get_rc(2), start_speed);
                } else if event.easing_type <= 1 {
                    if start_speed * end_speed < 0. {
                        let x = start_speed / (start_speed - end_speed);
                        let mid = f64::tween(&start_time, &end_time, x);
                        for (start_time, end_time, start, end) in [(start_time, mid, start_speed, 0.), (mid, end_time, 0., end_speed)] {
                            let factor = start.midpoint(end);
                            let tween = speed_linear_tween(start, end);
                            push_kf(start_time, end_time, tween, factor);
                        }
                    } else {
                        let factor = start_speed.midpoint(end_speed);
                        let tween = speed_linear_tween(start_speed, end_speed);
                        push_kf(start_time, end_time, tween, factor);
                    }
                } else {
                    let (tween, factor) = speed_segment_tween(mode, start_speed, end_speed, event.tween(bezier_map));
                    push_kf(start_time, end_time, tween, factor);
                }
            }
            cursor = end_time;
            last_speed = end_speed;
        }

        push_kf(cursor, max_time, StaticTween::get_rc(2), last_speed);
        if let Some(last) = kfs.last() {
            if (last.time - max_time).abs() > EPS {
                kfs.push(Keyframe::new(max_time, height as f32, 0));
            }
        }
        anis.push(AnimFloat::new(kfs));
    }
    if anis.is_empty() {
        return Ok(AnimFloat::default());
    }
    Ok(AnimFloat::chain(anis))
}

fn parse_speed_events_legacy(r: &mut BpmList, rpe: &[RPEEventLayer], max_time: f64) -> Result<AnimFloat> {
    let rpe: Vec<_> = rpe.iter().filter_map(|it| it.speed_events.as_ref()).collect();
    if rpe.is_empty() {
        // TODO or is it?
        return Ok(AnimFloat::default());
    };
    let anis: Vec<_> = rpe
        .into_iter()
        .filter_map(|it| {
            if it.is_empty() {
                return None;
            }
            let mut kfs = Vec::new();
            for e in it {
                kfs.push(Keyframe::new(r.time(&e.start_time), e.start, 2));
                kfs.push(Keyframe::new(r.time(&e.end_time), e.end, 0));
            }
            Some(AnimFloat::new(kfs))
        })
        .collect();
    if anis.is_empty() {
        return Ok(AnimFloat::default());
    }
    let mut pts: Vec<_> = anis.iter().flat_map(|it| it.keyframes.iter().map(|it| it.time.not_nan())).collect();
    pts.push(max_time.not_nan());
    pts.sort();
    pts.dedup();
    let mut sani = AnimFloat::chain(anis);
    sani.map_value(|v| v * SPEED_RATIO as f32);
    for i in 0..(pts.len() - 1) {
        let now_time = *pts[i];
        let end_time = *pts[i + 1];
        sani.set_time(now_time);
        let speed = sani.now();
        sani.set_time(end_time - 1e-4);
        let end_speed = sani.now();
        if speed.signum() * end_speed.signum() < 0. {
            pts.push(f64::tween(&now_time, &end_time, speed / (speed - end_speed)).not_nan());
        }
    }
    pts.sort();
    pts.dedup();
    let mut kfs = Vec::new();
    let mut height = 0f64;
    for i in 0..(pts.len() - 1) {
        let now_time = *pts[i];
        let end_time = *pts[i + 1];
        sani.set_time(now_time);
        let speed = sani.now();
        // this can affect a lot! do not use end_time...
        // using end_time causes Hold tween (x |-> 0) to be recognized as Linear tween (x |-> x)
        sani.set_time(end_time - 1e-4);
        let end_speed = sani.now();
        kfs.push(if (speed - end_speed).abs() < EPS as f32 {
            Keyframe::new(now_time, height as f32, 2)
        } else if speed.abs() > end_speed.abs() {
            Keyframe {
                time: now_time,
                value: height as f32,
                tween: Rc::new(ClampedTween::new(7 /*quadOut*/, 0.0..(1. - end_speed / speed))),
            }
        } else {
            Keyframe {
                time: now_time,
                value: height as f32,
                tween: Rc::new(ClampedTween::new(6 /*quadIn*/, (speed / end_speed)..1.)),
            }
        });
        height += (speed + end_speed) as f64 * (end_time - now_time) / 2.;
    }
    if kfs.is_empty() {
        return Ok(Anim::default());
    }
    kfs.push(Keyframe::new(max_time, height as f32, 0));
    Ok(AnimFloat::new(kfs))
}

fn parse_gif_events<V: Clone + Into<f32>>(r: &mut BpmList, rpe: &[RPEEvent<V>], bezier_map: &BezierMap, gif: &GifFrames) -> Result<Anim<f32>> {
    let mut kfs = Vec::new();
    kfs.push(Keyframe::new(0.0, 0.0, 2));
    let mut next_rep_time: u128 = 0;
    for e in rpe {
        while r.time(&e.start_time) > next_rep_time as f64 / 1000. {
            kfs.push(Keyframe::new(next_rep_time as f64 / 1000., 1.0, 0));
            kfs.push(Keyframe::new(next_rep_time as f64 / 1000., 0.0, 2));
            next_rep_time += gif.total_time();
        }
        let stop_prog = 1. - (next_rep_time as f64 - r.time(&e.start_time) * 1000.) / gif.total_time() as f64;
        kfs.push(Keyframe::new(r.time(&e.start_time), stop_prog as f32, 0));
        kfs.push(Keyframe {
            time: r.time(&e.start_time),
            value: e.start.clone().into(),
            tween: e.tween(bezier_map),
        });
        kfs.push(Keyframe::new(r.time(&e.end_time), e.end.clone().into(), 2));
        next_rep_time = (r.time(&e.end_time) * 1000. + gif.total_time() as f64 * (1. - e.end.clone().into()) as f64).round() as u128;
    }

    // TODO maybe a better approach?
    const GIF_MAX_TIME: f64 = 2000.;
    while GIF_MAX_TIME > next_rep_time as f64 / 1000. {
        kfs.push(Keyframe::new(next_rep_time as f64 / 1000., 1.0, 0));
        kfs.push(Keyframe::new(next_rep_time as f64 / 1000., 0.0, 2));
        next_rep_time += gif.total_time();
    }
    if kfs.is_empty() {
        return Ok(Anim::default());
    }
    Ok(Anim::new(kfs))
}

async fn parse_notes(
    r: &mut BpmList,
    rpe: Vec<RPENote>,
    fs: &mut dyn FileSystem,
    height: &mut AnimFloat,
    hitsounds: &mut HitSoundMap,
) -> Result<Vec<Note>> {
    let mut notes = Vec::new();
    for note in rpe {
        let time = r.time(&note.start_time);
        height.set_time(time);
        let note_height = height.now();
        let y_offset = note.y_offset * 2. / RPE_HEIGHT * note.speed;
        let kind = match note.kind {
            1 => NoteKind::Click,
            2 => {
                let end_time = r.time(&note.end_time);
                height.set_time(end_time);
                NoteKind::Hold {
                    end_time,
                    end_height: height.now() as f64,
                }
            }
            3 => NoteKind::Flick,
            4 => NoteKind::Drag,
            _ => ptl!(bail "unknown-note-type", "type" => note.kind),
        };
        let hitsound = match note.hitsound {
            Some(s) => {
                // TODO: RPE doc needed...
                if s == "flick.mp3" {
                    HitSound::Flick
                } else if s == "tap.mp3" {
                    HitSound::Click
                } else if s == "drag.mp3" {
                    HitSound::Drag
                } else {
                    if hitsounds.get(&s).is_none() {
                        let data = fs.load_file(&s).await;
                        if let Ok(data) = data {
                            hitsounds.insert(s.clone(), AudioClip::new(data)?);
                        } else {
                            ptl!(bail "hitsound-missing", "name" => s);
                        }
                    }
                    HitSound::Custom(String::from_str(&s)?)
                }
            }
            None => HitSound::default_from_kind(&kind),
        };
        notes.push(Note {
            object: Object {
                alpha: if note.visible_time >= time {
                    if note.alpha >= 255 {
                        AnimFloat::default()
                    } else {
                        AnimFloat::fixed(note.alpha as f32 / 255.)
                    }
                } else {
                    let alpha = note.alpha.min(255) as f32 / 255.;
                    AnimFloat::new(vec![Keyframe::new(0.0, 0.0, 0), Keyframe::new(time - note.visible_time, alpha, 0)])
                },
                translation: AnimVector(AnimFloat::fixed(note.position_x / (RPE_WIDTH / 2.)), AnimFloat::fixed(y_offset)),
                scale: AnimVector(AnimFloat::fixed(note.size), AnimFloat::fixed(note.size)),
                ..Default::default()
            },
            kind,
            hitsound,
            time,
            height: note_height as f64,
            speed: note.speed as f64,
            color: note.tint.map_or(WHITE, |[r, g, b]| Color::from_rgba(r, g, b, 255)),
            fx_color: note.tint_hit_effects.map(|[r, g, b]| Color::from_rgba(r, g, b, 255)),
            judge_area: note.judge_area.unwrap_or(1.0),

            above: note.above == 1,
            multiple_hint: false,
            fake: note.is_fake != 0,
            judge: JudgeStatus::NotJudged,
        })
    }
    Ok(notes)
}

fn parse_ctrl_events(rpe: &[RPECtrlEvent], key: &str) -> AnimFloat {
    let vals: Vec<_> = rpe.iter().map(|it| it.value[key]).collect();
    if rpe.is_empty() || (rpe.len() == 2 && rpe[0].easing == 1 && (vals[0] - 1.).abs() < 1e-4) {
        return AnimFloat::default();
    }
    // In RPE, each control event's easing governs the interval ending at that
    // event's x, not starting from it. The Anim system uses kf[i].tween for
    // the interval [kf[i], kf[i+1]], so we shift the tween assignment: each
    // keyframe gets the tween from the next event.
    let tweens: Vec<Rc<dyn TweenFunction>> = rpe
        .iter()
        .skip(1)
        .map(|it| StaticTween::get_rc(RPE_TWEEN_MAP.get(it.easing.max(1) as usize).copied().unwrap_or(RPE_TWEEN_MAP[0])))
        .chain(std::iter::once(StaticTween::get_rc(0)))
        .collect();
    AnimFloat::new(
        rpe.iter()
            .zip(vals)
            .zip(tweens)
            .map(|((it, val), tween)| Keyframe {
                time: it.x,
                value: val,
                tween,
            })
            .collect(),
    )
}

#[allow(clippy::too_many_arguments)]
async fn parse_judge_line(
    r: &mut BpmList,
    rpe: RPEJudgeLine,
    max_time: f64,
    speed_mode: SpeedEasingMode,
    fs: &mut dyn FileSystem,
    use_rpe_170_speed: bool,
    bezier_map: &BezierMap,
    hitsounds: &mut HitSoundMap,
    line_texture_map: &mut HashMap<String, SafeTexture>,
) -> Result<JudgeLine> {
    let event_layers: Vec<_> = rpe.event_layers.into_iter().flatten().collect();
    fn events_with_factor(
        r: &mut BpmList,
        event_layers: &[RPEEventLayer],
        get: impl Fn(&RPEEventLayer) -> &Option<Vec<RPEEvent>>,
        factor: f32,
        desc: &str,
        bezier_map: &BezierMap,
    ) -> Result<AnimFloat> {
        let anis: Vec<_> = event_layers
            .iter()
            .filter_map(|it| get(it).as_ref().map(|es| parse_events(r, es, None, bezier_map)))
            .collect::<Result<_>>()
            .with_context(|| ptl!("type-events-parse-failed", "type" => desc))?;
        let mut res = AnimFloat::chain(anis);
        if res.is_default() {
            return Ok(AnimFloat::fixed(0.0));
        }
        res.map_value(|v| v * factor);
        Ok(res)
    }
    let mut height = if use_rpe_170_speed {
        parse_speed_events(r, &event_layers, bezier_map, max_time, speed_mode)?
    } else {
        parse_speed_events_legacy(r, &event_layers, max_time)?
    };
    let mut notes = parse_notes(r, rpe.notes.unwrap_or_default(), fs, &mut height, hitsounds).await?;
    let cache = JudgeLineCache::new(&mut notes);
    Ok(JudgeLine {
        object: Object {
            alpha: events_with_factor(r, &event_layers, |it| &it.alpha_events, 1. / 255., "alpha", bezier_map)?,
            rotation: events_with_factor(r, &event_layers, |it| &it.rotate_events, -1., "rotate", bezier_map)?,
            translation: AnimVector(
                events_with_factor(r, &event_layers, |it| &it.move_x_events, 2. / RPE_WIDTH, "move X", bezier_map)?,
                events_with_factor(r, &event_layers, |it| &it.move_y_events, 2. / RPE_HEIGHT, "move Y", bezier_map)?,
            ),
            scale: {
                fn parse(r: &mut BpmList, opt: &Option<Vec<RPEEvent>>, factor: f32, bezier_map: &BezierMap) -> Result<AnimFloat> {
                    let mut res = opt
                        .as_ref()
                        .map(|it| parse_events(r, it, None, bezier_map))
                        .transpose()?
                        .unwrap_or_default();
                    res.map_value(|v| v * factor);
                    Ok(res)
                }
                let factor = if rpe.texture == "line.png" { 1. } else { 2. / RPE_WIDTH };
                rpe.extended
                    .as_ref()
                    .map(|e| -> Result<_> {
                        Ok(AnimVector(
                            parse(
                                r,
                                &e.scale_x_events,
                                factor
                                    * if rpe.texture == "line.png"
                                        && rpe
                                            .extended
                                            .as_ref()
                                            .and_then(|it| it.text_events.as_ref())
                                            .is_none_or(|it| it.is_empty())
                                        && rpe.attach_ui.is_none()
                                    {
                                        0.5
                                    } else {
                                        1.
                                    },
                                bezier_map,
                            )?,
                            parse(r, &e.scale_y_events, factor, bezier_map)?,
                        ))
                    })
                    .transpose()?
                    .unwrap_or_default()
            },
        },
        ctrl_obj: RefCell::new(CtrlObject {
            alpha: parse_ctrl_events(&rpe.alpha_control, "alpha"),
            size: parse_ctrl_events(&rpe.size_control, "size"),
            pos: parse_ctrl_events(&rpe.pos_control, "pos"),
            y: parse_ctrl_events(&rpe.y_control, "y"),
            // RPEN `skewControl` 由调用方（parse_rpe 里拿到原始 JSON 后）填入；
            // 非 RPEN 谱面保持默认空动画。
            skew: AnimFloat::default(),
        }),
        height,
        incline: if let Some(events) = rpe.extended.as_ref().and_then(|e| e.incline_events.as_ref()) {
            parse_events(r, events, Some(0.), bezier_map).with_context(|| ptl!("incline-events-parse-failed"))?
        } else {
            AnimFloat::default()
        },
        notes,
        kind: if rpe.texture == "line.png" {
            if let Some(events) = rpe.extended.as_ref().and_then(|e| e.paint_events.as_ref()) {
                JudgeLineKind::Paint(
                    parse_events(r, events, Some(-1.), bezier_map).with_context(|| ptl!("paint-events-parse-failed"))?,
                    RefCell::default(),
                )
            } else if let Some(extended) = rpe.extended.as_ref() {
                if let Some(events) = extended.text_events.as_ref() {
                    JudgeLineKind::Text(parse_events(r, events, Some(String::new()), bezier_map).with_context(|| ptl!("text-events-parse-failed"))?)
                } else {
                    JudgeLineKind::Normal
                }
            } else {
                JudgeLineKind::Normal
            }
        } else if let Some(extended) = rpe.extended.as_ref() {
            if let Some(events) = extended.gif_events.as_ref() {
                let data = fs
                    .load_file(&rpe.texture)
                    .await
                    .with_context(|| ptl!("gif-load-failed", "path" => rpe.texture.clone()))?;
                let frames = GifFrames::new(
                    tokio::spawn(async move {
                        let decoder = gif::GifDecoder::new(Cursor::new(data))?;
                        debug!("decoding gif");
                        Ok::<std::vec::Vec<_>, ImageError>(decoder.into_frames().collect())
                    })
                    .into_future()
                    .await??
                    .into_iter()
                    .map(|frame| -> (u128, SafeTexture) {
                        let frame = frame.unwrap();
                        let delay: Duration = frame.delay().into();
                        (delay.as_millis(), SafeTexture::from(DynamicImage::ImageRgba8(frame.into_buffer())))
                    })
                    .collect(),
                );
                debug!("gif decoded");
                let events = parse_gif_events(r, events, bezier_map, &frames).with_context(|| ptl!("gif-events-parse-failed"))?;
                JudgeLineKind::TextureGif(events, frames, rpe.texture.clone())
            } else if let Some(texture) = line_texture_map.get(&rpe.texture) {
                debug!("texture {} reused, id: {}", rpe.texture.clone(), texture.clone().into_inner().raw_miniquad_texture_handle().gl_internal_id());
                JudgeLineKind::Texture(texture.clone(), rpe.texture.clone())
            } else {
                let texture = SafeTexture::from(image::load_from_memory(
                    &fs.load_file(&rpe.texture)
                        .await
                        .with_context(|| ptl!("illustration-load-failed", "path" => rpe.texture.clone()))?,
                )?)
                .with_mipmap();
                line_texture_map.insert(rpe.texture.clone(), texture.clone());
                JudgeLineKind::Texture(texture, rpe.texture.clone())
            }
        } else if let Some(texture) = line_texture_map.get(&rpe.texture) {
            debug!("texture {} reused, id: {}", rpe.texture.clone(), texture.clone().into_inner().raw_miniquad_texture_handle().gl_internal_id());
            JudgeLineKind::Texture(texture.clone(), rpe.texture.clone())
        } else {
            let texture = SafeTexture::from(image::load_from_memory(
                &fs.load_file(&rpe.texture)
                    .await
                    .with_context(|| ptl!("illustration-load-failed", "path" => rpe.texture.clone()))?,
            )?)
            .with_mipmap();
            line_texture_map.insert(rpe.texture.clone(), texture.clone());
            JudgeLineKind::Texture(texture, rpe.texture.clone())
        },
        color: if let Some(events) = rpe.extended.as_ref().and_then(|e| e.color_events.as_ref()) {
            parse_events(r, events, Some(WHITE), bezier_map).with_context(|| ptl!("color-events-parse-failed"))?
        } else {
            Anim::default()
        },
        parent: {
            let parent = rpe.parent.unwrap_or(-1);
            if parent == -1 {
                None
            } else {
                Some(parent as usize)
            }
        },
        rot_with_parent: rpe.rotate_with_father.unwrap_or(false),
        z_index: rpe.z_order,
        show_below: rpe.is_cover != 1,
        attach_ui: rpe.attach_ui,

        trail: RefCell::default(),
        cache,
    })
}

fn add_bezier<T>(map: &mut BezierMap, event: &RPEEvent<T>) {
    if event.bezier != 0 {
        let p = &event.bezier_points;
        let int = |p: f32| (p * 100.).round() as i16;
        map.entry(((int(p[0]) * 100 + int(p[1])) as u16, int(p[2]), int(p[3])))
            .or_insert_with(|| Rc::new(BezierTween::new((p[0], p[1]), (p[2], p[3]))));
    }
}

macro_rules! process_bezier {
    ($event_layer:expr, $map:expr, $($field:ident),*) => {
        $(
            for event in $event_layer.$field.iter().flatten() {
                add_bezier($map, event);
            }
        )*
    };
}

fn get_bezier_map(rpe: &RPEChart) -> BezierMap {
    let mut map = HashMap::new();
    for line in &rpe.judge_line_list {
        for event_layer in line.event_layers.iter().flatten() {
            process_bezier!(event_layer, &mut map, alpha_events, move_x_events, move_y_events, rotate_events);
        }
        if let Some(ext_layer) = &line.extended {
            process_bezier!(ext_layer, &mut map, paint_events, scale_x_events, scale_y_events, gif_events, incline_events, text_events, color_events);
        }
    }
    map
}

/* ==========================================================================
 *  RPE Recorder 的噪域（第 9 章噪区 / "block area"）
 *
 *  逻辑移植自 `Phira-Pro-0.8.2-pro.11` 的 `prpr/src/core/rpe_block.rs`（同血脉升级版）。
 *  RPE 的噪域在 JSON 里**没有专门字段**，全靠约定：
 *
 *  * 判定线的 `Texture` 是 `isSubtract0.png`（普通块）或 `isSubtract1.png`（反转块）
 *    → 这根线是**噪域专用线**：音符不渲染、贴图本身也不画；
 *  * 生命周期有**两条来源，速度码优先、不透明度轨兜底**：
 *      - 速度轨道（RPE Recorder 约定）：只读每个速度事件的**开始时间 + 开始数值**，
 *        1 出现 / 2 激活 / 3 失活 / 4 消失。数值**回退**（`code <= previous`）表示
 *        上一段已经结束、这里另起一段 —— 文档里的 `2→4→2→4` 就是这个规则。
 *      - 不透明度轨道（老谱面兜底）：`<=0` 隐藏 / `<=128/255` 预览 / 否则激活。
 *        ⚠️ 这些数值是**状态码**不是填充不透明度（示例谱面用 121/127/128 表示预览）。
 *  * 颜色走 `colorEvents`（彩色噪域）；官方 `blockAreaList` 一律用原生红。
 *  * 位置 / 旋转 / 大小走这根线自己的移动、旋转、缩放事件。RPE 是 1350×900 满屏
 *    （= [`RPE_WIDTH`] × [`RPE_HEIGHT`]），所以缩放值就是**像素宽高**。
 *
 *  与官方 `blockAreaList`（`parse/pgr.rs`）是两套入口，但落到同一个 [`BlockArea`]
 *  运行时：出现 / 失活 = `BlockPhase::Disabled`（显示但不挡触摸），
 *  激活 = `BlockPhase::Active`（挡触摸）。
 *
 *  ⚠️ 已简化 / 未实现（都写在这里，别当成 bug）：
 *     1. **闪烁**：`BlockPhase` 只有 Hidden / Disabled / Active 三态，没有单独的闪烁态；
 *        激活前 0.5 秒仍然是 Disabled —— 惩罚判定是对的，只是少一段动画。
 *     2. **缓动**：RPE 的 `easingType`（0..29）和官方 BlockArea 的 `easeType`（0..14）
 *        不是一套，没法直译。这里在每个原始关键帧处下一个采样点、段内线性。
 *     3. **锚点**：统一用矩形中心 (0.5, 0.5)。
 *     4. **颜色不随时间变化**：`BlockArea.color` 是静态的，所以按每个可见段的**中点**
 *        采样一次（噪域的颜色一般就是常量，够用）。
 *     5. **大小只认** `extended.scaleXEvents / scaleYEvents`；写在别处就报 warning 跳过。
 * ========================================================================== */

/// 噪域专用线的贴图名 → `is_subtract`（true = 反转块，在已有噪域上挖洞）。
///
/// ⚠️ 必须**精确匹配文件名**（大小写不敏感）。曾经用 `starts_with("issubtract0")`，
/// 那样 `isSubtract1.png.backup` 这类备份文件也会被当成噪域线。
fn marker_kind(texture: &str) -> Option<bool> {
    let name = texture.rsplit(['/', '\\']).next()?;
    if name.eq_ignore_ascii_case("isSubtract0.png") {
        Some(false)
    } else if name.eq_ignore_ascii_case("isSubtract1.png") {
        Some(true)
    } else {
        None
    }
}

/// 由**不透明度轨道**推相位（老谱面的兜底；有速度码时被覆盖）。
fn phase_of_alpha(alpha: f32) -> BlockPhase {
    if !alpha.is_finite() || alpha <= 0. {
        BlockPhase::Hidden
    } else if alpha <= 128. / 255. {
        BlockPhase::Disabled
    } else {
        BlockPhase::Active
    }
}

/// 一段相位区间，以及它的 `[appear, enable, disable, disappear]`。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Span {
    start: f64,
    end: f64,
    phase: BlockPhase,
    timings: [f64; 4],
}

/// 一条 AnimFloat 链上所有关键帧的时间（含 `next` 链）。
fn keyframe_times(a: &AnimFloat, out: &mut Vec<f64>) {
    let mut cur = Some(a);
    while let Some(a) = cur {
        out.extend(a.keyframes.iter().map(|kf| kf.time).filter(|t| t.is_finite()));
        cur = a.next.as_deref();
    }
}

fn eval(a: &mut AnimFloat, t: f64, default: f32) -> f32 {
    a.set_time(t);
    a.now_opt().unwrap_or(default)
}

/// 从 alpha 轨解出相位区间（老谱面兜底）。
///
/// 关键点：相位跳变可能发生在**两个关键帧之间的插值段里**（Hold / Jump 缓动尤其明显），
/// 所以除了关键帧本身，还要在每对相邻关键帧之间**二分**找阈值穿越点。
fn spans_from_alpha(alpha: &AnimFloat) -> Vec<Span> {
    let mut knots = vec![0.];
    keyframe_times(alpha, &mut knots);
    knots.sort_by(f64::total_cmp);
    knots.dedup();

    let mut anim = alpha.clone();
    let mut sample = |time: f64| {
        anim.set_time(time);
        anim.now_opt().unwrap_or(1.)
    };

    let mut boundaries = knots.clone();
    for pair in knots.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let before_b = b - ((b - a) * 1e-8).min(1e-8);
        let (left, right) = (sample(a), sample(before_b));
        for threshold in [0., 128. / 255.] {
            if left.is_finite() && right.is_finite() && (left > threshold) != (right > threshold) {
                let mut lo = a;
                let mut hi = before_b;
                for _ in 0..40 {
                    let mid = (lo + hi) * 0.5;
                    if (sample(mid) > threshold) == (left > threshold) {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                boundaries.push(hi);
            }
        }
    }
    boundaries.sort_by(f64::total_cmp);
    boundaries.dedup();

    let mut spans: Vec<Span> = Vec::new();
    for (i, &start) in boundaries.iter().enumerate() {
        let end = boundaries.get(i + 1).copied().unwrap_or(f64::INFINITY);
        let state = phase_of_alpha(sample(if end.is_finite() { (start + end) * 0.5 } else { start }));
        if let Some(last) = spans.last_mut().filter(|last| last.phase == state) {
            last.end = end;
        } else {
            spans.push(Span {
                start,
                end,
                phase: state,
                timings: [0.; 4],
            });
        }
    }
    spans
}

/// 用速度码覆盖相位区间（新谱面）。返回 `true` 表示速度码生效了。
///
/// 文档：1 出现 / 2 激活 / 3 失活 / 4 消失；只读**开始时间和开始数值**。
/// 数值**回退**（`code <= previous`）说明上一段生命周期已经结束 → 在这里收尾并另起一段。
/// 没有速度码时不动 `spans`（保留 alpha 兜底）。
///
/// ⚠️ 这条路径**自带 timings**（下面 `finish` 里直接写），所以调用方**不能**再跑
/// [`fill_timings`] —— 那会把它算好的 appear/enable/disable/disappear 覆盖掉，
/// 而且会把两段独立生命周期当成一段（速度码路径里没有 Hidden 段来分隔它们）。
fn spans_from_lifecycle(spans: &mut Vec<Span>, events: &[(f64, f32)]) -> bool {
    let mut events: Vec<_> = events
        .iter()
        .copied()
        .filter(|(time, code)| time.is_finite() && [1., 2., 3., 4.].contains(code))
        .collect();
    if events.is_empty() {
        return false;
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));

    *spans = Vec::new();
    let mut timings = [f64::INFINITY; 4];
    let mut previous = 0;
    let mut first = 0;
    let finish = |spans: &mut Vec<Span>, timings: &mut [f64; 4], end: f64, first: usize| {
        timings[3] = end;
        for span in &mut spans[first..] {
            span.end = span.end.min(end);
            span.timings = *timings;
        }
        *timings = [f64::INFINITY; 4];
    };
    for (time, code) in events {
        let code = code as usize;
        if code <= previous && timings[0].is_finite() {
            finish(spans, &mut timings, time, first);
            first = spans.len();
        }
        if let Some(last) = spans.last_mut() {
            last.end = last.end.min(time);
        }
        if code == 4 {
            finish(spans, &mut timings, time, first);
            first = spans.len();
        } else {
            timings[0] = timings[0].min(time);
            timings[code - 1] = time;
            spans.push(Span {
                start: time,
                end: f64::INFINITY,
                phase: if code == 2 { BlockPhase::Active } else { BlockPhase::Disabled },
                timings: [0.; 4],
            });
        }
        previous = code;
    }
    finish(spans, &mut timings, f64::INFINITY, first);
    true
}

/// 给每个「可见段」补全 `timings`：从一段连续可见区里取出现 / 激活 / 失活 / 消失。
fn fill_timings(spans: &mut [Span]) {
    let mut first = 0;
    while first < spans.len() {
        if spans[first].phase == BlockPhase::Hidden {
            first += 1;
            continue;
        }
        let end = spans[first..]
            .iter()
            .position(|span| span.phase == BlockPhase::Hidden)
            .map_or(spans.len(), |i| first + i);
        let appear = spans[first].start;
        let disappear = spans.get(end).map_or(f64::INFINITY, |span| span.start);
        let mut enable = f64::INFINITY;
        for span in spans[first..end].iter_mut().rev() {
            if span.phase == BlockPhase::Active {
                enable = span.start;
            }
            let disable = if span.phase == BlockPhase::Active { span.end } else { f64::INFINITY };
            span.timings = [appear, enable, disable, disappear];
        }
        // 激活前 0.5 秒的 Ready 窗口：把「出现」提前到 `start - 0.5`（取更早的那个）。
        let mut was_active = false;
        for span in &mut spans[first..end] {
            if was_active {
                span.timings[0] = appear.min(span.start - 0.5);
            }
            was_active |= span.phase == BlockPhase::Active;
        }
        first = end;
    }
}

/// BlockArea 的缓动枚举里代表「线性」的那个值（见上面「已简化」第 2 条）。
const BLOCK_EASE_LINEAR: i32 = 0;

fn chain_events<'a>(
    r: &mut BpmList,
    layers: impl Iterator<Item = &'a RPEEventLayer>,
    get: impl Fn(&RPEEventLayer) -> &Option<Vec<RPEEvent>>,
    bezier_map: &BezierMap,
) -> Result<AnimFloat> {
    let anis: Vec<AnimFloat> = layers
        .filter_map(|it| get(it).as_ref().map(|es| parse_events(r, es, None, bezier_map)))
        .collect::<Result<_>>()?;
    Ok(AnimFloat::chain(anis))
}

/// 把一根噪域专用线转成若干 [`BlockArea`]。
async fn parse_rpe_block_areas(
    r: &mut BpmList,
    rpe: &RPEJudgeLine,
    is_subtract: bool,
    fs: &mut dyn FileSystem,
    bezier_map: &BezierMap,
) -> Result<Vec<BlockArea>> {
    // ---- 1. 生命周期：速度码优先，alpha 兜底 ----
    let alpha = chain_events(r, rpe.event_layers.iter().flatten(), |it| &it.alpha_events, bezier_map)?;
    let mut spans = spans_from_alpha(&alpha);
    let mut speed_marks: Vec<(f64, f32)> = Vec::new();
    for layer in rpe.event_layers.iter().flatten() {
        for e in layer.speed_events.iter().flatten() {
            speed_marks.push((r.time(&e.start_time), e.start));
        }
    }
    if !spans_from_lifecycle(&mut spans, &speed_marks) {
        // 没有速度码 → 用 alpha 轨推出的相位区间，需要补全 timings。
        fill_timings(&mut spans);
    }
    if !spans.iter().any(|span| span.phase != BlockPhase::Hidden) {
        warn!("噪域线「{}」没有可见段（速度轨道需要 1/2/3/4，或把不透明度拉起来），已忽略", rpe.name);
        return Ok(Vec::new());
    }

    // ---- 2. 几何：位置 / 旋转 / 大小 ----
    let mut move_x = chain_events(r, rpe.event_layers.iter().flatten(), |it| &it.move_x_events, bezier_map)?;
    let mut move_y = chain_events(r, rpe.event_layers.iter().flatten(), |it| &it.move_y_events, bezier_map)?;
    let mut rotate = chain_events(r, rpe.event_layers.iter().flatten(), |it| &it.rotate_events, bezier_map)?;
    let (mut scale_x, mut scale_y) = match rpe.extended.as_ref() {
        Some(e) => (
            e.scale_x_events
                .as_ref()
                .map(|es| parse_events(r, es, None, bezier_map))
                .transpose()?
                .unwrap_or_default(),
            e.scale_y_events
                .as_ref()
                .map(|es| parse_events(r, es, None, bezier_map))
                .transpose()?
                .unwrap_or_default(),
        ),
        None => (AnimFloat::default(), AnimFloat::default()),
    };
    if scale_x.is_default() && scale_y.is_default() {
        warn!("噪域线「{}」没有缩放事件，拿不到块的大小，已忽略（宽高应写在 extended.scaleX/YEvents 上）", rpe.name);
        return Ok(Vec::new());
    }

    // 采样点 = 所有几何关键帧时间的并集。
    let mut times = Vec::new();
    keyframe_times(&move_x, &mut times);
    keyframe_times(&move_y, &mut times);
    keyframe_times(&rotate, &mut times);
    keyframe_times(&scale_x, &mut times);
    keyframe_times(&scale_y, &mut times);
    times.sort_by(f64::total_cmp);
    times.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    // ⚠️ 缩放事件是**倍数**，不是像素！
    //
    // RPE 里「实际宽高代替缩放倍数」**没打开**时（样本谱面就是这种），
    // `scaleX/YEvents` 的值是相对**贴图自身尺寸**的倍数：
    //   * 样本：scaleX 首个值 1.0、scaleY 首个值 0.2222，贴图 900×900
    //     → 真正的块尺寸 = (900, 200) px，正好是屏宽 2/3、屏高 22% 的一条噪域。
    //   * 直接拿 1.0 / 0.2222 当像素会得到「1 像素大的块」= 什么都不显示。
    //
    // pro.11 的做法（`chart.rs::rpe_marker_areas` → `rpe_block::Marker::transform`）：
    //   `size = Vector2::new(texture.width(), texture.height()) * scale`
    // 所以这里也要拿到贴图的像素尺寸。贴图就是谱面里那张 `isSubtract0/1.png`
    // （对 RPE Recorder 来说它只是**编辑器参考图**，运行时只用来定尺寸）。
    let (tex_w, tex_h) = {
        let bytes = match fs.load_file(&rpe.texture).await {
            Ok(bytes) => bytes,
            Err(err) => {
                warn!("噪域线「{}」的贴图读不到（{}）：{:?}，已忽略", rpe.name, rpe.texture, err);
                return Ok(Vec::new());
            }
        };
        match image::load_from_memory(&bytes) {
            Ok(img) => (img.width() as f32, img.height() as f32),
            Err(err) => {
                warn!("噪域线「{}」的贴图解析失败（{}）：{:?}，已忽略", rpe.name, rpe.texture, err);
                return Ok(Vec::new());
            }
        }
    };

    // 基准**倍数**：取**第一个两个轴都非零**的采样点。
    //
    // ⚠️ 不能直接取第一个采样点 —— 噪域经常带「从 0 放大」的生长动画
    // （样本 line 54 的 scaleX 首个值就是 0.0），那样基准会是 0、整条线被跳过。
    // pro.11 是每帧取瞬时 scale 再乘贴图尺寸，没有「基准」这个概念，所以不会踩这个坑。
    let (sx0, sy0) = match times.iter().find_map(|&t| {
        let (x, y) = (eval(&mut scale_x, t, 0.), eval(&mut scale_y, t, 0.));
        (x.abs() > 1e-6 && y.abs() > 1e-6).then_some((x, y))
    }) {
        Some(pair) => pair,
        None => {
            warn!("噪域线「{}」的缩放一直是 0，已忽略（贴图 {}×{}）", rpe.name, tex_w, tex_h);
            return Ok(Vec::new());
        }
    };
    let (w0, h0) = (tex_w * sx0, tex_h * sy0);
    if w0 <= 0. || h0 <= 0. {
        warn!("噪域线「{}」的缩放值为负（{}×{}，贴图 {}×{}），已忽略", rpe.name, sx0, sy0, tex_w, tex_h);
        return Ok(Vec::new());
    }

    // 基准矩形：以屏幕中心为心、w0×h0 为大小（实际位置由 move_events 绝对指定）。
    let half = Vector::new(w0 / RPE_WIDTH / 2., h0 / RPE_HEIGHT / 2.);
    let bottom_left = Vector::new(0.5, 0.5) - half;
    let top_right = Vector::new(0.5, 0.5) + half;

    // 有移动事件时 BlockArea::transform 用移动点当中心、忽略锚点，所以锚点填中心即可。
    let anchor = Vector::new(0.5, 0.5);
    let move_events = if move_x.is_default() && move_y.is_default() {
        Vec::new()
    } else {
        times
            .iter()
            .map(|&t| BlockMoveEvent {
                end: Vector::new(0.5 + eval(&mut move_x, t, 0.) / RPE_WIDTH, 0.5 + eval(&mut move_y, t, 0.) / RPE_HEIGHT),
                time: t,
                ease_x: BLOCK_EASE_LINEAR,
                ease_y: BLOCK_EASE_LINEAR,
            })
            .collect()
    };
    let rotate_events = if rotate.is_default() {
        Vec::new()
    } else {
        times
            .iter()
            .map(|&t| BlockRotateEvent {
                anchor,
                time: t,
                ease: BLOCK_EASE_LINEAR,
                // RPE 的旋转顺时针为正（prpr 判定线那边也取负），BlockArea 用逆时针。
                rotation: -eval(&mut rotate, t, 0.),
            })
            .collect()
    };
    let scale_events = if scale_x.is_default() && scale_y.is_default() {
        Vec::new()
    } else {
        times
            .iter()
            .map(|&t| BlockScaleEvent {
                anchor,
                time: t,
                ease_x: BLOCK_EASE_LINEAR,
                ease_y: BLOCK_EASE_LINEAR,
                // 相对基准**倍数**的比值（不是相对像素）。
                scale: Vector::new(eval(&mut scale_x, t, sx0) / sx0, eval(&mut scale_y, t, sy0) / sy0),
            })
            .collect()
    };

    // ---- 3. 颜色（彩色噪域）：`colorEvents` 取一次，按可见段中点采样 ----
    let mut color_anim = match rpe.extended.as_ref().and_then(|e| e.color_events.as_ref()) {
        Some(events) => parse_events(r, events, Some(WHITE), bezier_map)?,
        None => Anim::default(),
    };
    let color_at = |anim: &mut Anim<Color>, t: f64| -> [f32; 3] {
        anim.set_time(t);
        anim.now_opt().map_or(crate::core::DEFAULT_BLOCK_COLOR, |c| [c.r, c.g, c.b].map(|v| v.clamp(0., 1.)))
    };

    // ---- 4. 每个可见段一个 BlockArea（timings 相同的合并） ----
    let mut out: Vec<BlockArea> = Vec::new();
    for span in spans.iter().filter(|span| span.phase != BlockPhase::Hidden) {
        let [appear, enable, disable, disappear] = span.timings;
        if !appear.is_finite() {
            continue;
        }
        if let Some(last) = out.last().filter(|last| {
            (last.appear_time, last.enable_time, last.disable_time, last.disappear_time) == (appear, enable, disable, disappear)
        }) {
            let _ = last;
            continue;
        }
        let sample_at = if span.end.is_finite() { (span.start + span.end) * 0.5 } else { span.start };
        out.push(BlockArea {
            top_right,
            bottom_left,
            appear_time: appear,
            enable_time: enable,
            disable_time: disable,
            disappear_time: disappear,
            is_subtract,
            layer: 0,
            color: color_at(&mut color_anim, sample_at),
            rotate_events: rotate_events.clone(),
            move_events: move_events.clone(),
            scale_events: scale_events.clone(),
        });
    }
    Ok(out)
}

/// 把 RPEN 的 `skewControl` 落成判定线上的 skew 控制动画。
///
/// 数据形态：`[{ "x": <控制点坐标>, "skew": <度>, "easing": <缓动 id> }, ...]`。
/// 上游做法：按 `x` 排序，每个点一帧，缓动取**下一个点**的 `easing`（末点用 1），
/// 少于 2 个点就不生效（单点无法构成动画）。
fn apply_skew_control(line: &mut JudgeLine, points: &serde_json::Value) {
    if let Some(keys) = skew_keys(points) {
        line.ctrl_obj.get_mut().skew = AnimFloat::new(keys);
    }
}

/// `skewControl` → 关键帧（纯计算，便于单测）。少于 2 个点返回 `None`（单点构不成动画）。
fn skew_keys(points: &serde_json::Value) -> Option<Vec<Keyframe<f32>>> {
    let mut points = points.as_array().cloned().unwrap_or_default();
    if points.len() < 2 {
        return None;
    }
    points.sort_by(|a, b| super::rpen::number(&a["x"], 0.).total_cmp(&super::rpen::number(&b["x"], 0.)));
    Some(
        points
            .iter()
            .enumerate()
            .map(|(i, p)| {
                // 缓动取**下一个点**的 easing（末点用 1），与上游一致。
                let easing = points
                    .get(i + 1)
                    .map_or(1., |p| super::rpen::number(&p["easing"], 1.));
                let id = (easing as usize).clamp(1, RPE_TWEEN_MAP.len() - 1);
                Keyframe::new(
                    super::rpen::number(&p["x"], 0.),
                    super::rpen::number(&p["skew"], 0.) as f32,
                    RPE_TWEEN_MAP[id],
                )
            })
            .collect(),
    )
}

/// 把 RPEN 的**曲线轨迹**（`moveXEvents` 里的 `trajectory`）接进判定线的
/// 平移 / 旋转轨道。
///
/// 与上游一致的三条轴：
/// - `moveXEvents` → x（缩放 2/1350）
/// - `moveYEvents` + 带轨迹/切线/旋转表达式的 `moveXEvents` → y（缩放 2/900）
/// - `rotateEvents` + 开了 `tangentRotation`/`rotationExpression` 的 `moveXEvents` → 旋转（缩放 -1）
///
/// 只有该线**确实出现**了轨迹事件时才覆盖原动画，其余谱面完全走原路径。
fn apply_trajectory(line: &mut JudgeLine, raw: &serde_json::Value, bpm: &mut BpmList) -> anyhow::Result<()> {
    for (axis, track, scale) in [(0usize, "moveXEvents", 2. / 1350.), (1, "moveYEvents", 2. / 900.), (2, "rotateEvents", -1.)] {
        let mut anis = Vec::new();
        let mut has_trajectory = false;
        for layer in raw["eventLayers"].as_array().into_iter().flatten().filter(|v| v.is_object()) {
            let mut events = layer[track].as_array().cloned().unwrap_or_default();
            if axis != 0 {
                events.extend(
                    layer["moveXEvents"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|e| {
                            e["trajectory"].is_object()
                                && (axis == 1
                                    || super::rpen::flag(&e["trajectory"]["options"]["tangentRotation"])
                                    || e["trajectory"]["options"]["rotationExpression"]
                                        .as_str()
                                        .is_some_and(|s| !s.trim().is_empty()))
                        })
                        .cloned(),
                );
            }
            has_trajectory |= events.iter().any(|e| e["trajectory"].is_object());
            anis.push(super::rpen::motion(&events, axis, bpm, 1.)?);
        }
        if has_trajectory {
            let mut anim = AnimFloat::chain(anis);
            anim.map_value(|v| v * scale as f32);
            match axis {
                0 => line.object.translation.0 = anim,
                1 => line.object.translation.1 = anim,
                _ => line.object.rotation = anim,
            }
        }
    }
    Ok(())
}

pub async fn parse_rpe(
    source: &str,
    fs: &mut dyn FileSystem,
    extra: ChartExtra,
    use_rpe_170_speed: bool,
    use_rpen: bool,
) -> Result<Chart> {
    let rpe: RPEChart = serde_json::from_str(source).with_context(|| ptl!("json-parse-failed"))?;
    // RPEN 的 `bpmfactor` / `skewControl` 不在 `RPEChart` 里（那是给引擎用的精简结构），
    // 需要另外留一份原始 JSON 来读。
    let raw: serde_json::Value = if use_rpen {
        serde_json::from_str(source).with_context(|| ptl!("json-parse-failed"))?
    } else {
        serde_json::Value::Null
    };
    let speed_mode = if rpe.meta.rpe_version >= 170 {
        SpeedEasingMode::Modern
    } else {
        SpeedEasingMode::Legacy
    };
    let bezier_map = get_bezier_map(&rpe);
    // 原始 BPM 项留一份：`bpmfactor` 要靠「把 BPM 除以 factor」实现，每条线各建一个 BpmList。
    let bpm_items: Vec<(f64, f64)> = rpe.bpm_list.iter().map(|it| (it.start_time.beats(), it.bpm)).collect();
    let mut r = BpmList::new(bpm_items.clone());
    fn vec<T>(v: &Option<Vec<T>>) -> impl Iterator<Item = &T> {
        v.iter().flat_map(|it| it.iter())
    }
    let mut hitsounds = HashMap::new();
    #[rustfmt::skip]
    let max_time = *rpe
        .judge_line_list
        .iter()
        .map(|line| {
            line.notes.as_ref().map(|notes| {
                notes
                    .iter()
                    .map(|note| r.time(&note.end_time).not_nan())
                    .max()
                    .unwrap_or_default()
            }).unwrap_or_default().max(
                line.event_layers.iter().filter_map(|it| it.as_ref().map(|layer| {
                    vec(&layer.alpha_events)
                        .chain(vec(&layer.move_x_events))
                        .chain(vec(&layer.move_y_events))
                        .chain(vec(&layer.rotate_events))
                        .map(|it| r.time(&it.end_time).not_nan())
                        .max().unwrap_or_default()
                })).max().unwrap_or_default()
            ).max(
                line.extended.as_ref().map(|e| {
                    vec(&e.scale_x_events)
                        .chain(vec(&e.scale_y_events))
                        .map(|it| r.time(&it.end_time).not_nan())
                        .max().unwrap_or_default()
                        .max(vec(&e.text_events).map(|it| r.time(&it.end_time).not_nan()).max().unwrap_or_default())
                }).unwrap_or_default()
            )
        })
        .max().unwrap_or_default() + 1.;
    // don't want to add a whole crate for a mere join_all...
    let mut lines = Vec::new();
    let mut line_texture_map = HashMap::new();
    // RPE 的噪域（`isSubtract0/1.png`）：不落在 lines 里，而是转成 BlockArea。
    let mut block_areas: Vec<BlockArea> = Vec::new();
    for (id, mut rpe) in rpe.judge_line_list.into_iter().enumerate() {
        let name = rpe.name.clone();
        let name_for_log = name.clone();
        // RPEN `bpmfactor`：把这条线的 BPM 全除以 factor → 这条线的时间轴整体拉长 factor 倍。
        // （上游就是这么做的，比逐事件乘 factor 简单且不会漏掉任何轨道。）
        let factor = if use_rpen {
            super::rpen::number(&raw["judgeLineList"][id]["bpmfactor"], 1.)
        } else {
            1.
        };
        anyhow::ensure!(factor.is_finite() && factor > 0., "invalid bpmfactor on line {id}");
        let mut line_bpm = BpmList::new(bpm_items.iter().map(|&(b, v)| (b, v / factor)).collect());
        let line_max_time = max_time * factor;
        if let Some(is_subtract) = marker_kind(&rpe.texture) {
            match parse_rpe_block_areas(&mut line_bpm, &rpe, is_subtract, fs, &bezier_map).await {
                Ok(areas) => block_areas.extend(areas),
                Err(err) => warn!("噪域线「{}」解析失败，已忽略：{:?}", name, err),
            }
            // 转成噪区之后这根判定线就不能再画东西了：不渲染音符、不画贴图本身。
            // ⚠️ 但仍然要**留在 lines 里** —— 其它线的 `parent` 是按原始下标索引的，
            //    把它抽走会让后面的线全部串位。
            //   * notes = None → 没有音符
            //   * texture = "line.png" + extended = None → kind 落到 Normal，不加载贴图
            //   * 清掉 alpha_events → alpha 恒为 0（events_with_factor 对空事件返回 fixed(0.)）
            rpe.notes = None;
            rpe.extended = None;
            rpe.texture = "line.png".to_owned();
            for layer in rpe.event_layers.iter_mut().flatten() {
                layer.alpha_events = None;
            }
        }
        let mut line = parse_judge_line(
            &mut line_bpm,
            rpe,
            line_max_time,
            speed_mode,
            fs,
            use_rpe_170_speed,
            &bezier_map,
            &mut hitsounds,
            &mut line_texture_map,
        )
        .await
        .with_context(move || ptl!("judge-line-location-name", "jlid" => id, "name" => name))?;
        // RPEN `skewControl`：按 x 排序后转成该线的 skew 控制动画（音符渲染时按高度采样）。
        if use_rpen {
            let raw_line = &raw["judgeLineList"][id];
            apply_skew_control(&mut line, &raw_line["skewControl"]);
            // 轨迹解析失败不该让整张谱面打不开：只影响这一条线，记日志降级。
            if let Err(err) = apply_trajectory(&mut line, raw_line, &mut line_bpm) {
                warn!("判定线「{}」的 RPEN 轨迹解析失败，已忽略：{err:#}", name_for_log);
            }
        }
        lines.push(line);
    }
    fn has_cycle(line: &JudgeLine, lines: &[JudgeLine], visited: &mut Vec<usize>) -> Option<usize> {
        if let Some(parent_index) = line.parent {
            if visited.contains(&parent_index) {
                return Some(parent_index);
            }
            visited.push(parent_index);
            return has_cycle(&lines[parent_index], lines, visited);
        }
        None
    }
    for (i, line) in lines.iter().enumerate() {
        let mut vec = Vec::new();
        vec.push(i);
        if let Some(line) = has_cycle(line, &lines, &mut vec) {
            ptl!(bail "found-infinite-recursive-parent", "line" => line)
        }
    }
    process_lines(&mut lines);
    let mut chart = Chart::new(rpe.meta.offset as f32 / 1000.0, lines, r, ChartSettings::default(), extra, hitsounds);
    chart.block_areas = block_areas;
    // RPEN 分层噪域：新格式 `blockAreaList`（带 layerId / isInvert）单独存一份。
    if use_rpen {
        match super::rpen::parse_noise_layers(&raw) {
            Ok(layer_ids) => match super::rpen::parse_noise_areas(&raw, &bpm_items, &layer_ids) {
                Ok(areas) => {
                    chart.next_block_areas = areas;
                    // 有分层声明时每层一个独立渲染器 + 可选皮肤。
                    if !layer_ids.is_empty() {
                        chart.next_layer_renderers = (0..layer_ids.len()).map(|_| Default::default()).collect();
                        for layer in raw["noiseAreaLayers"].as_array().into_iter().flatten() {
                            let skin = match crate::core::rpen_skin::Skin::load(layer, fs).await {
                                Ok(mut skin) => {
                                    if let Some(skin) = &mut skin {
                                        skin.time_offset = chart.offset as f64;
                                    }
                                    skin
                                }
                                Err(err) => {
                                    warn!("RPEN 噪域皮肤加载失败，已回退原生材质：{err:#}");
                                    None
                                }
                            };
                            chart.next_layer_skins.push(skin);
                        }
                    }
                }
                Err(err) => warn!("RPEN 噪域解析失败，已忽略：{err:#}"),
            },
            Err(err) => warn!("RPEN 噪域分层解析失败，已忽略：{err:#}"),
        }
        // 新版反转规则：`noiseAreaOptions.ignoreTripleInversion` 或用了 RPEN 扩展时，
        // 按「某层恰好 1 个反转区」判定（否则沿用奇数个的旧规则）。
        let ignore_triple = raw["noiseAreaOptions"]["ignoreTripleInversion"].as_bool().unwrap_or(false);
        chart.single_inversion = ignore_triple || super::rpen::features(&raw);
        // 全局轨道 + 相机（globalEvents）。失败降级为「无相机」，不让谱面打不开。
        match crate::core::rpen_scene::Scene::new(&raw, &mut chart.bpm_list.borrow_mut()) {
            Ok(scene) => chart.rpen = Some(scene),
            Err(err) => warn!("RPEN globalEvents/相机解析失败，已忽略：{err:#}"),
        }
    }
    Ok(chart)
}

pub async fn lint(source: &str) -> Result<ParseWarnings> {
    // RPEN 探测要看原始 JSON 形态（RPEVersion / blockAreaList / extended…），
    // 而 `RPEChart` 把这些字段大多忽略了，所以单独解析一份原始值。
    let raw: serde_json::Value = serde_json::from_str(source).with_context(|| ptl!("json-parse-failed"))?;
    let has_rpen_features = super::rpen::features(&raw);
    let rpe: RPEChart = serde_json::from_str(source).with_context(|| ptl!("json-parse-failed"))?;
    let has_new_speed_events = rpe
        .judge_line_list
        .iter()
        .flat_map(|line| line.event_layers.iter().flatten())
        .flat_map(|layer| layer.speed_events.iter())
        .flatten()
        .any(|event| event.easing_type > 1);
    let has_attach_ui = rpe.judge_line_list.iter().any(|line| line.attach_ui.is_some());

    Ok(ParseWarnings {
        has_new_speed_events,
        has_attach_ui,
        has_rpen_features,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 120 BPM：1 拍 = 0.5 秒，方便手算。
    fn bpm() -> BpmList {
        BpmList::new(vec![(0., 120.)])
    }

    /// 端到端：拿上游的 `rpen-extensions` fixture 走一遍真实 `parse_rpe`，
    /// 一次性验证 bpmfactor / skewControl / 曲线轨迹三件事，并确认关掉 `use_rpen` 会回退。
    #[test]
    fn rpen_extensions_fixture_end_to_end() {
        let source = include_str!("../../tests/fixtures/rpen-extensions/chart.json");
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();

        let mut fs_on = crate::fs::ExternalFileSystem(std::sync::Arc::new(crate::dir::Dir::new(dir.path()).unwrap()));
        let enabled = runtime
            .block_on(parse_rpe(source, &mut fs_on, ChartExtra::default(), false, true))
            .expect("开启 RPEN 应该能解析");

        let mut fs_off = crate::fs::ExternalFileSystem(std::sync::Arc::new(crate::dir::Dir::new(dir.path()).unwrap()));
        let disabled = runtime
            .block_on(parse_rpe(source, &mut fs_off, ChartExtra::default(), false, false))
            .expect("关闭 RPEN 应该能解析");

        // ① bpmfactor = 2：120BPM 下第 4 拍 = 2s，翻倍后 = 4s
        let min_time = |c: &crate::core::Chart| {
            c.lines[0]
                .notes
                .iter()
                .map(|n| n.time)
                .fold(f64::INFINITY, f64::min)
        };
        assert!((min_time(&enabled) - 4.).abs() < 1e-6, "bpmfactor=2 时首个音符应在 4s，实际 {}", min_time(&enabled));
        assert!((min_time(&disabled) - 2.).abs() < 1e-6, "关闭 RPEN 时应回到 2s，实际 {}", min_time(&disabled));

        // ② skewControl：开了才有非默认 skew
        assert!(!enabled.lines[0].ctrl_obj.borrow().skew.is_default(), "开启 RPEN 时 skew 应生效");
        assert!(disabled.lines[0].ctrl_obj.borrow().skew.is_default(), "关闭 RPEN 时 skew 应保持默认");

        // ③ 曲线轨迹：开启后平移轨道应被轨迹覆盖（关键帧多于默认单帧）
        assert!(
            enabled.lines[0].object.translation.0.keyframes.len() > 1,
            "开启 RPEN 时 x 平移应被轨迹接管"
        );

        // ④ 分层噪域：两条 blockAreaList 分别落在 layer 0 / 1，且第二条是反转区
        assert_eq!(enabled.next_block_areas.len(), 2, "应有 2 个新格式噪区");
        assert_eq!(enabled.next_block_areas[0].layer, 0);
        assert_eq!(enabled.next_block_areas[1].layer, 1, "upper 层是第 1 层");
        let first = enabled.next_block_areas[0].sample(0.5, 16. / 9.).expect("0.5s 时应可见");
        let second = enabled.next_block_areas[1].sample(0.5, 16. / 9.).expect("0.5s 时应可见");
        assert_eq!(first.state, 2, "activeIntervals 内应为激活态");
        assert!(!first.zone.invert);
        assert!(second.zone.invert, "isInvert=true 应转成反转区");
        assert!(enabled.single_inversion, "用了 RPEN 扩展应启用新版反转规则");
        // 关闭 RPEN 时这些都不该生效
        assert!(disabled.next_block_areas.is_empty(), "关闭 RPEN 不该解析新格式噪区");
        assert!(!disabled.single_inversion);
    }

    #[test]
    fn skew_keys_need_two_points_and_are_sorted() {
        assert!(skew_keys(&json!([])).is_none());
        assert!(skew_keys(&json!([{ "x": 0, "skew": 1 }])).is_none(), "单点不成动画");
        let keys = skew_keys(&json!([
            { "x": 100, "skew": 0.5 },
            { "x": -100, "skew": 0.25 }
        ]))
        .expect("两个点应该生效");
        assert_eq!(keys.len(), 2);
        // 按 x 升序排列
        assert!(keys[0].time < keys[1].time);
        assert!((keys[0].value - 0.25).abs() < 1e-6, "x=-100 的 skew 排在前面");
        assert!((keys[1].value - 0.5).abs() < 1e-6);
    }

    /// `marks` = [(拍, 速度轨道上的值)]
    fn speed_layer(marks: &[(i32, f32)]) -> RPEEventLayer {
        let events: Vec<serde_json::Value> = marks
            .iter()
            .map(|(beat, v)| {
                json!({
                    "easingLeft": 0., "easingRight": 1., "bezier": 0, "bezierPoints": [0., 0., 0., 0.],
                    "easingType": 1, "start": v, "end": v,
                    "startTime": [beat, 0, 1], "endTime": [beat, 0, 1],
                })
            })
            .collect();
        serde_json::from_value(json!({ "speedEvents": events })).unwrap()
    }

    /// `marks` = [(拍, 速度轨道上的值)]，只取**开始时间 + 开始数值**。
    fn speed_marks(marks: &[(i32, f32)]) -> Vec<(f64, f32)> {
        let mut r = bpm();
        let layer = speed_layer(marks);
        let mut out = Vec::new();
        for e in layer.speed_events.iter().flatten() {
            out.push((r.time(&e.start_time), e.start));
        }
        out
    }

    /// 速度码优先：alpha 轨故意全 0（全隐藏），看速度码能不能覆盖它。
    ///
    /// ⚠️ 速度码路径**自带 timings**，所以这里不能再调 `fill_timings`（那会覆盖掉，
    /// 并把两段独立生命周期当成一段 —— 速度码路径里没有 Hidden 段分隔它们）。
    fn spans_from_codes(marks: &[(i32, f32)]) -> Vec<Span> {
        let alpha = AnimFloat::fixed(0.);
        let mut spans = spans_from_alpha(&alpha);
        assert!(spans_from_lifecycle(&mut spans, &speed_marks(marks)), "速度码应该生效");
        spans
    }

    /// 取 `t` 时刻所在**可见段**的 `[appear, enable, disable, disappear]`。
    fn timings_at(spans: &[Span], t: f64) -> Option<[f64; 4]> {
        spans
            .iter()
            .find(|s| s.phase != BlockPhase::Hidden && s.start <= t && t < s.end)
            .map(|s| s.timings)
    }

    /// 移植自 pro.11 的 `speed_codes_override_editor_alpha_and_restart_lifecycles`。
    ///
    /// 速度码覆盖 alpha 轨，并且**数值回退**（`code <= previous`）时另起一段。
    #[test]
    fn speed_codes_override_alpha_and_restart_lifecycles() {
        // pro.11 用例里的时间是**秒**；这里的数据是**拍**（120 BPM → 秒 = 拍 × 0.5），
        // 所以拍号 = 秒 × 2。
        let spans = spans_from_codes(&[(0, 10.), (4, 1.), (8, 2.), (12, 3.), (16, 2.), (18, 4.), (20, 2.), (22, 4.)]);
        // 120 BPM → 拍 × 0.5 = 秒
        assert!(timings_at(&spans, 1.).is_none(), "1 秒时还没出现");
        assert_eq!(timings_at(&spans, 3.), Some([2., 4., 6., 8.]));
        assert_eq!(timings_at(&spans, 4.), Some([2., 4., 6., 8.]), "激活区间内");
        // 第二段：8s 的「2」在「3」之后 → 另起一段
        assert!(timings_at(&spans, 8.).is_some_and(|t| t[0] == 8.), "8s 起第二段");
        assert!(timings_at(&spans, 9.).is_none(), "9s 的「4」把它关掉了");
        assert!(timings_at(&spans, 10.).is_some_and(|t| t[0] == 10.), "10s 起第三段");
        assert!(timings_at(&spans, 11.).is_none());
    }

    /// 文档里的 `2→4→2→4`：两段「直接激活 + 消失」。
    #[test]
    fn direct_2_4_twice() {
        let spans = spans_from_codes(&[(0, 2.), (2, 4.), (4, 2.), (6, 4.)]);
        let first = timings_at(&spans, 0.5).expect("第一段");
        assert_eq!([first[0], first[1]], [0., 0.], "直接激活时出现与激活同时");
        assert_eq!(first[3], 1.);
        let second = timings_at(&spans, 2.5).expect("第二段");
        assert_eq!([second[0], second[1]], [2., 2.]);
        assert_eq!(second[3], 3.);
    }

    /// 没有速度码时**回退到不透明度轨**（老谱面）。
    #[test]
    fn alpha_track_is_used_when_no_speed_codes() {
        // alpha：0s 起 0（隐藏）→ 1s 起 1（激活）→ 2s 起 0（隐藏）
        let alpha = AnimFloat::new(vec![
            Keyframe::new(0., 0., 0),
            Keyframe::new(1., 1., 0),
            Keyframe::new(2., 0., 0),
        ]);
        let mut spans = spans_from_alpha(&alpha);
        fill_timings(&mut spans);
        assert_eq!(timings_at(&spans, 1.5), Some([1., 1., 2., 2.]));
        assert!(timings_at(&spans, 0.5).is_none(), "激活前是隐藏");
        assert!(timings_at(&spans, 2.5).is_none(), "消失后是隐藏");
    }

    /// alpha 的三档阈值：这些数值是**状态码**，不是填充不透明度。
    #[test]
    fn alpha_phase_thresholds() {
        assert_eq!(phase_of_alpha(0.), BlockPhase::Hidden);
        assert_eq!(phase_of_alpha(-1.), BlockPhase::Hidden);
        assert_eq!(phase_of_alpha(f32::NAN), BlockPhase::Hidden);
        assert_eq!(phase_of_alpha(121. / 255.), BlockPhase::Disabled, "示例谱面的预览码");
        assert_eq!(phase_of_alpha(128. / 255.), BlockPhase::Disabled);
        assert_eq!(phase_of_alpha(1.), BlockPhase::Active);
    }

    /// ⚠️ 必须**精确匹配文件名**：`isSubtract1.png.backup` 不该被当成噪域线
    /// （旧实现用 `starts_with` 会误判）。
    #[test]
    fn marker_kind_is_an_exact_filename_match() {
        assert_eq!(marker_kind("isSubtract0.png"), Some(false));
        assert_eq!(marker_kind("isSubtract1.png"), Some(true));
        assert_eq!(marker_kind("some/dir/ISSUBTRACT1.PNG"), Some(true));
        assert_eq!(marker_kind("dir\\isSubtract0.png"), Some(false));
        assert_eq!(marker_kind("isSubtract1.png.backup"), None);
        assert_eq!(marker_kind("isSubtract2.png"), None);
        assert_eq!(marker_kind("line.png"), None);
        assert_eq!(marker_kind(""), None);
    }
}
