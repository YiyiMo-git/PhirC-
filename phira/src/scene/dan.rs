//! 段位考核模式：三门连打 + 共享血条 + 禁重试 / 禁倒流 → 合格 / 不合格。
//!
//! 和课题模式（challenge.rs）同一套骨架：本地选 3 张谱 → 依次 Overlay 进
//! GameScene（`challenge_mode` 开启，成绩经 `ChallengeEvent` 回传，不进结算页）。
//! 规则差异：
//! * **共享血条**：初始 100；每门结束扣 `Miss×3 + Bad×1`，通过一门回 15（上限 100）；
//!   任一门结束时血 ≤ 0 → 整段不合格（只能从头再来）。
//! * **禁倒流**：进考核时把 `config.rewind_count` 清零，暂停菜单不会出现倒流按钮。
//! * **禁自动游玩**：`enable_challenge()` 已去掉 AUTOPLAY。
//! * 结算画面有金色「合格」徽章；历史成绩记在 `<root>/dan.json`（含截图存档）。
use crate::{
    dir, get_data, get_data_mut,
    newui::{self, Metrics},
    page::ChartItem,
    save_data,
    scene::{fs_from_path, show_error},
};
use anyhow::{anyhow, Result};
use macroquad::prelude::*;
use std::ops::DerefMut;
use prpr::{
    ext::{poll_future, LocalTask, RectExt},
    fs,
    judge::PlayResult,
    scene::{ChallengeEvent, GameMode, LoadingScene, NextScene, Scene, SimpleRecord},
    time::TimeManager,
    ui::{DRectButton, Ui},
};
use serde_json::json;
use std::any::Any;

#[derive(PartialEq)]
enum Phase {
    Playing,
    Passed,
    Failed,
}

#[derive(Clone, Copy)]
struct DanResult {
    score: u32,
    accuracy: f64,
    counts: [u32; 4],
}

pub struct DanScene {
    charts: [ChartItem; 3],
    name: String,
    index: usize,
    life: f64,
    results: Vec<DanResult>,
    phase: Phase,
    /// 曲间黑场过渡的起始时刻；None = 没有过渡。
    transition: Option<f64>,
    waiting: bool,
    loading: LocalTask<Result<LoadingScene>>,
    next: Option<NextScene>,
    back: DRectButton,
    retry: DRectButton,
    save_shot: DRectButton,
    /// 点了「保存截图」：本帧渲染完抓屏存 PNG。
    capture_pending: bool,
    history: (u32, u32),
    target: Option<RenderTarget>,
}

/// 读段位历史 `(尝试次数, 合格次数)`。
fn load_history() -> (u32, u32) {
    let path = match dir::root() {
        Ok(r) => std::path::Path::new(&r).join("dan.json"),
        Err(_) => return (0, 0),
    };
    let Ok(text) = std::fs::read_to_string(path) else { return (0, 0) };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    (
        v.get("attempts").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        v.get("passed").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
    )
}

/// 追加一条段位记录。
fn append_history(name: &str, total: i64, passed: bool, life: f64) -> Result<()> {
    let root = dir::root()?;
    let path = std::path::Path::new(&root).join("dan.json");
    let mut v: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({ "attempts": 0, "passed": 0, "records": [] }));
    let obj = v.as_object_mut().ok_or_else(|| anyhow!("dan.json 不是对象"))?;
    *obj.entry("attempts").or_insert(json!(0)) = json!(obj.get("attempts").and_then(|x| x.as_u64()).unwrap_or(0) + 1);
    if passed {
        *obj.entry("passed").or_insert(json!(0)) = json!(obj.get("passed").and_then(|x| x.as_u64()).unwrap_or(0) + 1);
    }
    let records = obj.entry("records").or_insert(json!([]));
    if let Some(arr) = records.as_array_mut() {
        arr.push(json!({
            "name": name,
            "total": total,
            "passed": passed,
            "life": (life * 10.).round() / 10.,
            "time": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        }));
        let n = arr.len();
        if n > 50 {
            arr.drain(..n - 50);
        }
    }
    std::fs::write(path, serde_json::to_string_pretty(&v)?)?;
    Ok(())
}
impl DanScene {
    pub fn new(charts: [ChartItem; 3], name: String) -> Self {
        Self {
            charts,
            name,
            index: 0,
            life: 100.,
            results: Vec::new(),
            phase: Phase::Playing,
            transition: None,
            waiting: false,
            loading: None,
            next: None,
            back: DRectButton::new(),
            retry: DRectButton::new(),
            save_shot: DRectButton::new(),
            capture_pending: false,
            history: load_history(),
            target: None,
        }
    }

    fn restart(&mut self) {
        self.index = 0;
        self.life = 100.;
        self.results.clear();
        self.phase = Phase::Playing;
        self.waiting = false;
        self.loading = None;
        self.next = None;
        self.transition = None;
        self.back.inner.cancel();
        self.retry.inner.cancel();
        self.save_shot.inner.cancel();
    }

    fn fail(&mut self, error: anyhow::Error) {
        show_error(error);
        self.next = Some(NextScene::Pop);
    }

    /// 准备当前门：只支持本地谱面（考核入口已经保证选的是本地的）。
    fn prepare(&mut self) -> Result<()> {
        let chart = &mut self.charts[self.index];
        let Some(path) = chart.local_path.clone() else {
            return Err(anyhow!("第 {} 门谱面没有本地文件", self.index + 1));
        };
        let mut config = get_data().config.clone();
        config.mods = get_data()
            .charts
            .iter()
            .find(|c| c.local_path == path)
            .map(|c| c.mods)
            .unwrap_or(config.mods);
        // 考核规则：禁倒流、禁自动游玩；成绩经 ChallengeEvent 回传
        config.rewind_count = 0;
        config.enable_challenge();
        let data = get_data();
        config.res_pack_path = if data.respack_id == 0 {
            None
        } else {
            data.respacks
                .get(data.respack_id - 1)
                .map(|name| dir::respacks().map(|root| format!("{root}/{name}")))
                .transpose()?
        };
        let id = chart.info.id;
        self.loading = Some(Box::pin(async move {
            let mut fs = fs_from_path(&path)?;
            let mut info = fs::load_info(fs.deref_mut()).await?;
            info.id = id;
            LoadingScene::new(GameMode::Normal, info, config, fs, None, None, None, None, None).await
        }));
        Ok(())
    }

    /// 单门结束：扣血 → 判生死 → 记录。
    fn complete_chart(&mut self, tm: &mut TimeManager, result: &PlayResult) -> Result<()> {
        let life = (self.life - (result.counts[3] * 3 + result.counts[2]) as f64).max(0.);
        self.life = life;
        self.results.push(DanResult {
            score: result.score,
            accuracy: result.accuracy,
            counts: result.counts,
        });
        // 单门成绩照常写进本地 best（和课题模式一致，不联网）
        let chart = &self.charts[self.index];
        let record = SimpleRecord {
            score: result.score as i32,
            accuracy: result.accuracy as f32,
            full_combo: result.max_combo == result.num_of_notes,
            counts: result.counts,
            max_combo: result.max_combo,
            num_of_notes: result.num_of_notes,
            hist: result.hist.to_vec(),
        };
        if let Some(path) = &chart.local_path {
            let data = get_data_mut();
            if let Some(local) = data.charts.iter_mut().find(|c| c.local_path == *path) {
                if let Some(old) = &mut local.record {
                    old.update(&record);
                } else {
                    local.record = Some(record);
                }
            } else if let Some(old) = data.local_records.entry(path.clone()).or_default().as_mut() {
                old.update(&record);
            } else {
                *data.local_records.get_mut(path).unwrap() = Some(record);
            }
        }
        crate::charts_view::NEED_UPDATE.store(true, std::sync::atomic::Ordering::Relaxed);
        save_data()?;

        self.index += 1;
        if life <= 0. {
            // 血打空：整段不合格
            self.phase = Phase::Failed;
            append_history(&self.name, self.total(), false, self.life)?;
            return Ok(());
        }
        if self.index >= 3 {
            // 三门全过：合格，回一点血做展示
            self.life = (self.life + 15.).min(100.);
            self.phase = Phase::Passed;
            append_history(&self.name, self.total(), true, self.life)?;
            self.history.0 += 1;
            self.history.1 += 1;
        } else {
            // 过门回血
            self.life = (self.life + 15.).min(100.);
            self.transition = Some(tm.real_time());
        }
        Ok(())
    }

    fn total(&self) -> i64 {
        self.results.iter().map(|r| r.score as i64).sum()
    }

    fn life_color(&self) -> Color {
        if self.life > 60. {
            Color::from_rgba(120, 220, 120, 255)
        } else if self.life > 25. {
            Color::from_rgba(240, 200, 90, 255)
        } else {
            Color::from_rgba(255, 110, 110, 255)
        }
    }

    fn render_results(&mut self, ui: &mut Ui, t: f32, passed: bool) {
        let m = &Metrics::new(ui);
        let th = newui::theme();
        let screen = ui.screen_rect();
        ui.fill_rect(screen, Color::from_rgba(13, 13, 13, 255));
        let top = -ui.top;
        let x0 = screen.x + m.u(24.);
        let x1 = screen.right() - m.u(24.);

        let back = Rect::new(x0, top + m.u(16.), m.u(88.), m.u(34.));
        let retry = Rect::new(back.right() + m.u(8.), back.y, m.u(110.), m.u(34.));
        self.back.render_text(ui, back, t, "返回", m.f(14.), false);
        self.retry.render_text(ui, retry, t, "再来一次", m.f(14.), false);
        if passed {
            newui::text(ui, m, format!("段位「{}」合格", self.name), x1, back.center().y, 22., Color::from_rgba(255, 214, 90, 255), (1., 0.5));
        } else {
            newui::text(ui, m, "考核不合格", x1, back.center().y, 22., Color::from_rgba(255, 120, 120, 255), (1., 0.5));
        }

        // 血条
        let life_bar = Rect::new(x0, back.bottom() + m.u(12.), x1 - x0, m.u(14.));
        ui.fill_rect(life_bar, Color::from_rgba(255, 255, 255, 30));
        ui.fill_rect(
            Rect::new(life_bar.x, life_bar.y, life_bar.w * (self.life / 100.) as f32, life_bar.h),
            self.life_color(),
        );
        newui::text(ui, m, format!("剩余血量 {:.0}", self.life), life_bar.right() + m.u(10.), life_bar.center().y, 12., th.text_dim, (0., 0.5));

        // 三行成绩
        let gap = m.u(10.);
        let row_h = m.u(96.);
        let mut y = life_bar.bottom() + m.u(14.);
        for (i, result) in self.results.iter().enumerate() {
            let row = Rect::new(x0, y, x1 - x0, row_h);
            newui::acrylic(ui, row, m.u(10.), &th);
            let cover = Rect::new(row.x + m.u(8.), row.y + m.u(8.), row_h - m.u(16.), row_h - m.u(16.));
            let chart = &mut self.charts[i];
            chart.illu.notify();
            ui.fill_path(&cover.rounded(m.u(8.)), chart.illu.shading(cover, t));
            let tx = cover.right() + m.u(14.);
            newui::text(ui, m, format!("第 {} 门", i + 1), tx, row.y + m.u(16.), 11., th.text_mute, (0., 0.5));
            newui::text_ellipsis(
                ui,
                m,
                chart.info.name.clone(),
                tx,
                row.y + m.u(38.),
                17.,
                (row.right() - m.u(280.) - tx).max(m.u(60.)),
                th.text_hi,
                (0., 0.5),
            );
            newui::text(
                ui,
                m,
                format!("{}  {:.1}  ·  ACC {:.2}%", chart.info.level, chart.info.difficulty, result.accuracy * 100.),
                tx,
                row.y + m.u(62.),
                13.,
                th.text_dim,
                (0., 0.5),
            );
            let counts_x = row.right() - m.u(268.);
            for (j, label) in ["Perfect", "Good", "Bad", "Miss"].iter().enumerate() {
                let cx = counts_x + j as f32 * m.u(52.);
                newui::text_center(ui, m, result.counts[j].to_string(), cx, row.y + m.u(36.), 16., th.text_hi);
                newui::text_center(ui, m, *label, cx, row.y + m.u(58.), 10., th.text_mute);
            }
            newui::text(ui, m, format!("{:07}", result.score), row.right() - m.u(16.), row.center().y, 26., th.text_hi, (1., 0.5));
            y = row.bottom() + gap;
        }

        // 总分 + 保存截图
        let total = Rect::new(x0, y, x1 - x0, m.u(64.));
        newui::acrylic(ui, total, m.u(10.), &th);
        newui::text(ui, m, "总分", total.x + m.u(18.), total.center().y, 15., th.text_dim, (0., 0.5));
        newui::text_center(ui, m, format!("{:07}", self.total()), total.center().x, total.center().y, 28., th.text_hi);
        newui::text(
            ui,
            m,
            format!("历史 {} 次考核 / 合格 {} 次", self.history.0, self.history.1),
            total.right() - m.u(140.),
            total.y + m.u(12.),
            11.,
            th.text_mute,
            (1., 0.),
        );
        let shot = Rect::new(total.right() - m.u(130.), total.bottom() - m.u(34.), m.u(114.), m.u(28.));
        self.save_shot.render_text(ui, shot, t, "保存截图", m.f(12.), true);
    }
}
impl Scene for DanScene {
    fn enter(&mut self, _tm: &mut TimeManager, target: Option<RenderTarget>) -> Result<()> {
        self.target = target;
        Ok(())
    }
    fn on_result(&mut self, tm: &mut TimeManager, result: Box<dyn Any>) -> Result<()> {
        self.waiting = false;
        let result = match result.downcast::<ChallengeEvent>() {
            Ok(event) => {
                match *event {
                    ChallengeEvent::Completed(result) => {
                        if let Err(error) = self.complete_chart(tm, &result) {
                            show_error(error);
                        }
                    }
                    ChallengeEvent::Restart => self.restart(),
                    ChallengeEvent::Aborted => {
                        // 中途退出：不记历史（没打完）
                        self.next = Some(NextScene::Pop);
                    }
                }
                return Ok(());
            }
            Err(result) => result,
        };
        if let Ok(error) = result.downcast::<anyhow::Error>() {
            self.fail(*error);
        } else {
            self.fail(anyhow!("考核游玩未返回有效结果"));
        }
        Ok(())
    }
    fn touch(&mut self, tm: &mut TimeManager, touch: &Touch) -> Result<bool> {
        let t = tm.now() as f32;
        if self.back.touch(touch, t) {
            self.next = Some(NextScene::Pop);
            return Ok(true);
        }
        if self.phase != Phase::Playing {
            if self.retry.touch(touch, t) {
                self.restart();
            }
            if self.save_shot.touch(touch, t) && self.phase == Phase::Passed {
                self.capture_pending = true;
                crate::scene::show_message("已保存合格截图").kind(prpr::ui::MessageKind::Ok);
            }
        }
        Ok(true)
    }
    fn update(&mut self, tm: &mut TimeManager) -> Result<()> {
        for chart in &mut self.charts {
            chart.illu.settle(tm.now() as f32);
        }
        if self.next.is_some() || self.phase != Phase::Playing || self.waiting {
            return Ok(());
        }
        if let Some(loading) = &mut self.loading {
            if let Some(result) = poll_future(loading.as_mut()) {
                self.loading = None;
                match result {
                    Ok(scene) => {
                        self.waiting = true;
                        self.next = Some(NextScene::Overlay(Box::new(scene)));
                    }
                    Err(error) => self.fail(error),
                }
            }
        } else if let Err(error) = self.prepare() {
            self.fail(error);
        }
        Ok(())
    }
    fn render(&mut self, tm: &mut TimeManager, ui: &mut Ui) -> Result<()> {
        let mut camera = ui.camera();
        camera.render_target = self.target;
        set_camera(&camera);
        let t = tm.now() as f32;
        match self.phase {
            Phase::Playing => {
                ui.fill_rect(ui.screen_rect(), Color::from_rgba(17, 23, 34, 255));
                ui.full_loading(format!("段位考核「{}」· {} / 3", self.name, self.index + 1), t);
                // 血条常驻（考核中可见压力）
                let bar = ui.screen_rect();
                let r = Rect::new(bar.x + bar.w * 0.25, bar.bottom() - 0.05, bar.w * 0.5, 0.012);
                ui.fill_rect(r, Color::from_rgba(255, 255, 255, 40));
                ui.fill_rect(Rect::new(r.x, r.y, r.w * (self.life / 100.) as f32, r.h), self.life_color());
                self.back.render_text(ui, Rect::new(-0.93, -ui.top + 0.04, 0.18, 0.065), t, "放弃", 0.4, false);
            }
            Phase::Passed => self.render_results(ui, t, true),
            Phase::Failed => self.render_results(ui, t, false),
        }
        // 曲间黑场（同课题模式）
        if let Some(start) = self.transition {
            let elapsed = (tm.real_time() - start) as f32;
            if elapsed >= 0.58 {
                self.transition = None;
            } else {
                let alpha = if elapsed < 0.22 {
                    elapsed / 0.22
                } else if elapsed < 0.34 {
                    1.
                } else {
                    ((0.58 - elapsed) / 0.24).clamp(0., 1.)
                };
                ui.fill_rect(ui.screen_rect(), Color::new(0., 0., 0., alpha));
            }
        }
        // 「保存截图」：本帧画完抓屏 → PNG（带完整结算画面和字体）
        if self.capture_pending {
            self.capture_pending = false;
            let img = get_screen_data();
            if let Some(dir) = dir::root().ok() {
                let out = std::path::Path::new(&dir).join("dan");
                let _ = std::fs::create_dir_all(&out);
                let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                let path = out.join(format!("dan_{}_{}.png", self.name, ts));
                // macroquad Image → image crate → PNG（字节序 BGRA → RGBA）
                let rgba: Vec<u8> = img.bytes.chunks_exact(4).flat_map(|px| [px[2], px[1], px[0], px[3]]).collect();
                let save = image::RgbaImage::from_raw(u32::from(img.width), u32::from(img.height), rgba)
                    .ok_or_else(|| anyhow!("截图尺寸异常"))
                    .and_then(|buf| image::DynamicImage::ImageRgba8(buf).save(&path).map_err(|e| anyhow!("截图写入失败：{e}")));
                if let Err(err) = save {
                    show_error(err);
                }
            }
        }
        Ok(())
    }
    fn next_scene(&mut self, _tm: &mut TimeManager) -> NextScene {
        self.next.take().unwrap_or_default()
    }
}
