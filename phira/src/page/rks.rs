//! 自定义 RKS（T7.4）：本地计算 + 编辑界面。
//!
//! 移植自 PhiraiAd 的 `page/rks.rs`，做了两处简化：
//! 1. **不移植 AI 定数**（`ai_service` / `ai_chart_adapter` 整个模块都不引入），
//!    所以没有「AI 计算」这个定数来源，界面上也不再显示 AI 状态；
//!    但 `DifficultySource::Ai` 枚举值保留，避免旧配置文件解析失败。
//! 2. 原版把「每张谱面的 RKS / 勾选」画在曲库网格里（要改 `charts_view.rs`），
//!    这里改成编辑面板内部自带一个分页列表，功能等价而改动面小得多。
//!
//! 所有配置都写在 `data/custom-rks.json`，不碰用户存档里的任何既有字段。
use crate::{
    custom_rks::*,
    dir, get_data,
    newui::{self, Metrics},
};
use anyhow::{anyhow, Result};
use inputbox::InputBox;
use macroquad::prelude::*;
use prpr::{
    ext::RectExt,
    scene::request_input,
    ui::{button_hit, DRectButton, Ui},
};
use std::{collections::HashMap, path::PathBuf};

/// 本地谱面列表每页显示多少行。
const ROW_PAGE: usize = 8;
/// 额外条目每页显示多少行。
const EXTRA_PAGE: usize = 5;

#[derive(Clone, Debug)]
enum Target {
    Local(String),
    Extra(u64),
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Local,
    Extra,
    Rules,
}

pub struct RksPanel {
    pub editing: bool,
    settings: Settings,
    path: PathBuf,
    /// 当前曲库里的本地谱面（含已解析出的准确率）。
    locals: Vec<LocalInput>,
    summary: Summary,
    tab: Tab,
    target: Option<Target>,
    /// 本地谱面列表当前页 / 过滤串。
    page: usize,
    filter: String,
    extra_page: usize,
    buttons: HashMap<String, DRectButton>,
    active: Vec<String>,
    pending: Option<(String, Option<Target>)>,
    input_serial: u64,
    /// 编辑器刚打开时置位，曲库页看到后调 [`RksPanel::refresh`] 重建数据。
    refresh_requested: bool,
}

impl RksPanel {
    pub fn new() -> Result<Self> {
        let path = PathBuf::from(dir::root()?).join("custom-rks.json");
        let settings = Settings::load(&path)?;
        Ok(Self {
            editing: false,
            settings,
            path,
            locals: Vec::new(),
            summary: Summary::default(),
            tab: Tab::Local,
            target: None,
            page: 0,
            filter: String::new(),
            extra_page: 0,
            buttons: HashMap::new(),
            active: Vec::new(),
            pending: None,
            input_serial: 0,
            refresh_requested: false,
        })
    }

    /// 取出「需要重建数据」的请求（只生效一次）。
    pub fn take_refresh_request(&mut self) -> bool {
        std::mem::take(&mut self.refresh_requested)
    }

    /// 徽章 / 最佳谱面榜用：当前计入统计的成绩条数。
    pub fn best_board_count(&self) -> usize {
        self.summary.best_used + self.summary.ap_used
    }
    /// 徽章 / 最佳谱面榜用：排好序的条目 + 总 RKS。
    pub fn best_board(&self) -> (Vec<RankedEntry>, f64) {
        (ranked_entries(&self.settings, &self.locals), self.summary.rks)
    }
    /// 面板显示出来的 RKS（两位小数）。
    #[allow(dead_code)]
    pub fn rks(&self) -> f64 {
        self.summary.rks
    }

    /// 从曲库状态重建本地谱面输入。只在需要时调用（进编辑器 / 曲库变动）。
    pub fn refresh(&mut self, charts_local: &[crate::page::ChartItem]) {
        let data = get_data();
        let records: HashMap<_, _> = data.charts.iter().map(|c| (c.local_path.as_str(), c.record.as_ref())).collect();
        self.locals = charts_local
            .iter()
            .filter_map(|c| {
                let path = c.local_path.as_ref()?;
                // 普通本地记录 与 `local_records` 里取更高的那个
                let ordinary = records.get(path.as_str()).copied().flatten().map(|r| r.accuracy as f64 * 100.);
                let fallback = data.local_records.get(path).and_then(|r| r.as_ref()).map(|r| r.accuracy as f64 * 100.);
                Some(LocalInput {
                    path: path.clone(),
                    name: c.info.name.clone(),
                    level: c.info.level.clone(),
                    difficulty: c.info.difficulty as f64,
                    // 不接入 AI：永远没有 AI 定数
                    ai_difficulty: None,
                    accuracy: highest_record_accuracy(ordinary, fallback),
                })
            })
            .collect();
        self.recalculate();
    }

    fn recalculate(&mut self) {
        let (_, summary) = calculate(&self.settings, &self.locals);
        self.summary = summary;
    }

    fn commit(&mut self, next: Settings) -> Result<()> {
        next.save(&self.path)?;
        self.settings = next;
        self.recalculate();
        Ok(())
    }

    pub fn exit(&mut self) {
        self.editing = false;
        self.target = None;
        self.tab = Tab::Local;
        self.filter.clear();
        self.cancel_buttons();
    }
    /// 返回键：优先关掉详情面板，其次退出编辑器。返回是否消费了这次返回。
    pub fn back(&mut self) -> bool {
        if !self.editing {
            return false;
        }
        if self.target.take().is_none() {
            self.exit();
        } else {
            self.cancel_buttons();
        }
        true
    }
    fn cancel_buttons(&mut self) {
        for b in self.buttons.values_mut() {
            b.inner.cancel();
        }
        self.active.clear();
    }

    fn edit_input(&mut self, field: &str, value: String) {
        self.input_serial += 1;
        let id = format!("rks:{}:{field}", self.input_serial);
        self.pending = Some((id.clone(), self.target.clone()));
        request_input(&id, InputBox::new().default_text(value));
    }
    pub fn set_filter(&mut self, text: String) {
        self.filter = text;
        self.page = 0;
    }
    /// 处理 `rks:` 开头的输入框结果。
    pub fn input(&mut self, id: &str, text: String) -> Result<()> {
        let Some((expected, target)) = self.pending.take() else {
            return Ok(());
        };
        if id != expected {
            self.pending = Some((expected, target));
            return Ok(());
        }
        let field = id.rsplit(':').next().unwrap_or("");
        let text = text.trim();
        let mut next = self.settings.clone();
        if field == "name" {
            if text.is_empty() || text.chars().count() > 150 {
                return Err(anyhow!("歌曲名需为 1～150 个字符"));
            }
            if let Some(Target::Extra(id)) = target {
                if let Some(e) = next.extras.iter_mut().find(|e| e.id == id) {
                    e.name = text.to_owned();
                }
            }
        } else if field == "best" || field == "ap" {
            let n: usize = text.parse().map_err(|_| anyhow!("请输入 0～1000 的整数"))?;
            if n > 1000 {
                return Err(anyhow!("数量范围为 0～1000"));
            }
            if field == "best" {
                next.best_count = n;
            } else {
                next.ap_count = n;
            }
            if next.best_count + next.ap_count == 0 {
                return Err(anyhow!("两种成绩数量不能同时为零"));
            }
        } else {
            let raw: f64 = text.parse().map_err(|_| anyhow!("请输入有效数字"))?;
            let value = if field == "difficulty" {
                difficulty_value(raw).ok_or_else(|| anyhow!("定数必须是有效的非负数"))?
            } else {
                let a = accuracy_value(raw).ok_or_else(|| anyhow!("准确率范围为 0～100%"))?;
                (a * 100.).round() / 100.
            };
            match target {
                Some(Target::Local(path)) => {
                    let c = next.charts.entry(path).or_default();
                    if field == "difficulty" {
                        c.custom_difficulty = Some(value);
                        c.difficulty_source = DifficultySource::Custom;
                    } else {
                        c.custom_accuracy = Some(value);
                        c.accuracy_source = AccuracySource::Custom;
                    }
                }
                Some(Target::Extra(id)) => {
                    if let Some(e) = next.extras.iter_mut().find(|e| e.id == id) {
                        if field == "difficulty" {
                            e.difficulty = value;
                        } else {
                            e.accuracy = value;
                        }
                    }
                }
                None => return Ok(()),
            }
        }
        self.commit(next)
    }

    /// 本地谱面列表（按过滤串筛过）。
    fn filtered(&self) -> Vec<&LocalInput> {
        let f = self.filter.trim().to_lowercase();
        self.locals
            .iter()
            .filter(|i| f.is_empty() || i.name.to_lowercase().contains(&f) || i.path.to_lowercase().contains(&f))
            .collect()
    }

    fn command(&mut self, id: &str) -> Result<()> {
        button_hit();
        match id {
            "rks-entry" => {
                self.editing = true;
                self.tab = Tab::Local;
                self.target = None;
                self.page = 0;
                self.refresh_requested = true;
            }
            "done" => self.exit(),
            "local" => {
                self.tab = Tab::Local;
                self.target = None;
            }
            "extras" => {
                self.tab = Tab::Extra;
                self.target = None;
            }
            "rules" => {
                self.tab = Tab::Rules;
                self.target = None;
            }
            "filter" => request_input("rks:filter", InputBox::new().default_text(&self.filter)),
            "select-all" | "clear-all" => {
                let mut next = self.settings.clone();
                let included = id == "select-all";
                match self.tab {
                    Tab::Local => {
                        for chart in &self.locals {
                            next.charts.entry(chart.path.clone()).or_default().included = included;
                        }
                    }
                    Tab::Extra => {
                        for entry in &mut next.extras {
                            entry.included = included;
                        }
                    }
                    Tab::Rules => return Ok(()),
                }
                self.commit(next)?;
            }
            "close" => self.target = None,
            "prev" => self.page = self.page.saturating_sub(1),
            "next" => self.page = (self.page + 1).min(self.filtered().len().saturating_sub(1) / ROW_PAGE),
            "extra-prev" => self.extra_page = self.extra_page.saturating_sub(1),
            "extra-next" => self.extra_page = (self.extra_page + 1).min(self.settings.extras.len().saturating_sub(1) / EXTRA_PAGE),
            "add" => {
                let mut next = self.settings.clone();
                let id = next.next_id();
                next.extras.push(ExtraEntry {
                    id,
                    name: "自定义歌曲".into(),
                    difficulty: 0.,
                    accuracy: 100.,
                    included: true,
                });
                self.commit(next)?;
                self.extra_page = self.settings.extras.len().saturating_sub(1) / EXTRA_PAGE;
                self.target = Some(Target::Extra(id));
            }
            "best" => self.edit_input("best", self.settings.best_count.to_string()),
            "ap" => self.edit_input("ap", self.settings.ap_count.to_string()),
            "file" | "custom-d" | "record" | "custom-a" | "difficulty" | "accuracy" | "name" | "included" | "delete" => {
                let mut next = self.settings.clone();
                match self.target.clone() {
                    Some(Target::Local(path)) => {
                        let Some(input) = self.locals.iter().find(|i| i.path == path).cloned() else {
                            self.target = None;
                            return Ok(());
                        };
                        let c = next.charts.entry(path).or_default();
                        match id {
                            "file" => c.difficulty_source = DifficultySource::File,
                            "custom-d" => {
                                c.difficulty_source = DifficultySource::Custom;
                                if c.custom_difficulty.is_none() {
                                    c.custom_difficulty = difficulty_value(input.difficulty);
                                }
                            }
                            "record" => c.accuracy_source = AccuracySource::Record,
                            "custom-a" => {
                                c.accuracy_source = AccuracySource::Custom;
                                if c.custom_accuracy.is_none() {
                                    c.custom_accuracy = input.accuracy.map(|a| (a * 100.).round() / 100.);
                                }
                            }
                            "included" => c.included = !c.included,
                            "difficulty" => {
                                self.edit_input("difficulty", c.custom_difficulty.map(format_difficulty).unwrap_or_default());
                                return Ok(());
                            }
                            "accuracy" => {
                                self.edit_input("accuracy", c.custom_accuracy.map(|a| format!("{a:.2}")).unwrap_or_default());
                                return Ok(());
                            }
                            _ => return Ok(()),
                        }
                    }
                    Some(Target::Extra(key)) => {
                        let Some(e) = next.extras.iter_mut().find(|e| e.id == key) else {
                            return Ok(());
                        };
                        match id {
                            "included" => e.included = !e.included,
                            "name" => {
                                self.edit_input("name", e.name.clone());
                                return Ok(());
                            }
                            "difficulty" => {
                                self.edit_input("difficulty", format_difficulty(e.difficulty));
                                return Ok(());
                            }
                            "accuracy" => {
                                self.edit_input("accuracy", format!("{:.2}", e.accuracy));
                                return Ok(());
                            }
                            "delete" => {
                                next.extras.retain(|e| e.id != key);
                                self.target = None;
                            }
                            _ => return Ok(()),
                        }
                    }
                    None => return Ok(()),
                }
                self.commit(next)?;
            }
            _ => {
                // 行内按钮：`row{i}` 打开详情，`t{i}` 切换是否计入
                if let Some(n) = id.strip_prefix("row").and_then(|s| s.parse::<usize>().ok()) {
                    if let Some(input) = self.filtered().get(n).map(|i| (*i).clone()) {
                        self.target = Some(Target::Local(input.path));
                    }
                    self.cancel_buttons();
                    return Ok(());
                }
                if let Some(n) = id.strip_prefix('t').and_then(|s| s.parse::<usize>().ok()) {
                    if let Some(input) = self.filtered().get(n).map(|i| (*i).clone()) {
                        let mut next = self.settings.clone();
                        let c = next.charts.entry(input.path).or_default();
                        c.included = !c.included;
                        self.commit(next)?;
                    }
                    self.cancel_buttons();
                    return Ok(());
                }
                if let Some(n) = id.strip_prefix("extra-").and_then(|s| s.parse::<u64>().ok()) {
                    self.target = Some(Target::Extra(n));
                }
            }
        }
        self.cancel_buttons();
        Ok(())
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<bool> {
        let mut clicked = None;
        for id in &self.active {
            let Some(b) = self.buttons.get_mut(id) else {
                continue;
            };
            if b.touch(touch, t) {
                clicked = Some(id.clone());
            }
        }
        if let Some(id) = clicked {
            self.command(&id)?;
            return Ok(true);
        }
        // 编辑器展开时吃掉整个内容区的触摸，别漏给底下的曲库
        Ok(self.editing)
    }

    pub fn begin_render(&mut self) {
        self.active.clear();
    }

    fn button(&mut self, ui: &mut Ui, t: f32, id: &str, text: impl Into<String>, r: Rect, selected: bool) {
        self.active.push(id.to_owned());
        // `render_text` 的 size 是世界坐标：m.f(p) == 12.5 × m.u(p)，
        // 这里取「字高约为按钮设计高度的 34%」，即 size = rect.h × 12.5 × 0.34。
        let size = r.h * 4.25;
        self.buttons
            .entry(id.to_owned())
            .or_insert_with(DRectButton::new)
            .render_text(ui, r, t, text.into(), size, selected);
    }

    /// 只注册热区、不画任何东西的按钮（整行可点那种）。
    fn hotspot(&mut self, ui: &mut Ui, t: f32, id: &str, r: Rect) {
        self.active.push(id.to_owned());
        self.buttons
            .entry(id.to_owned())
            .or_insert_with(DRectButton::new)
            .build(ui, t, r, |_, _| {});
    }

    /// 曲库工具行上的入口按钮：「RKS: xx.xx」。
    pub fn render_entry(&mut self, ui: &mut Ui, t: f32, m: &Metrics, rect: Rect) {
        let th = newui::theme();
        self.active.push("rks-entry".to_owned());
        let btn = self.buttons.entry("rks-entry".to_owned()).or_insert_with(DRectButton::new);
        btn.build(ui, t, rect, |ui, path| {
            ui.fill_path(&path, th.wash(0.02));
            newui::rounded_stroke(ui, rect, m.u(8.), m.u(1.), th.border);
        });
        newui::text_center(
            ui,
            m,
            format!("RKS {:.2}", self.summary.rks),
            rect.center().x,
            rect.center().y,
            13.,
            th.text_hi,
        );
    }
    /// 这一帧没画入口按钮时调用，免得 touch 命中上一帧的位置。
    pub fn hide_entry(&mut self) {
        if let Some(b) = self.buttons.get_mut("rks-entry") {
            b.invalidate();
        }
    }

    /// 完整的 RKS 编辑器。`area` 是整个内容区。
    pub fn render_overlay(&mut self, ui: &mut Ui, t: f32, m: &Metrics, area: Rect) {
        if !self.editing {
            return;
        }
        let th = newui::theme();
        newui::acrylic(ui, area, m.u(12.), &th);
        let pad = m.u(14.);
        let tab_h = m.u(32.);

        // ---- 顶部标签行 ----
        let mut x = area.x + pad;
        let y = area.y + pad;
        for (id, label, w, on) in [
            ("local", "本地谱面", m.u(84.), self.tab == Tab::Local),
            ("extras", "额外条目", m.u(84.), self.tab == Tab::Extra),
            ("rules", "计算规则", m.u(84.), self.tab == Tab::Rules),
        ] {
            self.button(ui, t, id, label, Rect::new(x, y, w, tab_h), on);
            x += w + m.u(6.);
        }
        // 右侧：RKS 数值 / 搜索 / 全选 / 全取消 / 完成
        let mut rx = area.right() - pad;
        let done_w = m.u(72.);
        rx -= done_w;
        self.button(ui, t, "done", "完成", Rect::new(rx, y, done_w, tab_h), true);
        if self.tab != Tab::Rules {
            rx -= m.u(6.) + m.u(72.);
            self.button(ui, t, "clear-all", "全取消", Rect::new(rx, y, m.u(72.), tab_h), false);
            rx -= m.u(6.) + m.u(64.);
            self.button(ui, t, "select-all", "全选", Rect::new(rx, y, m.u(64.), tab_h), false);
            rx -= m.u(6.) + m.u(64.);
            let label = if self.filter.is_empty() { "搜索".to_owned() } else { self.filter.clone() };
            self.button(ui, t, "filter", label, Rect::new(rx, y, m.u(120.), tab_h), !self.filter.is_empty());
            rx -= m.u(6.) + m.u(110.);
        } else {
            rx -= m.u(6.) + m.u(110.);
        }
        newui::text(ui, m, format!("RKS {:.2}", self.summary.rks), rx, y + tab_h / 2., 15., th.text_hi, (0., 0.5));

        let body = Rect::new(area.x + pad, y + tab_h + m.u(10.), area.w - pad * 2., area.bottom() - pad - (y + tab_h + m.u(10.)));
        let panel_w = m.u(300.);
        let list_w = if self.target.is_some() { (body.w - panel_w - m.u(10.)).max(m.u(120.)) } else { body.w };

        match self.tab {
            Tab::Rules => {
                self.render_rules(ui, t, m, body);
                return;
            }
            Tab::Extra => {
                self.render_extras(ui, t, m, Rect::new(body.x, body.y, list_w, body.h));
            }
            Tab::Local => {
                self.render_locals(ui, t, m, Rect::new(body.x, body.y, list_w, body.h));
            }
        }
        if self.target.is_some() {
            let r = Rect::new(body.right() - panel_w, body.y, panel_w, body.h);
            newui::acrylic(ui, r, m.u(10.), &th);
            self.render_detail(ui, t, m, r);
        }
    }

    fn render_rules(&mut self, ui: &mut Ui, t: f32, m: &Metrics, body: Rect) {
        let th = newui::theme();
        let x = body.x + m.u(8.);
        let w = body.w - m.u(16.);
        let mut y = body.y + m.u(8.);
        newui::text(ui, m, "自定义 RKS · 汇总规则", x, y, 18., th.text_hi, (0., 0.5));
        y += m.u(28.);
        self.button(
            ui,
            t,
            "best",
            format!("最佳成绩数量：{}", self.settings.best_count),
            Rect::new(x, y, w.min(m.u(420.)), m.u(34.)),
            false,
        );
        y += m.u(42.);
        self.button(
            ui,
            t,
            "ap",
            format!("AP 成绩数量：{}", self.settings.ap_count),
            Rect::new(x, y, w.min(m.u(420.)), m.u(34.)),
            false,
        );
        y += m.u(50.);
        for line in [
            "单曲：准确率低于 70% 为 0",
            "否则：定数 × ((准确率 − 55) / 45)²",
            "两组独立取最高值，AP 可以同时进入最佳成绩",
            "两组求和 ÷ 总数量；缺少的位置按 0 计",
            "AP 必须实际达到 100%，不按显示值判断",
        ] {
            newui::text(ui, m, line, x, y, 14., th.text_dim, (0., 0.5));
            y += m.u(24.);
        }
    }

    fn render_locals(&mut self, ui: &mut Ui, t: f32, m: &Metrics, list: Rect) {
        let th = newui::theme();
        // 复制一份，避免与 `self.button` 的可变借用打架
        let rows: Vec<LocalInput> = self.filtered().into_iter().cloned().collect();
        let row_count = rows.len();
        let total_page = row_count.saturating_sub(1) / ROW_PAGE;
        self.page = self.page.min(total_page);
        let start = self.page * ROW_PAGE;
        let row_h = m.u(46.);
        let gap = m.u(4.);
        for (i, input) in rows.iter().skip(start).take(ROW_PAGE).enumerate() {
            let idx = start + i;
            let r = Rect::new(list.x, list.y + i as f32 * (row_h + gap), list.w, row_h);
            let c = self.settings.chart(&input.path);
            let resolved = resolve(input, &c);
            let selected = matches!(&self.target, Some(Target::Local(p)) if p == &input.path);
            ui.fill_path(&r.rounded(m.u(8.)), if selected { th.with_a(th.accent, 0.16) } else { th.wash(0.025) });
            newui::rounded_stroke(
                ui,
                r,
                m.u(8.),
                m.u(1.),
                if selected { th.with_a(th.glow, 0.55) } else { th.border },
            );
            self.button(
                ui,
                t,
                &format!("t{idx}"),
                if c.included { "✓" } else { "○" },
                Rect::new(r.x + m.u(6.), r.center().y - m.u(14.), m.u(28.), m.u(28.)),
                c.included,
            );
            self.hotspot(ui, t, &format!("row{idx}"), Rect::new(r.x + m.u(40.), r.y, r.w - m.u(46.), r.h));
            let tx = r.x + m.u(48.);
            newui::text_ellipsis(
                ui,
                m,
                input.name.clone(),
                tx,
                r.y + m.u(15.),
                14.,
                (r.w - m.u(220.)).max(m.u(60.)),
                th.text_hi,
                (0., 0.5),
            );
            let d = resolved.difficulty.map(format_difficulty).unwrap_or_else(|| "待补全".into());
            let a = resolved.accuracy.map(|a| format!("{a:.2}%")).unwrap_or_else(|| "无记录".into());
            newui::text(ui, m, format!("定数 {d} · 准确率 {a}"), tx, r.y + m.u(33.), 11., th.text_mute, (0., 0.5));
            let rks = resolved.single_rks.map(|v| format!("{v:.2}")).unwrap_or_else(|| "—".into());
            newui::text(
                ui,
                m,
                rks,
                r.right() - m.u(14.),
                r.center().y,
                16.,
                if resolved.single_rks.is_some() { th.text_hi } else { th.text_mute },
                (1., 0.5),
            );
        }
        // 分页
        let py = list.bottom() - m.u(30.);
        self.button(ui, t, "prev", "上一页", Rect::new(list.x, py, m.u(84.), m.u(30.)), false);
        self.button(ui, t, "next", "下一页", Rect::new(list.x + m.u(90.), py, m.u(84.), m.u(30.)), false);
        newui::text(
            ui,
            m,
            format!("{} / {}", self.page + 1, total_page + 1),
            list.x + m.u(186.),
            py + m.u(15.),
            13.,
            th.text_dim,
            (0., 0.5),
        );
        newui::text(
            ui,
            m,
            format!("共 {row_count} 张谱面"),
            list.right() - m.u(6.),
            py + m.u(15.),
            13.,
            th.text_mute,
            (1., 0.5),
        );
    }

    fn render_extras(&mut self, ui: &mut Ui, t: f32, m: &Metrics, list: Rect) {
        let th = newui::theme();
        self.button(ui, t, "add", "＋ 添加条目", Rect::new(list.x, list.y, list.w, m.u(34.)), false);
        let total_page = self.settings.extras.len().saturating_sub(1) / EXTRA_PAGE;
        self.extra_page = self.extra_page.min(total_page);
        let start = self.extra_page * EXTRA_PAGE;
        let rows: Vec<_> = self.settings.extras.iter().skip(start).take(EXTRA_PAGE).cloned().collect();
        let row_h = m.u(46.);
        let gap = m.u(4.);
        for (i, e) in rows.iter().enumerate() {
            let r = Rect::new(list.x, list.y + m.u(44.) + i as f32 * (row_h + gap), list.w, row_h);
            let selected = matches!(&self.target, Some(Target::Extra(id)) if *id == e.id);
            ui.fill_path(&r.rounded(m.u(8.)), if selected { th.with_a(th.accent, 0.16) } else { th.wash(0.025) });
            newui::rounded_stroke(
                ui,
                r,
                m.u(8.),
                m.u(1.),
                if selected { th.with_a(th.glow, 0.55) } else { th.border },
            );
            self.hotspot(ui, t, &format!("extra-{}", e.id), r);
            newui::text_ellipsis(
                ui,
                m,
                format!("{} {}", if e.included { "✓" } else { "○" }, e.name),
                r.x + m.u(14.),
                r.center().y,
                14.,
                (r.w - m.u(220.)).max(m.u(60.)),
                th.text_hi,
                (0., 0.5),
            );
            newui::text(
                ui,
                m,
                format!("定数 {} · {:.2}%", format_difficulty(e.difficulty), e.accuracy),
                r.right() - m.u(14.),
                r.center().y,
                12.,
                th.text_mute,
                (1., 0.5),
            );
        }
        let py = list.bottom() - m.u(30.);
        self.button(ui, t, "extra-prev", "上一页", Rect::new(list.x, py, m.u(84.), m.u(30.)), false);
        self.button(ui, t, "extra-next", "下一页", Rect::new(list.x + m.u(90.), py, m.u(84.), m.u(30.)), false);
        newui::text(
            ui,
            m,
            format!("{} / {}", self.extra_page + 1, total_page + 1),
            list.x + m.u(186.),
            py + m.u(15.),
            13.,
            th.text_dim,
            (0., 0.5),
        );
    }

    fn render_detail(&mut self, ui: &mut Ui, t: f32, m: &Metrics, r: Rect) {
        let th = newui::theme();
        let x = r.x + m.u(14.);
        let w = r.w - m.u(28.);
        let mut y = r.y + m.u(14.);
        self.button(ui, t, "close", "×", Rect::new(r.right() - m.u(38.), y, m.u(24.), m.u(24.)), false);
        match self.target.clone() {
            Some(Target::Local(path)) => {
                let Some(input) = self.locals.iter().find(|i| i.path == path).cloned() else {
                    self.target = None;
                    return;
                };
                let c = self.settings.chart(&path);
                let resolved = resolve(&input, &c);
                newui::text_ellipsis(ui, m, input.name.clone(), x, y + m.u(12.), 16., w - m.u(30.), th.text_hi, (0., 0.5));
                y += m.u(38.);
                self.button(
                    ui,
                    t,
                    "included",
                    if c.included { "✓ 已计入 RKS" } else { "○ 未计入 RKS" },
                    Rect::new(x, y, w, m.u(32.)),
                    c.included,
                );
                y += m.u(44.);
                newui::text(ui, m, "定数来源", x, y, 12., th.text_mute, (0., 0.5));
                y += m.u(22.);
                let cw = (w - m.u(8.)) / 2.;
                self.button(ui, t, "file", "跟随文件", Rect::new(x, y, cw, m.u(30.)), c.difficulty_source == DifficultySource::File);
                self.button(
                    ui,
                    t,
                    "custom-d",
                    "自定义",
                    Rect::new(x + cw + m.u(8.), y, cw, m.u(30.)),
                    c.difficulty_source == DifficultySource::Custom,
                );
                y += m.u(40.);
                if c.difficulty_source == DifficultySource::Custom {
                    self.button(
                        ui,
                        t,
                        "difficulty",
                        format!("定数：{}", c.custom_difficulty.map(format_difficulty).unwrap_or_else(|| "点击填写".into())),
                        Rect::new(x, y, w, m.u(32.)),
                        false,
                    );
                } else {
                    newui::text(
                        ui,
                        m,
                        format!("定数：{}", resolved.difficulty.map(format_difficulty).unwrap_or_else(|| "待补全".into())),
                        x,
                        y + m.u(16.),
                        14.,
                        th.text_hi,
                        (0., 0.5),
                    );
                }
                y += m.u(46.);
                newui::text(ui, m, "准确率来源", x, y, 12., th.text_mute, (0., 0.5));
                y += m.u(22.);
                self.button(
                    ui,
                    t,
                    "record",
                    "跟随记录",
                    Rect::new(x, y, w / 2. - m.u(4.), m.u(30.)),
                    c.accuracy_source == AccuracySource::Record,
                );
                self.button(
                    ui,
                    t,
                    "custom-a",
                    "自定义",
                    Rect::new(x + w / 2. + m.u(4.), y, w / 2. - m.u(4.), m.u(30.)),
                    c.accuracy_source == AccuracySource::Custom,
                );
                y += m.u(40.);
                if c.accuracy_source == AccuracySource::Custom {
                    self.button(
                        ui,
                        t,
                        "accuracy",
                        format!("准确率：{}", c.custom_accuracy.map(|a| format!("{a:.2}%")).unwrap_or_else(|| "点击填写".into())),
                        Rect::new(x, y, w, m.u(32.)),
                        false,
                    );
                } else {
                    newui::text(
                        ui,
                        m,
                        format!("准确率：{}", resolved.accuracy.map(|a| format!("{a:.2}%")).unwrap_or_else(|| "暂无记录".into())),
                        x,
                        y + m.u(16.),
                        14.,
                        th.text_hi,
                        (0., 0.5),
                    );
                }
                y += m.u(46.);
                newui::text(
                    ui,
                    m,
                    resolved
                        .single_rks
                        .map(|v| format!("单曲 RKS：{v:.2}"))
                        .unwrap_or_else(|| "数据不全，暂不计入结果".into()),
                    x,
                    y,
                    14.,
                    th.text_dim,
                    (0., 0.5),
                );
            }
            Some(Target::Extra(id)) => {
                let Some(e) = self.settings.extras.iter().find(|e| e.id == id).cloned() else {
                    self.target = None;
                    return;
                };
                newui::text(ui, m, "额外条目", x, y + m.u(12.), 16., th.text_hi, (0., 0.5));
                y += m.u(38.);
                for (key, label) in [
                    ("included", if e.included { "✓ 已计入".to_owned() } else { "○ 未计入".to_owned() }),
                    ("name", format!("歌曲名：{}", e.name)),
                    ("difficulty", format!("定数：{}", format_difficulty(e.difficulty))),
                    ("accuracy", format!("准确率：{:.2}%", e.accuracy)),
                    ("delete", "删除此条目".to_owned()),
                ] {
                    self.button(ui, t, key, label, Rect::new(x, y, w, m.u(32.)), key == "included" && e.included);
                    y += m.u(40.);
                }
                newui::text(ui, m, "仅用于 RKS，不生成谱面", x, y, 12., th.text_mute, (0., 0.5));
            }
            None => {}
        }
    }
}
