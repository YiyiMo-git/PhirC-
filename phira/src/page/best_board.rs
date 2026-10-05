//! 最佳谱面榜（T7.2）：把计入自定义 RKS 的成绩按三列卡片铺开。
//!
//! 移植自 PhiraiAd 的 `page/best_board.rs`。原版还依赖 `replica_local_records`
//! （meew-11 没有这个字段），这里只从 `charts[].record` 和 `local_records` 取分。
//! 画面保持原版的几何尺寸（导出 PNG 时用的是同一套坐标），只把文字统一成纯白、
//! 卡片底色压成偏暗的面板色。
use super::{Illustration, SharedState};
use crate::{custom_rks::RankedEntry, get_data};
use macroquad::prelude::*;
use prpr::ext::RectExt;
use prpr::ui::{clip_rounded_rect, DRectButton, Scroll, Ui};

const UNIT: f32 = 1.88 / 1200.;
const FONT_SCALE: f32 = 0.83;
const FONT_UNIT: f32 = UNIT * 1.121 / 0.08;
const GAP: f32 = 12. * UNIT;
const CARD_WIDTH: f32 = 394. * 0.9 * 0.95 * UNIT;
const CARD_HEIGHT: f32 = 85. * UNIT;
const BOARD_WIDTH: f32 = CARD_WIDTH * 3. + GAP * 2.;
/// 卡片圆角半径（用户要求「做圆角卡片」）。用设计稿 px 表达：`UNIT` 的 1 单位 = 1200px。
const CARD_RADIUS: f32 = 10. * UNIT;
/// 卡片 1px 描边：不加的话圆角卡片在深色底上会显得很平（和 New UI 其它卡片不一致）。
const CARD_BORDER: Color = Color::new(1., 1., 1., 0.07);
/// 卡片 / 背景底色（和课题结算一致的深色面板）。
const BOARD_BG: Color = Color::new(17. / 255., 23. / 255., 34. / 255., 1.);
/// 卡片底色：**半透明**，背景从下面透上来 —— 磨砂玻璃的第一层。
const CARD_BG: Color = Color::new(34. / 255., 44. / 255., 60. / 255., 0.82);
const COVER_BG: Color = Color::new(53. / 255., 65. / 255., 83. / 255., 1.);

/// 卡片面 = 磨砂玻璃（用户要求「所有卡片统一」成磨砂玻璃观感）。
///
/// ⚠️ 这里**不能直接用 `newui::acrylic`**：那套是跟着主题走的，而这张榜
/// 还会被 `render_export` 导出成图片 —— 浅色主题下导出会变成一块白。
/// 所以这里用**固定深色**复刻同一套层次：半透明底 → 1px 浅边 → 顶部高光 → 磨砂颗粒。
fn frosted_card(ui: &mut Ui, r: Rect, radius: f32) {
    ui.fill_path(&r.rounded(radius), CARD_BG);
    // 雾面：上半层往下渐隐的白膜（「磨砂」最直观的一层，原来缺这层）
    let haze = Rect::new(r.x, r.y, r.w, r.h * 0.55);
    ui.fill_path(
        &haze.rounded(radius),
        (Color::new(1., 1., 1., 0.05), (r.x, r.y), Color::new(1., 1., 1., 0.), (r.x, r.y + haze.h)),
    );
    ui.stroke_path(&r.rounded(radius), 2. * UNIT, CARD_BORDER);
    // 顶部高光（玻璃反光）：加粗加亮，原来 0.001 高 / 0.06 alpha 基本看不见
    ui.fill_rect(
        Rect::new(r.x + radius * 0.5, r.y + 0.0012, (r.w - radius).max(0.), 0.0018),
        Color::new(1., 1., 1., 0.16),
    );
    // 磨砂颗粒：细密横线（低 alpha，只加亮不加暗）
    let step = 0.0073;
    let mut y = (r.y / step).ceil() * step;
    while y < r.bottom() {
        ui.fill_rect(Rect::new(r.x + 0.002, y, (r.w - 0.004).max(0.), 0.0006), Color::new(1., 1., 1., 0.030));
        y += step;
    }
}

use crate::challenge::difficulty_label;

struct Card {
    entry: RankedEntry,
    illustration: Option<Illustration>,
    score: Option<i32>,
    level: &'static str,
}

pub struct BestBoard {
    pub open: bool,
    pub entry_active: bool,
    entry_button: DRectButton,
    close_button: DRectButton,
    export_button: DRectButton,
    pub export_requested: bool,
    scroll: Scroll,
    cards: Vec<Card>,
    player: String,
    rks: f64,
    course_badge: Option<crate::challenge::ChallengeBadge>,
}

impl Default for BestBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl BestBoard {
    pub fn new() -> Self {
        Self {
            open: false,
            entry_active: false,
            entry_button: DRectButton::new(),
            close_button: DRectButton::new(),
            export_button: DRectButton::new(),
            export_requested: false,
            scroll: Scroll::new(),
            cards: Vec::new(),
            player: String::new(),
            rks: 0.,
            course_badge: None,
        }
    }

    /// 打开榜单。`entries` / `rks` 由 `RksPanel::best_board` 提供。
    pub fn show(&mut self, entries: Vec<RankedEntry>, rks: f64, state: &SharedState) {
        let data = get_data();
        self.player = data.me.as_ref().map(|user| user.name.clone()).unwrap_or_else(|| "离线玩家".into());
        self.rks = rks;
        // 课题模式徽章（T7.3）会画在标题行里
        self.course_badge = data.challenge_badges.display().cloned();
        self.cards = entries
            .into_iter()
            .map(|entry| {
                let chart = entry
                    .path
                    .as_ref()
                    .and_then(|path| state.charts_local.iter().find(|chart| chart.local_path.as_ref() == Some(path)));
                // 本地最高分：`charts[].record` 与 `local_records` 取大者
                let score = entry.path.as_ref().and_then(|path| {
                    data.charts
                        .iter()
                        .filter(|chart| &chart.local_path == path)
                        .filter_map(|chart| chart.record.as_ref().map(|record| record.score))
                        .chain(data.local_records.get(path).and_then(|record| record.as_ref()).map(|record| record.score))
                        .max()
                });
                let level = difficulty_label(chart.map(|chart| chart.info.level.as_str()).unwrap_or(""), entry.difficulty);
                Card {
                    level,
                    illustration: chart.map(|chart| chart.illu.clone()),
                    entry,
                    score,
                }
            })
            .collect();
        self.scroll = Scroll::new();
        self.entry_button.inner.cancel();
        self.close_button.inner.cancel();
        self.export_button.inner.cancel();
        self.export_requested = false;
        self.open = true;
    }

    pub fn close(&mut self) -> bool {
        let was_open = self.open;
        self.open = false;
        self.entry_button.inner.cancel();
        self.close_button.inner.cancel();
        self.export_button.inner.cancel();
        self.export_requested = false;
        self.cards.clear();
        was_open
    }

    pub fn entry_touch(&mut self, touch: &Touch, t: f32) -> bool {
        self.entry_active && self.entry_button.touch(touch, t)
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) {
        if self.close_button.touch(touch, t) {
            self.close();
            return;
        }
        if self.export_button.touch(touch, t) {
            self.export_requested = true;
            return;
        }
        self.scroll.touch(touch, t);
    }

    pub fn update(&mut self, t: f32) {
        if self.open {
            self.scroll.update(t);
            for card in &mut self.cards {
                if let Some(illustration) = &mut card.illustration {
                    illustration.settle(t);
                }
            }
        }
    }

    pub fn render_entry(&mut self, ui: &mut Ui, t: f32, rect: Rect, n: usize) {
        self.entry_active = true;
        let size = rect.h * 3.2;
        self.entry_button.render_text(ui, rect, t, format!("查看 B{n}"), size, false);
    }

    /// 这一帧没画入口按钮时调用，免得 touch 命中上一帧的位置。
    pub fn hide_entry(&mut self) {
        self.entry_active = false;
        self.entry_button.invalidate();
    }

    pub fn render(&mut self, ui: &mut Ui, t: f32) {
        if !self.open {
            return;
        }
        ui.fill_rect(ui.screen_rect(), BOARD_BG);
        let width = BOARD_WIDTH;
        let top = -ui.top + 0.035;
        self.render_header(ui, top, -width / 2., width / 2. - 0.39);
        self.export_button
            .render_text(ui, Rect::new(width / 2. - 0.37, top, 0.18, 0.07), t, "导出图片", 0.34, false);
        self.close_button
            .render_text(ui, Rect::new(width / 2. - 0.17, top, 0.17, 0.07), t, "关闭", 0.4, false);
        let viewport_height = (ui.top * 2. - 0.24).max(0.05);
        let content_height = self.cards_height().max(viewport_height);
        let offset = self.scroll.y_scroller.offset;
        self.scroll.size((width, viewport_height));
        ui.scope(|ui| {
            ui.dx(-width / 2.);
            ui.dy(top + 0.1);
            self.scroll.render(ui, |ui| {
                Self::render_cards(&mut self.cards, ui, t, offset, viewport_height);
                (width, content_height)
            });
        });
        let footer_y = ui.top - 0.055;
        ui.text("Best Board").pos(0., footer_y).anchor(0.5, 0.5).size(0.46).color(WHITE).draw();
    }
    fn cards_height(&self) -> f32 {
        if self.cards.is_empty() {
            0.20
        } else {
            self.cards.len().div_ceil(3) as f32 * (CARD_HEIGHT + GAP) - GAP
        }
    }
    /// 整行居中；空间不够时先缩短玩家名。
    fn render_header(&self, ui: &mut Ui, y: f32, left: f32, right: f32) {
        let size = 0.6;
        let measure = |ui: &mut Ui, text: &str| ui.text(text).size(size).measure().w;
        let middle = format!(" · RKS {:.4}{}", self.rks, if self.course_badge.is_some() { " · " } else { "" });
        let end = format!(" · B{}", self.cards.len());
        let middle_width = measure(ui, &middle);
        let end_width = measure(ui, &end);
        let badge_width = self.course_badge.as_ref().map(|badge| (measure(ui, &badge.label()) + 0.04).max(0.10)).unwrap_or(0.);
        let fixed_width = middle_width + badge_width + end_width;
        let name = fit_header_name(ui, &self.player, size, (right - left - fixed_width).max(0.001));
        let name_width = measure(ui, &name);
        let total = name_width + fixed_width;
        let mut x = (-total / 2.).min(right - total).max(left);
        let center_y = y + 0.035;
        ui.text(&name).pos(x, center_y).anchor(0., 0.5).no_baseline().size(size).color(WHITE).draw();
        x += name_width;
        ui.text(&middle).pos(x, center_y).anchor(0., 0.5).no_baseline().size(size).color(WHITE).draw();
        x += middle_width;
        if let Some(badge) = &self.course_badge {
            crate::challenge_ui::render_display_badge(ui, Rect::new(x, center_y - 0.0275, badge_width, 0.055), badge);
            x += badge_width;
        }
        ui.text(&end).pos(x, center_y).anchor(0., 0.5).no_baseline().size(size).color(WHITE).draw();
    }
    pub fn export_width(&self) -> f32 {
        BOARD_WIDTH
    }
    pub fn export_height(&self) -> f32 {
        0.135 + self.cards_height() + 0.11
    }
    pub fn illustrations_ready(&self, t: f32) -> bool {
        let mut ready = true;
        for illu in self.cards.iter().filter_map(|card| card.illustration.as_ref()) {
            illu.notify();
            ready &= illu.alpha(t) >= 1.;
        }
        ready
    }
    /// 整张长图的绘制入口（由 `best_board_export` 分块调用）。
    pub fn render_export(&mut self, ui: &mut Ui, t: f32, offset: f32) {
        ui.fill_rect(ui.screen_rect(), BOARD_BG);
        let y = -ui.top - offset;
        self.render_header(ui, y + 0.035, -BOARD_WIDTH / 2., BOARD_WIDTH / 2.);
        let viewport_height = ui.top * 2.;
        ui.scope(|ui| {
            ui.dx(-BOARD_WIDTH / 2.);
            ui.dy(y + 0.135);
            Self::render_cards(&mut self.cards, ui, t, offset - 0.135, viewport_height);
        });
        ui.text("Best Board")
            .pos(0., y + self.export_height() - 0.055)
            .anchor(0.5, 0.5)
            .size(0.46)
            .color(WHITE)
            .draw();
    }
    fn render_cards(cards: &mut [Card], ui: &mut Ui, t: f32, offset: f32, viewport_height: f32) {
        let gap = GAP;
        let card_width = CARD_WIDTH;
        let card_height = CARD_HEIGHT;
        let width = BOARD_WIDTH;
        let title_size = 18. * FONT_SCALE * FONT_UNIT;
        let score_size = 20. * FONT_SCALE * FONT_UNIT;
        let detail_size = 16. * FONT_SCALE * FONT_UNIT;
        for (index, card) in cards.iter_mut().enumerate() {
            let x = (index % 3) as f32 * (card_width + gap);
            let y = (index / 3) as f32 * (card_height + gap);
            if y + card_height < offset || y > offset + viewport_height {
                continue;
            }
            let rect = Rect::new(x, y, card_width, card_height);
            // ---------- 圆角 + 磨砂玻璃卡片 ----------
            // 卡面（半透明底 / 浅边 / 顶部高光 / 磨砂颗粒）由 `frosted_card` 一次画完。
            // 封面**不改几何尺寸**，而是用「卡片那个圆角矩形」做裁剪：
            // 封面只占卡片左侧，右缘是卡片内部的一条直边 —— 裁剪后左边两角跟着卡片变圆，
            // 右边两角落在卡片内部、本来就看不出来，正好是想要的效果。
            // （不能只把底色画圆：封面是直角矩形且贴着卡片左缘，会把左边两角整个盖住。）
            frosted_card(ui, rect, CARD_RADIUS);
            let cover = Rect::new(x, y, card_height * 2., card_height);
            if let Some(illustration) = &card.illustration {
                illustration.notify();
                let shading = illustration.shading(cover, t);
                clip_rounded_rect(ui, rect, CARD_RADIUS, |ui| ui.fill_rect(cover, shading));
            } else {
                clip_rounded_rect(ui, rect, CARD_RADIUS, |ui| {
                    ui.fill_rect(cover, COVER_BG);
                    ui.text("♪").pos(cover.center().x, cover.center().y).anchor(0.5, 0.5).size(0.8).color(WHITE).draw();
                });
            }
            let tx = cover.right() + 10. * UNIT;
            let text_width = rect.right() - tx - 8. * UNIT;
            let level_size = detail_size * 0.85;
            let level_width = ui.text(card.level).size(level_size).measure().w;
            let ap_width = if card.entry.ap { 34. * FONT_SCALE * UNIT } else { 0. };
            let title = ui
                .text(&card.entry.name)
                .pos(tx, y + 24. * UNIT)
                .anchor(0., 1.)
                .size(title_size)
                .max_width((text_width - ap_width - level_width - 5. * UNIT).max(0.001))
                .color(WHITE)
                .draw();
            ui.text(card.level)
                .pos(tx + title.w + 5. * UNIT, y + 24. * UNIT)
                .anchor(0., 1.)
                .size(level_size)
                .color(WHITE)
                .draw();
            if card.entry.ap {
                // AP 标记：按「文字全白」约束用纯白（区分靠文字本身，不用彩色）
                ui.text("AP")
                    .pos(rect.right() - 8. * UNIT, y + 24. * UNIT)
                    .anchor(1., 1.)
                    .size(detail_size)
                    .color(WHITE)
                    .draw();
            }
            let score = card.score.map_or_else(|| "—".into(), |score| format!("{score:07}"));
            let score_rect = ui
                .text(score)
                .pos(tx, y + 50. * UNIT)
                .anchor(0., 1.)
                .size(score_size)
                .max_width(text_width)
                .color(WHITE)
                .draw();
            let acc_x = tx + score_rect.w + 7. * UNIT;
            ui.text(format!("ACC {:.2}%", card.entry.accuracy))
                .pos(acc_x, y + 50. * UNIT)
                .anchor(0., 1.)
                .size(detail_size)
                .max_width((rect.right() - 8. * UNIT - acc_x).max(0.001))
                .color(WHITE)
                .draw();
            ui.text(format!("{:.1} | {:.2}", card.entry.difficulty, card.entry.rks))
                .pos(tx, y + 74. * UNIT)
                .anchor(0., 1.)
                .size(detail_size)
                .max_width(text_width)
                .color(WHITE)
                .draw();
        }
        if cards.is_empty() {
            ui.text("暂无可计入的成绩")
                .pos(width / 2., 0.1)
                .anchor(0.5, 0.)
                .size(0.5)
                .color(WHITE)
                .draw();
        }
    }
}

/// 按字符边界缩短名字（用真实字形宽度量）。
fn fit_header_name(ui: &mut Ui, name: &str, size: f32, width: f32) -> String {
    if ui.text(name).size(size).measure().w <= width {
        return name.to_owned();
    }
    if ui.text("…").size(size).measure().w > width {
        return String::new();
    }
    let ends: Vec<_> = name.char_indices().map(|(i, _)| i).chain(std::iter::once(name.len())).collect();
    let (mut low, mut high) = (0, ends.len() - 1);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if ui.text(format!("{}…", &name[..ends[mid]])).size(size).measure().w <= width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    format!("{}…", &name[..ends[low]])
}
