prpr_l10n::tl_file!("home");

// New UI 曲库的「曲目详情」页 —— 设计稿 `New UI/曲库.html` 的右侧详情面板
// （桌面端是右栏、移动端是全屏浮层；这里做成叠加 Page，两种形态天然都覆盖）。
//
// 内容自上而下：大封面 → 标题 / 曲师 → 元信息 chip → 最佳成绩 → 开始游戏。
// 点「开始游戏」走 `SFader` 渐隐进 `SongScene`（原曲库的唱片转场留给歌单页）。

use super::{Page, SFader, SharedState};
use crate::{
    dir, get_data,
    icons::Icons,
    newui::{self, Icon, Metrics, Theme},
};
use anyhow::Result;
use macroquad::prelude::*;
use prpr::{
    config::Mods,
    ext::{RectExt, SafeTexture},
    scene::NextScene,
    ui::{clip_rounded_rect, DRectButton, Ui},
};
use std::sync::Arc;

pub struct SongDetailPage {
    chart: crate::page::ChartItem,
    /// 实际可用的本地路径（远程谱先落到 `download/{id}`，本地谱就是原路径）。
    path: Option<String>,
    mods: Mods,
    icons: Arc<Icons>,
    rank_icons: [SafeTexture; 8],

    start_btn: DRectButton,

    enter_t: f32,
    sf: SFader,

    /// 名字里筛出来的封面占位字形（前两个字母，空则 "PH"）。
    glyph: String,
    /// 难度简称（EZ / HD / IN / AT / ?），用于封面色调与成绩牌。
    diff_tag: String,
}

impl SongDetailPage {
    pub fn new(chart: crate::page::ChartItem, icons: Arc<Icons>, rank_icons: [SafeTexture; 8]) -> Result<Self> {
        let path = chart.local_path.clone().or_else(|| {
            let id = chart.info.id?;
            let p = format!("download/{id}");
            std::path::Path::new(&format!("{}/{p}", dir::charts().ok()?)).exists().then_some(p)
        });
        let mods = path
            .as_ref()
            .and_then(|p| get_data().charts.iter().find(|it| &it.local_path == p).map(|it| it.mods))
            .unwrap_or_default();
        let up = chart.info.level.to_uppercase();
        let diff_tag = if up.contains("EZ") {
            "EZ"
        } else if up.contains("AT") {
            "AT"
        } else if up.contains("IN") {
            "IN"
        } else if up.contains("HD") {
            "HD"
        } else {
            "?"
        }
        .to_owned();
        let glyph = chart
            .info
            .name
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .take(2)
            .collect::<String>()
            .to_uppercase();
        let glyph = if glyph.is_empty() { "PH".to_owned() } else { glyph };

        Ok(Self {
            chart,
            path,
            mods,
            icons,
            rank_icons,

            start_btn: DRectButton::new().with_radius(0.014),
            enter_t: f32::NEG_INFINITY,
            sf: SFader::new(),
            glyph,
            diff_tag,
        })
    }

    /// 元信息 chip 一项：`(图标, 标签, 加粗值)`。值为空则整项跳过。
    fn chip_items(&self) -> Vec<(Icon, &'static str, String)> {
        let info = &self.chart.info;
        let mut v = vec![(Icon::Chart, "难度", info.level.clone())];
        if !info.charter.is_empty() {
            v.push((Icon::Edit, "谱师", info.charter.clone()));
        }
        if !info.composer.is_empty() {
            v.push((Icon::Music, "曲师", info.composer.clone()));
        }
        v
    }
}

fn fmt_score(score: i32) -> String {
    let s = score.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl Page for SongDetailPage {
    fn label(&self) -> std::borrow::Cow<'static, str> {
        "library.scene".into()
    }

    fn enter(&mut self, s: &mut SharedState) -> Result<()> {
        self.enter_t = s.t;
        Ok(())
    }

    fn touch(&mut self, touch: &Touch, s: &mut SharedState) -> Result<bool> {
        if self.sf.transiting() {
            return Ok(true);
        }
        let t = s.t;
        if self.start_btn.touch(touch, t) {
            let scene = crate::scene::SongScene::new(
                self.chart.clone(),
                self.path.clone(),
                Arc::clone(&self.icons),
                self.rank_icons.clone(),
                self.mods,
            );
            self.sf.goto(t, scene);
            return Ok(true);
        }
        Ok(false)
    }

    fn update(&mut self, _s: &mut SharedState) -> Result<()> {
        Ok(())
    }

    fn render(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        let m = Metrics::new(ui);
        let th = newui::theme();

        // 状态栏：选中曲目
        newui::set_status_extra(Some((String::new(), format!("{} · {}", self.chart.info.name, self.chart.info.level))));

        s.render_fader(ui, |ui| {
            self.render_detail(ui, t, &m, &th)?;
            Ok(())
        })
    }

    fn next_scene(&mut self, s: &mut SharedState) -> NextScene {
        self.sf.next_scene(s.t).unwrap_or_default()
    }
}

impl SongDetailPage {
    /// 整个详情面板：从右滑入（移动端浮层语义），内部元素交错浮现。
    fn render_detail(&mut self, ui: &mut Ui, t: f32, m: &Metrics, th: &Theme) -> Result<()> {
        let c = m.content(ui);
        // 页面自身的入场：0.35s 从右滑入 + 淡入
        let p0 = newui::enter_p((t - self.enter_t).max(0.), 0.);
        let enter = (t - self.enter_t).max(0.);
        // 难度色（EZ 绿 / HD 蓝 / IN 红 / AT 黑 / SP 彩虹）；黑与彩虹用白色替代做边/光
        let kind = th.diff_kind(&self.chart.info.level);
        let edge = match kind {
            newui::DiffKind::Neon(c) => c,
            newui::DiffKind::Black => Color::new(1., 1., 1., 1.),
            newui::DiffKind::Rainbow => th.glow,
        };

        // 版式（设计稿 px → 世界坐标；整体放大一档）
        let cover_px = if m.h < 700. { 210. } else { 290. };
        let cover_s = m.u(cover_px).min(c.w * 0.46);
        let title_h = m.u(36.);
        let artist_h = m.u(22.);
        let chips_h = m.u(30.);
        let best_h = m.u(66.);
        let start_h = m.u(62.);
        let gaps = [m.u(18.), m.u(8.), m.u(14.), m.u(16.), m.u(14.)];

        let total = cover_s + title_h + artist_h + chips_h + best_h + start_h + gaps.iter().sum::<f32>();
        let y0 = (c.center().y - total * 0.5).max(c.y + m.u(8.));
        let cx = c.center().x;
        let panel_w = (m.u(460.)).min(c.w - m.u(24.));

        ui.scope(|ui| {
            ui.alpha(p0, |ui| {
                ui.dx((1. - p0) * m.u(48.));

                let mut y = y0;

                // ---------- 大封面 ----------
                {
                    let p = newui::enter_p(enter, 0.04);
                    ui.alpha(p, |ui| {
                        ui.scope(|ui| {
                            ui.dy((1. - p) * m.u(16.));
                            let cover = Rect::new(cx - cover_s * 0.5, y, cover_s, cover_s);
                            let radius = m.u(16.);
                            clip_rounded_rect(ui, cover, radius, |ui| {
                                // 占位底：难度色渐变（等贴图淡入后自然盖掉）
                                newui::rounded(ui, cover, radius, th.bg_elev);
                                ui.fill_rect(cover, newui::vgrad(cover, th.with_a(edge, 0.18), th.with_a(th.bg, 0.72)));
                                // 占位字形（带柔光）
                                newui::text_center(ui, m, &self.glyph, cover.center().x, cover.center().y, (cover_px * 0.16).clamp(26., 40.), th.on_image(0.92));
                                // 真实封面贴图（加载完自己淡入）
                                self.chart.illu.notify();
                                ui.fill_path(&cover.rounded(radius), self.chart.illu.shading(cover, t));
                            });
                            newui::rounded_stroke(ui, cover, radius, m.u(1.), th.with_a(edge, 0.35));
                            y += cover_s + gaps[0];
                        });
                    });
                }

                // ---------- 标题 / 曲师 ----------
                {
                    let p = newui::enter_p(enter, 0.12);
                    ui.alpha(p, |ui| {
                        ui.scope(|ui| {
                            ui.dy((1. - p) * m.u(12.));
                            newui::text_center(ui, m, self.chart.info.name.clone(), cx, y + title_h * 0.5, 26., th.text_hi);
                            y += title_h + gaps[1];
                            newui::text_center(ui, m, &self.chart.info.composer, cx, y + artist_h * 0.5, 13.5, th.white(1.));
                            y += artist_h + gaps[2];
                        });
                    });
                }

                // ---------- 元信息 chip ----------
                {
                    let p = newui::enter_p(enter, 0.2);
                    ui.alpha(p, |ui| {
                        let items = self.chip_items();
                        let chip_w = items
                            .iter()
                            .map(|(_, label, val)| newui::measure(ui, m, format!("{label} {val}"), 11.).w + m.u(38.))
                            .sum::<f32>()
                            + (items.len().saturating_sub(1)) as f32 * m.u(8.);
                        let mut x = cx - chip_w * 0.5;
                        for (i, (icon, label, val)) in items.into_iter().enumerate() {
                            let w = newui::measure(ui, m, format!("{label} {val}"), 11.).w + m.u(38.);
                            let chip = Rect::new(x, y, w, chips_h);
                            // 难度 chip（i == 0）是**纯白字且已经没有投影**了（newui::text_diff
                            // 去掉了偏移阴影），所以它必须自带深底，否则浅色主题下白字压在白面板上
                            // 等于看不见。其余 chip 维持原来的浅玻璃底 + 主题墨色字。
                            let is_diff = i == 0;
                            newui::rounded(
                                ui,
                                chip,
                                m.u(13.),
                                if is_diff {
                                    th.with_a(th.black(1.), if th.dark { 0.30 } else { 0.58 })
                                } else {
                                    th.wash(0.025)
                                },
                            );
                            newui::rounded_stroke(
                                ui,
                                chip,
                                m.u(13.),
                                m.u(1.),
                                if is_diff { th.with_a(th.white(1.), 0.22) } else { th.border },
                            );
                            let (icon_c, label_c) = if is_diff { (th.white(0.9), th.white(0.78)) } else { (th.teal, th.text_dim) };
                            newui::draw_icon(ui, m, icon, Rect::new(chip.x + m.u(10.), chip.center().y - m.u(5.5), m.u(11.), m.u(11.)), icon_c, 1.9);
                            newui::text(ui, m, label, chip.x + m.u(25.), chip.center().y, 11.5, label_c, (0., 0.5));
                            let lw = newui::measure(ui, m, label.to_owned(), 10.5).w;
                            let vx = chip.x + m.u(25.) + lw + m.u(6.);
                            let vmax = chip.right() - m.u(8.) - vx;
                            if is_diff {
                                // 难度值：EZ 绿 / HD 蓝 / IN 红 / AT 黑 / SP 彩虹（统一白字，深底上清晰）
                                newui::text_diff(ui, val, vx, chip.center().y, m.f(11.5), kind, (0., 0.5), vmax, t);
                            } else {
                                newui::text_ellipsis(ui, m, val, vx, chip.center().y, 10.5, vmax, th.text_hi, (0., 0.5));
                            }
                            x += w + m.u(8.);
                        }
                        y += chips_h + gaps[3];
                    });
                }

                // ---------- 最佳成绩 ----------
                {
                    let p = newui::enter_p(enter, 0.28);
                    ui.alpha(p, |ui| {
                        ui.scope(|ui| {
                            ui.dy((1. - p) * m.u(10.));
                            let best = Rect::new(cx - panel_w * 0.5, y, panel_w, best_h);
                            newui::rounded(ui, best, m.u(11.), th.wash(0.02));
                            newui::rounded_stroke(ui, best, m.u(11.), m.u(1.), th.border);
                            let bx = best.x + m.u(18.);
                            newui::text(ui, m, "BEST SCORE", bx, best.y + m.u(15.), 9.5, th.text_mute, (0., 0.));

                            let score = self.path.as_ref().and_then(|p| get_data().local_records.get(p).and_then(|r| r.as_ref()));
                            let (score_s, acc_s) = match score {
                                Some(r) => (fmt_score(r.score), format!("ACC {:.2}%", r.accuracy)),
                                None => ("—".to_owned(), "尚未游玩".to_owned()),
                            };
                            // 分数文字：纯白（用户要求一切 UI 文字白色）
                            newui::text(ui, m, &score_s, bx, best.bottom() - m.u(15.), 17., th.white(1.), (0., 0.5));

                            // 右侧：ACC + 难度成绩牌（去色白字，牌底加深保证无投影也清晰）
                            let rank = Rect::new(best.right() - m.u(18.) - m.u(38.), best.center().y - m.u(19.), m.u(38.), m.u(38.));
                            newui::rounded(ui, rank, m.u(9.), th.with_a(th.black(1.), if th.dark { 0.35 } else { 0.55 }));
                            newui::rounded_stroke(ui, rank, m.u(9.), m.u(1.), th.with_a(th.white(1.), 0.20));
                            // 难度字样：统一白字（**无投影**，见 newui::text_diff）
                            newui::text_diff(ui, &self.diff_tag, rank.center().x, rank.center().y, m.f(16.), kind, (0.5, 0.5), rank.w - m.u(4.), t);
                            newui::text_ellipsis(ui, m, &acc_s, rank.x - m.u(12.), best.center().y, 11.5, panel_w * 0.5 - m.u(28.), th.white(1.), (1., 0.5));
                            y += best_h + gaps[4];
                        });
                    });
                }

                // ---------- 开始游戏 ----------
                {
                    let p = newui::enter_p(enter, 0.36);
                    ui.alpha(p, |ui| {
                        ui.scope(|ui| {
                            ui.dy((1. - p) * m.u(10.));
                            let r = Rect::new(cx - panel_w * 0.5, y, panel_w, start_h);
                            self.start_btn.render_shadow(ui, r, t, |ui, path| {
                                let c1 = rgb(0x6e, 0xf2, 0xd2);
                                let c2 = rgb(0x43, 0xc6, 0xff);
                                ui.fill_path(&path, (c1, (r.x, r.y), c2, (r.x + r.w, r.y)));
                                // 扫光（设计稿 `.start-btn::after` 4.5s 循环）
                                let sp = (t / 4.5).fract() * 1.75 - 0.375;
                                ui.scissor(r, |ui| {
                                    let bx = r.x + sp * r.w;
                                    let bw = r.w * 0.3;
                                    let band = Rect::new(bx, r.y, bw, r.h);
                                    ui.fill_rect(band, newui::hgrad(band, Color { a: 0., ..WHITE }, Color::new(1., 1., 1., 0.5)));
                                });
                                let txt = format!("{} · {}", tl!("menu-start"), self.chart.info.level);
                                let tw = newui::measure(ui, m, txt.clone(), 15.).w;
                                let ix = cx - (tw + m.u(24.)) * 0.5;
                                newui::draw_icon(ui, m, Icon::Play, Rect::new(ix, r.center().y - m.u(8.5), m.u(17.), m.u(17.)), rgb(0x04, 0x12, 0x1a), 1.);
                                newui::text(ui, m, txt, ix + m.u(24.), r.center().y, 16.5, rgb(0x04, 0x12, 0x1a), (0., 0.5));
                            });
                        });
                    });
                }
            });
        });

        Ok(())
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::new(r as f32 / 255., g as f32 / 255., b as f32 / 255., 1.)
}