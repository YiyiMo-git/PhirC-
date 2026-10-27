//! UI utilities.
prpr_l10n::tl_file!("scene" ttl);
mod billboard;
pub use billboard::{BillBoard, Message, MessageHandle, MessageKind};

mod chart_info;
pub use chart_info::*;

mod dialog;
pub use dialog::Dialog;

mod scroll;
use inputbox::{InputBox, InputMode};
pub use scroll::*;

mod shading;
pub use shading::*;

mod shadow;
pub use shadow::*;

mod text;
pub use text::{DrawText, TextPainter};

pub mod theme;

pub use glyph_brush::ab_glyph::FontArc;

use crate::{
    core::{Matrix, Point, Vector},
    ext::{get_viewport, nalgebra_to_glm, semi_black, semi_white, source_of_image, RectExt, SafeTexture, ScaleType},
    judge::Judge,
    scene::{request_input, return_input, show_error, take_input},
};
use core::f32;
use lyon::{
    lyon_tessellation::{
        BuffersBuilder, FillOptions, FillTessellator, FillVertex, FillVertexConstructor, StrokeOptions, StrokeTessellator, StrokeVertex,
        StrokeVertexConstructor, VertexBuffers,
    },
    math as lm,
    path::{LineCap, Path, PathEvent},
};
use macroquad::prelude::*;
use miniquad::PassAction;
use sasa::{AudioManager, PlaySfxParams, Sfx};
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
    ops::{Deref, DerefMut, Range},
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

pub static PREFER_REDUCED_MOTION: AtomicBool = AtomicBool::new(false);
pub static UI_SFX_VOLUME: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

thread_local! {
    /// 滑块是否画成 Phigros 的**斜切轨道 + 斜切手柄**。
    ///
    /// 只影响观感，不影响拖拽命中逻辑（命中仍按未斜切的矩形算，斜切只差几像素边角）。
    /// 由设置页渲染期间置位，渲染完复位；其它页面（如铺面偏移）保持原来的圆头轨道。
    static SKEW_SLIDERS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 设置 / 复位滑块的斜切样式（见 [`SKEW_SLIDERS`]）。
pub fn set_skew_sliders(v: bool) {
    SKEW_SLIDERS.with(|c| c.set(v));
}

/// 平行四边形路径（斜切轨道 / 手柄用）。
fn skew_path_of(r: Rect, skew: f32) -> Path {
    let d = skew * 0.5;
    let mut p = Path::builder();
    p.begin(lm::point(r.x + d, r.y));
    p.line_to(lm::point(r.right() + d, r.y));
    p.line_to(lm::point(r.right() - d, r.bottom()));
    p.line_to(lm::point(r.x - d, r.bottom()));
    p.end(true);
    p.build()
}

#[derive(Default, Clone, Copy)]
pub struct Gravity(u8);

impl Gravity {
    pub const LEFT: u8 = 0;
    pub const HCENTER: u8 = 1;
    pub const RIGHT: u8 = 2;
    pub const TOP: u8 = 0;
    pub const VCENTER: u8 = 4;
    pub const BOTTOM: u8 = 8;

    pub const BEGIN: u8 = Self::LEFT | Self::TOP;
    pub const CENTER: u8 = Self::HCENTER | Self::VCENTER;
    pub const END: u8 = Self::RIGHT | Self::BOTTOM;

    fn value(mode: u8) -> f32 {
        match mode {
            0 => 0.,
            1 => 0.5,
            2 => 1.,
            _ => unreachable!(),
        }
    }

    pub fn offset(&self, total: (f32, f32), content: (f32, f32)) -> (f32, f32) {
        (Self::value(self.0 & 3) * (total.0 - content.0), Self::value((self.0 >> 2) & 3) * (total.1 - content.1))
    }

    pub fn from_point(&self, point: (f32, f32), content: (f32, f32)) -> (f32, f32) {
        (point.0 - content.0 * Self::value(self.0 & 3), point.1 - content.1 * Self::value((self.0 >> 2) & 3))
    }
}

impl From<u8> for Gravity {
    fn from(val: u8) -> Self {
        Self(val)
    }
}

struct ShadedConstructor<T: Shading>(Matrix, pub T, f32);
impl<T: Shading> FillVertexConstructor<Vertex> for ShadedConstructor<T> {
    fn new_vertex(&mut self, vertex: FillVertex) -> Vertex {
        let pos = vertex.position();
        self.1.new_vertex(&self.0, &Point::new(pos.x, pos.y), self.2)
    }
}
impl<T: Shading> StrokeVertexConstructor<Vertex> for ShadedConstructor<T> {
    fn new_vertex(&mut self, vertex: StrokeVertex) -> Vertex {
        let pos = vertex.position();
        self.1.new_vertex(&self.0, &Point::new(pos.x, pos.y), self.2)
    }
}

pub struct VertexBuilder<T: Shading> {
    matrix: Matrix,
    vertices: Vec<Vertex>,
    indices: Vec<u16>,
    shading: T,
    alpha: f32,
}

impl<T: Shading> VertexBuilder<T> {
    fn new(matrix: Matrix, shading: T, alpha: f32) -> Self {
        Self {
            matrix,
            vertices: Vec::new(),
            indices: Vec::new(),
            shading,
            alpha,
        }
    }

    pub fn add(&mut self, x: f32, y: f32) {
        self.vertices.push(self.shading.new_vertex(&self.matrix, &Point::new(x, y), self.alpha));
    }

    pub fn triangle(&mut self, x: u16, y: u16, z: u16) {
        self.indices.push(x);
        self.indices.push(y);
        self.indices.push(z);
    }

    pub fn commit(&self) {
        let gl = unsafe { get_internal_gl() }.quad_gl;
        gl.texture(self.shading.texture());
        gl.draw_mode(DrawMode::Triangles);
        gl.geometry(&self.vertices, &self.indices);
    }
}

#[derive(Default)]
pub struct LongTouchState {
    start: Option<(Vec2, f32)>,
}
impl LongTouchState {
    pub fn reset(&mut self) {
        self.start = None;
    }
}

#[derive(Clone, Copy)]
pub struct RectButton {
    pts: Option<[Vec2; 4]>,
    id: Option<u64>,
}

impl Default for RectButton {
    fn default() -> Self {
        Self::new()
    }
}

impl RectButton {
    pub fn new() -> Self {
        Self { pts: None, id: None }
    }

    pub fn touching(&self) -> bool {
        self.id.is_some()
    }

    pub fn cancel(&mut self) {
        self.id = None;
    }

    pub fn contains(&self, pos: Vec2) -> bool {
        if let Some([a, b, c, d]) = self.pts {
            let abp = (b - a).perp_dot(pos - a);
            let bcp = (c - b).perp_dot(pos - b);
            let cdp = (d - c).perp_dot(pos - c);
            let dap = (a - d).perp_dot(pos - d);
            (abp >= 0. && bcp >= 0. && cdp >= 0. && dap >= 0.) || (abp <= 0. && bcp <= 0. && cdp <= 0. && dap <= 0.)
        } else {
            false
        }
    }

    pub fn set(&mut self, ui: &mut Ui, rect: Rect) {
        let mat = nalgebra_to_glm(&ui.transform) * ui.gl_transform;
        let tr = |x: f32, y: f32| {
            let pos = mat * vec4(x, y, 0., 1.);
            pos.xy() / pos.w
        };
        self.pts = Some([
            tr(rect.x, rect.y),
            tr(rect.right(), rect.y),
            tr(rect.right(), rect.bottom()),
            tr(rect.x, rect.bottom()),
        ]);
    }

    pub fn touch(&mut self, touch: &Touch) -> bool {
        let inside = self.contains(touch.position);
        match touch.phase {
            TouchPhase::Started => {
                if inside {
                    self.id = Some(touch.id);
                }
            }
            TouchPhase::Moved | TouchPhase::Stationary => {
                if self.id == Some(touch.id) && !inside {
                    self.id = None;
                }
            }
            TouchPhase::Cancelled => {
                self.id = None;
            }
            TouchPhase::Ended => {
                if self.id.take() == Some(touch.id) && inside {
                    return true;
                }
            }
        }
        false
    }

    pub fn long_touch(&mut self, touch: &Touch, t: f32, state: &mut LongTouchState) -> bool {
        match touch.phase {
            TouchPhase::Started => {
                if self.id == Some(touch.id) {
                    state.start = Some((touch.position, t));
                }
            }
            TouchPhase::Moved | TouchPhase::Stationary => {
                if self.id == Some(touch.id) {
                    if let Some((start_pos, start_time)) = state.start {
                        if (touch.position - start_pos).length() > 0.02 {
                            state.reset();
                        } else if t > start_time + 0.5 {
                            state.reset();
                            return true;
                        }
                    }
                }
            }
            TouchPhase::Cancelled => {
                if self.id == Some(touch.id) {
                    state.reset();
                }
            }
            TouchPhase::Ended => {
                if self.id.take() == Some(touch.id) {
                    state.reset();
                }
            }
        }
        false
    }

    pub fn update_long_touch(&self, t: f32, state: &mut LongTouchState) -> bool {
        if self.id.is_some() {
            if let Some((_, start_time)) = state.start {
                if t > start_time + 0.5 {
                    state.reset();
                    return true;
                }
            }
        }
        false
    }
}

#[derive(Clone)]
pub struct DRectButton {
    pub inner: RectButton,
    last_touching: bool,
    start_time: Option<f32>,
    pub config: ShadowConfig,
    delta: f32,
    play_sound: bool,
    /// 悬停进度（0~1，缓动跟上）：kaichang 玻璃按钮的「提亮 + 轻微上浮」。
    hover: f32,
}
impl Default for DRectButton {
    fn default() -> Self {
        Self::new()
    }
}

/// 鼠标指针在 UI 世界坐标里的位置（与 `Touch::position` 同一套换算）。
///
/// 换算方式抄自 `newui::mouse_world` / `judge::Judge::touch_transform`：
/// 那是「屏幕像素 → 世界坐标」唯一的权威映射，自己推容易把 y 的正负号搞反。
fn mouse_world_pt() -> Option<Vec2> {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        return None; // 触屏设备没有「悬停」这回事
    }
    let (mx, my) = mouse_position();
    if mx == 0. && my == 0. {
        return None; // 指针不在窗口内（GLFW 把窗外位置报成 0,0）
    }
    let vp = crate::ext::get_viewport();
    let (vw, vh) = ((vp.2.max(1)) as f32, (vp.3.max(1)) as f32);
    Some(vec2(
        (mx - vp.0 as f32) / vw * 2. - 1.,
        ((my - (screen_height() - (vp.1 + vp.3) as f32)) / vh * 2. - 1.) / (vw / vh),
    ))
}

/// 指针是否悬停在这个按钮上（`RectButton` 的命中区已经是世界坐标，直接比）。
fn hovering(btn: &RectButton) -> bool {
    mouse_world_pt().is_some_and(|p| btn.contains(p))
}

/// **kaichang `.glass-btn` 底板**：柔和阴影 + 磨砂底板 + 顶部高光 + 1px 浅边。
///
/// 全 App 的 `DRectButton` / `Ui::button` 都走这里 —— 圆角、描边、悬停提亮
/// 从此只有一处定义（和开始界面的 `glass_plate` 同一套配方）。
fn glass_plate(ui: &mut Ui, r: Rect, radius: f32, hp: f32, chosen: bool, tint: Option<Color>) {
    let radius = radius.min(r.h * 0.45).min(r.w * 0.45).max(0.0005);
    // 柔和阴影（`box-shadow: 0 4px 18px rgba(0,0,0,.12)` 的对应物）
    rounded_rect_shadow(
        ui,
        r,
        &ShadowConfig {
            elevation: 0.0055 * (1. + 0.8 * hp),
            radius,
            base: theme::glass::shadow(hp),
        },
    );
    let path = r.rounded(radius);
    ui.fill_path(
        &path,
        if chosen {
            theme::glass::plate_accent(hp)
        } else {
            theme::glass::plate(hp)
        },
    );
    if let Some(c) = tint {
        ui.fill_path(
            &path,
            Color {
                a: (c.a * (0.8 + 0.4 * hp)).min(1.),
                ..c
            },
        );
    }
    // 顶部玻璃反光
    let hl = Rect::new(r.x + radius * 0.6, r.y + 0.0012, (r.w - radius * 1.2).max(0.), 0.0012);
    ui.fill_rect(hl, theme::glass::highlight(hp));
    ui.stroke_path(
        &path,
        0.0013,
        if chosen {
            theme::glass::border_accent(hp)
        } else {
            theme::glass::border(hp)
        },
    );
}

/// 统一的玻璃圆角（约 12px @1920 —— 开始界面按钮是 14px，列表行矮一档）。
const GLASS_RADIUS: f32 = 0.012;
/// 悬停时的上浮量（世界坐标）。
const GLASS_LIFT: f32 = 0.0035;

impl DRectButton {
    pub const TIME: f32 = 0.2;

    pub fn new() -> Self {
        Self {
            inner: RectButton::new(),
            last_touching: false,
            start_time: None,
            config: ShadowConfig::default(),
            delta: -0.006,
            play_sound: true,
            hover: 0.,
        }
    }

    /// 推进悬停进度（指数缓动，与帧率无关），返回当前值。
    fn step_hover(&mut self) -> f32 {
        let target = if hovering(&self.inner) { 1. } else { 0. };
        let k = 1. - (-get_frame_time() * 18.).exp();
        self.hover += (target - self.hover) * k;
        if (target - self.hover).abs() < 0.002 {
            self.hover = target;
        }
        self.hover
    }

    pub fn build(&mut self, ui: &mut Ui, t: f32, r: Rect, f: impl FnOnce(&mut Ui, Path)) {
        self.inner.set(ui, r);
        // let r = r.feather((1. - self.progress(t)) * self.delta);
        let ct = r.center();
        let ct = Vector::new(ct.x, ct.y);
        if PREFER_REDUCED_MOTION.load(Ordering::Relaxed) {
            f(ui, r.rounded(self.config.radius));
            return;
        }
        ui.with(
            Matrix::new_translation(&-ct)
                .append_scaling(1. - (1. - self.progress(t)) * 0.04)
                .append_translation(&ct),
            |ui| {
                f(ui, r.rounded(self.config.radius));
            },
        );
    }

    pub fn invalidate(&mut self) {
        self.inner.pts = None;
    }

    pub fn render_shadow(&mut self, ui: &mut Ui, r: Rect, t: f32, f: impl FnOnce(&mut Ui, Path)) {
        let p = self.progress(t);
        let config = ShadowConfig {
            elevation: self.config.elevation * p,
            radius: self.config.radius,
            ..self.config
        };
        ui.scope(|ui| {
            ui.dy((1. - p) * 0.004);
            self.build(ui, t, r, |ui, path| {
                rounded_rect_shadow(ui, r, &config);
                f(ui, path);
            });
        });
    }

    pub fn render_text<'a>(&mut self, ui: &mut Ui, r: Rect, t: f32, text: impl Into<Cow<'a, str>>, size: f32, chosen: bool) {
        let oh = r.h;
        let hp = self.step_hover();
        let radius = self.config.radius.max(GLASS_RADIUS);
        self.build(ui, t, r, |ui, _path| {
            ui.dy(-hp * GLASS_LIFT);
            // 磨砂玻璃底板（悬停提亮 + 上浮；选中态 = 强调色底白字）
            glass_plate(ui, r, radius, hp, chosen, None);
            let ct = r.center();
            ui.text(text)
                .pos(ct.x, ct.y)
                .anchor(0.5, 0.5)
                .no_baseline()
                .size(size * (1. - (1. - r.h / oh).powf(1.3)))
                .max_width(r.w)
                .color(if chosen { Color::new(1., 1., 1., 1.) } else { theme::ink(0.66 + 0.32 * hp) })
                .draw();
        });
    }

    /// `render_text` 的带色版本：按钮文字用指定颜色（黑名单页的「解除」用红色）。
    #[allow(clippy::too_many_arguments)]
    pub fn render_text_color<'a>(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        t: f32,
        text: impl Into<Cow<'a, str>>,
        size: f32,
        chosen: bool,
        color: Color,
    ) {
        let oh = r.h;
        let hp = self.step_hover();
        let radius = self.config.radius.max(GLASS_RADIUS);
        self.build(ui, t, r, |ui, _path| {
            ui.dy(-hp * GLASS_LIFT);
            glass_plate(ui, r, radius, hp, chosen, None);
            let ct = r.center();
            ui.text(text)
                .pos(ct.x, ct.y)
                .anchor(0.5, 0.5)
                .no_baseline()
                .size(size * (1. - (1. - r.h / oh).powf(1.3)))
                .max_width(r.w)
                .color(Color {
                    a: (color.a * (0.85 + 0.15 * hp)).min(1.),
                    ..color
                })
                .draw();
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_text_left<'a>(&mut self, ui: &mut Ui, r: Rect, t: f32, alpha: f32, text: impl Into<Cow<'a, str>>, size: f32, chosen: bool) {
        let oh = r.h;
        let hp = self.step_hover();
        let radius = self.config.radius.max(GLASS_RADIUS);
        self.build(ui, t, r, |ui, _path| {
            ui.dy(-hp * GLASS_LIFT);
            // 玻璃行卡：选中 = 强调色底 + 白字；未选中 = 磨砂玻璃 + 主题墨色
            glass_plate(ui, r, radius, hp, chosen, None);
            ui.text(text)
                .pos(r.x + 0.02, r.center().y)
                .anchor(0., 0.5)
                .max_width(r.w - 0.04)
                .no_baseline()
                .size(size * r.h / oh)
                .color(if chosen {
                    Color::new(1., 1., 1., alpha)
                } else {
                    theme::ink(alpha * (0.72 + 0.28 * hp))
                })
                .draw();
        });
    }

    #[inline]
    pub fn render_input<'a>(&mut self, ui: &mut Ui, r: Rect, t: f32, text: impl Into<Cow<'a, str>>, hint: impl Into<Cow<'a, str>>, size: f32) {
        let text = text.into();
        if text.trim().is_empty() {
            self.render_text_left(ui, r, t, 0.7, hint, size, false);
        } else {
            self.render_text_left(ui, r, t, 1., text, size, false);
        }
    }

    #[inline]
    pub fn no_sound(mut self) -> Self {
        self.play_sound = false;
        self
    }

    #[inline]
    pub fn with_radius(mut self, radius: f32) -> Self {
        self.config.radius = radius;
        self
    }

    #[inline]
    pub fn with_elevation(mut self, elevation: f32) -> Self {
        self.config.elevation = elevation;
        self
    }

    #[inline]
    pub fn with_base(mut self, base: f32) -> Self {
        self.config.base = base;
        self
    }

    #[inline]
    pub fn with_delta(mut self, delta: f32) -> Self {
        self.delta = delta;
        self
    }

    pub fn progress(&mut self, t: f32) -> f32 {
        if self.start_time.as_ref().is_some_and(|it| t > *it + Self::TIME) || PREFER_REDUCED_MOTION.load(Ordering::Relaxed) {
            self.start_time = None;
        }
        let p = if let Some(time) = &self.start_time {
            (t - time) / Self::TIME
        } else {
            1.
        };
        if self.last_touching {
            1. - p
        } else {
            p
        }
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> bool {
        let res = self.inner.touch(touch);
        let touching = self.inner.touching();
        if self.last_touching != touching {
            self.last_touching = touching;
            self.start_time = Some(t);
        }
        if res && self.play_sound {
            button_hit();
        }
        res
    }

    pub fn long_touch(&mut self, touch: &Touch, t: f32, state: &mut LongTouchState) -> bool {
        if self.inner.long_touch(touch, t, state) {
            self.last_touching = false;
            self.start_time = Some(t);
            if self.play_sound {
                button_hit();
            }
            true
        } else {
            false
        }
    }

    pub fn update_long_touch(&mut self, t: f32, state: &mut LongTouchState) -> bool {
        if self.inner.update_long_touch(t, state) {
            self.last_touching = false;
            self.start_time = Some(t);
            if self.play_sound {
                button_hit();
            }
            true
        } else {
            false
        }
    }
}

pub struct Slider {
    range: Range<f32>,
    step: f32,

    btn_dec: DRectButton,
    btn_inc: DRectButton,

    touch: Option<(u64, f32, bool)>,
    rect: Rect,
    pos: f32,
}

impl Slider {
    const RADIUS: f32 = 0.028;
    const THRESHOLD: f32 = 0.05;

    pub fn new(range: Range<f32>, step: f32) -> Self {
        Self {
            range,
            step,

            btn_dec: DRectButton::new().with_delta(-0.002),
            btn_inc: DRectButton::new().with_delta(-0.002),

            touch: None,
            rect: Rect::default(),
            pos: <f32>::INFINITY,
        }
    }

    pub fn touch(&mut self, touch: &Touch, t: f32, dst: &mut f32) -> Option<bool> {
        if self.btn_dec.touch(touch, t) {
            *dst = (*dst - self.step).max(self.range.start);
            return Some(true);
        }
        if self.btn_inc.touch(touch, t) {
            *dst = (*dst + self.step).min(self.range.end);
            return Some(true);
        }
        if let Some((id, start_pos, unlocked)) = &mut self.touch {
            if touch.id == *id {
                match touch.phase {
                    TouchPhase::Started | TouchPhase::Moved | TouchPhase::Stationary => {
                        if (touch.position.x - *start_pos).abs() >= Self::THRESHOLD {
                            *unlocked = true;
                        }
                        if *unlocked {
                            let p = (touch.position.x - self.rect.x) / self.rect.w;
                            let p = p.clamp(0., 1.);
                            let p = self.range.start + (self.range.end - self.range.start) * p;
                            *dst = (p / self.step).round() * self.step;
                            return Some(true);
                        }
                    }
                    TouchPhase::Cancelled | TouchPhase::Ended => {
                        self.touch = None;
                    }
                }
                return Some(false);
            }
        } else if touch.phase == TouchPhase::Started {
            let pos = (self.pos, self.rect.center().y);
            if (touch.position.x - pos.0).hypot(touch.position.y - pos.1) <= Self::RADIUS {
                self.touch = Some((touch.id, touch.position.x, false));
                return Some(false);
            }
        }
        None
    }

    pub fn render(&mut self, ui: &mut Ui, mut r: Rect, t: f32, p: f32, text: String) {
        r.x -= 0.1;
        r.x -= r.w * 0.2;
        r.w *= 1.2;
        let pad = 0.04;
        let size = 0.026;
        let cy = r.center().y;
        self.btn_dec
            .render_text(ui, Rect::new(r.x - pad - size, cy, 0., 0.).feather(size), t, "-", 0.7, true);
        self.btn_inc
            .render_text(ui, Rect::new(r.right() + pad + size, cy, 0., 0.).feather(size), t, "+", 0.7, true);
        self.rect = ui.rect_to_global(r);
        ui.text(text)
            .pos(r.x - (pad + size) * 2., cy)
            .anchor(1., 0.5)
            .no_baseline()
            .size(0.6)
            .draw();
        let p = (p - self.range.start) / (self.range.end - self.range.start);
        let pos = (r.x + r.w * p, cy);
        self.pos = ui.to_global(pos).0;
        if SKEW_SLIDERS.with(|c| c.get()) {
            // Phigros 版式：斜切轨道（未填充 = 暗墨色 / 已填充 = 白）+ 斜切白色手柄。
            let h = 0.026;
            let sk = h * 0.36;
            ui.fill_path(&skew_path_of(Rect::new(r.x, cy - h * 0.5, r.w, h), sk), Color { a: 0.28, ..theme::ink(1.) });
            if p > 0.002 {
                ui.fill_path(&skew_path_of(Rect::new(r.x, cy - h * 0.5, r.w * p, h), sk), WHITE);
            }
            let hs = 0.042;
            ui.fill_path(&skew_path_of(Rect::new(pos.0 - hs * 0.5, cy - hs * 0.5, hs, hs), hs * 0.36), WHITE);
            return;
        }
        use lyon::math::point;
        ui.stroke_options = ui.stroke_options.with_line_cap(LineCap::Round);
        // 轨道统一：**已填充 = 强调色，未填充 = 主题墨色的低透明度**。
        // 原来是「已填充 = 面板白 / 未填充 = 墨色 80%」——浅色主题下画出来就是
        // 「白条 + 一条粗黑杠」，滑块看着像坏了。
        let ink = theme::ink(1.);
        ui.stroke_path(
            &{
                let mut p = Path::builder();
                p.begin(point(r.x, cy));
                p.line_to(point(pos.0, cy));
                p.end(false);
                p.build()
            },
            0.02,
            theme::accent(),
        );
        ui.stroke_path(
            &{
                let mut p = Path::builder();
                p.begin(point(pos.0, cy));
                p.line_to(point(r.right(), cy));
                p.end(false);
                p.build()
            },
            0.02,
            Color { a: 0.16, ..ink },
        );
        ui.stroke_options = ui.stroke_options.with_line_cap(LineCap::Square);
        rounded_rect_shadow(
            ui,
            Rect::new(pos.0, pos.1, 0., 0.).feather(Self::RADIUS),
            &ShadowConfig {
                radius: Self::RADIUS,
                base: 0.7,
                ..Default::default()
            },
        );
        ui.fill_circle(pos.0, pos.1, Self::RADIUS, WHITE);
    }
}

thread_local! {
    static STATE: RefCell<HashMap<String, Option<u64>>> = RefCell::new(HashMap::new());
}

pub struct InputParams<'a> {
    pub changed: Option<&'a mut bool>,
    pub mode: InputMode,
    pub length: f32,
}

impl From<()> for InputParams<'_> {
    fn from(_: ()) -> Self {
        Self {
            changed: None,
            mode: InputMode::Text,
            length: 0.3,
        }
    }
}

impl From<InputMode> for InputParams<'_> {
    fn from(mode: InputMode) -> Self {
        Self { mode, ..().into() }
    }
}

impl From<f32> for InputParams<'_> {
    fn from(length: f32) -> Self {
        Self { length, ..().into() }
    }
}

impl<'a> From<(f32, &'a mut bool)> for InputParams<'a> {
    fn from((length, changed): (f32, &'a mut bool)) -> Self {
        Self {
            changed: Some(changed),
            mode: InputMode::Text,
            length,
        }
    }
}

pub struct Ui<'a> {
    pub top: f32,
    pub viewport: (i32, i32, i32, i32),

    pub text_painter: &'a mut TextPainter,

    pub transform: Matrix,
    pub gl_transform: Mat4,
    scissor: Option<(i32, i32, i32, i32)>,
    touches: Option<Vec<Touch>>,

    vertex_buffers: VertexBuffers<Vertex, u16>,
    fill_tess: FillTessellator,
    fill_options: FillOptions,
    stroke_tess: StrokeTessellator,
    pub stroke_options: StrokeOptions,

    pub alpha: f32,
}

impl<'a> Ui<'a> {
    pub fn new(text_painter: &'a mut TextPainter, viewport: Option<(i32, i32, i32, i32)>) -> Self {
        unsafe { get_internal_gl() }.quad_context.begin_default_pass(PassAction::Clear {
            depth: None,
            stencil: Some(0),
            color: None,
        });
        let viewport = viewport.unwrap_or_else(|| (0, 0, screen_width() as i32, screen_height() as i32));
        Self {
            top: viewport.3 as f32 / viewport.2 as f32,
            viewport,

            text_painter,

            transform: Matrix::identity(),
            gl_transform: Mat4::IDENTITY,
            scissor: None,
            touches: None,

            vertex_buffers: VertexBuffers::new(),
            fill_tess: FillTessellator::new(),
            fill_options: FillOptions::default(),
            stroke_tess: StrokeTessellator::new(),
            stroke_options: StrokeOptions::default(),

            alpha: 1.,
        }
    }

    pub fn camera(&self) -> Camera2D {
        Camera2D {
            zoom: vec2(1., -self.viewport.2 as f32 / self.viewport.3 as f32),
            viewport: Some(self.viewport),
            ..Default::default()
        }
    }

    pub fn ensure_touches(&mut self) -> &mut Vec<Touch> {
        if self.touches.is_none() {
            self.touches = Some(Judge::get_touches());
        }
        self.touches.as_mut().unwrap()
    }

    pub(crate) fn set_touches(&mut self, touches: Vec<Touch>) {
        self.touches = Some(touches);
    }

    /// 本帧的触摸列表（由 `Main::render` 每帧通过 [`Self::set_touches`] 写入）。
    ///
    /// 给「画在引擎层、但要自己判定命中」的界面用 —— 例如 `phira` 的自绘输入框。
    /// ⚠️ 别再用 `Judge::get_touches()`：那只在**游戏内**有数据，主菜单/设置/曲库里
    /// 恒为空，会导致输入框的确定/取消按钮**永远点不到**（弹窗卡死 → 模态守卫把
    /// 全界面的点击都吃掉）。帧内没有触摸时返回空切片。
    pub fn touches(&self) -> &[Touch] {
        self.touches.as_deref().unwrap_or(&[])
    }

    pub fn builder<T: IntoShading>(&self, shading: T) -> VertexBuilder<T::Target> {
        VertexBuilder::new(self.transform, shading.into_shading(), self.alpha)
    }

    pub fn fill_rect(&mut self, rect: Rect, shading: impl IntoShading) {
        let mut b = self.builder(shading);
        b.add(rect.x, rect.y);
        b.add(rect.x + rect.w, rect.y);
        b.add(rect.x, rect.y + rect.h);
        b.add(rect.x + rect.w, rect.y + rect.h);
        b.triangle(0, 1, 2);
        b.triangle(1, 2, 3);
        b.commit();
    }

    fn set_tolerance(&mut self) {
        let tol = 0.15 / (self.transform.transform_vector(&Vector::new(1., 0.)).norm() * screen_width() / 2.);
        self.fill_options.tolerance = tol;
        self.stroke_options.tolerance = tol;
    }

    fn draw_lyon<T: Shading>(&mut self, shading: T, f: impl FnOnce(&mut Self, ShadedConstructor<T>)) {
        self.set_tolerance();
        let shaded = ShadedConstructor(self.transform, shading.into_shading(), self.alpha);
        let tex = shaded.1.texture();
        f(self, shaded);
        self.emit_lyon(tex);
    }

    pub fn fill_path(&mut self, path: impl IntoIterator<Item = PathEvent>, shading: impl IntoShading) {
        self.draw_lyon(shading.into_shading(), |this, shaded| {
            this.fill_tess
                .tessellate(path, &this.fill_options, &mut BuffersBuilder::new(&mut this.vertex_buffers, shaded))
                .unwrap();
        });
    }

    pub fn fill_circle(&mut self, x: f32, y: f32, radius: f32, shading: impl IntoShading) {
        self.draw_lyon(shading.into_shading(), |this, shaded| {
            this.fill_tess
                .tessellate_circle(lm::point(x, y), radius, &this.fill_options, &mut BuffersBuilder::new(&mut this.vertex_buffers, shaded))
                .unwrap();
        });
    }

    pub fn stroke_circle(&mut self, x: f32, y: f32, radius: f32, width: f32, shading: impl IntoShading) {
        self.draw_lyon(shading.into_shading(), |this, shaded| {
            this.stroke_options.line_width = width;
            this.stroke_tess
                .tessellate_circle(lm::point(x, y), radius, &this.stroke_options, &mut BuffersBuilder::new(&mut this.vertex_buffers, shaded))
                .unwrap();
        });
    }

    pub fn stroke_path(&mut self, path: &Path, width: f32, shading: impl IntoShading) {
        self.draw_lyon(shading.into_shading(), |this, shaded| {
            this.stroke_options.line_width = width;
            this.stroke_tess
                .tessellate_path(path, &this.stroke_options, &mut BuffersBuilder::new(&mut this.vertex_buffers, shaded))
                .unwrap();
        });
    }

    fn emit_lyon(&mut self, texture: Option<Texture2D>) {
        let gl = unsafe { get_internal_gl() }.quad_gl;
        gl.texture(texture);
        gl.draw_mode(DrawMode::Triangles);
        gl.geometry(&std::mem::take(&mut self.vertex_buffers.vertices), &std::mem::take(&mut self.vertex_buffers.indices));
    }

    pub fn screen_rect(&self) -> Rect {
        Rect::new(-1., -self.top, 2., self.top * 2.)
    }

    pub fn dialog_rect() -> Rect {
        let hw = 0.45;
        let hh = 0.34;
        Rect::new(-hw, -hh, hw * 2., hh * 2.)
    }

    pub fn rect_to_global(&self, rect: Rect) -> Rect {
        let pt = self.to_global((rect.x, rect.y));
        let vec = self.vec_to_global((rect.w, rect.h));
        Rect::new(pt.0, pt.1, vec.0, vec.1)
    }

    pub fn vec_to_global(&self, vec: (f32, f32)) -> (f32, f32) {
        let r = self.transform.transform_vector(&Vector::new(vec.0, vec.1));
        (r.x, r.y)
    }

    pub fn to_global(&self, pt: (f32, f32)) -> (f32, f32) {
        let r = self.transform.transform_point(&Point::new(pt.0, pt.1));
        (r.x, r.y)
    }

    pub fn to_local(&self, pt: (f32, f32)) -> (f32, f32) {
        let r = self.transform.try_inverse().unwrap().transform_point(&Point::new(pt.0, pt.1));
        (r.x, r.y)
    }

    pub fn dx(&mut self, x: f32) {
        self.transform.append_translation_mut(&Vector::new(x, 0.));
    }

    pub fn dy(&mut self, y: f32) {
        self.transform.append_translation_mut(&Vector::new(0., y));
    }

    #[inline]
    pub fn alpha<R>(&mut self, alpha: f32, f: impl FnOnce(&mut Self) -> R) -> R {
        let old = self.alpha;
        self.alpha = old * alpha;
        let res = f(self);
        self.alpha = old;
        res
    }

    #[inline]
    pub fn with<R>(&mut self, transform: Matrix, f: impl FnOnce(&mut Self) -> R) -> R {
        let old = self.transform;
        self.transform = old * transform;
        let res = f(self);
        self.transform = old;
        res
    }

    #[inline]
    pub fn scope<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let old = self.transform;
        let res = f(self);
        self.transform = old;
        res
    }

    #[inline]
    pub fn abs_scope<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let old = self.transform;
        self.transform = Matrix::identity();
        let res = f(self);
        self.transform = old;
        res
    }

    #[inline]
    pub fn with_gl<R>(&mut self, transform: Mat4, f: impl FnOnce(&mut Self) -> R) -> R {
        let old = self.gl_transform;
        // self.gl_transform = old * transform;
        let gl = unsafe { get_internal_gl() }.quad_gl;
        gl.push_model_matrix(transform);
        let res = f(self);
        self.gl_transform = old;
        unsafe { get_internal_gl() }.flush();
        gl.pop_model_matrix();
        res
    }

    #[inline]
    pub fn apply<R>(&mut self, f: impl FnOnce(&mut Ui) -> R) -> R {
        unsafe { get_internal_gl() }.quad_gl.push_model_matrix(nalgebra_to_glm(&self.transform));
        let res = f(self);
        unsafe { get_internal_gl() }.quad_gl.pop_model_matrix();
        res
    }

    pub fn scissor<R>(&mut self, rect: Rect, f: impl FnOnce(&mut Ui) -> R) -> R {
        let igl = unsafe { get_internal_gl() };
        let gl = igl.quad_gl;
        let rect = self.rect_to_global(rect);
        let vp = get_viewport();
        let pt = (
            vp.0 as f32 + (rect.x + 1.) / 2. * vp.2 as f32,
            (screen_height() - (vp.1 + vp.3) as f32) + (rect.y * vp.2 as f32 / vp.3 as f32 + 1.) / 2. * vp.3 as f32,
        );

        let old = self.scissor;
        self.scissor = {
            let mut l = pt.0 as i32;
            let mut t = pt.1 as i32;
            let mut r = (pt.0 + rect.w * vp.2 as f32 / 2.) as i32;
            let mut b = (pt.1 + rect.h * vp.2 as f32 / 2.) as i32;
            if let Some((l0, t0, w0, h0)) = old {
                l = l.max(l0);
                t = t.max(t0);
                r = r.min(l0 + w0);
                b = b.min(t0 + h0);
            }
            Some((l, t, r - l, b - t))
        };

        gl.scissor(self.scissor);
        let res = f(self);
        self.scissor = old;
        gl.scissor(old);
        res
    }

    pub fn text<'s, 'ui>(&'ui mut self, text: impl Into<Cow<'s, str>>) -> DrawText<'a, 's, 'ui> {
        DrawText::new(self, text.into())
    }

    fn clicked(&mut self, rect: Rect, entry: &mut Option<u64>) -> bool {
        let rect = self.rect_to_global(rect);
        let mut exists = false;
        let mut any = false;
        let old_entry = *entry;
        let mut res = false;
        self.ensure_touches().retain(|touch| {
            exists = exists || old_entry == Some(touch.id);
            if !rect.contains(touch.position) {
                return true;
            }
            any = true;
            match touch.phase {
                TouchPhase::Started => {
                    *entry = Some(touch.id);
                    false
                }
                TouchPhase::Moved | TouchPhase::Stationary => {
                    if *entry != Some(touch.id) {
                        *entry = None;
                        true
                    } else {
                        false
                    }
                }
                TouchPhase::Cancelled => {
                    *entry = None;
                    true
                }
                TouchPhase::Ended => {
                    if entry.take() == Some(touch.id) {
                        res = true;
                        false
                    } else {
                        true
                    }
                }
            }
        });
        if res {
            return true;
        }
        if !any && exists {
            *entry = None;
        }
        false
    }

    /// 主题强调色。**只此一处定义**，见 `ui::theme`。
    pub fn accent(&self) -> Color {
        if theme::is_light() {
            theme::light::ACCENT
        } else {
            theme::color::ACCENT
        }
    }

    /// 面板/按钮底色。全 App 的面板都从这一处取色，见 `ui::theme`。
    pub fn background(&self) -> Color {
        if theme::is_light() {
            theme::light::SURFACE
        } else {
            theme::color::SURFACE
        }
    }

    /// 次级卡片 / 悬停 / 选中态的底色，比 [`Self::background`] 亮一档。
    pub fn surface_hi(&self) -> Color {
        if theme::is_light() {
            theme::light::SURFACE_HI
        } else {
            theme::color::SURFACE_HI
        }
    }

    pub fn button(&mut self, id: &str, rect: Rect, text: impl Into<String>) -> bool {
        let text = text.into();
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let entry = state.entry(id.to_owned()).or_default();
            // kaichang 玻璃按钮：悬停提亮 + 轻微上浮，按下时再压一层墨色
            let hover = mouse_world_pt().is_some_and(|p| self.rect_to_global(rect).contains(p));
            let pressed = entry.is_some();
            let hp = if hover { 1. } else { 0. };
            self.scope(|ui| {
                if hover && !pressed {
                    ui.dy(-GLASS_LIFT);
                }
                glass_plate(
                    ui,
                    rect,
                    GLASS_RADIUS,
                    hp,
                    false,
                    if pressed { Some(theme::ink(0.12)) } else { None },
                );
                let ct = rect.center();
                ui.text(text)
                    .pos(ct.x, ct.y)
                    .anchor(0.5, 0.5)
                    .max_width(rect.w)
                    .size(0.42)
                    // 文字色跟主题走：浅色主题下玻璃底偏白，硬编码白字会「白底白字」
                    .color(theme::ink(0.78 + 0.22 * hp))
                    .no_baseline()
                    .draw();
            });
            self.clicked(rect, entry)
        })
    }

    /// 胶囊开关（Pill Switch）：全局替代方形勾选框。
    ///
    /// 轨道 = 圆角胶囊，滑块 = 带投影的白色圆点；打开时轨道用强调色。
    /// 命中区是「标签 + 胶囊」整行，点文字或点胶囊都能切换。
    pub fn checkbox(&mut self, text: impl Into<String>, value: &mut bool) -> Rect {
        let text = text.into();
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let entry = state.entry(format!("chkbox#{text}")).or_default();
            let w = 0.08;
            let label = self.text(text).pos(w, 0.).size(0.47).no_baseline().color(theme::ink(1.)).draw();
            let ink = theme::ink(1.);
            let track_w = 0.062;
            let track_h = 0.031;
            let track = Rect::new(
                label.right() + 0.02,
                label.center().y - track_h * 0.5,
                track_w,
                track_h,
            );
            let radius = track_h * 0.5;
            self.fill_path(
                &track.rounded(radius),
                if *value { theme::color::ACCENT } else { Color { a: 0.16, ..ink } },
            );
            self.stroke_path(&track.rounded(radius), 0.0012, Color { a: 0.30, ..ink });
            let knob_r = (track_h * 0.5 - 0.004).max(0.0015);
            let cx = if *value { track.right() - track_h * 0.5 } else { track.x + track_h * 0.5 };
            let cy = track.center().y;
            self.fill_circle(cx + 0.0012, cy + 0.0018, knob_r, Color::new(0., 0., 0., 0.35));
            self.fill_circle(cx, cy, knob_r, WHITE);
            // 命中区覆盖整行（标签 + 胶囊）
            let r = Rect::new(w * 0.5, track.y.min(label.y), track.right() - w * 0.5, (label.bottom() - track.y.min(label.y)).max(track_h));
            if self.clicked(r, entry) {
                *value ^= true;
            }
            r
        })
    }

    pub fn input<'b>(&mut self, label: impl Into<String>, value: &mut String, params: impl Into<InputParams<'b>>) -> Rect {
        let label = label.into();
        let params = params.into();
        let id = format!("input#{label}");
        let r = self.text(label).anchor(1., 0.).size(0.47).draw();
        let lf = r.x;
        let r = Rect::new(0.02, r.y - 0.01, params.length, r.h + 0.02);
        if if params.mode == InputMode::Password {
            self.button(&id, r, "*".repeat(value.chars().count()))
        } else {
            self.button(&id, r, value.lines().next().unwrap_or_default())
        } {
            request_input(&id, InputBox::new().default_text(value.as_str()).mode(params.mode));
        }
        if let Some((its_id, text)) = take_input() {
            if its_id == id {
                if let Some(changed) = params.changed {
                    *changed = true;
                }
                *value = text;
            } else {
                return_input(its_id, text);
            }
        }
        Rect::new(lf, r.y, r.right() - lf, r.h)
    }

    pub fn slider(&mut self, text: impl Into<String>, range: Range<f32>, step: f32, value: &mut f32, length: Option<f32>) -> Rect {
        let text = text.into();
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let entry = state.entry(text.clone()).or_default();

            let len = length.unwrap_or(0.3);
            let s = 0.0035;
            let tr = self.text(format!("{text}: {value:.3}")).size(0.4).color(theme::ink(1.)).draw();
            let cy = tr.h + 0.03;
            let p = ((*value - range.start) / (range.end - range.start)).clamp(0., 1.);
            let ink = theme::ink(1.);
            // 轨道：圆角；未填充部分淡，已填充部分强调色
            let track = Rect::new(0., cy - s, len, s * 2.);
            let radius = s;
            self.fill_path(&track.rounded(radius), Color { a: 0.16, ..ink });
            let knob_x = len * p;
            if knob_x > 0.0005 {
                let fill = Rect::new(0., track.y, knob_x.max(radius * 2.), track.h);
                self.fill_path(&fill.rounded(radius), theme::color::ACCENT);
            }
            // 滑块：更大的圆点 + 投影 + 细描边
            let kr = 0.017;
            self.fill_circle(knob_x + 0.0015, cy + 0.0025, kr, Color::new(0., 0., 0., 0.35));
            self.fill_circle(knob_x, cy, kr, WHITE);
            self.stroke_path(
                &Rect::new(knob_x - kr, cy - kr, kr * 2., kr * 2.).rounded(kr),
                0.0012,
                Color { a: 0.25, ..ink },
            );
            let r = Rect::new(0., cy - (kr + 0.005), len, (kr + 0.005) * 2.);
            let r = self.rect_to_global(r);
            self.ensure_touches();
            if let Some(id) = entry {
                if let Some(touch) = self.touches.as_ref().unwrap().iter().rfind(|it| it.id == *id) {
                    let Vec2 { x, y } = touch.position;
                    let (x, _) = self.to_local((x, y));
                    let p = (x / len).clamp(0., 1.);
                    *value = range.start + (range.end - range.start) * p;
                    *value = (*value / step).round() * step;
                    if matches!(touch.phase, TouchPhase::Cancelled | TouchPhase::Ended) {
                        *entry = None;
                    }
                }
            } else if let Some(touch) = self.touches.as_ref().unwrap().iter().find(|it| r.contains(it.position)) {
                if touch.phase == TouchPhase::Started {
                    *entry = Some(touch.id);
                }
            }

            let s = 0.025;
            let mut x = len + 0.02;
            let r = Rect::new(x, cy - s, s * 2., s * 2.);
            glass_plate(self, r, 0.008, 0., false, None);
            self.text("-")
                .pos(r.center().x, r.center().y)
                .anchor(0.5, 0.5)
                .size(0.4)
                .color(theme::ink(1.))
                .no_baseline()
                .draw();
            if self.clicked(r, state.entry(format!("{text}:-")).or_default()) {
                *value = (*value - step).max(range.start);
            }
            x += s * 2. + 0.01;
            let r = Rect::new(x, cy - s, s * 2., s * 2.);
            glass_plate(self, r, 0.008, 0., false, None);
            self.text("+")
                .pos(r.center().x, r.center().y)
                .anchor(0.5, 0.5)
                .size(0.4)
                .color(theme::ink(1.))
                .no_baseline()
                .draw();
            if self.clicked(r, state.entry(format!("{text}:+")).or_default()) {
                *value = (*value + step).min(range.end);
            }

            Rect::new(0., 0., x + s * 2., cy + s)
        })
    }

    /// 练习模式滑条：拖动时**捕获**发起的那根手指，直到抬起 / 取消，中间空帧也不丢。
    /// 这是练习面板专用（普通 `slider` 命中太小、点不动）。从 PhiraiAd 移植。
    pub fn practice_speed_slider(
        &mut self,
        input_id: &str,
        text: impl Into<String>,
        range: Range<f32>,
        step: f32,
        value: &mut f32,
        length: Option<f32>,
    ) -> Rect {
        self.practice_value_slider(
            input_id,
            text,
            crate::practice_view::SliderSpec {
                min: range.start,
                max: range.end,
                midpoint: 1.,
                step,
                suffix: "",
            },
            value,
            length,
        )
    }

    /// 练习面板的通用数值滑条：拖动捕获 + 可点数字框直接输入 + ± 微调。
    /// 配色按本分支主题取（`theme::ink` / `background` / `accent`），逻辑照抄 PhiraiAd。
    pub fn practice_value_slider(
        &mut self,
        input_id: &str,
        text: impl Into<String>,
        spec: crate::practice_view::SliderSpec,
        value: &mut f32,
        length: Option<f32>,
    ) -> Rect {
        let text = text.into();
        let len = length.unwrap_or(0.46);
        let cy = 0.115;
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let entry = state.entry(format!("{input_id}:drag")).or_default();
            let hit = self.rect_to_global(Rect::new(-0.025, cy - 0.045, len + 0.05, 0.09));
            self.ensure_touches();
            if entry.is_none() {
                if let Some(touch) = self
                    .touches
                    .as_ref()
                    .unwrap()
                    .iter()
                    .find(|it| it.phase == TouchPhase::Started && hit.contains(it.position))
                {
                    *entry = Some(touch.id);
                }
            }
            if let Some(id) = *entry {
                if let Some(touch) = self.touches.as_ref().unwrap().iter().rfind(|it| it.id == id) {
                    let Vec2 { x, y } = touch.position;
                    let (x, _) = self.to_local((x, y));
                    if touch.phase != TouchPhase::Cancelled {
                        *value = spec.drag_value(x / len);
                    }
                    if matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                        *entry = None;
                    }
                }
                // 空事件帧里捕获仍然有效；被捕获的手指不能再点别的控件。
                self.touches.as_mut().unwrap().retain(|it| it.id != id);
            }

            let ink = theme::ink(1.);
            let label = self.text(&text).size(0.4).max_width(len - 0.18).no_baseline().draw();
            let number = Rect::new(label.right() + 0.012, label.y - 0.012, 0.16, 0.07);
            // 数值框 = 磨砂玻璃（原来也是纯色 `background()`；点它会弹出输入框）
            glass_plate(self, number, 0.01, 0., false, None);
            // 视角数值是像素 / 百分比原值，流速按 `practice_speed` 的规则显示。
            let number_text = if input_id.starts_with("exercise_view_") {
                value.to_string()
            } else {
                crate::practice_speed::display(*value)
            };
            self.text(format!("{}{}", number_text, spec.suffix))
                .pos(number.center().x, number.center().y)
                .anchor(0.5, 0.5)
                .size(0.36)
                .max_width(number.w)
                .no_baseline()
                .draw();
            if self.clicked(number, state.entry(input_id.to_owned()).or_default()) {
                request_input(input_id, InputBox::new().default_text(number_text));
            }

            // 轨道 + 中点刻度（1x / 100% 处），滑块用强调色。
            self.fill_rect(Rect::new(0., cy - 0.002, len, 0.004), ink);
            self.fill_rect(Rect::new(len * 0.5 - 0.001, cy - 0.008, 0.002, 0.016), ink);
            let p = spec.position(*value);
            self.fill_circle(len * p, cy, 0.019, self.accent());
            let button_size = 0.08;
            let gap = 0.025;
            let mut x = len + 0.035;
            for (label, delta, suffix) in [("−", -spec.step, "-"), ("+", spec.step, "+")] {
                let r = Rect::new(x, cy - button_size / 2., button_size, button_size);
                self.fill_path(&r.rounded(0.012), self.background());
                self.text(label)
                    .pos(r.center().x, r.center().y)
                    .anchor(0.5, 0.5)
                    .size(0.48)
                    .color(ink)
                    .no_baseline()
                    .draw();
                if self.clicked(r, state.entry(format!("{input_id}:{suffix}")).or_default()) {
                    *value = (*value + delta).clamp(spec.min, spec.max);
                }
                x += button_size + gap;
            }
            Rect::new(-0.025, -0.012, x - gap + 0.025, cy + 0.045 + 0.012)
        })
    }

    /// 场景切换 / 失焦 / 关闭暂停 UI 时清掉练习滑条的手指捕获状态。
    /// 从 PhiraiAd 移植。
    pub fn clear_practice_speed_drag() {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            for id in [
                "exercise_speed",
                "exercise_note_flow",
                "exercise_extra",
                "exercise_view_scale",
                "exercise_view_x",
                "exercise_view_y",
            ] {
                for suffix in ["", ":drag", ":-", ":+"] {
                    state.remove(&format!("{id}{suffix}"));
                }
            }
        });
    }

    pub fn hgrids(&mut self, width: f32, height: f32, row_num: u32, count: u32, mut content: impl FnMut(&mut Self, u32)) -> (f32, f32) {
        let mut sh = 0.;
        let w = width / row_num as f32;
        for i in (0..count).step_by(row_num as usize) {
            let mut sw = 0.;
            for j in 0..(count - i).min(row_num) {
                content(self, i + j);
                self.dx(w);
                sw += w;
            }
            self.dx(-sw);
            self.dy(height);
            sh += height;
        }
        self.dy(-sh);
        (width, sh)
    }

    pub fn avatar(&mut self, cx: f32, cy: f32, r: f32, t: f32, avatar: Result<Option<SafeTexture>, SafeTexture>) -> Rect {
        rounded_rect_shadow(
            self,
            Rect::new(cx - r, cy - r, r * 2., r * 2.),
            &ShadowConfig {
                radius: r,
                ..Default::default()
            },
        );
        let rect = Rect::new(cx - r, cy - r, r * 2., r * 2.);
        match avatar {
            Ok(Some(avatar)) => {
                self.fill_circle(cx, cy, r, (*avatar, rect));
            }
            Ok(None) => {
                self.loading(
                    cx,
                    cy,
                    t,
                    WHITE,
                    LoadingParams {
                        radius: r * 0.6,
                        width: 0.008,
                        ..Default::default()
                    },
                );
            }
            Err(icon) => {
                self.fill_circle(cx, cy, r, semi_black(0.2));
                self.fill_circle(cx, cy, r, (*icon, rect.feather(-0.025), ScaleType::CropCenter, WHITE));
            }
        }
        self.stroke_circle(cx, cy, r, 0.004, WHITE);
        rect
    }

    pub fn loading_path(start: f32, len: f32, r: f32) -> Path {
        use lyon::math::{point, vector, Angle};
        let mut path = Path::svg_builder();
        let pt = |a: f32| {
            let (sin, cos) = a.sin_cos();
            point(sin * r, cos * r)
        };
        path.move_to(pt(-start));
        path.arc(point(0., 0.), vector(r, r), Angle::radians(len), Angle::radians(0.));
        path.build()
    }

    const LOADING_SCALE: f32 = 0.74;
    const LOADING_CHANGE_SPEED: f32 = 3.5;
    const LOADING_ROTATE_SPEED: f32 = 4.1;

    pub fn loading<'b>(&mut self, cx: f32, cy: f32, t: f32, shading: impl IntoShading, params: impl Into<LoadingParams<'b>>) {
        use std::f32::consts::PI;

        let params = params.into();
        let (st, mut len) = if let Some(p) = params.progress {
            (t * Self::LOADING_ROTATE_SPEED, p * PI * 2.)
        } else {
            let ct = t * Self::LOADING_CHANGE_SPEED;
            let round = (ct / (PI * 2.)).floor();
            let st = round * Self::LOADING_SCALE + {
                let t = ct - round * PI * 2.;
                if t < PI {
                    0.
                } else {
                    ((t - PI * 3. / 2.).sin() + 1.) * Self::LOADING_SCALE / 2.
                }
            };
            let st = st * PI * 2. + t * Self::LOADING_ROTATE_SPEED;
            let len = (-ct.cos() * Self::LOADING_SCALE / 2. + 0.5) * PI * 2.;
            (st, len)
        };
        if let Some(last) = params.last {
            len = (*last * 5. + len) / 6.;
            *last = len;
        }
        self.scope(|ui| {
            ui.dx(cx);
            ui.dy(cy);
            ui.stroke_path(&Self::loading_path(st, len, params.radius), params.width, shading);
        });
    }

    #[inline]
    pub fn back_rect(&self) -> Rect {
        Rect::new(-0.97, -self.top + 0.04, 0.08, 0.08)
    }

    #[inline]
    pub fn tab_rects<'b>(&mut self, t: f32, it: impl IntoIterator<Item = (&'b mut DRectButton, Cow<'b, str>, bool)>) {
        // 左侧活动栏已删除，标签栏左缘从 -0.86 收到 -0.94（贴近新的 content_rect 左缘）
        let mut r = Rect::new(-0.94, -self.top + 0.18, 0.2, 0.11);
        for (btn, text, chosen) in it {
            btn.render_text(self, r, t, text, 0.5, chosen);
            r.y += 0.125;
        }
    }

    #[inline]
    pub fn content_rect(&self) -> Rect {
        // 左缘原来写 -0.7，是为了给 New UI 的**左侧活动栏（48 设计稿 px）**让位。
        // 用户要求把那条栏整条删掉，腾出来的空间全部还给内容：左缘 -0.94 /
        // 右缘 0.94，两侧各留 3% 屏宽的安全边距（≈43px @1440），不再出现空带。
        Rect::new(-0.94, -self.top + 0.15, 1.88, self.top * 2. - 0.18)
    }

    pub fn full_loading<'b>(&mut self, text: impl Into<Cow<'b, str>>, t: f32) {
        self.fill_rect(self.screen_rect(), semi_black(0.6));
        self.loading(0., -0.03, t, WHITE, ());
        self.text(text.into()).pos(0., 0.05).anchor(0.5, 0.).size(0.6).draw();
    }

    pub fn full_loading_simple(&mut self, t: f32) {
        self.fill_rect(self.screen_rect(), semi_black(0.6));
        self.loading(0., 0., t, WHITE, ());
    }

    pub fn main_sub_colors(use_black: bool, alpha: f32) -> (Color, Color) {
        if use_black {
            (semi_black(alpha), semi_black(alpha * 0.64))
        } else {
            (semi_white(alpha), semi_white(alpha * 0.64))
        }
    }
}

pub struct LoadingParams<'a> {
    pub radius: f32,
    pub width: f32,
    pub progress: Option<f32>,
    pub last: Option<&'a mut f32>,
}
impl Default for LoadingParams<'_> {
    fn default() -> Self {
        Self {
            radius: 0.05,
            width: 0.012,
            progress: None,
            last: None,
        }
    }
}
impl From<()> for LoadingParams<'_> {
    fn from(_: ()) -> Self {
        Self::default()
    }
}
impl From<f32> for LoadingParams<'_> {
    fn from(progress: f32) -> Self {
        Self {
            progress: Some(progress),
            ..Self::default()
        }
    }
}
impl<'a> From<(Option<f32>, &'a mut f32)> for LoadingParams<'a> {
    fn from((progress, last): (Option<f32>, &'a mut f32)) -> Self {
        Self {
            progress,
            last: Some(last),
            ..Self::default()
        }
    }
}
pub struct SafeAudio(pub Option<AudioManager>);

impl Deref for SafeAudio {
    type Target = AudioManager;
    fn deref(&self) -> &AudioManager {
        self.0.as_ref().expect("SafeAudio is already dropped")
    }
}

impl DerefMut for SafeAudio {
    fn deref_mut(&mut self) -> &mut AudioManager {
        self.0.as_mut().expect("SafeAudio is already dropped")
    }
}

impl Drop for SafeAudio {
    fn drop(&mut self) {
        if let Some(inner) = self.0.take() {
            let prev_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                drop(inner);
            }));
            std::panic::set_hook(prev_hook);
            if result.is_err() {
                // cleanup_audio() was not called before process exit;
                // the audio backend thread may have already terminated.
                // Nothing we can do — just let the OS reclaim resources.
            }
        }
    }
}

/// Drop the UI audio manager explicitly, before thread-local destructors run.
/// Must be called before the process exits to avoid panicking in cpal's WASAPI
/// stream drop, which tries to join an already-terminated audio thread.
pub fn cleanup_audio() {
    UI_AUDIO.with(|it| {
        it.borrow_mut().0.take();
    });
}

// This function is used to create UI audio manager.
#[allow(clippy::blocks_in_conditions)]
fn build_audio() -> SafeAudio {
    match {
        #[cfg(target_os = "android")]
        {
            use sasa::backend::oboe::*;
            AudioManager::new(OboeBackend::new(OboeSettings {
                performance_mode: PerformanceMode::PowerSaving,
                usage: Usage::Game,
                ..Default::default()
            }))
        }
        #[cfg(target_env = "ohos")]
        {
            use sasa::backend::ohos::*;
            AudioManager::new(OhosBackend::new(OhosSettings {
                buffer_size: Some(512),
                sample_rate: Some(48000),
                channels: 2,
            }))
        }
        #[cfg(not(any(target_os = "android", target_env = "ohos")))]
        {
            use sasa::backend::cpal::*;
            AudioManager::new(CpalBackend::new(CpalSettings::default()))
        }
    } {
        Ok(manager) => SafeAudio(Some(manager)),
        Err(e) => {
            show_error(e.context(ttl!("audio-backend-init-failed")));
            SafeAudio(Some(AudioManager::new(DummyBackend).expect("Failed to create dummy audio backend, this should not happen")))
        }
    }
}

struct DummyBackend;

impl sasa::backend::Backend for DummyBackend {
    fn setup(&mut self, setup: sasa::backend::BackendSetup) -> anyhow::Result<()> {
        let _ = setup;
        Ok(())
    }
    fn start(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
    fn consume_broken(&self) -> bool {
        false
    }
}

thread_local! {
    pub static UI_AUDIO: RefCell<SafeAudio> = RefCell::new(build_audio());
    pub static UI_BTN_HITSOUND_LARGE: RefCell<Option<Sfx>> = const { RefCell::new(None) };
    pub static UI_BTN_HITSOUND: RefCell<Option<Sfx>> = const { RefCell::new(None) };
    pub static UI_SWITCH_SOUND: RefCell<Option<Sfx>> = const { RefCell::new(None) };
}

pub fn button_hit() {
    UI_BTN_HITSOUND.with(|it| {
        if let Some(sfx) = it.borrow_mut().as_mut() {
            let _ = sfx.play(PlaySfxParams {
                amplifier: f32::from_bits(UI_SFX_VOLUME.load(Ordering::Relaxed)),
            });
        }
    });
}

pub fn button_hit_large() {
    UI_BTN_HITSOUND_LARGE.with(|it| {
        if let Some(sfx) = it.borrow_mut().as_mut() {
            let _ = sfx.play(PlaySfxParams {
                amplifier: f32::from_bits(UI_SFX_VOLUME.load(Ordering::Relaxed)),
            });
        }
    });
}

pub fn list_switch() {
    UI_SWITCH_SOUND.with(|it| {
        if let Some(sfx) = it.borrow_mut().as_mut() {
            let _ = sfx.play(PlaySfxParams {
                amplifier: f32::from_bits(UI_SFX_VOLUME.load(Ordering::Relaxed)),
            });
        }
    });
}
