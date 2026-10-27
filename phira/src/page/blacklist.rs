prpr_l10n::tl_file!("blacklist");

// 玩家黑名单管理页：查看 / 解除 / 手动输入 Phira 数字 ID 添加。
// 移植自 Phira Pro（`baiji666/Phira-Pro`，GPL-3.0-only）的 `phira/src/page/blacklist.rs`：
// 去掉了那边 HUD 自定义编辑器的槽位依赖，改回本仓库的固定布局。
// （注：模块文档注释不能写成 //! —— tl_file! 展开后必须是文件开头）

use super::{Page, SharedState};
use crate::{blacklist, client::UserManager};
use anyhow::Result;
use inputbox::InputBox;
use macroquad::prelude::*;
use prpr::{
    ext::RectExt,
    scene::{request_input, return_input, show_error, show_message, take_input},
    ui::{DRectButton, Scroll, Ui},
};
use std::borrow::Cow;

const ROW_H: f32 = 0.085;
const GAP: f32 = 0.012;

pub struct BlacklistPage {
    entries: Vec<blacklist::Entry>,
    scroll: Scroll,
    btn_add: DRectButton,
    /// 每行一个「解除」按钮（渲染时 set，触摸时用）。
    row_btns: Vec<DRectButton>,
}

impl BlacklistPage {
    pub fn new() -> Self {
        blacklist::ensure_loaded();
        let entries = blacklist::all();
        for e in &entries {
            UserManager::request(e.id);
        }
        Self {
            entries,
            scroll: Scroll::new(),
            btn_add: DRectButton::new().with_radius(0.008),
            row_btns: Vec::new(),
        }
    }

    fn reload(&mut self) {
        self.entries = blacklist::all();
        for e in &self.entries {
            UserManager::request(e.id);
        }
    }
}

impl Page for BlacklistPage {
    fn label(&self) -> Cow<'static, str> {
        tl!("label")
    }

    fn update(&mut self, s: &mut SharedState) -> Result<()> {
        self.scroll.update(s.t);
        if let Some((id, text)) = take_input() {
            if id == "blacklist-add" {
                match text.trim().parse::<i32>() {
                    Ok(v) if v > 0 => {
                        // 昵称尽量用在线解析结果；拿不到就留空，管理页会退回显示 ID。
                        let name = UserManager::name_and_color(v).map(|(n, _)| n).unwrap_or_default();
                        match blacklist::add(v, &name) {
                            Ok(true) => {
                                self.reload();
                                show_message(tl!("add-done")).ok();
                            }
                            Ok(false) => {
                                self.reload();
                                show_message(tl!("add-exists")).ok();
                            }
                            Err(err) => show_error(err.context(tl!("save-failed"))),
                        }
                    }
                    _ => show_error(anyhow::anyhow!(tl!("add-invalid").into_owned())),
                }
            } else {
                return_input(id, text);
            }
        }
        Ok(())
    }

    fn touch(&mut self, touch: &Touch, s: &mut SharedState) -> Result<bool> {
        let t = s.t;
        if self.scroll.touch(touch, t) {
            return Ok(true);
        }
        if self.btn_add.touch(touch, t) {
            request_input("blacklist-add", InputBox::new().title(tl!("add-title")).prompt(tl!("add-hint")));
            return Ok(true);
        }
        // 顺序要和渲染时一致
        let ids: Vec<i32> = self.entries.iter().map(|it| it.id).collect();
        for (i, btn) in self.row_btns.iter_mut().enumerate() {
            if btn.touch(touch, t) {
                if let Some(id) = ids.get(i) {
                    if let Err(err) = blacklist::remove(*id) {
                        show_error(err.context(tl!("save-failed")));
                    } else {
                        self.reload();
                        show_message(tl!("removed")).ok();
                    }
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn render(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        // kaichang 玻璃语言：标题带字距、行卡是磨砂玻璃（浅色主题下深色字）
        let m = crate::newui::Metrics::new(ui);
        let th = crate::newui::theme();
        let (mx, my) = crate::newui::mouse_world();
        let mouse = vec2(mx, my);
        let mut cr = ui.content_rect();
        // 活动栏删除后 content_rect 已经是 -0.94..0.94；这里只再收一点点做**对称**内边距。
        cr.x += 0.02;
        cr.w -= 0.04;

        s.render_fader(ui, |ui| {
            // HUD 自定义布局：列表大框 / 行高参数
            let outer = crate::hud::slot_or(ui, "blacklist", "list", crate::hud::Cap(true, true, true), cr.feather(-0.005));
            let row_h = crate::hud::param("blacklist", "row_h", ROW_H).clamp(0.05, 0.2);
            self.scroll.size((outer.w, outer.h));
            ui.dx(outer.x);
            ui.dy(outer.y);
            self.scroll.render(ui, |ui| {
                let w = outer.w;
                let mut y = 0.;

                // ---------------- 顶部：添加 ----------------
                self.btn_add.render_text(ui, Rect::new(0., y, w, 0.085), t, tl!("add"), 0.5, false);
                y += 0.085 + GAP;

                // ---------------- 标题 ----------------
                // 标题压在页面底（不是面板）上 → 必须跟主题，浅色主题下不能是白的
                let ink = |a: f32| prpr::ui::theme::ink(a);
                let ttl = crate::newui::text_tracked(ui, &m, &tl!("label"), 0.004, y + 0.065, 30., ink(1.), 2., (0., 0.5), true);
                let n = self.entries.len();
                crate::newui::text(
                    ui,
                    &m,
                    tl!("count", "count" => n.to_string()),
                    w - 0.004,
                    ttl.center().y,
                    15.,
                    ink(0.55),
                    (1., 0.5),
                );
                // 标题下的分隔线（浅色主题加黑 / 深色主题加白）
                crate::newui::rounded(ui, Rect::new(0.004, ttl.bottom() + 0.012, w - 0.008, 0.0016), 0.0008, th.wash(0.10));
                y = ttl.bottom() + 0.03;

                if n == 0 {
                    crate::newui::text_center(ui, &m, tl!("empty"), w / 2., y + 0.04, 20., ink(0.5));
                    y += 0.12;
                    return (w, y + GAP);
                }

                while self.row_btns.len() < n {
                    self.row_btns.push(DRectButton::new().with_radius(0.012));
                }

                let remove_label = tl!("remove").into_owned();
                // ---- iOS 分组列表（inset grouped）----
                // 整组**共用一个圆角容器**，行与行紧贴、靠内缩发丝线分隔。
                // 原来是「每行一张独立圆角卡 + 8 单位间隙」，那是卡片语言。
                // ⚠️ 容器必须先铺：这是即时模式，晚画会盖住已经画好的行内容。
                let group_radius = 0.014;
                crate::newui::group_plate(ui, &th, Rect::new(0., y, w, row_h * n as f32), group_radius);
                for (i, entry) in self.entries.iter().enumerate() {
                    let r = Rect::new(0., y, w, row_h);
                    let hp = if ui.rect_to_global(r).contains(mouse) { 1. } else { 0. };
                    // 行自己不填底板（底板来自 `group_plate`），只叠悬停 + 发丝线；
                    // 分隔线内缩到文字左缘（`#id` 从 0.026 开始），对齐 iOS 的做法。
                    crate::newui::group_row(ui, &th, r, hp, i == 0, i + 1 == n, group_radius, 0.026, Color::new(0., 0., 0., 0.));
                    // 左侧强调条：跟主题的强调色（原来是一条死黑的 ink 竖条）
                    crate::newui::accent_bar(
                        ui,
                        Rect::new(r.x + 0.006, r.y + 0.013, 0.007, r.h - 0.026),
                        th.glow,
                        th.teal,
                        0.0035,
                    );
                    let cy = r.center().y;
                    crate::newui::text(ui, &m, format!("#{}", entry.id), r.x + 0.026, cy, 16., ink(0.6), (0., 0.5));
                    let name = UserManager::name_and_color(entry.id)
                        .map(|(n, _)| n)
                        .filter(|it| !it.is_empty())
                        .unwrap_or_else(|| entry.name.clone());
                    crate::newui::text_ellipsis(
                        ui,
                        &m,
                        if name.is_empty() { "—".to_owned() } else { name },
                        r.x + 0.30,
                        cy,
                        15.,
                        r.w - 0.30 - 0.26,
                        ink(0.92),
                        (0., 0.5),
                    );
                    let br = Rect::new(r.right() - 0.235, cy - 0.032, 0.22, 0.064);
                    self.row_btns[i].render_text_color(ui, br, t, remove_label.as_str(), 0.4, false, RED);
                    y += row_h;
                }
                (w, y + GAP)
            });
        });
        Ok(())
    }
}
