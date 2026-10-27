//! 课题模式（T7.3）的运行时场景：连打三门课题 → 结算 → 保存徽章。
//!
//! 移植自 PhiraiAd 的 `scene/challenge.rs`。原版依赖一堆 meew-11 没有的
//! Data 字段（`replica_local_records` 等）与导出/AI 模块，这里做了简化：
//! 成绩只按普通本地记录写回，不联网上传，画面改成 New UI 的磨砂亚克力风格。
use crate::{
    challenge::{BadgeNumber, ChallengeProgress, ChallengeResult, ChallengeSong},
    challenge_ui::{render_badge, BadgePicker},
    client::{Chart, Client},
    dir, get_data, get_data_mut,
    newui::{self, Metrics},
    page::ChartItem,
    save_data,
    scene::{fs_from_path, Downloading, SongScene},
};
use anyhow::{anyhow, Result};
use macroquad::prelude::*;
use prpr::{
    ext::{poll_future, LocalTask, RectExt},
    fs,
    scene::{show_error, ChallengeEvent, GameMode, LoadingScene, NextScene, Scene, SimpleRecord},
    task::Task,
    time::TimeManager,
    ui::{DRectButton, Ui},
};
use std::{any::Any, ops::DerefMut, path::Path, sync::Arc};

#[derive(PartialEq)]
enum Phase {
    Playing,
    Results,
    Badges,
}

pub struct ChallengeScene {
    charts: [ChartItem; 3],
    progress: ChallengeProgress,
    phase: Phase,
    /// 曲间黑场过渡的起始时刻（`TimeManager::real_time()`）；None = 没有过渡。
    transition: Option<f64>,
    waiting: bool,
    loading: LocalTask<Result<LoadingScene>>,
    fetch: Option<Task<Result<Arc<Chart>>>>,
    download: Option<Downloading>,
    next: Option<NextScene>,
    back: DRectButton,
    retry: DRectButton,
    proceed: DRectButton,
    done: DRectButton,
    ticks: [DRectButton; 2],
    picker: BadgePicker,
    target: Option<RenderTarget>,
}
impl ChallengeScene {
    pub fn new(charts: [ChartItem; 3]) -> Self {
        Self {
            charts,
            progress: ChallengeProgress::default(),
            phase: Phase::Playing,
            transition: None,
            waiting: false,
            loading: None,
            fetch: None,
            download: None,
            next: None,
            back: DRectButton::new(),
            retry: DRectButton::new(),
            proceed: DRectButton::new(),
            done: DRectButton::new(),
            ticks: std::array::from_fn(|_| DRectButton::new()),
            picker: BadgePicker::default(),
            target: None,
        }
    }
    /// 从头再打一遍。只清状态，按钮位置在下次 render 时重建。
    fn restart(&mut self) {
        self.progress.restart();
        self.phase = Phase::Playing;
        self.waiting = false;
        self.loading = None;
        self.fetch = None;
        self.download = None;
        self.next = None;
        self.picker.open = false;
        self.back.inner.cancel();
        self.retry.inner.cancel();
        self.proceed.inner.cancel();
        self.done.inner.cancel();
        for tick in &mut self.ticks {
            tick.inner.cancel();
        }
    }
    fn fail(&mut self, error: anyhow::Error) {
        show_error(error);
        self.next = Some(NextScene::Pop);
    }
    /// 准备下一门课题：先把本地文件找齐，没有就联网取。
    fn prepare(&mut self) -> Result<()> {
        let index = self.progress.index();
        let chart = &mut self.charts[index];
        if chart.local_path.is_none() {
            let id = chart.info.id.ok_or_else(|| anyhow!("谱面没有可用的文件路径"))?;
            let path = format!("download/{id}");
            if Path::new(&format!("{}/{path}", dir::charts()?)).exists() {
                chart.local_path = Some(path);
            } else {
                if self.fetch.is_none() && self.download.is_none() {
                    self.fetch = Some(Task::new(async move { Client::load::<Chart>(id).await }));
                }
                return Ok(());
            }
        }
        let path = chart.local_path.clone().unwrap();
        let mut config = get_data().config.clone();
        // 单谱面自己的 mods 优先，和普通游玩保持一致
        config.mods = get_data()
            .charts
            .iter()
            .find(|c| c.local_path == path)
            .map(|c| c.mods)
            .unwrap_or(config.mods);
        config.enable_challenge();
        config.player_name = get_data().me.as_ref().map(|u| u.name.clone()).unwrap_or_else(|| "离线玩家".into());
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
            // 课题不保存成绩、不上传，一切结果都交给 ChallengeScene 处理
            LoadingScene::new(GameMode::Normal, info, config, fs, None, None, None, None, None).await
        }));
        Ok(())
    }
    /// 把单门成绩写回本地记录（不联网，不改用户存档语义）。
    fn save_local_result(&self, index: usize, result: &prpr::judge::PlayResult) -> Result<()> {
        let chart = &self.charts[index];
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
            } else {
                let old = data.local_records.entry(path.clone()).or_default();
                if let Some(old) = old {
                    old.update(&record);
                } else {
                    *old = Some(record);
                }
            }
        }
        crate::charts_view::NEED_UPDATE.store(true, std::sync::atomic::Ordering::Relaxed);
        save_data()
    }
    fn render_results(&mut self, ui: &mut Ui, t: f32) {
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
        self.retry.render_text(ui, retry, t, "重新开始", m.f(14.), false);
        newui::text(ui, m, "课题结算", x1, back.center().y, 22., th.text_hi, (1., 0.5));

        // 三行成绩
        let gap = m.u(10.);
        let row_h = m.u(112.);
        let mut y = back.bottom() + m.u(14.);
        for (i, result) in self.progress.results.iter().enumerate() {
            let row = Rect::new(x0, y, x1 - x0, row_h);
            newui::acrylic(ui, row, m.u(10.), &th);
            let cover = Rect::new(row.x + m.u(8.), row.y + m.u(8.), row_h - m.u(16.), row_h - m.u(16.));
            let chart = &mut self.charts[i];
            chart.illu.notify();
            ui.fill_path(&cover.rounded(m.u(8.)), chart.illu.shading(cover, t));
            let tx = cover.right() + m.u(14.);
            newui::text(ui, m, ["1st", "2nd", "3rd"][i], tx, row.y + m.u(16.), 11., th.text_mute, (0., 0.5));
            newui::text_ellipsis(
                ui,
                m,
                result.song.name.clone(),
                tx,
                row.y + m.u(38.),
                17.,
                (row.right() - m.u(280.) - tx).max(m.u(60.)),
                th.text_hi,
                (0., 0.5),
            );
            let label = crate::challenge::difficulty_label(&result.song.level, result.song.difficulty);
            newui::text(
                ui,
                m,
                format!("{label}  {:.1}", result.song.difficulty),
                tx,
                row.y + m.u(64.),
                13.,
                th.text_dim,
                (0., 0.5),
            );
            // 判定计数
            let counts_x = row.right() - m.u(268.);
            for (j, label) in ["Perfect", "Good", "Bad", "Miss"].iter().enumerate() {
                let cx = counts_x + j as f32 * m.u(52.);
                newui::text_center(ui, m, result.counts[j].to_string(), cx, row.y + m.u(38.), 16., th.text_hi);
                newui::text_center(ui, m, *label, cx, row.y + m.u(60.), 10., th.text_mute);
            }
            newui::text(ui, m, format!("{:07}", result.score), row.right() - m.u(16.), row.center().y, 28., th.text_hi, (1., 0.5));
            y = row.bottom() + gap;
        }

        // 总分 + 继续
        let total = Rect::new(x0, y, x1 - x0, m.u(74.));
        newui::acrylic(ui, total, m.u(10.), &th);
        newui::text(ui, m, "总分", total.x + m.u(18.), total.center().y, 15., th.text_dim, (0., 0.5));
        newui::text_center(ui, m, format!("{:07}", self.progress.total()), total.center().x, total.center().y, 30., th.text_hi);
        let proceed = Rect::new(total.right() - m.u(130.), total.center().y - m.u(18.), m.u(114.), m.u(36.));
        self.proceed.render_text(ui, proceed, t, "继续", m.f(14.), true);
    }
    fn render_badges(&mut self, ui: &mut Ui, t: f32) {
        let m = &Metrics::new(ui);
        let th = newui::theme();
        let screen = ui.screen_rect();
        ui.fill_rect(screen, Color::from_rgba(13, 13, 13, 255));
        let top = -ui.top;
        let back = Rect::new(screen.x + m.u(24.), top + m.u(16.), m.u(88.), m.u(34.));
        self.back.render_text(ui, back, t, "返回", m.f(14.), false);
        newui::text_center(ui, m, format!("总分 {}", self.progress.total()), screen.center().x, back.center().y, 20., th.text_hi);

        let w = (screen.w - m.u(72.)) / 2.;
        for (i, number) in [BadgeNumber::Integer, BadgeNumber::Precise].into_iter().enumerate() {
            let r = Rect::new(screen.x + m.u(24.) + i as f32 * (w + m.u(24.)), screen.center().y - m.u(70.), w, m.u(150.));
            newui::acrylic(ui, r, m.u(12.), &th);
            if let Some(badge) = self.progress.badge(number) {
                let br = Rect::new(r.x + m.u(20.), r.y + m.u(34.), r.w - m.u(40.), m.u(74.));
                render_badge(ui, br, &badge);
                let tick = Rect::new(r.center().x - m.u(46.), r.bottom() - m.u(46.), m.u(92.), m.u(32.));
                self.ticks[i].render_text(ui, tick, t, "保存", m.f(14.), true);
            } else {
                newui::text_center(ui, m, "未达成", r.center().x, r.center().y, 16., th.text_mute);
            }
            let label = if i == 0 { "整数定数" } else { "一位小数定数" };
            newui::text_center(ui, m, label, r.center().x, r.y + m.u(18.), 12., th.text_dim);
        }

        let done = Rect::new(screen.center().x - m.u(60.), screen.center().y + m.u(110.), m.u(120.), m.u(38.));
        self.done.render_text(ui, done, t, "完成", m.f(15.), true);
    }
}
impl Scene for ChallengeScene {
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
                        let i = self.progress.index();
                        if i >= 3 {
                            return Ok(());
                        }
                        if let Err(error) = self.save_local_result(i, &result) {
                            show_error(error);
                        }
                        let chart = &self.charts[i];
                        self.progress.complete(ChallengeResult {
                            song: ChallengeSong {
                                name: chart.info.name.clone(),
                                level: chart.info.level.clone(),
                                difficulty: chart.info.difficulty as f64,
                            },
                            score: result.score,
                            accuracy: result.accuracy,
                            counts: result.counts,
                            early: result.early_kind[1],
                            late: result.late_kind[1],
                        });
                        if self.progress.index() == 3 {
                            self.phase = Phase::Results;
                        } else {
                            // 曲间黑场过渡：三首连打之间插一段淡入淡出的黑场，
                            // 避免上一首的结算画面直接硬切到下一首的加载页。
                            self.transition = Some(tm.real_time());
                        }
                    }
                    ChallengeEvent::Restart => self.restart(),
                    ChallengeEvent::Aborted => {
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
            self.fail(anyhow!("课题游玩未返回有效结果"));
        }
        Ok(())
    }
    fn touch(&mut self, tm: &mut TimeManager, touch: &Touch) -> Result<bool> {
        let t = tm.now() as f32;
        if self.picker.touch(touch, t)? {
            return Ok(true);
        }
        if let Some(download) = &mut self.download {
            if download.touch(touch, t) {
                self.next = Some(NextScene::Pop);
            }
            return Ok(true);
        }
        if self.back.touch(touch, t) {
            if self.phase == Phase::Badges {
                self.phase = Phase::Results;
            } else {
                self.next = Some(NextScene::Pop);
            }
            return Ok(true);
        }
        match self.phase {
            Phase::Results => {
                if self.retry.touch(touch, t) {
                    self.restart();
                } else if self.proceed.touch(touch, t) {
                    if self.progress.badge(BadgeNumber::Integer).is_some() {
                        self.phase = Phase::Badges;
                    } else {
                        self.next = Some(NextScene::Pop);
                    }
                }
            }
            Phase::Badges => {
                for i in 0..2 {
                    if self.ticks[i].touch(touch, t) {
                        if let Some(badge) = self.progress.badge(if i == 0 { BadgeNumber::Integer } else { BadgeNumber::Precise }) {
                            self.picker.save(badge);
                        }
                    }
                }
                if self.done.touch(touch, t) {
                    self.next = Some(NextScene::Pop);
                }
            }
            Phase::Playing => {}
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
        if let Some(fetch) = &mut self.fetch {
            if let Some(result) = fetch.take() {
                self.fetch = None;
                match result {
                    Ok(entity) => match SongScene::global_start_download(self.charts[self.progress.index()].info.clone(), (*entity).clone(), None) {
                        Ok(download) => self.download = Some(download),
                        Err(error) => self.fail(error),
                    },
                    Err(error) => self.fail(error),
                }
            }
            return Ok(());
        }
        if let Some(download) = &mut self.download {
            match download.check()? {
                Some(Some(_)) => {
                    let i = self.progress.index();
                    self.charts[i].local_path = Some(format!("download/{}", self.charts[i].info.id.unwrap()));
                    self.download = None;
                }
                Some(None) => {
                    self.download = None;
                    self.next = Some(NextScene::Pop);
                }
                None => {}
            }
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
                ui.full_loading(format!("课题模式 · {} / 3", self.progress.index() + 1), t);
                self.back.render_text(ui, Rect::new(-0.93, -ui.top + 0.04, 0.18, 0.065), t, "取消", 0.4, false);
            }
            Phase::Results => self.render_results(ui, t),
            Phase::Badges => self.render_badges(ui, t),
        }
        // 曲间黑场：0 → 0.22s 压黑，0.22 → 0.34s 保持，0.34 → 0.58s 淡出
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
        if let Some(download) = &mut self.download {
            download.render(ui, t);
        }
        self.picker.render(ui, t);
        Ok(())
    }
    fn next_scene(&mut self, _tm: &mut TimeManager) -> NextScene {
        self.next.take().unwrap_or_default()
    }
}
