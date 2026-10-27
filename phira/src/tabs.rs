use crate::{anim::Anim, get_data, Result};
use macroquad::prelude::*;
use prpr::{
    ext::RectExt,
    ui::{button_hit, RectButton, Ui},
};
use std::borrow::Cow;

pub type TitleFn = fn() -> Cow<'static, str>;

/// 标签栏样式。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TabStyle {
    /// 默认：竖屏横排圆角块 / 横屏左侧竖栏（iOS 分组观感）。
    Default,
    /// **Phigros 设置页**：顶部横排**斜切标签**，选中 = 白色斜块 + 黑粗字
    /// （参考 `D:\Phigros\kaichang\Setting.html`）。
    Phigros,
}

struct TabItem<T> {
    value: T,
    title: TitleFn,
    btn: RectButton,
}

/// 量出一组 Phigros 标签的宽度（文字宽 + 左右内边距）。
///
/// 抽成自由函数是为了能在「超出可用宽度时整体缩小」那一步**重测一次**
/// （闭包会长期借用 `ui`，和后面的渲染打架）。
fn tab_widths<T>(
    ui: &mut Ui,
    items: &[TabItem<T>],
    selected: usize,
    fs_idle: f32,
    fs_act: f32,
    pad_idle: f32,
    pad_act: f32,
) -> Vec<f32> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let (fs, pad) = if i == selected { (fs_act, pad_act) } else { (fs_idle, pad_idle) };
            ui.text((item.title)()).size(fs).no_baseline().measure().w + pad * 2.
        })
        .collect()
}

pub struct Tabs<T> {
    items: Vec<TabItem<T>>,
    selected: usize,

    /// 渲染样式（见 [`TabStyle`]）。
    style: TabStyle,

    /// 竖排（左侧分类栏）还是横排（顶部分类条）。
    ///
    /// 竖屏（`Metrics::compact`）下改用横排：竖排在手机宽度上会吃掉 1/3 屏宽，
    /// 网格只剩一条缝。横排铺满整宽、只占一行高度，是移动端该有的样子。
    horizontal: bool,

    /// 标签文字缩放（默认 1.0）。曲库页顶部整体放大时由 `set_label_scale` 设置。
    label_scale: f32,

    /// 选中块的动画区间：竖排时是 `[上, 下]`，横排时是 `[左, 右]`。
    y_upper: Anim<f32>,
    y_lower: Anim<f32>,

    content_progress: Anim<f32>,
    prev_go_up: bool,
    prev: usize,

    changed: bool,
}

impl<T> Tabs<T> {
    #[allow(dead_code)]
    const LEFT: f32 = -0.88;
    #[allow(dead_code)]
    const WIDTH: f32 = 0.17;
    const DURATIONS: (f32, f32) = (0.24, 0.35);
    const CONTENT_DY: f32 = 0.06;
    const CONTENT_DURATION: f32 = 0.4;
    /// 竖排每项高度（世界坐标）。与原实现一致。
    const ITEM_H: f32 = 0.155;

    pub fn new(items: impl IntoIterator<Item = (T, TitleFn)>) -> Self {
        Tabs {
            items: items
                .into_iter()
                .map(|(value, title)| TabItem {
                    value,
                    title,
                    btn: RectButton::new(),
                })
                .collect(),
            selected: 0,
            style: TabStyle::Default,
            label_scale: 1.,
            horizontal: false,

            y_upper: Anim::new(0.),
            y_lower: Anim::new(0.),

            content_progress: Anim::new(1.),
            prev_go_up: false,
            prev: 0,

            changed: false,
        }
    }

    /// 切换横排 / 竖排（竖屏用横排，见字段注释）。
    pub fn set_horizontal(&mut self, v: bool) {
        self.horizontal = v;
    }

    /// 切换标签栏样式（见 [`TabStyle`]）。
    pub fn set_style(&mut self, v: TabStyle) {
        self.style = v;
    }

    /// 标签文字缩放（默认 1.0）。
    ///
    /// 曲库页顶部三行整体放大时，标签条里的字也要跟着放大，否则会出现
    /// 「条变高了、字还是那么小」的错位。量宽度（`label_widths`）与真正绘制
    /// 使用**同一个**缩放，否则栏宽会和实际文字对不上。
    pub fn set_label_scale(&mut self, v: f32) {
        self.label_scale = v;
    }

    /// 竖排时每一项的高度（世界坐标）：设计稿 0.155，但必须能塞进 `strip`。
    ///
    /// 超宽比例（21:9）下屏幕的世界高度只有 0.86，5 项 ×0.155 = 0.775
    /// 加上头部会把最后一项（热门）顶到状态栏外面 —— 所以按可用高度等分。
    fn item_h(&self, strip: Rect) -> f32 {
        let n = self.items.len().max(1) as f32;
        let avail = (strip.h - 0.02).max(0.04);
        Self::ITEM_H.min(avail / n)
    }

    /// 第 `index` 项的矩形。竖排从 `strip.y + 0.02` 往下排，横排把 `strip.w` 等分。
    fn item_rect(&self, strip: Rect, index: usize) -> Rect {
        let n = self.items.len().max(1) as f32;
        if self.horizontal {
            let w = strip.w / n;
            Rect::new(strip.x + w * index as f32, strip.y, w, strip.h)
        } else {
            let h = self.item_h(strip);
            Rect::new(strip.x, strip.y + 0.02 + h * index as f32, strip.w, h)
        }
    }

    pub fn selected(&self) -> &T {
        &self.items[self.selected].value
    }

    pub fn selected_mut(&mut self) -> &mut T {
        &mut self.items[self.selected].value
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.items.iter_mut().map(|item| &mut item.value)
    }

    /// 各个标签的文字宽度（世界坐标），用竖排时实际使用的字号量。
    ///
    /// 给「左侧分类栏多宽」用：原来调用方硬编码 `m.u(160.)`，比实际标签宽出一大截，
    /// 分类栏看着又粗又空。按真实文字量出来，栏宽就能正好贴着最长标签。
    pub fn label_widths(&self, ui: &mut Ui) -> Vec<f32> {
        self.items
            .iter()
            .map(|item| ui.text((item.title)()).size(0.5 * self.label_scale).no_baseline().measure().w)
            .collect()
    }

    pub fn changed(&mut self) -> bool {
        let changed = self.changed;
        self.changed = false;
        changed
    }

    pub fn goto(&mut self, t: f32, index: usize) {
        if index == self.selected {
            return;
        }

        let (mut upper, mut lower) = if get_data().prefer_reduced_motion { (0., 0.) } else { Self::DURATIONS };
        if index > self.selected {
            std::mem::swap(&mut upper, &mut lower);
            self.prev_go_up = true;
        } else {
            self.prev_go_up = false;
        }

        self.prev = self.selected;
        self.selected = index;
        self.y_upper.begin(t, upper);
        self.y_lower.begin(t, lower);
        self.content_progress.start(0., 1., t, Self::CONTENT_DURATION);

        self.changed = true;
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> bool {
        for (index, item) in self.items.iter_mut().enumerate() {
            if item.btn.touch(touch) {
                button_hit();
                self.goto(t, index);
                return true;
            }
        }

        false
    }

    fn render_plain(&mut self, ui: &mut Ui, c: Color, first: bool, strip: Rect) {
        let th = crate::newui::theme();
        let horizontal = self.horizontal;
        let step = self.item_h(strip);
        let mut r = self.item_rect(strip, 0);
        for (index, item) in self.items.iter_mut().enumerate() {
            // 选中块的区间（竖排 = 上下，横排 = 左右）
            if index == self.selected {
                if horizontal {
                    self.y_upper.alter_to(r.x);
                    self.y_lower.alter_to(r.right());
                } else {
                    self.y_upper.alter_to(r.y);
                    self.y_lower.alter_to(r.bottom());
                }
            }
            item.btn.set(ui, r);
            if first {
                // 未选中项：**圆角块**，不是一整条深色大板 ——
                // 原来每项都铺 `semi_black(0.4)`，相邻项连成一块硬边黑砖。
                let bg = if th.dark {
                    Color { a: 0.5 * c.a, ..th.bg_elev }
                } else {
                    Color { a: 0.75 * c.a, ..th.white(1.) }
                };
                crate::newui::rounded(ui, r.feather(-0.005), 0.010, bg);
                ui.stroke_path(&r.feather(-0.005).rounded(0.010), 0.0015, Color { a: 0.9 * c.a, ..th.border });
            }
            // 标签文字完整显示：加宽标签条后不再截断（原 max_width 会砍掉「谱面UI」等长标签）
            ui.text((item.title)())
                .pos(r.center().x, r.center().y)
                .anchor(0.5, 0.5)
                .no_baseline()
                .max_width(r.w - 0.015)
                .size(if horizontal { 0.45 } else { 0.5 } * self.label_scale)
                .color(c)
                .draw();
            if horizontal {
                r.x += r.w;
            } else {
                r.y += step;
            }
        }
    }

    /// **Phigros 样式**：顶部横排斜切标签 + 扁平内容区。
    ///
    /// 参考 `D:\Phigros\kaichang\Setting.html`：每个标签是一块平行四边形
    /// （CSS `skewX(-15deg)`），未选中 = 灰字、选中 = 白色斜块 + 黑粗字，
    /// 内容区直接铺在页面底上（不套面板）。
    fn render_phigros(
        &mut self,
        ui: &mut Ui,
        t: f32,
        strip: Rect,
        cr: Rect,
        off: Vec2,
        mut f: impl FnMut(&mut Ui, &mut T) -> Result<()>,
    ) -> Result<()> {
        let th = crate::newui::theme();
        let m = crate::newui::Metrics::new(ui);
        let n = self.items.len();
        if n == 0 {
            return Ok(());
        }

        // 设计稿 px → 世界坐标
        let mut fs_idle = m.f(20.);
        let mut fs_act = m.f(26.);
        let mut pad_idle = m.u(24.);
        let mut pad_act = m.u(42.);
        let mut h_idle = m.u(44.);
        let mut h_act = m.u(64.);
        let mut gap = m.u(8.);

        // 每项宽度 = 文字宽 + 左右内边距（选中项字号更大、内边距更宽）
        let mut widths = tab_widths(ui, &self.items, self.selected, fs_idle, fs_act, pad_idle, pad_act);
        let mut total: f32 = widths.iter().sum::<f32>() + gap * (n.saturating_sub(1)) as f32;
        // 8 个标签在窄窗口下会顶到返回键 —— 超出就整体等比缩小（字号 / 内边距 / 高度）。
        let max_w = strip.w * 0.92;
        if total > max_w && total > 1e-6 {
            let s = (max_w / total).max(0.45);
            fs_idle *= s;
            fs_act *= s;
            pad_idle *= s;
            pad_act *= s;
            h_idle *= s;
            h_act *= s;
            gap *= s;
            widths = tab_widths(ui, &self.items, self.selected, fs_idle, fs_act, pad_idle, pad_act);
            total = widths.iter().sum::<f32>() + gap * (n.saturating_sub(1)) as f32;
        }

        // ≈ tan(19°)；参考图是 15°，视觉上略微加陡一点在暗背景上更立得住。
        let skew = h_act * 0.34;
        let cy = strip.center().y;
        let x0 = strip.center().x - total * 0.5;

        // 标签栏跟随 HUD 槽位偏移（内容区不跟随，与原实现一致）
        ui.scope(|ui| {
            ui.dx(off.x);
            ui.dy(off.y);

            // 命中区：用未斜切的包围盒（斜切只差几像素边角，够用）
            let mut cx = x0;
            for (i, item) in self.items.iter_mut().enumerate() {
                let w = widths[i];
                item.btn.set(ui, Rect::new(cx, cy - h_idle * 0.5, w, h_idle));
                cx += w + gap;
            }

            // 未选中：灰字
            let mut cx = x0;
            for (i, item) in self.items.iter().enumerate() {
                let w = widths[i];
                if i != self.selected {
                    let r = Rect::new(cx, cy - h_idle * 0.5, w, h_idle);
                    ui.text((item.title)())
                        .pos(r.center().x, r.center().y)
                        .anchor(0.5, 0.5)
                        .no_baseline()
                        .max_width(w - m.u(6.))
                        .size(fs_idle)
                        .color(th.text_dim)
                        .draw();
                }
                cx += w + gap;
            }

            // 选中项：白色斜块 + 黑粗字（最后画，压住相邻标签）
            let sel = self.selected;
            let w = widths[sel];
            let x = x0 + widths[..sel].iter().sum::<f32>() + gap * sel as f32;
            let r = Rect::new(x, cy - h_act * 0.5, w, h_act);
            crate::newui::skew_rect(ui, r.feather(m.u(5.)), skew, Color::new(1., 1., 1., 0.12));
            crate::newui::skew_rect(ui, r, skew, Color::new(1., 1., 1., 1.));
            ui.text((self.items[sel].title)())
                .pos(r.center().x, r.center().y)
                .anchor(0.5, 0.5)
                .no_baseline()
                .max_width(w - m.u(8.))
                .size(fs_act)
                .color(Color::new(0., 0., 0., 1.))
                .draw();
        });

        // 内容区：扁平（不铺面板），只保留切换过渡
        let p = if get_data().prefer_reduced_motion {
            1.
        } else {
            self.content_progress.now(t)
        };
        ui.scissor::<Result<()>>(cr, |ui| {
            if p < 1. {
                ui.scope(|ui| {
                    let dy = Self::CONTENT_DY * p;
                    ui.dy(if self.prev_go_up { -dy } else { dy });
                    ui.alpha(1. - p, |ui| f(ui, &mut self.items[self.prev].value))
                })?;
            }
            ui.scope(|ui| {
                let dy = Self::CONTENT_DY * (1. - p);
                ui.dy(if self.prev_go_up { dy } else { -dy });
                ui.alpha(p, |ui| f(ui, &mut self.items[self.selected].value))
            })?;
            Ok(())
        })?;

        Ok(())
    }

    pub fn render(&mut self, ui: &mut Ui, t: f32, page: &'static str, strip: Rect, cr: Rect, mut f: impl FnMut(&mut Ui, &mut T) -> Result<()>) -> Result<()> {
        // HUD 自定义布局：tab 栏整体可移动（key 由调用方给固定页面 key，避免非编辑模式下串页）。
        // 编辑模式下还能直接切分页。
        let slot = crate::hud::slot_or(ui, page, "tabs", crate::hud::Cap(true, true, false), strip);
        crate::hud::set_tab_info(self.items.len(), self.selected);
        if let Some(idx) = crate::hud::take_tab_request() {
            if idx < self.items.len() {
                self.goto(t, idx);
            }
        }
        let off = vec2(slot.x - strip.x, slot.y - strip.y);

        // Phigros 样式走独立分支（顶部斜切标签 + 扁平内容）
        if self.style == TabStyle::Phigros {
            return self.render_phigros(ui, t, slot, cr, off, f);
        }

        let th = crate::newui::theme();

        // 整个 tab 栏画在带偏移的坐标空间里（渲染与命中一起跟随槽位）
        ui.scope(|ui| {
            ui.dx(off.x);
            ui.dy(off.y);

            self.render_plain(ui, th.text_dim, true, strip);

            let y_upper = self.y_upper.now(t);
            let y_lower = self.y_lower.now(t);
            let horizontal = self.horizontal;
            // ⚠️ 这里原来调 `rounded_rect_shadow(base: 0.5)`，画出来是选中块旁边
            // 一条**硬边灰带**（不是柔和阴影），非常显眼。直接去掉，用主题色填充。
            let r = if horizontal {
                // 横排：区间是左右，选中块上下略收一点（feather 的 y 是负 = 内缩）
                Rect::new(y_upper, strip.y, y_lower - y_upper, strip.h).nonuniform_feather(-0.004, 0.005)
            } else {
                Rect::new(strip.x, y_upper, strip.w, y_lower - y_upper).nonuniform_feather(0.005, -0.012)
            };
            crate::newui::rounded(ui, r, 0.010, if th.dark { th.bg_hover } else { th.white(1.) });
            if y_upper < y_lower {
                // 选中项强调条：竖排贴在左边，横排贴在底边（2px 视觉宽度）
                let bar = if horizontal {
                    Rect::new(r.x + r.w * 0.22, r.bottom() - 0.0035, r.w * 0.56, 0.0025)
                } else {
                    Rect::new(r.x, r.center().y - r.h * 0.28, 0.0025, r.h * 0.56)
                };
                crate::newui::accent_bar(ui, bar, th.glow, th.teal, 0.0012);
            }
            ui.scissor(r, |ui| self.render_plain(ui, th.text_hi, false, strip));
        });

        // ⚠️ 这里原来写死 `semi_black(0.4)` —— 浅色主题下整个内容区会变成一块脏灰
        // （用户原话：「和大便没什么区别」）。改成磨砂亚克力面板，跟着主题走。
        crate::newui::acrylic(ui, cr, 0.008, &th);
        ui.scissor::<Result<()>>(cr, |ui| {
            let p = if get_data().prefer_reduced_motion {
                1.
            } else {
                self.content_progress.now(t)
            };
            if p < 1. {
                ui.scope(|ui| {
                    let dy = Self::CONTENT_DY * p;
                    ui.dy(if self.prev_go_up { -dy } else { dy });
                    ui.alpha(1. - p, |ui| f(ui, &mut self.items[self.prev].value))
                })?;
            }

            ui.scope(|ui| {
                let dy = Self::CONTENT_DY * (1. - p);
                ui.dy(if self.prev_go_up { dy } else { -dy });
                ui.alpha(p, |ui| f(ui, &mut self.items[self.selected].value))
            })?;

            Ok(())
        })?;

        Ok(())
    }
}
