prpr_l10n::tl_file!("history");

// 成绩历史页：列表 / 趋势图 / PB 对比 / 判定分布对比 / 筛选 / 导入导出。
// （注：模块文档注释不能写成 //! —— tl_file! 展开后必须是文件开头）

use super::{Page, SharedState};
use crate::get_data;
use crate::history::{self, Record};
use anyhow::Result;
use macroquad::prelude::*;
use prpr::{
    ext::{semi_white, RectExt},
    scene::{request_file, show_error, show_message, take_file},
    ui::{
        theme::{space, text_faint, text_primary, text_secondary, text_size},
        DRectButton, RectButton, Scroll, Ui,
    },
};
use std::{borrow::Cow, io::Write};

const ROW_H: f32 = 0.082;
const GAP: f32 = 0.014;
const CARD_BG: f32 = 0.34;
/// 卡片内边距（`card()` 用的就是它，卡片高度也按它反推）
const CARD_PAD: f32 = 0.018;

// ---- 排版尺度（2026-09-30 修正）--------------------------------------------
// 这一页原来的字号比全 App 小约 4 倍（页面标题 0.115，而设置页标题是 0.6），
// 1080p 下正文只有 ~3px 高。下面这组值按「容器高度」反推：
//   世界坐标里 1 单位 ≈ 视口高/2 px，而字号 px = 0.04 * size * 视口宽，
//   所以文字占容器的高度比 ≈ 0.08 * size * (视口宽/高)。
// 取手机 16:9 横屏（2.22）时不超过容器高的 ~75%，桌面（1.6）约 50%。
// 层级同时靠字号与 alpha（theme::text_alpha），不靠堆边框。
const SZ_TITLE: f32 = 0.50; // 页面标题
const SZ_ROW: f32 = text_size::CAPTION; // 0.35 列表行主数值（分数）
const SZ_ROW2: f32 = 0.30; // 行内次级列（ACC）
const SZ_ROW3: f32 = 0.26; // 行内其余列 / FC 徽标
const SZ_CARD_VALUE: f32 = 0.32; // 统计卡主数值
const SZ_CARD_LABEL: f32 = 0.20; // 卡片小标题 / 单位 / 图例
const SZ_BODY: f32 = 0.34; // 正文（空状态 / 每日挑战谱面名）
const SZ_CAP: f32 = 0.24; // 坐标轴 / 页脚说明

/// 分数对应的段位色（列表左侧色条 / 分数本身 / 趋势折线）
fn score_color(score: u32) -> Color {
    match score {
        s if s >= 1_000_000 => Color::new(1.00, 0.86, 0.42, 1.),
        s if s >= 960_000 => Color::new(0.96, 0.62, 1.00, 1.),
        s if s >= 920_000 => Color::new(0.58, 0.78, 1.00, 1.),
        s if s >= 880_000 => Color::new(0.58, 0.96, 0.72, 1.),
        s if s >= 820_000 => Color::new(0.98, 0.92, 0.58, 1.),
        _ => Color::new(0.72, 0.72, 0.76, 1.),
    }
}

/// 画一张圆角卡片，返回内容区。
///
/// 卡面 = 磨砂玻璃（kaichang `.glass-btn` 那套：半透明底 + 1px 浅边 + 顶部高光），
/// 原来是 `semi_black(0.34)` 这种"半透明黑砖"，浅色主题下会变成一块脏灰。
fn card(ui: &mut Ui, r: Rect) -> Rect {
    let th = crate::newui::theme();
    crate::newui::acrylic(ui, r, 0.014, &th);
    r.feather(-CARD_PAD)
}

pub struct HistoryPage {
    records: Vec<Record>,
    /// `None` = 全部谱面；`Some(key)` = 只看这一首
    selected: Option<String>,
    only_fc: bool,
    scroll: Scroll,
    btn_all: DRectButton,
    btn_fc: DRectButton,
    btn_export: DRectButton,
    btn_import: DRectButton,
    import_waiting: bool,
    /// 今日挑战：开始按钮
    btn_daily: DRectButton,
    /// 每行一个按钮，用来点选「只看这首」（渲染时 set，touch 时用）
    row_btns: Vec<RectButton>,
}

impl HistoryPage {
    pub fn new() -> Self {
        history::ensure_loaded();
        Self {
            records: history::all(),
            selected: None,
            only_fc: false,
            scroll: Scroll::new(),
            btn_all: DRectButton::new().with_radius(0.008),
            btn_fc: DRectButton::new().with_radius(0.008),
            btn_export: DRectButton::new().with_radius(0.008),
            btn_import: DRectButton::new().with_radius(0.008),
            import_waiting: false,
            btn_daily: DRectButton::new().with_radius(0.008),
            row_btns: Vec::new(),
        }
    }

    fn reload(&mut self) {
        self.records = history::all();
    }

    /// 当前筛选后的记录（按时间从旧到新，方便画趋势）。
    fn filtered(&self) -> Vec<Record> {
        let mut v: Vec<Record> = match &self.selected {
            Some(key) => self.records.iter().filter(|it| &it.key == key).cloned().collect(),
            None => self.records.clone(),
        };
        if self.only_fc {
            v.retain(|it| it.is_full_combo());
        }
        v.sort_by_key(|it| it.time);
        v
    }

    fn start_export(&mut self) {
        match history::export_json() {
            Ok(text) => {
                super::library::request_export("phira-history.json".to_owned());
                // 系统弹窗是异步的：先把文本放内存，等 take_export 拿到文件再写
                PENDING_EXPORT.with(|it| *it.borrow_mut() = Some(text));
            }
            Err(err) => show_error(err.context(tl!("export-failed"))),
        }
    }

    fn poll_import(&mut self) {
        if !self.import_waiting {
            return;
        }
        let Some(file) = take_file() else {
            return;
        };
        self.import_waiting = false;
        let (_, path) = file;
        if path.is_empty() {
            return;
        }
        match std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|text| history::import_json(&text))
        {
            Ok(n) => {
                self.reload();
                show_message(tl!("import-done", "count" => n.to_string()));
            }
            Err(err) => show_error(err.context(tl!("import-failed"))),
        }
    }

    fn poll_export_write() {
        PENDING_EXPORT.with(|slot| {
            if slot.borrow().is_none() {
                return;
            }
            // 系统「保存到哪」对话框是异步的：拿到文件之前不能把文本丢掉
            if let Some(Ok(mut config)) = super::library::take_export() {
                let text = slot.borrow_mut().take().unwrap_or_default();
                let res = config.file.write_all(text.as_bytes()).map_err(anyhow::Error::from);
                super::library::resolve_export();
                if let Err(err) = res {
                    show_error(err.context(tl!("export-failed")));
                } else {
                    show_message(tl!("export-done"));
                }
            }
        });
    }
}

thread_local! {
    static PENDING_EXPORT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

impl Page for HistoryPage {
    fn label(&self) -> Cow<'static, str> {
        tl!("label")
    }

    fn update(&mut self, s: &mut SharedState) -> Result<()> {
        self.scroll.update(s.t);
        self.poll_import();
        Self::poll_export_write();
        Ok(())
    }

    fn touch(&mut self, touch: &Touch, s: &mut SharedState) -> Result<bool> {
        let t = s.t;
        if self.scroll.touch(touch, t) {
            return Ok(true);
        }
        if self.btn_all.touch(touch, t) {
            self.selected = None;
            return Ok(true);
        }
        if self.btn_fc.touch(touch, t) {
            self.only_fc ^= true;
            return Ok(true);
        }
        if self.btn_daily.touch(touch, t) {
            let charts = super::load_local();
            if let Some(challenge) = crate::daily::today_challenge(charts.len()) {
                let chart = charts[challenge.chart_index].clone();
                let local_path = chart.local_path.clone();
                let mods = local_path
                    .as_ref()
                    .and_then(|path| get_data().charts.iter().find(|it| &it.local_path == path).map(|it| it.mods))
                    .unwrap_or_default();
                crate::daily::start(&challenge);
                DAILY_START.with(|it| *it.borrow_mut() = Some((chart, local_path, mods)));
            } else {
                show_message(tl!("daily-no-chart")).warn();
            }
            return Ok(true);
        }
        if self.btn_export.touch(touch, t) {
            self.start_export();
            return Ok(true);
        }
        if self.btn_import.touch(touch, t) {
            self.import_waiting = true;
            request_file("history_import");
            return Ok(true);
        }
        // 点某一行 = 只看这一首（再点一次取消）；顺序要和渲染时一致
        let keys: Vec<String> = self.filtered().iter().rev().take(300).map(|it| it.key.clone()).collect();
        for (i, btn) in self.row_btns.iter_mut().enumerate() {
            if btn.touch(touch) {
                if let Some(key) = keys.get(i) {
                    if self.selected.as_deref() == Some(key.as_str()) {
                        self.selected = None;
                    } else {
                        self.selected = Some(key.clone());
                    }
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn next_scene(&mut self, s: &mut SharedState) -> prpr::scene::NextScene {
        let Some((chart, local_path, mods)) = DAILY_START.with(|it| it.borrow_mut().take()) else {
            return prpr::scene::NextScene::None;
        };
        let Some(icons) = super::global_icons() else {
            show_error(anyhow::anyhow!("UI icons are not ready"));
            return prpr::scene::NextScene::None;
        };
        prpr::scene::NextScene::Overlay(Box::new(crate::scene::SongScene::new(chart, local_path, icons, s.icons.clone(), mods)))
    }

    fn render(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        // 鼠标悬停判定：与 `Touch::position` 同一套世界坐标（滚动/位移都已含在 rect_to_global 里）
        let (mx, my) = crate::newui::mouse_world();
        let mouse = vec2(mx, my);
        let mut cr = ui.content_rect();
        // 活动栏删除后 content_rect 已经是 -0.94..0.94；这里只再收一点点做**对称**内边距
        // （原来只单边 +0.025 是为了躲开左侧活动栏，现在不需要了）。
        cr.x += 0.02;
        cr.w -= 0.04;
        let records = self.filtered();
        let selected_name = self
            .selected
            .as_ref()
            .and_then(|key| self.records.iter().find(|it| &it.key == key))
            .map(|it| it.name.clone());

        s.render_fader(ui, |ui| {
            // HUD 自定义布局：列表大框 / 行高参数
            let outer = crate::hud::slot_or(ui, "history", "list", crate::hud::Cap(true, true, true), cr.feather(-0.005));
            self.scroll.size((outer.w, outer.h));
            ui.dx(outer.x);
            ui.dy(outer.y);
            self.scroll.render(ui, |ui| {
                let w = outer.w;
                let mut y = 0.;
                let row = |y: f32, h: f32| Rect::new(0., y, w, h);

                // ---------------- 顶部：筛选 + 导入导出 ----------------
                let bh = 0.085;
                let bw = (w - GAP * 3.) / 4.;
                let mut rb = Rect::new(0., y, bw, bh);
                self.btn_all
                    .render_text(ui, rb, t, tl!("filter-all"), 0.42, self.selected.is_none() && !self.only_fc);
                rb.x += bw + GAP;
                self.btn_fc.render_text(ui, rb, t, tl!("filter-fc"), 0.42, self.only_fc);
                rb.x += bw + GAP;
                self.btn_export.render_text(ui, rb, t, tl!("export"), 0.4, false);
                rb.x += bw + GAP;
                self.btn_import
                    .render_text(ui, rb, t, if self.import_waiting { tl!("import-waiting") } else { tl!("import") }, 0.4, false);
                y += bh + GAP;

                // ---------------- 标题 ----------------
                let title = match &selected_name {
                    Some(name) => name.clone(),
                    None => tl!("stat-all-title").to_string(),
                };
                // 页面标题：带字距（kaichang 排版），主标题大而亮、计数小而弱
                let ttl = crate::newui::text_tracked_w(ui, &title, 0.004, y, SZ_TITLE, text_primary(), 0.012, (0., 0.), true);
                let title_h = ttl.h;
                if !records.is_empty() {
                    let count = tl!("stat-count", "count" => records.len().to_string());
                    let count_h = ui.text(&count).size(SZ_CAP).no_baseline().measure().h;
                    ui.text(count)
                        .pos(w - 0.004, y + (title_h - count_h).max(0.))
                        .anchor(1., 0.)
                        .no_baseline()
                        .size(SZ_CAP)
                        .color(text_secondary())
                        .draw();
                }
                y += title_h + space::S;

                if records.is_empty() {
                    let r = row(y, 0.34);
                    let inner = card(ui, r);
                    ui.text(tl!("empty"))
                        .pos(inner.center().x, inner.center().y)
                        .anchor(0.5, 0.5)
                        .no_baseline()
                        .size(SZ_BODY)
                        .color(text_secondary())
                        .draw();
                    return (w, y + 0.34 + GAP);
                }

                let pb = records.iter().max_by_key(|it| it.score).cloned().unwrap_or_default();
                let latest = records.last().cloned().unwrap_or_default();
                let avg_score = records.iter().map(|it| it.score as f64).sum::<f64>() / records.len() as f64;
                let avg_acc = records.iter().map(|it| it.accuracy).sum::<f64>() / records.len() as f64;

                // ---------------- 今日挑战 ----------------
                {
                    let charts = super::load_local();
                    let dc = crate::daily::today_challenge(charts.len());
                    // 卡高按实际文字高度算：字号 px 随视口宽、布局随视口高，两种比例不能共用固定卡高
                    let lh = ui.text(tl!("daily-title")).size(SZ_CARD_LABEL).no_baseline().measure().h;
                    let nh = ui.text("WXYZ 15.4").size(SZ_BODY).no_baseline().measure().h;
                    let sth = ui.text("constraint · status").size(SZ_CARD_LABEL).no_baseline().measure().h;
                    let dh = (2. * CARD_PAD + lh + space::XS + nh + sth).max(0.145);
                    let inner = card(ui, row(y, dh));
                    let mut iy = inner.y;
                    iy += ui
                        .text(tl!("daily-title"))
                        .pos(inner.x, iy)
                        .anchor(0., 0.)
                        .no_baseline()
                        .size(SZ_CARD_LABEL)
                        .color(text_faint())
                        .draw()
                        .h
                        + space::XS;
                    match dc {
                        Some(c) => {
                            let name = charts
                                .get(c.chart_index)
                                .map(|it| format!("{}  {}", it.info.name, it.info.level))
                                .unwrap_or_default();
                            let cons = constraint_text(c.constraint);
                            let rec = crate::daily::record_of(c.day);
                            let status = if rec.done {
                                tl!("daily-done", "score" => format!("{:07}", rec.score)).to_string()
                            } else {
                                tl!("daily-todo").to_string()
                            };
                            iy += ui
                                .text(name)
                                .pos(inner.x, iy)
                                .anchor(0., 0.)
                                .no_baseline()
                                .size(SZ_BODY)
                                .color(text_primary())
                                .max_width(inner.w - 0.26)
                                .draw()
                                .h;
                            ui.text(format!("{}  ·  {}", cons, status))
                                .pos(inner.x, iy)
                                .anchor(0., 0.)
                                .no_baseline()
                                .size(SZ_CARD_LABEL)
                                .color(if rec.done { Color::new(0.6, 0.95, 0.7, 1.) } else { text_secondary() })
                                .draw();
                            let bw = 0.24;
                            let bh = 0.08;
                            let br = Rect::new(inner.right() - bw, inner.center().y - bh / 2., bw, bh);
                            self.btn_daily.render_text(ui, br, t, tl!("daily-start"), 0.26, false);
                        }
                        None => {
                            ui.text(tl!("daily-no-chart"))
                                .pos(inner.x, iy)
                                .anchor(0., 0.)
                                .no_baseline()
                                .size(SZ_BODY)
                                .color(text_secondary())
                                .draw();
                        }
                    }
                    y += dh + GAP;
                }

                // ---------------- 三张统计卡 ----------------
                // 卡高同样按三行文字的实际高度算（见上面每日挑战卡的注释）
                let sl = ui.text("WXYZ").size(SZ_CARD_LABEL).no_baseline().measure().h;
                let sv = ui.text("0000000").size(SZ_CARD_VALUE).no_baseline().measure().h;
                let se = ui.text("100.00%  FC").size(SZ_CARD_LABEL).no_baseline().measure().h;
                let sh = 2. * CARD_PAD + sl + space::XS + sv + space::XS + se;
                let sw = (w - GAP * 2.) / 3.;
                let stat = |ui: &mut Ui, x: f32, label: Cow<'static, str>, value: String, value_color: Color, extra: String| {
                    let r = Rect::new(x, y, sw, sh);
                    let inner = card(ui, r);
                    let mut iy = inner.y;
                    iy += ui
                        .text(label)
                        .pos(inner.x, iy)
                        .anchor(0., 0.)
                        .no_baseline()
                        .size(SZ_CARD_LABEL)
                        .color(text_faint())
                        .draw()
                        .h
                        + space::XS;
                    iy += ui
                        .text(value)
                        .pos(inner.x, iy)
                        .anchor(0., 0.)
                        .no_baseline()
                        .size(SZ_CARD_VALUE)
                        .color(value_color)
                        .draw()
                        .h
                        + space::XS;
                    ui.text(extra)
                        .pos(inner.x, iy)
                        .anchor(0., 0.)
                        .no_baseline()
                        .size(SZ_CARD_LABEL)
                        .color(text_secondary())
                        .draw();
                };
                stat(
                    ui,
                    0.,
                    tl!("stat-latest"),
                    format!("{:07}", latest.score),
                    score_color(latest.score),
                    format!("{:.2}%{}", latest.accuracy * 100., if latest.is_full_combo() { "  FC" } else { "" }),
                );
                stat(
                    ui,
                    sw + GAP,
                    tl!("stat-pb"),
                    format!("{:07}", pb.score),
                    score_color(pb.score),
                    format!("{:.2}%{}", pb.accuracy * 100., if pb.is_full_combo() { "  FC" } else { "" }),
                );
                stat(ui, (sw + GAP) * 2., tl!("stat-avg"), format!("{avg_score:.0}"), semi_white(0.9), format!("{:.2}%", avg_acc * 100.));
                y += sh + GAP;

                // ---------------- 趋势图 ----------------
                let ch = 0.34;
                let inner = card(ui, row(y, ch));
                let trend_title_h = ui
                    .text(tl!("trend-title"))
                    .pos(inner.x, inner.y)
                    .anchor(0., 0.)
                    .no_baseline()
                    .size(SZ_CARD_LABEL)
                    .color(text_faint())
                    .draw()
                    .h;
                let axis_h = ui.text(tl!("trend-axis")).size(SZ_CAP).no_baseline().measure().h;
                let plot_top = inner.y + trend_title_h + space::S;
                let plot = Rect::new(
                    inner.x,
                    plot_top,
                    inner.w,
                    (inner.bottom() - axis_h - space::XS - plot_top).max(0.08),
                );
                for i in 0..=4 {
                    let gy = plot.y + plot.h * i as f32 / 4.;
                    ui.fill_rect(Rect::new(plot.x, gy, plot.w, 0.0012), semi_white(0.08));
                }
                let pb_y = plot.bottom() - plot.h * (pb.score as f32 / 1_000_000.).clamp(0., 1.);
                ui.fill_rect(Rect::new(plot.x, pb_y, plot.w, 0.0022), Color::new(1., 0.86, 0.42, 0.5));
                ui.text(format!("PB {}", pb.score))
                    .pos(plot.right(), pb_y - 0.006)
                    .anchor(1., 1.)
                    .no_baseline()
                    .size(SZ_CAP)
                    .color(Color::new(1., 0.86, 0.42, 0.95))
                    .draw();
                // 折线本体：用细矩形拼（Ui 的 dx/dy 偏移对 raw draw_line 不生效）
                let n = records.len();
                let mut prev: Option<(f32, f32)> = None;
                for (i, rec) in records.iter().enumerate() {
                    let x = if n == 1 {
                        plot.center().x
                    } else {
                        plot.x + plot.w * i as f32 / (n - 1) as f32
                    };
                    let v = (rec.score as f32 / 1_000_000.).clamp(0., 1.);
                    let yv = plot.bottom() - plot.h * v;
                    if let Some((px, py)) = prev {
                        let steps = (((x - px).abs() / 0.0035).ceil() as i32).max(1);
                        for k in 0..=steps {
                            let p = k as f32 / steps as f32;
                            let cx = px + (x - px) * p;
                            let cy = py + (yv - py) * p;
                            ui.fill_rect(Rect::new(cx, cy - 0.0013, 0.0035, 0.0026), score_color(rec.score));
                        }
                    }
                    ui.fill_circle(x, yv, 0.0042, score_color(rec.score));
                    prev = Some((x, yv));
                }
                ui.text(tl!("trend-axis"))
                    .pos(plot.x, plot.bottom() + space::XS)
                    .anchor(0., 0.)
                    .no_baseline()
                    .size(SZ_CAP)
                    .color(text_faint())
                    .draw();
                y += ch + GAP;

                // ---------------- 判定分布对比（只看单曲时）----------------
                if self.selected.is_some() && (!pb.hist.is_empty() || !latest.hist.is_empty()) {
                    let dh = 0.26;
                    let inner = card(ui, row(y, dh));
                    let dist_title_h = ui
                        .text(tl!("dist-title"))
                        .pos(inner.x, inner.y)
                        .anchor(0., 0.)
                        .no_baseline()
                        .size(SZ_CARD_LABEL)
                        .color(text_faint())
                        .draw()
                        .h;
                    ui.text(tl!("dist-legend"))
                        .pos(inner.right(), inner.y)
                        .anchor(1., 0.)
                        .no_baseline()
                        .size(SZ_CARD_LABEL)
                        .color(text_faint())
                        .draw();
                    let plot = Rect::new(
                        inner.x,
                        inner.y + dist_title_h + space::S,
                        inner.w,
                        inner.h - dist_title_h - space::S - space::M,
                    );
                    let bars = latest.hist.len().max(pb.hist.len()).max(1);
                    let maxv = latest.hist.iter().chain(pb.hist.iter()).copied().max().unwrap_or(0).max(1) as f32;
                    let bw = plot.w / bars as f32;
                    for i in 0..bars {
                        let lv = latest.hist.get(i).copied().unwrap_or(0) as f32;
                        let bv = pb.hist.get(i).copied().unwrap_or(0) as f32;
                        let bh2 = plot.h * (bv / maxv);
                        ui.fill_rect(Rect::new(plot.x + bw * i as f32, plot.bottom() - bh2, bw * 0.78, bh2), semi_white(0.22));
                        let lh = plot.h * (lv / maxv);
                        ui.fill_rect(Rect::new(plot.x + bw * i as f32, plot.bottom() - lh, bw * 0.78, lh), Color::new(0.42, 0.74, 1., 0.9));
                    }
                    ui.fill_rect(Rect::new(plot.center().x, plot.y, 0.0012, plot.h), semi_white(0.28));
                    y += dh + GAP;
                }

                // ---------------- 列表 ----------------
                let list_title_h = ui
                    .text(tl!("list-title"))
                    .pos(0.004, y)
                    .anchor(0., 0.)
                    .no_baseline()
                    .size(SZ_CARD_LABEL)
                    .color(text_faint())
                    .draw()
                    .h;
                y += list_title_h + space::XS;
                let visible: Vec<Record> = records.iter().rev().take(300).cloned().collect();
                // 行高与徽标高按最大字号实测：手机比例（宽/高 2.2）下同样的 size 会更大
                let row_text_h = ui.text("0000000").size(SZ_ROW).no_baseline().measure().h;
                let row_h = crate::hud::param("history", "row_h", ROW_H).clamp(0.05, 0.2).max(row_text_h + 0.022);
                let badge_h = ui.text("FC").size(SZ_ROW3).no_baseline().measure().h + 0.014;
                while self.row_btns.len() < visible.len() {
                    self.row_btns.push(RectButton::new());
                }
                let th = crate::newui::theme();
                let n_rows = visible.len();
                // ---- iOS 分组列表（inset grouped）----
                // 整组共用一个圆角容器，行间靠内缩发丝线分隔。
                // 原来是「每行一张独立圆角卡 + 8 单位间隙」，那是卡片语言的观感。
                // ⚠️ 容器必须先铺：这是即时模式，晚画会盖住已经画好的行内容。
                let group_radius = 0.012;
                crate::newui::group_plate(ui, &th, row(y, row_h * n_rows as f32), group_radius);
                for (i, rec) in visible.iter().enumerate() {
                    let r = row(y, row_h);
                    self.row_btns[i].set(ui, r);
                    let is_selected = self.selected.as_deref() == Some(rec.key.as_str());
                    let hp = if ui.rect_to_global(r).contains(mouse) { 1. } else { 0. };
                    // 行自己**不填底板**（底板来自 `group_plate`）：只叠「选中」强调色
                    // + 悬停墨色 + 底部内缩发丝线，三者都由 `group_row` 统一处理。
                    crate::newui::group_row(
                        ui,
                        &th,
                        r,
                        hp,
                        i == 0,
                        i + 1 == n_rows,
                        group_radius,
                        0.026,
                        if is_selected {
                            th.with_a(th.glow, 0.16)
                        } else {
                            Color::new(0., 0., 0., 0.)
                        },
                    );
                    // 左侧段位色条
                    crate::newui::rounded(
                        ui,
                        Rect::new(r.x + 0.006, r.y + 0.013, 0.007, r.h - 0.026),
                        0.003,
                        score_color(rec.score),
                    );
                    let ix = r.x + 0.026;
                    let cy = r.center().y;
                    ui.text(format!("{:07}", rec.score))
                        .pos(ix, cy)
                        .anchor(0., 0.5)
                        .size(SZ_ROW)
                        .color(score_color(rec.score))
                        .no_baseline()
                        .draw();
                    ui.text(format!("{:.2}%", rec.accuracy * 100.))
                        .pos(ix + r.w * 0.17, cy)
                        .anchor(0., 0.5)
                        .size(SZ_ROW2)
                        .color(text_primary())
                        .no_baseline()
                        .draw();
                    ui.text(format!("P{} G{} B{} M{}", rec.counts[0], rec.counts[1], rec.counts[2], rec.counts[3]))
                        .pos(ix + r.w * 0.29, cy)
                        .anchor(0., 0.5)
                        .size(SZ_ROW3)
                        .color(text_secondary())
                        .no_baseline()
                        .draw();
                    ui.text(format!("x{}", rec.max_combo))
                        .pos(ix + r.w * 0.50, cy)
                        .anchor(0., 0.5)
                        .size(SZ_ROW3)
                        .color(text_secondary())
                        .no_baseline()
                        .draw();
                    if rec.is_full_combo() {
                        let badge = Rect::new(ix + r.w * 0.585, cy - badge_h / 2., 0.085, badge_h);
                        ui.fill_path(&badge.rounded(0.010), Color::new(0.42, 0.74, 1., 0.22));
                        ui.text("FC")
                            .pos(badge.center().x, cy)
                            .anchor(0.5, 0.5)
                            .size(SZ_ROW3)
                            .color(Color::new(0.6, 0.85, 1., 1.))
                            .no_baseline()
                            .draw();
                    }
                    ui.text(format!("{} {:.1}", rec.level, rec.difficulty))
                        .pos(ix + r.w * 0.68, cy)
                        .anchor(0., 0.5)
                        .size(SZ_ROW3)
                        .color(text_secondary())
                        .no_baseline()
                        .draw();
                    ui.text(rec.time_text())
                        .pos(r.right() - 0.012, cy)
                        .anchor(1., 0.5)
                        .size(SZ_ROW3)
                        .color(text_faint())
                        .no_baseline()
                        .draw();
                    y += row_h;
                }
                if visible.len() < records.len() {
                    let more = tl!("list-more", "count" => (records.len() - visible.len()).to_string());
                    y += ui
                        .text(more)
                        .pos(0.004, y + space::XS)
                        .anchor(0., 0.)
                        .no_baseline()
                        .size(SZ_CAP)
                        .color(text_faint())
                        .draw()
                        .h
                        + space::S;
                }
                (w, y + GAP)
            });
        });
        Ok(())
    }
}

/// 约束的中文说明（用 tl! 取，保证和设置里一致）。
fn constraint_text(c: crate::daily::Constraint) -> String {
    use crate::daily::Constraint::*;
    match c {
        SpeedUp => tl!("daily-speed").to_string(),
        StrictJudge => tl!("daily-strict").to_string(),
        HealthMode => tl!("daily-health").to_string(),
        NoHints => tl!("daily-nohints").to_string(),
    }
}

thread_local! {
    /// 点了「开始」之后暂存要推出去的场景参数，由 next_scene 取走。
    static DAILY_START: std::cell::RefCell<Option<(crate::page::ChartItem, Option<String>, prpr::config::Mods)>> =
        const { std::cell::RefCell::new(None) };
}
