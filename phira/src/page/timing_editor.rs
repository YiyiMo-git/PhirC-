//! 按键准度条（Timing Bar）可视化编辑器。
//!
//! 从 PhiraiAd-1.1.0 的 `page/timing_editor.rs` 移植并简化：
//! 原版会在后台渲染一张空谱面的真实游戏画面作底图（依赖 iAd 独有的
//! `timing_preview` / `GameScene::render_timing_preview`）；本分支没有那套，
//! 改成纯色底 + 准度条预览，编辑能力（样式 / 位置 / 大小 / 记录时长）不变。

use super::{NextPage, Page, SharedState};
use crate::{get_data, get_data_mut, save_data};
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

pub struct TimingEditor {
    draft: TimingBarConfig,
    preview: TimingBar,
    windows: TimingWindows,
    done: bool,
    controls: bool,
    panel_rect: Option<Rect>,
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
            dragging: None,
            last_sample: -1.,
            sample_index: 0,
        }
    }
}

impl Page for TimingEditor {
    fn label(&self) -> Cow<'static, str> {
        "准度条编辑".into()
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
        ui.abs_scope(|ui| {
            let top = ui.top;
            // 纯色底（原版这里渲染空谱面预览）
            ui.fill_rect(Rect::new(-1., -top, 2., top * 2.), Color::from_rgba(18, 22, 30, 255));
            // 编辑时必须显示条，哪怕打歌开关是关的
            let mut preview_config = self.draft.clone();
            preview_config.enabled = true;
            timing_bar::render(ui, &preview_config, &self.preview, &self.windows);

            if ui.button("timing_editor_controls", Rect::new(-0.95, top - 0.075, 0.25, 0.06), if self.controls { "隐藏面板" } else { "显示面板" }) {
                self.controls ^= true;
            }
            if ui.button("timing_editor_save", Rect::new(0.74, top - 0.075, 0.21, 0.06), "保存") {
                self.done = true;
            }
            self.panel_rect = None;
            if self.controls {
                let scale = ((top * 2. - 0.28) / 0.64).clamp(0.1, 1.);
                let panel = Rect::new(0.98 - 0.57 * scale, -top + 0.18, 0.57 * scale, 0.64 * scale);
                self.panel_rect = Some(panel);
                // 控制面板 = 磨砂玻璃（原来是写死的深蓝实色，浅色主题下像块补丁）
                let th = crate::newui::theme();
                let m = crate::newui::Metrics::new(ui);
                crate::newui::acrylic(ui, panel, m.u(16.), &th);
                ui.scope(|ui| {
                    ui.dx(panel.x);
                    ui.dy(panel.y);
                    ui.with(prpr::core::Matrix::new_scaling(scale), |ui| {
                        ui.dx(0.02);
                        ui.dy(0.02);
                        // 面板标题（这段在缩放矩阵里，逐字符字距会因量宽不带变换而重叠，
                        // 所以这里用普通文字 + 稍大字号做层级）
                        ui.text("准度条")
                            .pos(0., 0.02)
                            .anchor(0., 0.5)
                            .no_baseline()
                            .size(0.052)
                            .color(th.text_hi)
                            .draw();
                        ui.dy(0.045);
                        if ui.button(
                            "timing_editor_style",
                            Rect::new(0., 0., 0.50, 0.06),
                            if self.draft.curved { "样式：弧形" } else { "样式：直线" },
                        ) {
                            self.draft.curved ^= true;
                        }
                        ui.dy(0.105);
                        // 【滑条长度】`ui.slider` 在轨道右侧还会画 `-` / `+` 两个按钮，
                        // 一共额外占 0.13 个局部单位（0.02 间隔 + 0.05×2 按钮 + 0.01 缝）。
                        // 面板内可用宽 = 0.57（面板宽）− 0.02（左内边距）= 0.55，
                        // 所以轨道最多 0.42；这里取 0.40，右边还留 0.02 的余量。
                        // ⚠️ 之前写的 `Some(0.5)` 会让总宽到 0.63，把 `-` / `+` 顶出面板、
                        // 被窗口右缘切掉（用户反馈「按钮全出谱面了」）。
                        // 注：PhiraiAd 原版这里是 `Some(0.34)`，因为原版每行还另配了一排
                        // 独立数值输入框（本分支移植时把它并进了滑条行，所以滑条更长）。
                        ui.slider("记录时间", 0.5..10.0, 0.1, &mut self.draft.record_seconds, Some(0.40));
                        ui.dy(0.12);
                        ui.slider("横坐标", -1.0..1.0, 0.01, &mut self.draft.x, Some(0.40));
                        ui.dy(0.12);
                        ui.slider("纵坐标", -1.0..1.0, 0.01, &mut self.draft.y, Some(0.40));
                        ui.dy(0.12);
                        ui.slider("大小", 5.0..500.0, 1., &mut self.draft.size, Some(0.40));
                    });
                });
            }
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
