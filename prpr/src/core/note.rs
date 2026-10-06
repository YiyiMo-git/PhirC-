use super::{chart::ChartSettings, AnimFloat, BpmList, CtrlObject, JudgeLine, Matrix, Object, Point, Resource};
pub use crate::{
    config::{Mods, NoteMapTarget},
    judge::{HitSound, JudgeStatus},
    parse::RPE_HEIGHT,
};
use macroquad::prelude::*;

const HOLD_PARTICLE_INTERVAL: f64 = 0.15;
const FADEOUT_TIME: f64 = 0.16;
const BAD_TIME: f64 = 0.5;

#[derive(Clone, Debug)]
pub enum NoteKind {
    Click,
    Hold { end_time: f64, end_height: f64 },
    Flick,
    Drag,
}

impl NoteKind {
    pub fn order(&self) -> i8 {
        match self {
            Self::Hold { .. } => 0,
            Self::Drag => 1,
            Self::Click => 2,
            Self::Flick => 3,
        }
    }
}

impl Note {
    /// 按「音符映射」把本音符整体换成另一种类型（见 [`crate::config::NoteMap`]）。
    ///
    /// - `target` 为 [`NoteMapTarget::Keep`]、或目标类型和当前类型相同时都不动，返回 `false`；
    /// - 变成**长条**时 `end_time = time + hold_secs`，`end_height` 取**判定线**在 `end_time`
    ///   的高度（与解析器里 hold 的算法一致）—— 直接用音符自己的高度会让长条长度变成 0；
    /// - 内建打击音效（click / flick / drag）跟着类型走，RPE 自定义音效（[`HitSound::Custom`]）保留。
    ///
    /// 返回是否真的改了类型。
    pub fn apply_map(&mut self, target: NoteMapTarget, hold_secs: f64, line_height: &mut AnimFloat) -> bool {
        let new_kind = match target {
            NoteMapTarget::Keep => return false,
            NoteMapTarget::Tap => NoteKind::Click,
            NoteMapTarget::Drag => NoteKind::Drag,
            NoteMapTarget::Flick => NoteKind::Flick,
            NoteMapTarget::Hold => {
                let end_time = self.time + hold_secs.max(1e-3);
                line_height.set_time(end_time);
                NoteKind::Hold {
                    end_time,
                    end_height: line_height.now() as f64,
                }
            }
        };
        if std::mem::discriminant(&self.kind) == std::mem::discriminant(&new_kind) {
            return false;
        }
        if !matches!(self.hitsound, HitSound::Custom(_)) {
            self.hitsound = HitSound::default_from_kind(&new_kind);
        }
        self.kind = new_kind;
        true
    }
}

pub struct Note {
    pub object: Object,
    pub kind: NoteKind,
    pub hitsound: HitSound,
    pub time: f64,
    pub height: f64,
    pub speed: f64,
    pub color: Color,
    pub fx_color: Option<Color>,
    pub judge_area: f32,

    /// From the other side of the line
    pub above: bool,
    pub multiple_hint: bool,
    pub fake: bool,
    pub judge: JudgeStatus,
}

pub struct RenderConfig<'a> {
    pub settings: &'a ChartSettings,
    pub ctrl_obj: &'a mut CtrlObject,
    pub line_height: f64,
    pub appear_before: f64,
    pub draw_below: bool,
    pub incline_sin: f32,
}

#[allow(clippy::too_many_arguments)]
fn draw_tex(res: &Resource, texture: Texture2D, order: i8, x: f32, y: f32, color: Color, mut params: DrawTextureParams, clip: bool) {
    let Vec2 { x: w, y: h } = params.dest_size.unwrap();
    if h < 0. {
        return;
    }
    let mut p = [Point::new(x, y), Point::new(x + w, y), Point::new(x + w, y + h), Point::new(x, y + h)];
    if clip {
        if y + h <= 0. {
            return;
        }
        if y <= 0. {
            let r = -y / (y + h);
            p[0].y = 0.;
            p[1].y = 0.;
            let mut source = params.source.unwrap_or_else(|| Rect::new(0., 0., 1., 1.));
            source.y += source.h * r;
            params.source = Some(source);
        }
    }
    params.flip_y = true;
    draw_tex_pts(res, texture, order, p, color, params);
}
fn draw_tex_pts(res: &Resource, texture: Texture2D, order: i8, p: [Point; 4], color: Color, params: DrawTextureParams) {
    let mut p = p.map(|it| res.world_to_screen(it));
    if p[0].x.min(p[1].x.min(p[2].x.min(p[3].x))) > 1.
        || p[0].x.max(p[1].x.max(p[2].x.max(p[3].x))) < -1.
        || p[0].y.min(p[1].y.min(p[2].y.min(p[3].y))) > 1.
        || p[0].y.max(p[1].y.max(p[2].y.max(p[3].y))) < -1.
    {
        return;
    }
    let Rect { x: sx, y: sy, w: sw, h: sh } = params.source.unwrap_or(Rect { x: 0., y: 0., w: 1., h: 1. });

    if params.flip_x {
        p.swap(0, 1);
        p.swap(2, 3);
    }
    if params.flip_y {
        p.swap(0, 3);
        p.swap(1, 2);
    }

    #[rustfmt::skip]
    let vertices = [
        Vertex::new(p[0].x, p[0].y, 0., sx     , sy     , color),
        Vertex::new(p[1].x, p[1].y, 0., sx + sw, sy     , color),
        Vertex::new(p[2].x, p[2].y, 0., sx + sw, sy + sh, color),
        Vertex::new(p[3].x, p[3].y, 0., sx     , sy + sh, color),
    ];
    res.note_buffer
        .borrow_mut()
        .push((order, texture.raw_miniquad_texture_handle().gl_internal_id()), vertices);
}

fn draw_center(res: &Resource, tex: Texture2D, order: i8, scale: f32, color: Color) {
    let hf = vec2(scale, tex.height() * scale / tex.width());
    draw_tex(
        res,
        tex,
        order,
        -hf.x,
        -hf.y,
        color,
        DrawTextureParams {
            dest_size: Some(hf * 2.),
            ..Default::default()
        },
        false,
    );
}

impl Note {
    pub fn rotation(&self, line: &JudgeLine) -> f32 {
        line.object.rotation.now() + if self.above { 0. } else { 180. }
    }

    pub fn plain(&self) -> bool {
        !self.fake && !matches!(self.kind, NoteKind::Hold { .. }) && self.object.translation.1.keyframes.len() <= 1
        // && self.ctrl_obj.is_default()
    }

    pub fn update(&mut self, res: &mut Resource, parent_rot: f32, parent_tr: &Matrix, ctrl_obj: &mut CtrlObject, line_height: f64) {
        self.object.set_time(res.time);
        if let Some(color) = if let JudgeStatus::Hold(perfect, at, ..) = &mut self.judge {
            if res.time > *at {
                *at += HOLD_PARTICLE_INTERVAL / res.config.speed as f64;
                Some(self.fx_color.unwrap_or_else(|| {
                    if *perfect {
                        res.res_pack.info.fx_perfect()
                    } else {
                        res.res_pack.info.fx_good()
                    }
                }))
            } else {
                None
            }
        } else {
            None
        } {
            self.init_ctrl_obj(ctrl_obj, line_height);
            res.with_model(parent_tr * self.now_transform(res, ctrl_obj, 0., 0.), |res| {
                res.emit_at_origin(parent_rot + if self.above { 0. } else { 180. }, color)
            });
        }
    }

    pub fn dead(&self) -> bool {
        (!matches!(self.kind, NoteKind::Hold { .. }) || matches!(self.judge, JudgeStatus::Judged)) && self.object.dead()
        // && self.ctrl_obj.dead()
    }

    fn init_ctrl_obj(&self, ctrl_obj: &mut CtrlObject, line_height: f64) {
        ctrl_obj.set_height((self.height - line_height + self.object.translation.1.now() as f64 / self.speed) * RPE_HEIGHT as f64 / 2.);
    }

    pub fn now_transform(&self, res: &Resource, ctrl_obj: &CtrlObject, base: f32, incline_sin: f32) -> Matrix {
        // 视觉流速倍率：只缩放音符离判定线的下落距离，不动谱面时间 / 音频 / 判定。
        let base = base * res.note_flow_speed;
        let incline_val = 1. - incline_sin * (base * res.aspect_ratio + self.object.translation.1.now()) * RPE_HEIGHT / 2. / 360.;
        let mut tr = self.object.now_translation(res);
        tr.x *= if matches!(self.kind, NoteKind::Hold { .. }) {
            1.
        } else {
            incline_val * ctrl_obj.pos.now_opt().unwrap_or(1.)
        };
        tr.y += base;
        let mut scale = self.object.scale.now_with_def(1.0, 1.0);
        scale.x *= ctrl_obj.size.now_opt().unwrap_or(1.0);
        if res.info.note_uniform_scale {
            scale.y *= ctrl_obj.size.now_opt().unwrap_or(1.0);
        } else {
            scale.y = 1.0;
        };
        self.object.now_rotation().append_nonuniform_scaling(&scale).append_translation(&tr)
    }

    pub fn render(&self, res: &mut Resource, config: &mut RenderConfig, bpm_list: &mut BpmList) {
        if matches!(self.judge, JudgeStatus::Judged) && !matches!(self.kind, NoteKind::Hold { .. }) {
            return;
        }
        if config.appear_before.is_finite() {
            // TODO optimize
            let beat = bpm_list.beat(self.time);
            let time = bpm_list.time_beats(beat - config.appear_before);
            if time > res.time {
                return;
            }
        }
        let scale = (if res.config.double_hint && self.multiple_hint {
            res.res_pack.note_style_mh.click.width() / res.res_pack.note_style.click.width()
        } else {
            1.0
        }) * res.note_width;
        let ctrl_obj = &mut config.ctrl_obj;
        self.init_ctrl_obj(ctrl_obj, config.line_height);
        let mut color = Color {
            a: self.object.now_alpha(),
            ..self.color
        };
        color.a *= res.alpha * ctrl_obj.alpha.now_opt().unwrap_or(1.);
        let spd = self.speed * ctrl_obj.y.now_opt().unwrap_or(1.) as f64;

        let line_height = config.line_height / res.aspect_ratio as f64 * spd;
        let height = self.height / res.aspect_ratio as f64 * spd;

        let base = height - line_height;
        let cover_base = if !config.settings.hold_partial_cover {
            height - line_height
        } else {
            match self.kind {
                NoteKind::Hold { end_time: _, end_height } => {
                    let end_height = end_height / res.aspect_ratio as f64 * spd;
                    end_height - line_height
                }
                _ => height - line_height,
            }
        };

        if !config.draw_below
            && (((res.time - FADEOUT_TIME >= self.time || self.fake && res.time >= self.time) && !matches!(self.kind, NoteKind::Hold { .. }))
                || (self.time > res.time && cover_base <= -0.001))
        {
            return;
        }
        let order = self.kind.order();
        // 拖影是否生效：总开关 + 该音符类型 + 只在上方（未判定的那侧）画
        let trail_on = res.config.note_trail
            && !config.draw_below
            && !self.fake
            && match self.kind {
                NoteKind::Click => res.config.note_trail_click,
                NoteKind::Drag => res.config.note_trail_drag,
                NoteKind::Flick => res.config.note_trail_flick,
                NoteKind::Hold { .. } => false,
            };
        let style = if res.config.double_hint && self.multiple_hint {
            &res.res_pack.note_style_mh
        } else {
            &res.res_pack.note_style
        };
        let limit_bad = res.config.limit_bad();
        let mod_alpha = if res.config.has_mod(Mods::FADE_OUT) {
            ((self.time - res.time - limit_bad) / limit_bad).clamp(0., 1.)
        } else if res.config.has_mod(Mods::FADE_IN) {
            (1. - (self.time - res.time - limit_bad) / limit_bad).clamp(0., 1.)
        } else {
            1.
        };
        let draw = |res: &mut Resource, tex: Texture2D| {
            let mut color = color;
            if !config.draw_below {
                let alpha = (self.time - res.time).min(0.) / FADEOUT_TIME + 1.;
                color.a *= if self.fake && res.time >= self.time { 0. } else { alpha as f32 };
            }
            color.a *= mod_alpha as f32;
            // ---- 音符拖影（V1）----
            // 音符沿判定线局部 +y 方向落下来，所以「过去的位置」就是 base 更大的地方；
            // 同一套 now_transform 只是把 y 偏移加大，不用额外算矩阵。
            // 只在正常情况下画（不画 show_below，也不画 Hold），而且要求 spd > 0：
            // 速度为 0 的音符根本没在动，画出来只会在原地叠成一大块。
            if trail_on && spd > 1e-3 {
                let span = res.config.note_trail_len_ratio() * 0.35;
                let base_a = res.config.note_trail_alpha_value() * color.a;
                if span > 0. && base_a > 0.004 {
                    // 残影数量可调（每个音符，最多 255）：数量越多拖影越连续
                    let ghosts = res.config.note_trail_count_value() as usize;
                    for i in 1..=ghosts {
                        let k = i as f32 / ghosts as f32;
                        let mut c = color;
                        // 越远越淡
                        c.a = base_a * (1. - k) * (1. - k) * 1.4;
                        if c.a <= 0.004 {
                            continue;
                        }
                        let off = span * k;
                        res.with_model(self.now_transform(res, ctrl_obj, base as f32 + off, config.incline_sin), |res| {
                            draw_center(res, tex, order, scale, c);
                        });
                    }
                }
            }
            res.with_model(self.now_transform(res, ctrl_obj, base as f32, config.incline_sin), |res| {
                draw_center(res, tex, order, scale, color);
            });
        };
        match self.kind {
            NoteKind::Click => {
                draw(res, *style.click);
            }
            NoteKind::Hold { end_time, end_height } => {
                res.with_model(self.now_transform(res, ctrl_obj, 0., 0.), |res| {
                    let style = if res.config.double_hint && self.multiple_hint {
                        &res.res_pack.note_style_mh
                    } else {
                        &res.res_pack.note_style
                    };
                    if matches!(self.judge, JudgeStatus::Judged) {
                        // miss
                        color.a *= 0.5;
                    }
                    if res.time >= end_time {
                        return;
                    }
                    let end_height = end_height / res.aspect_ratio as f64 * spd;
                    color.a *= mod_alpha as f32;

                    let h = if self.time <= res.time { line_height } else { height };
                    let flow = res.note_flow_speed as f64;
                    let bottom = ((h - line_height) * flow) as f32;
                    let top = ((end_height - line_height) * flow) as f32;
                    let tex = &style.hold;
                    let ratio = style.hold_ratio();
                    // body
                    // TODO (end_height - height) is not always total height
                    draw_tex(
                        res,
                        **(if res.res_pack.info.hold_repeat {
                            style.hold_body.as_ref().unwrap()
                        } else {
                            tex
                        }),
                        order,
                        -scale,
                        bottom,
                        color,
                        DrawTextureParams {
                            source: Some({
                                if res.res_pack.info.hold_repeat {
                                    let hold_body = style.hold_body.as_ref().unwrap();
                                    let width = hold_body.width();
                                    let height = hold_body.height();
                                    Rect::new(0., 0., 1., (top - bottom) / scale / 2. * width / height)
                                } else {
                                    style.hold_body_rect()
                                }
                            }),
                            dest_size: Some(vec2(scale * 2., top - bottom)),
                            ..Default::default()
                        },
                        false,
                    );
                    // head
                    if res.time < self.time || res.res_pack.info.hold_keep_head {
                        let r = style.hold_head_rect();
                        let hf = vec2(scale, r.h / r.w * scale * ratio);
                        draw_tex(
                            res,
                            **tex,
                            order,
                            -scale,
                            bottom - if res.res_pack.info.hold_compact { hf.y } else { hf.y * 2. },
                            color,
                            DrawTextureParams {
                                source: Some(r),
                                dest_size: Some(hf * 2.),
                                ..Default::default()
                            },
                            false,
                        );
                    }
                    // tail
                    let r = style.hold_tail_rect();
                    let hf = vec2(scale, r.h / r.w * scale * ratio);
                    draw_tex(
                        res,
                        **tex,
                        order,
                        -scale,
                        top - if res.res_pack.info.hold_compact { hf.y } else { 0. },
                        color,
                        DrawTextureParams {
                            source: Some(r),
                            dest_size: Some(hf * 2.),
                            ..Default::default()
                        },
                        false,
                    );
                });
            }
            NoteKind::Flick => {
                draw(res, *style.flick);
            }
            NoteKind::Drag => {
                draw(res, *style.drag);
            }
        }
    }
}

pub struct BadNote {
    pub time: f64,
    pub kind: NoteKind,
    pub matrix: Matrix,
}

impl BadNote {
    pub fn render(&self, res: &mut Resource) -> bool {
        if res.time > self.time + BAD_TIME {
            return false;
        }
        res.with_model(self.matrix, |res| {
            let style = &res.res_pack.note_style;
            draw_center(
                res,
                match &self.kind {
                    NoteKind::Click => *style.click,
                    NoteKind::Drag => *style.drag,
                    NoteKind::Flick => *style.flick,
                    _ => unreachable!(),
                },
                self.kind.order(),
                res.note_width,
                Color::new(0.423529, 0.262745, 0.262745, ((self.time - res.time).max(-1.) / BAD_TIME + 1.) as f32),
            );
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Anim, Keyframe, TweenId};

    /// `TWEEN_FUNCTIONS[2] = |x| x`（恒等 / 线性）。用别的 id 会被缓动函数扭曲，
    /// 断言里就得写缓动后的数，反而看不清是在测什么。
    const TWEEN_LINEAR: TweenId = 2;

    /// 判定线高度动画：t = 0 → 0，t = 1 → 100，线性 —— 查 t 处就是 `100t`。
    fn line_height() -> AnimFloat {
        Anim::new(vec![Keyframe::new(0., 0., TWEEN_LINEAR), Keyframe::new(1., 100., TWEEN_LINEAR)])
    }

    fn note(kind: NoteKind, time: f64) -> Note {
        Note {
            object: Object::default(),
            hitsound: HitSound::default_from_kind(&kind),
            kind,
            time,
            height: 0.,
            speed: 1.,
            color: WHITE,
            fx_color: None,
            judge_area: 1.,
            above: true,
            multiple_hint: false,
            fake: false,
            judge: JudgeStatus::NotJudged,
        }
    }

    #[test]
    fn keep_and_same_kind_are_no_ops() {
        let mut h = line_height();
        let mut n = note(NoteKind::Click, 1.);
        assert!(!n.apply_map(NoteMapTarget::Keep, 0.2, &mut h));
        // 目标类型和当前类型相同：不动
        assert!(!n.apply_map(NoteMapTarget::Tap, 0.2, &mut h));
        assert!(matches!(n.kind, NoteKind::Click));
    }

    #[test]
    fn converts_between_tap_drag_flick() {
        let mut h = line_height();
        let mut n = note(NoteKind::Click, 1.);
        assert!(n.apply_map(NoteMapTarget::Flick, 0.2, &mut h));
        assert!(matches!(n.kind, NoteKind::Flick));
        // 内建打击音效跟着类型走
        assert!(matches!(n.hitsound, HitSound::Flick));

        assert!(n.apply_map(NoteMapTarget::Drag, 0.2, &mut h));
        assert!(matches!(n.kind, NoteKind::Drag));
        assert!(matches!(n.hitsound, HitSound::Drag));
    }

    #[test]
    fn hold_to_tap_drops_the_tail() {
        let mut h = line_height();
        let mut n = note(
            NoteKind::Hold {
                end_time: 5.,
                end_height: 9.,
            },
            1.,
        );
        assert!(n.apply_map(NoteMapTarget::Tap, 0.2, &mut h));
        assert!(matches!(n.kind, NoteKind::Click));
        assert!(matches!(n.hitsound, HitSound::Click));
    }

    #[test]
    fn tap_to_hold_uses_line_height_at_end_time() {
        let mut h = line_height();
        let mut n = note(NoteKind::Click, 0.3);
        assert!(n.apply_map(NoteMapTarget::Hold, 0.2, &mut h));
        match n.kind {
            NoteKind::Hold { end_time, end_height } => {
                // end_time = 0.3 + 0.2
                assert!((end_time - 0.5).abs() < 1e-9, "end_time = {end_time}");
                // 判定线在 0.5 处的高度 = 100 * 0.5；直接用音符自己的高度会让长条长度变 0
                assert!((end_height - 50.).abs() < 1e-4, "end_height = {end_height}");
            }
            _ => panic!("应该变成 Hold"),
        }
    }

    #[test]
    fn custom_hitsound_is_preserved() {
        let mut h = line_height();
        let mut n = note(NoteKind::Click, 1.);
        n.hitsound = HitSound::Custom("custom.mp3".to_owned());
        assert!(n.apply_map(NoteMapTarget::Drag, 0.2, &mut h));
        assert!(matches!(n.hitsound, HitSound::Custom(ref s) if s == "custom.mp3"));
    }
}
