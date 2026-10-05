//! 课题模式（T7.3）在曲库页里的选择面板。
//!
//! 移植自 PhiraiAd 的 `page/challenge.rs`：条状选择器 + 曲库工具行上的入口按钮 +
//! 徽章展示按钮。面板背景走磨砂亚克力，文字纯白，与 meew-11 的 New UI 对齐。
use super::{ChartItem, ChartType, Illustration};
use crate::{
    challenge_ui::BadgePicker,
    get_data, get_data_mut,
    newui::{self, Metrics},
    save_data,
};
use anyhow::Result;
use macroquad::prelude::*;
use prpr::{
    ext::RectExt,
    ui::{DRectButton, Ui},
};

/// 选择条的高度（设计 px，交给 [`Metrics::u`] 换算）。
pub const SELECTOR_HEIGHT_PX: f32 = 96.;

pub struct ChallengePanel {
    pub active: bool,
    pub selected: crate::challenge::ChallengeSelection<ChartItem>,
    pub start: Option<[ChartItem; 3]>,
    entry: DRectButton,
    badge: DRectButton,
    cancel: DRectButton,
    clear: DRectButton,
    save_selection: DRectButton,
    positions: [DRectButton; 3],
    remove: [DRectButton; 3],
    begin: DRectButton,
    entry_ready: bool,
    area: Rect,
    picker: BadgePicker,
    restoring: Vec<(usize, i32, prpr::task::Task<Result<std::sync::Arc<crate::client::Chart>>>)>,
}
impl ChallengePanel {
    pub fn new() -> Self {
        Self {
            active: false,
            selected: Default::default(),
            start: None,
            entry: DRectButton::new(),
            badge: DRectButton::new(),
            cancel: DRectButton::new(),
            clear: DRectButton::new(),
            save_selection: DRectButton::new(),
            positions: std::array::from_fn(|_| DRectButton::new()),
            remove: std::array::from_fn(|_| DRectButton::new()),
            begin: DRectButton::new(),
            entry_ready: false,
            area: Rect::new(0., 0., 0., 0.),
            picker: BadgePicker::default(),
            restoring: Vec::new(),
        }
    }
    pub fn ready(&self) -> bool {
        self.selected.ready()
    }
    /// 把一首谱面放进当前聚焦的槽位（不允许重复选同一张）。
    pub fn select(&mut self, mut chart: ChartItem) {
        // 自定义 RKS 里解析过的定数优先（T7.4 接入；没有就退回谱面自带定数）。
        if let Some(value) = resolved_difficulty(&chart) {
            chart.info.difficulty = value as f32;
        }
        if !self.selected.select_unique(chart, |a, b| {
            a.info.id.zip(b.info.id).is_some_and(|(a, b)| a == b)
                || a.local_path.as_ref().zip(b.local_path.as_ref()).is_some_and(|(a, b)| a == b)
        }) {
            prpr::scene::show_message("不能重复选择同一个谱面").warn();
        }
    }
    pub fn close(&mut self) -> bool {
        if self.picker.open {
            self.picker.open = false;
            return true;
        }
        if !self.active {
            return false;
        }
        self.active = false;
        self.restoring.clear();
        self.selected = Default::default();
        self.start = None;
        true
    }
    pub fn update(&mut self, t: f32) {
        self.restoring.retain_mut(|(slot, id, task)| {
            if let Some(result) = task.take() {
                if let Ok(chart) = result {
                    if let Some(selected) = &mut self.selected.slots[*slot] {
                        if selected.info.id == Some(*id) {
                            selected.illu = Illustration::from_file_thumbnail(chart.illustration.clone());
                        }
                    }
                }
                false
            } else {
                true
            }
        });
        for chart in self.selected.slots.iter_mut().flatten() {
            chart.illu.settle(t);
        }
    }
    pub fn begin_render(&mut self) {
        self.entry_ready = false;
    }
    /// 本帧入口按钮没有画出来时调用，避免 touch 命中上一帧的残影位置。
    pub fn hide_entry(&mut self) {
        self.entry_ready = false;
        self.entry.invalidate();
        self.badge.invalidate();
    }
    /// 曲库工具行上的入口：`rect` 同时装下「课题模式」按钮和徽章展示按钮。
    pub fn render_entry(&mut self, ui: &mut Ui, t: f32, rect: Rect) {
        self.entry_ready = true;
        let badge_w = rect.h;
        let gap = (rect.h * 0.12).min(0.008);
        let entry_r = Rect::new(rect.x, rect.y, (rect.w - badge_w - gap).max(0.02), rect.h);
        let badge_rect = Rect::new(rect.right() - badge_w, rect.y, badge_w, rect.h);
        // `render_text` 的 size 是世界坐标：世界高度 ≈ 0.08 × size，
        // 因此「占按钮高度约 30% 的字」就是 rect.h × 3.2。
        let size = rect.h * 3.2;
        self.entry.render_text(ui, entry_r, t, "课题模式", size, false);
        let badge = get_data().challenge_badges.display();
        // 徽章槽：**有徽章时**才用强调色底。以前无条件传 `chosen = true`，
        // 没徽章时就是一个空的强调色方块（用户看到的就是那块蓝砖）。
        self.badge.render_text(ui, badge_rect, t, "", size, badge.is_some());
        if let Some(badge) = badge {
            crate::challenge_ui::render_display_badge(
                ui,
                Rect::new(badge_rect.x + 0.004, badge_rect.y + 0.004, badge_rect.w - 0.008, badge_rect.h - 0.008),
                badge,
            );
        } else {
            // 空槽：画一个弱化的奖杯占位，跟其他玻璃按钮保持同一套语言
            let th = newui::theme();
            newui::draw_icon(
                ui,
                &newui::th_icon_metrics(),
                newui::Icon::Trophy,
                badge_rect.feather(-badge_rect.h * 0.30),
                th.text_mute,
                1.7,
            );
        }
    }
    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<bool> {
        if self.picker.touch(touch, t)? {
            return Ok(true);
        }
        if !self.active {
            if self.entry_ready && self.entry.touch(touch, t) {
                self.active = true;
                self.selected = Default::default();
                self.restoring.clear();
                self.restore_saved_selection();
                return Ok(true);
            }
            if self.entry_ready && self.badge.touch(touch, t) {
                self.picker.display();
                return Ok(true);
            }
            return Ok(false);
        }
        if self.cancel.touch(touch, t) {
            self.close();
            return Ok(true);
        }
        if self.clear.touch(touch, t) {
            self.restoring.clear();
            self.selected = Default::default();
            return Ok(true);
        }
        if self.save_selection.touch(touch, t) {
            let saved = std::array::from_fn(|i| {
                self.selected.slots[i].as_ref().map(|chart| crate::data::SavedChallengeChart {
                    info: chart.info.clone(),
                    local_path: chart.local_path.clone(),
                })
            });
            let old = std::mem::replace(&mut get_data_mut().challenge_selection, saved);
            if let Err(error) = save_data() {
                get_data_mut().challenge_selection = old;
                return Err(error);
            }
            prpr::scene::show_message("课题选择已保存").ok();
            return Ok(true);
        }
        for i in 0..3 {
            // 先判「删除」（小按钮整个压在槽位卡片里），再判「切聚焦」
            if self.selected.slots[i].is_some() && self.remove[i].touch(touch, t) {
                self.selected.remove(i);
                return Ok(true);
            }
            if self.positions[i].touch(touch, t) {
                self.selected.focused = i;
                return Ok(true);
            }
        }
        if self.ready() && self.begin.touch(touch, t) {
            self.start = self.selected.course();
            return Ok(true);
        }
        Ok(self.area.contains(touch.position))
    }
    /// 打开面板时，把上次保存的三个槽位恢复回来。
    fn restore_saved_selection(&mut self) {
        for (i, saved) in get_data().challenge_selection.iter().enumerate() {
            if let Some(saved) = saved {
                let exists = saved
                    .local_path
                    .as_ref()
                    .is_some_and(|p| std::path::Path::new(&format!("{}/{p}", crate::dir::charts().unwrap_or_default())).exists());
                if !exists && saved.info.id.is_none() {
                    continue;
                }
                let local_path = if exists { saved.local_path.clone() } else { None };
                let illu = local_path
                    .as_ref()
                    .map(|p| super::local_illustration(p.clone(), prpr::ext::BLACK_TEXTURE.clone(), false))
                    .unwrap_or_else(|| Illustration::from_done(prpr::ext::BLACK_TEXTURE.clone()));
                if self.selected.slots.iter().flatten().any(|old| {
                    old.info.id.zip(saved.info.id).is_some_and(|(a, b)| a == b)
                        || old.local_path.as_ref().zip(local_path.as_ref()).is_some_and(|(a, b)| a == b)
                }) {
                    continue;
                }
                if !exists {
                    if let Some(id) = saved.info.id {
                        self.restoring
                            .push((i, id, prpr::task::Task::new(crate::client::Client::load::<crate::client::Chart>(id))));
                    }
                }
                self.selected.slots[i] = Some(ChartItem {
                    info: saved.info.clone(),
                    local_path,
                    illu,
                    chart_type: ChartType::Imported,
                });
            }
        }
        self.selected.focused = self.selected.slots.iter().position(Option::is_none).unwrap_or(0);
    }
    pub fn render(&mut self, ui: &mut Ui, t: f32, enabled: bool, m: &Metrics, strip: Rect) {
        self.entry_ready &= enabled && !self.active;
        let th = newui::theme();
        if self.active {
            self.area = strip;
            newui::acrylic(ui, strip, m.u(10.), &th);
            let pad = m.u(10.);
            let btn_h = m.u(30.);
            let right_w = m.u(150.);
            let right_x = strip.right() - pad - right_w;
            let row1 = Rect::new(right_x, strip.y + pad, right_w, btn_h);
            let row2 = Rect::new(right_x, strip.bottom() - pad - btn_h, right_w, btn_h);
            // 右侧：上排三个小按钮
            let bw = (right_w - m.u(8.)) / 3.;
            let mut x = right_x;
            for (btn, label) in [(&mut self.cancel, "取消"), (&mut self.clear, "清空"), (&mut self.save_selection, "保存")] {
                btn.render_text(ui, Rect::new(x, row1.y, bw, btn_h), t, label, m.f(13.), false);
                x += bw + m.u(4.);
            }
            // 右下：开始（未选满时灰显）
            if self.ready() {
                self.begin.render_text(ui, row2, t, "开始课题", m.f(14.), true);
            } else {
                ui.fill_path(&row2.rounded(m.u(8.)), th.wash(0.02));
                newui::rounded_stroke(ui, row2, m.u(8.), m.u(1.), th.border);
                newui::text_center(ui, m, "选满 3 张后开始", row2.center().x, row2.center().y, 13., th.text_mute);
            }
            // 左侧：三个曲目槽位
            let slots_w = right_x - strip.x - pad * 2.;
            let sw = (slots_w - m.u(8.) * 2.) / 3.;
            for i in 0..3 {
                let sr = Rect::new(strip.x + pad + i as f32 * (sw + m.u(8.)), strip.y + pad, sw, strip.h - pad * 2.);
                let focused = self.selected.focused == i;
                ui.fill_path(&sr.rounded(m.u(8.)), if focused { th.with_a(th.accent, 0.14) } else { th.wash(0.03) });
                newui::rounded_stroke(
                    ui,
                    sr,
                    m.u(8.),
                    m.u(1.),
                    if focused { th.with_a(th.glow, 0.6) } else { th.border },
                );
                // 整块可点（切聚焦），右上角的小叉负责删除
                self.positions[i].build(ui, t, sr, |_, _| {});
                newui::text(ui, m, ["1st", "2nd", "3rd"][i], sr.x + m.u(10.), sr.y + m.u(12.), 11., th.text_mute, (0., 0.5));
                match &self.selected.slots[i] {
                    Some(chart) => {
                        let cover = Rect::new(sr.x + m.u(10.), sr.y + m.u(24.), m.u(40.), m.u(40.));
                        chart.illu.notify();
                        ui.fill_path(&cover.rounded(m.u(6.)), chart.illu.shading(cover, t));
                        let tx = cover.right() + m.u(8.);
                        let max_w = (sr.right() - m.u(10.) - tx).max(m.u(20.));
                        newui::text_ellipsis(ui, m, chart.info.name.clone(), tx, sr.y + m.u(30.), 13., max_w, th.text_hi, (0., 0.5));
                        let label = crate::challenge::difficulty_label(&chart.info.level, chart.info.difficulty as f64);
                        newui::text(
                            ui,
                            m,
                            format!("{label}  {:.1}", chart.info.difficulty),
                            tx,
                            sr.y + m.u(50.),
                            12.,
                            th.text_dim,
                            (0., 0.5),
                        );
                        let rr = Rect::new(sr.right() - m.u(26.), sr.y + m.u(4.), m.u(22.), m.u(22.));
                        self.remove[i].render_text(ui, rr, t, "×", m.f(13.), false);
                    }
                    None => {
                        newui::text_center(ui, m, "点击曲目加入", sr.center().x, sr.center().y + m.u(6.), 13., th.text_mute);
                    }
                }
            }
        }
        self.picker.render(ui, t);
    }
}

impl Default for ChallengePanel {
    fn default() -> Self {
        Self::new()
    }
}

/// 自定义 RKS（T7.4）里解析出来的定数。
///
/// 读 `data/custom-rks.json`：玩家在 RKS 编辑器里给某张谱面设过自定义定数时，
/// 课题模式就用那个定数算徽章；没设过 / 读不出来就返回 `None`，退回谱面自带定数。
fn resolved_difficulty(chart: &ChartItem) -> Option<f64> {
    let path = chart.local_path.as_ref()?;
    let settings = crate::custom_rks::Settings::load(&std::path::PathBuf::from(crate::dir::root().ok()?).join("custom-rks.json")).ok()?;
    let input = crate::custom_rks::LocalInput {
        path: path.clone(),
        name: chart.info.name.clone(),
        level: chart.info.level.clone(),
        difficulty: chart.info.difficulty as f64,
        // 不接入 AI 定数
        ai_difficulty: None,
        accuracy: None,
    };
    crate::custom_rks::resolve(&input, settings.charts.get(path).unwrap_or(&Default::default())).difficulty
}
