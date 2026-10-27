use super::{Illustration, Page, SharedState};
use crate::{
    client::{Client, Event},
    icons::Icons,
    scene::EventScene,
};
use anyhow::Result;
use macroquad::prelude::*;
use nalgebra::Rotation2;
use prpr::{
    core::Tweenable,
    ext::{semi_black, RectExt, SafeTexture, ScaleType},
    scene::{show_error, NextScene},
    task::Task,
    ui::{button_hit_large, DRectButton, RectButton, Scroll, Ui},
};
use std::{borrow::Cow, sync::Arc};

const TRANSIT_TIME: f32 = 0.5;
const ILLU_FEATHER: f32 = 0.4;

struct Item {
    event: Event,
    illu: Illustration,
    btn: DRectButton,
}

impl Item {
    pub fn new(event: Event) -> Self {
        let illu = Illustration::from_file(event.illustration.clone());
        Self {
            event,
            illu,
            btn: DRectButton::new().no_sound(),
        }
    }
}

/// 活动时间区间文案（`2026-10-01 ~ 2026-10-31`），画在卡片曲名下方的弱化副标题。
fn event_time_range(event: &Event) -> Option<String> {
    let f = |t: chrono::DateTime<chrono::Utc>| t.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string();
    Some(format!("{} ~ {}", f(event.time_start), f(event.time_end)))
}

pub struct EventPage {
    fetch_task: Option<Task<Result<Vec<Event>>>>,
    scroll: Scroll,
    events: Option<Vec<Item>>,
    index: usize,

    btn_down: RectButton,
    btn_up: RectButton,

    tr_from: Rect,
    tr_start: f32,

    first_in: bool,

    next_scene: Option<NextScene>,

    icons: Arc<Icons>,
    rank_icons: [SafeTexture; 8],
}

impl EventPage {
    pub const LB_PAD: f32 = 0.05;

    pub fn new(icons: Arc<Icons>, rank_icons: [SafeTexture; 8]) -> Self {
        Self {
            fetch_task: Some(Task::new(async move { Ok(Client::query().send().await?.0) })),
            scroll: Scroll::new(),
            events: None,
            index: 0,

            btn_down: RectButton::new(),
            btn_up: RectButton::new(),

            tr_from: Rect::default(),
            tr_start: f32::NAN,

            first_in: true,

            next_scene: None,

            icons,
            rank_icons,
        }
    }

    fn loading(&self) -> bool {
        self.fetch_task.is_some()
    }
}

impl Page for EventPage {
    fn label(&self) -> Cow<'static, str> {
        use crate::scene::event::{tl, L10N_LOCAL};
        tl!("label")
    }

    fn enter(&mut self, s: &mut SharedState) -> Result<()> {
        if self.first_in {
            self.first_in = false;
        } else {
            self.tr_start = -s.rt;
        }
        Ok(())
    }

    fn touch(&mut self, touch: &Touch, s: &mut SharedState) -> Result<bool> {
        let t = s.t;
        if self.loading() {
            return Ok(true);
        }
        if self.scroll.touch(touch, t) {
            return Ok(true);
        }
        if let Some(events) = &mut self.events {
            for item in events.iter_mut() {
                if self.tr_start.is_nan() && item.btn.touch(touch, t) {
                    button_hit_large();
                    self.scroll.y_scroller.halt();
                    self.tr_start = s.rt;
                    return Ok(true);
                }
            }
            if self.btn_up.touch(touch) {
                self.index = self.index.saturating_sub(1);
                self.scroll.y_scroller.goto_step(self.index);
                return Ok(true);
            }
            if self.btn_down.touch(touch) {
                self.index = (self.index + 2).min(events.len()) - 1;
                self.scroll.y_scroller.goto_step(self.index);
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn update(&mut self, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        self.scroll.update(t);
        if let Some(events) = &mut self.events {
            for item in events {
                item.illu.settle(t);
            }
        }
        if let Some(task) = &mut self.fetch_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        use crate::scene::event::{tl, L10N_LOCAL};
                        show_error(err.context(tl!("load-list-failed")));
                    }
                    Ok(val) => {
                        self.events = Some(val.into_iter().map(Item::new).collect());
                    }
                }
                self.fetch_task = None;
            }
        }
        Ok(())
    }

    fn render(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        let m = crate::newui::Metrics::new(ui);
        let th = crate::newui::theme();
        // 鼠标悬停判定：`mouse_world` 与 `Touch::position` 同一套世界坐标，
        // 滚动与透视都已经体现在 `ui.rect_to_global` 里，所以直接比矩形即可。
        let (mx, my) = crate::newui::mouse_world();
        let mouse = vec2(mx, my);

        self.scroll.y_scroller.step = ui.top * 2.;

        s.render_fader(ui, |ui| {
            if let Some(events) = &mut self.events {
                ui.scope(|ui| {
                    ui.dx(-1.);
                    ui.dy(-ui.top);
                    self.scroll.size((2., ui.top * 2.));
                    self.scroll.render(ui, |ui| {
                        ui.dx(1.);
                        ui.dy(ui.top);
                        for (index, item) in events.iter_mut().enumerate() {
                            item.illu.notify();
                            let ca = item.illu.alpha(t);
                            let r = ui.screen_rect().nonuniform_feather(-0.24, -0.144);
                            let hover = ui.rect_to_global(r).contains(mouse);
                            // 卡片圆角跟 New UI 曲库卡片一致
                            item.btn.config.radius = m.u(14.);
                            item.btn.render_shadow(ui, r, t, |ui, path| {
                                ui.alpha(ca, |ui| {
                                    ui.fill_path(&path, item.illu.shading(r.feather(ILLU_FEATHER), t));
                                    ui.fill_path(&path, semi_black(0.4 * if item.illu.task.is_some() { 1. } else { ca }));
                                });
                                // 悬停反馈：强调色描边 + 一层极淡的强调色填充（阴影由 render_shadow 加深）
                                if hover {
                                    ui.fill_path(&path, th.with_a(th.glow, 0.06));
                                    ui.stroke_path(&path, 0.0016, th.with_a(th.glow, 0.5));
                                }
                                // 曲名压在插图上 → 强制纯白 + 黑投影（不能跟主题走，浅色主题下会变深色）
                                let name = item.event.name.clone();
                                let off = 0.003;
                                ui.text(name.clone())
                                    .pos(r.x + Self::LB_PAD + off, r.bottom() - Self::LB_PAD - off)
                                    .anchor(0., 1.)
                                    .size(1.3)
                                    .color(semi_black(0.72))
                                    .draw();
                                ui.text(name)
                                    .pos(r.x + Self::LB_PAD, r.bottom() - Self::LB_PAD)
                                    .anchor(0., 1.)
                                    .size(1.3)
                                    .color(WHITE)
                                    .draw();
                                // 副标题：活动时间（更小更弱，同样白字 + 投影）
                                if let Some(range) = event_time_range(&item.event) {
                                    let sy = r.bottom() - Self::LB_PAD + 0.026;
                                    ui.text(range.clone())
                                        .pos(r.x + Self::LB_PAD + 0.002, sy - 0.002)
                                        .anchor(0., 1.)
                                        .size(0.42)
                                        .color(semi_black(0.7))
                                        .draw();
                                    ui.text(range)
                                        .pos(r.x + Self::LB_PAD, sy)
                                        .anchor(0., 1.)
                                        .size(0.42)
                                        // 副标题和上面的曲名压在**同一张插图**上（插图底下叠了
                                        // `semi_black(0.4)`，恒深）→ 必须跟曲名一样写死白。
                                        // 原来这里用 `semi_white`，浅色主题下会翻成深色，配着
                                        // 上面那层黑投影正好糊成一团看不见。
                                        .color(Color::new(1., 1., 1., 0.82))
                                        .draw();
                                }
                            });
                            if index == self.index {
                                self.tr_from = ui.rect_to_global(r);
                            }
                            ui.dy(ui.top * 2.);
                        }
                        (2., events.len() as f32 * ui.top * 2.)
                    });
                });
            }
        });

        if let Some(events) = &self.events {
            s.render_fader(ui, |ui| {
                let d = ui.top - 0.057;
                let s = 0.04;
                let r = Rect::new(-d, 0., 0., 0.).feather(s);
                self.btn_up.set(ui, Rect::new(0., -d, 0., 0.).feather(s));
                self.btn_down.set(ui, Rect::new(0., d, 0., 0.).feather(s));
                self.index = (self.scroll.y_scroller.offset / (ui.top * 2.)).round() as usize;
                // 上下翻页箭头：图标底色跟主题走（浅色主题下不能是白的，否则白底白箭头）
                ui.with(Rotation2::new(std::f32::consts::FRAC_PI_2).into(), |ui| {
                    ui.fill_rect(r, (*self.icons.back, r, ScaleType::CropCenter, prpr::ui::theme::ink(if self.index == 0 { 0.3 } else { 1. })));
                });
                ui.with(Rotation2::new(-std::f32::consts::FRAC_PI_2).into(), |ui| {
                    ui.fill_rect(
                        r,
                        (*self.icons.back, r, ScaleType::CropCenter, prpr::ui::theme::ink(if self.index + 1 >= events.len() { 0.3 } else { 1. })),
                    );
                });
                if events.is_empty() {
                    ui.text(ttl!("list-empty")).anchor(0.5, 0.5).no_baseline().size(1.4).color(th.text_mute).draw();
                }
            });
        }

        if !self.tr_start.is_nan() {
            let item = &self.events.as_ref().unwrap()[self.index];
            let p = if self.tr_start.is_nan() {
                1.
            } else {
                let p = ((s.rt - self.tr_start.abs()) / TRANSIT_TIME).min(1.);
                let grow = self.tr_start > 0.;
                if p >= 1. {
                    if grow {
                        self.next_scene = Some(NextScene::Overlay(Box::new(EventScene::new(
                            item.event.clone(),
                            item.illu.clone(),
                            Arc::clone(&self.icons),
                            self.rank_icons.clone(),
                        ))));
                    }
                    self.tr_start = f32::NAN;
                }
                let p = (1. - p).powi(4);
                if grow {
                    1. - p
                } else {
                    p
                }
            };
            let r = Rect::tween(&self.tr_from, &ui.screen_rect(), p);
            let path = r.rounded(0.02 * (1. - p));
            ui.fill_path(&path, item.illu.shading(r.feather((1. - p) * ILLU_FEATHER), t));
            ui.fill_path(&path, semi_black(0.4));
            // 过渡中的曲名同样压在图上 → 纯白 + 投影
            ui.text(&item.event.name)
                .pos(r.x + Self::LB_PAD + 0.003, r.bottom() - Self::LB_PAD - 0.003)
                .anchor(0., 1.)
                .size(1.3 + p * 0.2)
                .color(semi_black(0.72))
                .draw();
            ui.text(&item.event.name)
                .pos(r.x + Self::LB_PAD, r.bottom() - Self::LB_PAD)
                .anchor(0., 1.)
                .size(1.3 + p * 0.2)
                .color(WHITE)
                .draw();
        }
        Ok(())
    }

    fn render_top(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        if self.loading() {
            ui.full_loading_simple(t);
        }
        Ok(())
    }

    fn on_back_pressed(&mut self, _s: &mut SharedState) -> bool {
        !self.tr_start.is_nan()
    }

    fn next_scene(&mut self, _s: &mut SharedState) -> NextScene {
        self.next_scene.take().unwrap_or_default()
    }
}
