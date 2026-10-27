// 按键准度条（Timing Bar）可视化编辑器。
//
// 从 PhiraiAd-1.1.0 的 `page/timing_editor.rs` 移植并简化：
// 原版会在后台渲染一张空谱面的真实游戏画面作底图（依赖 iAd 独有的
// `timing_preview` / `GameScene::render_timing_preview`）；本分支没有那套，
// 改成纯色底 + 准度条预览，编辑能力（样式 / 位置 / 大小 / 记录时长）不变。
//
// 2026-10-09 重做（iOS 18 平面风格）：
//   * 版式全部走 `newui::Metrics`（`m.u` / `m.f`），窗口缩放时比例不变；
//   * 顶栏两个按钮改用 `newui::draw_main_btn` / `draw_sec_btn`（原来是引擎的
//     `ui.button`，玻璃质感跟新界面不统一）；
//   * 控制面板 = `newui::group_plate` 铺的整组圆角容器 + `newui::group_row` 的行；
//   * 文案全部走 `tl!`；窄屏（`m.compact`）下面板改成整宽、贴底。
//
// ⚠️ 文案并进 `cali.ftl`（跟「延迟校准」共用同一个 ftl，两者都是校准类功能）。
//    不新建 `timing_editor.ftl` 是因为**新增 ftl 文件必须同时放进全部 15 个语言目录**
//    （`create_bundles!` 对每个 locale 都 `include_str!`，缺一个就编译失败），
//    为一个页面开新文件不划算。
//
// ⚠️ 模块文档注释必须写成 `//` 而不是 `//!`：`tl_file!` 展开后必须是文件开头。
prpr_l10n::tl_file!("cali");

use super::{NextPage, Page, SharedState};
use crate::{get_data, get_data_mut, newui, save_data};
use anyhow::Result;
use macroquad::prelude::*;
use prpr::{
    config::TimingBarConfig,
    core::NoteKind,
    judge::{JudgeReportEvent, Judgement},
    timing_bar::{self, TimingBar, TimingWindows},
    ui::Ui,
};
use std::borrow::Cow;

/// 预览画布的底色。
///
/// ⚠️ **恒深色、与主题无关**。准度条自己的配色（`TimingBarConfig`）是为深底设计的，
/// 浅色主题下换成白底会让整条判定线看不见。这跟 iOS 的「视频 / 图片编辑器用深色画布」
/// 是同一个道理 —— 画布不是 UI 面板，不该跟着主题翻。
const STAGE_BG: Color = Color::new(0.043, 0.047, 0.055, 1.);

pub struct TimingEditor {
    draft: TimingBarConfig,
    preview: TimingBar,
    windows: TimingWindows,
    done: bool,
    controls: bool,
    panel_rect: Option<Rect>,
    /// 顶栏两个按钮的**全局**矩形（每帧 `render` 里算，`touch` 里拿来命中）。
    /// 用存矩形 + `touch` 判定的方式，是为了能直接用 `newui` 的按钮绘制。
    save_rect: Option<Rect>,
    toggle_rect: Option<Rect>,
    dragging: Option<(u64, Vec2, Vec2)>,
    last_sample: f32,
    sample_index: usize,
}

impl TimingEditor {
    pub fn new() -> Self {
        let config = get_data().config.clone();
        Self {
            draft: config.timing_bar.clone(),
            preview: TimingBar::default(),
            windows: TimingWindows::from_config(&config),
            done: false,
            controls: true,
            panel_rect: None,
            save_rect: None,
            toggle_rect: None,
            dragging: None,
            last_sample: -1.,
            sample_index: 0,
        }
    }
}

impl Page for TimingEditor {
    fn label(&self) -> Cow<'static, str> {
        tl!("timing-editor-title")
    }

    fn can_play_bgm(&self) -> bool {
        false
    }

    fn exit(&mut self) -> Result<()> {
        // 编辑结果写回配置并保存；`enabled` 保持用户原值，别在这里擅自打开。
        get_data_mut().config.timing_bar = self.draft.clone();
        save_data()
    }

    fn touch(&mut self, touch: &Touch, _s: &mut SharedState) -> Result<bool> {
        // 顶栏按钮：先用上一帧算出来的全局矩形命中（`newui` 按钮不自带输入处理）。
        if touch.phase == TouchPhase::Started {
            if self.save_rect.is_some_and(|r| r.contains(touch.position)) {
                self.done = true;
                return Ok(true);
            }
            if self.toggle_rect.is_some_and(|r| r.contains(touch.position)) {
                self.controls ^= true;
                return Ok(true);
            }
        }

        let half_height = screen_height() / screen_width();
        if let Some((id, origin, anchor)) = self.dragging {
            if touch.id == id {
                let delta = touch.position - origin;
                self.draft.x = (anchor.x + delta.x).clamp(-1., 1.);
                self.draft.y = (anchor.y + delta.y / half_height).clamp(-1., 1.);
                if matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                    self.dragging = None;
                }
                return Ok(true);
            }
        }
        if touch.phase == TouchPhase::Started
            && touch.position.y > -half_height + 0.15
            && touch.position.y < half_height - 0.10
            && !self.panel_rect.is_some_and(|rect| rect.contains(touch.position))
        {
            let scale = self.draft.size.clamp(5., 500.) / 100.;
            let anchor = vec2(self.draft.x, self.draft.y * half_height);
            let r = Rect::new(
                anchor.x - 0.40 * scale,
                anchor.y - if self.draft.curved { 0.25 * scale } else { 0.07 * scale },
                0.80 * scale,
                if self.draft.curved { 0.34 * scale } else { 0.15 * scale },
            );
            if r.contains(touch.position) {
                self.dragging = Some((touch.id, touch.position, vec2(self.draft.x, self.draft.y)));
            }
        }
        Ok(false)
    }

    fn update(&mut self, s: &mut SharedState) -> Result<()> {
        // 用一串固定的模拟误差喂给准度条，便于看清五种判定的位置与大小。
        let interval = if self.sample_index != 0 && self.sample_index % 10 == 0 {
            2.2
        } else {
            0.6
        };
        if s.t - self.last_sample > interval {
            self.last_sample = s.t;
            let differences = [-0.18, -0.025, 0.02, 0.11, -0.06, 0.04, 0.005, -0.09, 0.13, -0.01];
            let mut diff: f64 = differences[self.sample_index % differences.len()];
            self.sample_index += 1;
            let miss = self.sample_index % 10 == 0;
            if miss {
                diff = self.windows.outer_late + 0.001;
            }
            let perfect = if diff < 0. {
                self.windows.perfect_early
            } else {
                self.windows.perfect_late
            };
            let good = if diff < 0. { self.windows.good_early } else { self.windows.good_late };
            let j = if miss {
                Judgement::Miss
            } else if diff.abs() <= perfect {
                Judgement::Perfect
            } else if diff.abs() <= good {
                Judgement::Good
            } else {
                Judgement::Bad
            };
            self.preview.record(
                &JudgeReportEvent {
                    time: s.t as f64,
                    line_id: 0,
                    note_id: 0,
                    judgement: Ok(j),
                    difference: diff,
                },
                &NoteKind::Click,
                &self.draft,
                s.rt as f64,
            );
        }
        self.preview.animate(s.rt as f64, &self.draft);
        Ok(())
    }

    fn render(&mut self, ui: &mut Ui, _s: &mut SharedState) -> Result<()> {
        let m = newui::Metrics::new(ui);
        let th = newui::theme();
        let (mx, my) = newui::mouse_world();
        let mouse = vec2(mx, my);

        ui.abs_scope(|ui| {
            let top = ui.top;

            // ---------- 预览画布（恒深） ----------
            ui.fill_rect(Rect::new(-1., -top, 2., top * 2.), STAGE_BG);

            // 编辑时必须显示条，哪怕打歌开关是关的
            let mut preview_config = self.draft.clone();
            preview_config.enabled = true;
            timing_bar::render(ui, &preview_config, &self.preview, &self.windows);

            // ---------- 顶栏：标题 + 「显示/隐藏面板」 + 「保存」 ----------
            newui::page_title(
                ui,
                &m,
                &th,
                -1. + m.u(16.),
                -top + m.u(26.),
                tl!("timing-editor-title").as_ref(),
                None,
                20.,
            );

            let bh = m.u(34.);
            let cy = -top + m.u(26.);

            // 「保存」= 主按钮（强调色实底 + 白字）
            let save_r = Rect::new(1. - m.u(16.) - m.u(92.), cy - bh * 0.5, m.u(92.), bh);
            self.save_rect = Some(ui.rect_to_global(save_r));
            let hp_save = if ui.rect_to_global(save_r).contains(mouse) { 1. } else { 0. };
            newui::draw_main_btn(ui, &m, &th, save_r, hp_save, tl!("timing-editor-save").as_ref(), "");

            // 「显示/隐藏面板」= 次级按钮
            let tg_w = m.u(118.);
            let tg_r = Rect::new(save_r.x - m.u(10.) - tg_w, cy - bh * 0.5, tg_w, bh);
            self.toggle_rect = Some(ui.rect_to_global(tg_r));
            let hp_tg = if ui.rect_to_global(tg_r).contains(mouse) { 1. } else { 0. };
            let tg_label = if self.controls { tl!("timing-editor-hide") } else { tl!("timing-editor-show") };
            newui::draw_sec_btn(ui, &m, &th, tg_r, hp_tg, tg_label.as_ref(), newui::Icon::Gear);

            // ---------- 控制面板 ----------
            self.panel_rect = None;
            if !self.controls {
                return Ok(());
            }

            // 窄屏：整宽贴底；横屏：右侧浮一张卡
            let pw = if m.compact { 2. - m.u(24.) } else { m.u(340.) };
            let px = if m.compact { -1. + m.u(12.) } else { 1. - m.u(16.) - pw };
            let ph = m.u(250.);
            let py = top - m.u(16.) - ph;
            let panel = Rect::new(px, py, pw, ph);
            self.panel_rect = Some(ui.rect_to_global(panel));

            let radius = m.u(newui::px::PANEL_RADIUS);
            newui::group_plate(ui, &th, panel, radius);

            let pad = m.u(14.);
            let inner_w = panel.w - pad * 2.;
            // 行高固定，先算出来给 `group_row` 用
            let row_h = m.u(46.);
            let n_rows = 5usize;

            ui.scope(|ui| {
                ui.dx(panel.x + pad);
                ui.dy(panel.y + pad);

                // 行 ①：样式切换（弧形 / 直线）
                let row = Rect::new(0., 0., inner_w, row_h);
                let hp = if ui.rect_to_global(row).contains(mouse) { 1. } else { 0. };
                newui::group_row(ui, &th, row, hp, true, false, 0., pad, Color::new(0., 0., 0., 0.));
                let style_label = if self.draft.curved {
                    tl!("timing-editor-style-curved")
                } else {
                    tl!("timing-editor-style-line")
                };
                newui::text(ui, &m, tl!("timing-editor-style").as_ref(), 0., row.center().y, 14., th.text, (0., 0.5));
                // 右侧一枚 tinted 小按钮，点一下切换
                let bw = m.u(112.);
                let br = Rect::new(inner_w - bw, row.center().y - m.u(14.), bw, m.u(28.));
                if ui.rect_to_global(br).contains(mouse) && is_mouse_button_pressed(MouseButton::Left) {
                    self.draft.curved ^= true;
                }
                let hp_b = if ui.rect_to_global(br).contains(mouse) { 1. } else { 0. };
                newui::rounded(ui, br, m.u(8.), newui::mix(th.with_a(th.accent, 0.12), th.with_a(th.accent, 0.20), hp_b));
                newui::text_center(ui, &m, style_label.as_ref(), br.center().x, br.center().y, 12.5, th.accent);
                ui.dy(row_h);

                // 行 ②~⑤：四个滑条。
                // 用引擎的 `ui.slider`（它会自己画标签 + 数值 + 轨道 + `-`/`+`，
                // 而且自带输入处理）。它的配色走 `ui.background()` / `theme::ink`，
                // 是主题自适应的，所以放在这块面板上观感一致。
                // 轨道长度取面板内宽减去 `-` / `+` 两个按钮占掉的宽度。
                let track = inner_w - m.u(64.);
                let gap = m.u(6.);
                for (i, row) in [
                    (tl!("timing-editor-record-seconds"), 0.5..10.0, 0.1),
                    (tl!("timing-editor-x"), -1.0..1.0, 0.01),
                    (tl!("timing-editor-y"), -1.0..1.0, 0.01),
                    (tl!("timing-editor-size"), 5.0..500.0, 1.),
                ]
                .into_iter()
                .enumerate()
                {
                    let r = Rect::new(0., 0., inner_w, row_h);
                    newui::group_row(ui, &th, r, 0., false, i + 2 == n_rows, 0., pad, Color::new(0., 0., 0., 0.));
                    let track_r = match i {
                        0 => ui.slider(row.0.as_ref(), row.1.clone(), row.2, &mut self.draft.record_seconds, Some(track)),
                        1 => ui.slider(row.0.as_ref(), row.1.clone(), row.2, &mut self.draft.x, Some(track)),
                        2 => ui.slider(row.0.as_ref(), row.1.clone(), row.2, &mut self.draft.y, Some(track)),
                        _ => ui.slider(row.0.as_ref(), row.1.clone(), row.2, &mut self.draft.size, Some(track)),
                    };
                    // 用滑条自己返回的高度推进，避免把行距写死（引擎滑条的尺寸是它自定的）
                    let consumed = track_r.bottom() + m.u(6.);
                    ui.dy(consumed.max(m.u(40.)) + gap);
                }
            });
            Ok(())
        })
    }

    fn next_page(&mut self) -> NextPage {
        if self.done {
            self.done = false;
            NextPage::Pop
        } else {
            NextPage::None
        }
    }
}
