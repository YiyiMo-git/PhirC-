use super::{import_chart, L10N_LOCAL};
use crate::{
    charts_view::NEED_UPDATE,
    data::LocalChart,
    dir, get_data, get_data_mut,
    mp::MPPanel,
    newui::{self, Activity, Metrics, Shell, ShellAction},
    page::{ExportInfo, HomePage, NextPage, Page, ResPackItem, SharedState},
    save_data,
    scene::{confirm_dialog, import_chart_to, parse_warnings_to_string, TEX_BACKGROUND, TEX_ICON_BACK},
};
use anyhow::{anyhow, Context, Result};
use macroquad::prelude::*;
use prpr::{
    core::ResPackInfo,
    ext::{unzip_into, RectExt, SafeTexture, ScaleType},
    info::ChartInfo,
    parse::ParseWarnings,
    scene::{return_file, show_error, show_message, take_file, NextScene, Scene},
    task::Task,
    time::TimeManager,
    ui::{button_hit, Dialog, FontArc, RectButton, Ui, UI_AUDIO},
};
use sasa::{AudioClip, Music};
use std::{
    any::Any,
    cell::RefCell,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    mem,
    path::{Component, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread_local,
    time::{Duration, Instant},
};
use tempfile::tempfile;
use uuid::Uuid;

const LOW_PASS: f32 = 0.95;

pub static BGM_VOLUME_UPDATED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static RESPACK_ITEM: RefCell<Option<ResPackItem>> = RefCell::default();
    pub static MP_PANEL: RefCell<Option<MPPanel>> = RefCell::default();
}

#[inline]
fn position_file() -> Result<String> {
    Ok(format!("{}/mp-pos", dir::root()?))
}

thread_local! {
    /// 开始界面背景底图（用户提供）
    static TEX_BEIJING: RefCell<Option<SafeTexture>> = const { RefCell::new(None) };
}

pub struct MainScene {
    state: SharedState,

    /// New UI 外壳（标题栏 / 活动栏 / 状态栏）。
    shell: Shell,
    /// 外壳请求退出进程。
    quit: bool,

    bgm: Option<Music>,

    bg_beijing: SafeTexture,
    btn_back: RectButton,

    /// 真·背景模糊管线（把背景渲到 RT → 高斯模糊 → 整屏铺底）。
    /// 关闭或不可用时自动降级为锐利背景 + 磨砂配方。
    backdrop: crate::backdrop::Backdrop,

    pages: Vec<Box<dyn Page>>,

    import_task: Option<Task<Result<(LocalChart, ParseWarnings)>>>,

    mp_btn: RectButton,
    mp_icon: SafeTexture,
    mp_btn_pos: Vec2,
    mp_move: Option<(u64, Vec2, Vec2)>,
    mp_moved: bool,
    mp_save_pos_at: Option<Instant>,

    // batch import
    batch_import_confirm: Arc<AtomicBool>,
    batch_import: Option<(String, ExportInfo)>,
    batch_import_task: Option<Task<Result<()>>>,
    batch_import_rx: Option<mpsc::Receiver<ImportChart>>,
    batch_imported_charts: Vec<ImportChart>,
    batch_import_total: usize,

    /// WebView 界面请求启动谱面时，在 `next_scene` 里一次性返回的 overlay。
    webui_next_scene: Option<NextScene>,
}

enum ImportChart {
    Imported(Box<LocalChart>, ParseWarnings),
    Skipped(String),
}

impl MainScene {
    // shall be call exactly once
    pub async fn new(fallback: FontArc) -> Result<Self> {
        Self::init().await?;
        // HUD 自定义布局编辑器（设 PHIRA_HUD_DEBUG=1 时可自检本地化文案）
        crate::hud::selftest();
        // New UI 的深浅主题跟配置走（标题栏右上角的太阳/月亮按钮改它）。
        newui::set_dark(get_data().config.ui_dark);

        #[cfg(closed)]
        let bgm = {
            let bgm_clip = AudioClip::new(crate::load_res("res/bgm").await)?;
            Some(UI_AUDIO.with(|it| {
                it.borrow_mut().create_music(
                    bgm_clip,
                    sasa::MusicParams {
                        amplifier: get_data().config.volume_bgm,
                        loop_mix_time: 5.46,
                        command_buffer_size: 64,
                        ..Default::default()
                    },
                )
            })?)
        };
        #[cfg(not(closed))]
        let bgm = None;

        let mut sf = Self::new_inner(bgm, fallback).await?;
        sf.pages.push(Box::new(HomePage::new().await?));
        Ok(sf)
    }

    async fn init() -> Result<()> {
        prpr::ui::UI_SFX_VOLUME.store(get_data().config.volume_sfx.to_bits(), Ordering::Relaxed);
        // init button hitsound
        macro_rules! load_sfx {
            ($name:ident, $path:literal) => {{
                let clip = AudioClip::new(load_file($path).await?)?;
                let sound = UI_AUDIO.with(|it| it.borrow_mut().create_sfx(clip, None))?;
                prpr::ui::$name.with(|it| *it.borrow_mut() = Some(sound));
            }};
        }
        load_sfx!(UI_BTN_HITSOUND_LARGE, "button_large.ogg");
        load_sfx!(UI_BTN_HITSOUND, "button.ogg");
        load_sfx!(UI_SWITCH_SOUND, "switch.ogg");

        let background: SafeTexture = load_texture("background.jpg").await?.into();
        let icon_back: SafeTexture = load_texture("back.png").await?.into();
        let beijing: SafeTexture = load_texture("beijing.jpeg").await?.into();

        TEX_BACKGROUND.with(|it| *it.borrow_mut() = Some(background));
        TEX_ICON_BACK.with(|it| *it.borrow_mut() = Some(icon_back));
        TEX_BEIJING.with(|it| *it.borrow_mut() = Some(beijing));

        Ok(())
    }

    async fn new_inner(bgm: Option<Music>, fallback: FontArc) -> Result<Self> {
        let state = SharedState::new(fallback).await?;
        let icon_user = load_texture("user.png").await?;
        MP_PANEL.with(|it| *it.borrow_mut() = Some(MPPanel::new(icon_user.into())));
        Ok(Self {
            state,

            shell: Shell::new(),
            quit: false,

            bgm,

            bg_beijing: TEX_BEIJING.with(|it| it.borrow().clone().unwrap()),
            btn_back: RectButton::new(),

            backdrop: Default::default(),

            pages: Vec::new(),

            import_task: None,

            mp_btn: RectButton::new(),
            mp_icon: SafeTexture::from(load_texture("multiplayer.png").await?).with_mipmap(),
            mp_btn_pos: (|| -> Result<Vec2> {
                let s = std::fs::read_to_string(position_file()?)?;
                let (x, y) = s.split_once(',').ok_or_else(|| anyhow!("invalid"))?;
                Ok(vec2(x.parse()?, y.parse()?))
            })()
            .unwrap_or_default(),
            mp_move: None,
            mp_moved: false,
            mp_save_pos_at: None,

            batch_import_confirm: Arc::default(),
            batch_import: None,
            batch_import_task: None,
            batch_import_rx: None,
            batch_imported_charts: Vec::new(),
            batch_import_total: 0,

            webui_next_scene: None,
        })
    }

    fn pop(&mut self) {
        if !self.pages.last().unwrap().can_play_bgm() && self.pages[self.pages.len() - 2].can_play_bgm() {
            if let Some(bgm) = &mut self.bgm {
                let _ = bgm.fade_in(0.5);
            }
        }
        // 照 phira-0.8.2：走 `fader.back()` 的过渡动画。
        //
        // 「目标是开始界面」这一特殊情形不在这里硬切 —— 垫背的根页（`HomePage`）由
        // `render` 里的守卫跳过，所以过渡期间露出的是 Rust 自己的 TriGrid 网格底，
        // 而开始界面（WebView / `ui/index.html`）用的正是同一套 TriGrid 背景，
        // 两者视觉是连贯的；外壳另在 `render` 里跟着淡出。
        // WebView 由 `update()` 在页面栈回到 1 页时铺回来。
        self.state.fader.back(self.state.t);
    }

    pub fn take_imported_respack() -> Option<ResPackItem> {
        RESPACK_ITEM.with(|it| it.borrow_mut().take())
    }

    /// 把页面栈弹回根页（开始界面），不带过渡动画 —— 活动栏切区用。
    fn reset_to_root(&mut self) {
        while self.pages.len() > 1 {
            if let Err(err) = self.pages.pop().unwrap().exit() {
                warn!("failed to exit page: {err:?}");
            }
        }
        self.state.fader.reset();
        if let Err(err) = self.pages[0].enter(&mut self.state) {
            warn!("failed to re-enter root page: {err:?}");
        }
        // 【关键】回到根页 = 该把 WebView 铺回来了，这里**立刻**置可见。
        //
        // `update()` 里那段「回到根页面就重新显示 WebView」是每帧末尾才跑的；
        // 而本函数从 `touch()`（早于同帧的 `update`）里调用，之间隔着好几帧的
        // 淡入淡出 —— 那几帧 `is_webui_visible()` 还是 false，`render()` 就照常画
        // 原生首页，于是观感是「切换回开始界面前闪过几帧原生界面」。
        // 在这里补上，让可见性在改变的**同一帧**就生效。
        // （`webui_next_scene` 有值时不抢：那是要开谱、WebView 该让位。）
        if self.webui_next_scene.is_none()
            && crate::webui::webui_embedded()
            && !crate::webui::is_webui_visible()
        {
            crate::webui::set_webui_visible(true);
        }
    }

    /// 处理外壳按钮。
    fn on_shell_action(&mut self, act: ShellAction) {
        match act {
            ShellAction::Activity(a) => {
                newui::set_current_activity(a);
                if a == Activity::Home {
                    self.reset_to_root();
                } else {
                    self.reset_to_root();
                    newui::request_activity(a);
                }
            }
            ShellAction::ToggleTheme => {
                let dark = !newui::is_dark();
                newui::set_dark(dark);
                get_data_mut().config.ui_dark = dark;
                let _ = save_data();
            }
            ShellAction::ToggleFullscreen => {
                let fs = !get_data().config.fullscreen_mode;
                get_data_mut().config.fullscreen_mode = fs;
                let _ = save_data();
                macroquad::window::set_fullscreen(fs);
            }
            ShellAction::Quit => {
                self.quit = true;
            }
        }
    }

    /// HUD 编辑器请求切页：把页面栈重建为「根页面 + 目标页」，并同步当前编辑页面。
    fn goto_hud_page(&mut self, p: crate::hud::PageId) -> Result<()> {
        use crate::hud::PageId;
        self.pages.truncate(1);
        self.state.fader.reset();
        crate::hud::set_cur_page(p);
        let icons = |page| crate::page::global_icons().with_context(|| format!("UI icons are not ready ({page})"));
        match p {
            PageId::Home => {}
            PageId::Library => {
                let page = crate::page::LibraryPage::new(icons("library")?, self.state.icons.clone())?;
                self.pages.push(Box::new(page));
            }
            PageId::Settings => {
                let icons = icons("settings")?;
                let page = crate::page::SettingsPage::new(icons.icon.clone(), icons.lang.clone());
                self.pages.push(Box::new(page));
            }
            PageId::Favorites => {
                let page = crate::page::FavoritesPage::new(icons("favorites")?, self.state.icons.clone(), None, None);
                self.pages.push(Box::new(page));
            }
            PageId::Message => {
                let page = crate::page::MessagePage::new(icons("message")?, self.state.icons.clone());
                self.pages.push(Box::new(page));
            }
            PageId::History => {
                self.pages.push(Box::new(crate::page::HistoryPage::new()));
            }
            PageId::Respack => {
                let page = crate::page::ResPackPage::new(icons("respack")?)?;
                self.pages.push(Box::new(page));
            }
            PageId::Blacklist => {
                self.pages.push(Box::new(crate::page::BlacklistPage::new()));
            }
        }
        if p != PageId::Home {
            self.pages.last_mut().unwrap().enter(&mut self.state)?;
        }
        Ok(())
    }
}

impl Scene for MainScene {
    fn on_result(&mut self, _tm: &mut TimeManager, result: Box<dyn Any>) -> Result<()> {
        self.pages.last_mut().unwrap().on_result(result, &mut self.state)
    }

    fn enter(&mut self, tm: &mut TimeManager, _target: Option<RenderTarget>) -> Result<()> {
        if let Some(bgm) = &mut self.bgm {
            let _ = bgm.fade_in(1.3);
        }
        self.state.update(tm);
        self.pages.last_mut().unwrap().enter(&mut self.state)?;
        MP_PANEL.with(|it| {
            if let Some(panel) = it.borrow_mut().as_mut() {
                panel.enter();
            }
        });
        Ok(())
    }

    fn resume(&mut self, tm: &mut TimeManager) -> Result<()> {
        if let Some(bgm) = &mut self.bgm {
            bgm.play()?;
        }
        self.state.update(tm);
        self.pages.last_mut().unwrap().resume()?;
        Ok(())
    }

    fn pause(&mut self, tm: &mut TimeManager) -> Result<()> {
        if let Some(bgm) = &mut self.bgm {
            bgm.pause()?;
        }
        self.state.update(tm);
        self.pages.last_mut().unwrap().pause()?;
        Ok(())
    }

    fn touch(&mut self, tm: &mut TimeManager, touch: &Touch) -> Result<bool> {
        if self.state.fader.transiting() {
            return Ok(false);
        }
        if self.import_task.is_some() {
            return Ok(true);
        }

        // 自定义布局编辑模式：所有触摸都交给编辑层
        if crate::hud::edit_active() {
            return Ok(crate::hud::editor_touch(touch));
        }

        // New UI 外壳（标题栏 / 活动栏 / 状态栏）优先吃掉落在自己身上的点击
        if let Some(act) = self.shell.touch(touch) {
            self.on_shell_action(act);
            return Ok(true);
        }

        if get_data().config.mp_enabled {
            if MP_PANEL.with(|it| it.borrow_mut().as_mut().is_some_and(|it| it.touch(tm, touch))) {
                return Ok(true);
            }
            if self.mp_btn.touch(touch) && !self.mp_moved {
                MP_PANEL.with(|it| {
                    if let Some(panel) = it.borrow_mut().as_mut() {
                        panel.show(tm.real_time() as _);
                    }
                });
                self.mp_move = None;
                self.mp_moved = false;
                return Ok(true);
            }
            if let Some((id, pos, btn_pos)) = self.mp_move {
                if touch.id == id {
                    if matches!(touch.phase, TouchPhase::Cancelled | TouchPhase::Ended) {
                        self.mp_move = None;
                        self.mp_moved = false;
                        return Ok(true);
                    }
                    let new_pos = touch.position;
                    if !self.mp_moved && (new_pos - pos).length() > 0.03 {
                        self.mp_moved = true;
                    }
                    if self.mp_moved {
                        self.mp_btn_pos = new_pos - pos + btn_pos;
                        self.mp_save_pos_at = Some(Instant::now() + Duration::from_secs(1));
                    }
                }
                return Ok(true);
            } else if self.mp_btn.touching() {
                self.mp_move = Some((touch.id, touch.position, self.mp_btn_pos));
                return Ok(true);
            }
        }

        let s = &mut self.state;
        s.update(tm);
        // 【顺序】返回箭头先判：它是 `render` 里最后画的外壳件（盖在页面之上），
        // 命中判定就该跟绘制层级一致。原来先问页面，页面若有整屏/左上角热区
        // 就会先把这次点击吃掉，表现就是「点了没反应」。
        if self.pages.len() > 1 && self.btn_back.touch(touch) {
            button_hit();
            // 左上角箭头 = **返回上一页**（照 phira-0.8.2 `MainScene::touch` 的写法）：
            // 先给当前页面一个拦截机会（`Page::on_back_pressed`，例如它自己开了子弹窗就先关掉），
            // 没被拦下才 `pop()` —— `pop()` 走 `fader.back()`，于是返回也有过渡动画，
            // 不再是原来 `reset_to_root()` 那种「一键弹回开始界面 + 硬切」。
            if !self.pages.last_mut().unwrap().on_back_pressed(s) {
                if self.pages.len() == 2 {
                    if let Some(bgm) = &mut self.bgm {
                        bgm.set_low_pass(0.)?;
                    }
                }
                self.pop();
            }
            return Ok(true);
        }
        if self.pages.last_mut().unwrap().touch(touch, s)? {
            return Ok(true);
        }
        Ok(false)
    }

    fn update(&mut self, tm: &mut TimeManager) -> Result<()> {
        // HUD 编辑器请求切页：重建页面栈（根页面保留、不重建，因此不需要异步）。
        if let Some(p) = crate::hud::take_goto() {
            self.goto_hud_page(p)?;
        }
        UI_AUDIO.with(|it| it.borrow_mut().recover_if_needed())?;
        // 游戏内暂停面板调过判定偏移：写回配置并保存
        if let Some(offset) = prpr::scene::take_pending_offset() {
            get_data_mut().config.offset = offset;
            let _ = save_data();
        }
        // 游戏内暂停面板调过音量：写回配置并保存
        if let Some((music, sfx)) = prpr::scene::take_pending_volume() {
            let config = &mut get_data_mut().config;
            config.volume_music = music;
            config.volume_sfx = sfx;
            let _ = save_data();
        }
        if get_data().config.mp_enabled {
            MP_PANEL.with(|it| {
                if let Some(panel) = it.borrow_mut().as_mut() {
                    panel.update(tm)
                } else {
                    Ok(())
                }
            })?;
        }
        let rank_icons = self.state.icons.clone();
        let s = &mut self.state;
        s.update(tm);
        if s.fader.transiting() {
            let pos = self.pages.len() - 2;
            self.pages[pos].update(s)?;
        }
        self.pages.last_mut().unwrap().update(s)?;
        if !s.fader.transiting() {
            match self.pages.last_mut().unwrap().next_page() {
                NextPage::Overlay(mut sub) => {
                    if self.pages.len() == 1 {
                        if let Some(bgm) = &mut self.bgm {
                            bgm.set_low_pass(LOW_PASS)?;
                        }
                    }
                    sub.enter(s)?;
                    if !sub.can_play_bgm() {
                        if let Some(bgm) = &mut self.bgm {
                            let _ = bgm.fade_out(0.5);
                        }
                    }
                    self.pages.push(sub);
                    s.fader.sub(s.t);
                }
                NextPage::Pop => {
                    self.pop();
                }
                NextPage::None => {}
            }
        } else if let Some(true) = s.fader.done(s.t) {
            self.pages.pop().unwrap().exit()?;
            self.pages.last_mut().unwrap().enter(s)?;
        }
        if let Some(bgm) = &mut self.bgm {
            if BGM_VOLUME_UPDATED.fetch_and(false, Ordering::Relaxed) {
                bgm.set_amplifier(get_data().config.volume_bgm)?;
            }
        }
        // WebView 界面（登录 / 开始 / 曲库）触发的待办：
        // 1) 启动谱面 → 在 next_scene 里一次性返回 SongScene overlay（复用原有游玩入口）
        // 2) 打开原生页面（设置 / 活动 / 资源包）→ 推到页面栈
        // 3) 开始页「开始游玩」→ 原生曲库（需要 state.icons，所以在这里单独构造）
        if crate::webui::take_pending_library() {
            let icons = crate::page::global_icons().with_context(|| "UI icons are not ready (library)")?;
            let st = &mut self.state;
            let mut sub = crate::page::LibraryPage::new(icons, st.icons.clone())?;
            sub.enter(st)?;
            self.pages.push(Box::new(sub));
            st.fader.sub(st.t);
        }
        if let Some(scene) = crate::webui::take_pending_scene(rank_icons.clone()) {
            self.webui_next_scene = Some(scene);
        }
        if let Some(mut sub) = crate::webui::take_pending_open_page(rank_icons) {
            // 重新借用 self.state：上方 `s` 的借用已在第 473 行结束，
            // 这里开一条新借用，避免与上面 match 分支里的 self.pop() 冲突。
            let st = &mut self.state;
            sub.enter(st)?;
            self.pages.push(sub);
            st.fader.sub(st.t);
        }
        // 回到根页面（开始界面）时，重新把 WebView 铺上来
        if self.pages.len() == 1
            && self.webui_next_scene.is_none()
            && crate::webui::webui_embedded()
            && !crate::webui::is_webui_visible()
        {
            crate::webui::set_webui_visible(true);
        }

        if let Some(task) = &mut self.import_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        show_error(err.context(itl!("import-failed")));
                    }
                    Ok((chart, warnings)) => {
                        if let Some(warn) = parse_warnings_to_string(&warnings) {
                            Dialog::plain(itl!("warning"), warn).show();
                        }
                        show_message(itl!("import-success")).ok();
                        get_data_mut().charts.push(chart);
                        save_data()?;
                        self.state.reload_local_charts();
                        NEED_UPDATE.store(true, Ordering::Relaxed);
                    }
                }
                self.import_task = None;
            }
        }
        if let Some((id, file)) = take_file() {
            match id.as_str() {
                "_import_auto" => {
                    let new_id = match File::open(&file).map(BufReader::new).map(zip::ZipArchive::new) {
                        Ok(Ok(zip)) => {
                            if zip.file_names().any(|name| name.ends_with("click.png")) {
                                "_import_respack"
                            } else {
                                "_import"
                            }
                        }
                        _ => "_import",
                    };
                    return_file(new_id.to_owned(), file);
                }
                "_import" => {
                    let export_info = (|| -> Result<Option<(ExportInfo, usize)>> {
                        let file = File::open(&file)?;
                        let mut archive = zip::ZipArchive::new(file)?;
                        let export_info = match archive.by_name("export.json") {
                            Err(zip::result::ZipError::FileNotFound) => {
                                return Ok(None);
                            }
                            Err(err) => {
                                return Err(err.into());
                            }
                            Ok(file) => serde_json::from_reader(file)?,
                        };
                        let mut count = 0;
                        for i in 0..archive.len() {
                            let file = archive.by_index(i)?;
                            if file.enclosed_name().is_some_and(|it| it.extension().is_some_and(|ext| ext == "zip")) {
                                count += 1;
                            }
                        }
                        Ok(Some((export_info, count)))
                    })();
                    match export_info {
                        Err(err) => {
                            show_error(err.context(itl!("import-failed")));
                        }
                        Ok(None) => {
                            self.import_task = Some(Task::new(async move {
                                let file = File::open(&file).context("cannot open file")?;
                                import_chart(file).await
                            }));
                        }
                        Ok(Some((info, count))) => {
                            self.batch_import = Some((file, info));
                            self.batch_import_total = count;
                            confirm_dialog(itl!("batch-import"), itl!("batch-import-confirm", "count" => count), self.batch_import_confirm.clone());
                        }
                    };
                }
                "_import_respack" => {
                    let root = dir::respacks()?;
                    let dir = prpr::dir::Dir::new(&root)?;
                    let mut dir_id: Option<String> = None;
                    let item: Result<ResPackItem> = (|| {
                        let config = {
                            let mut zip = zip::ZipArchive::new(BufReader::new(File::open(&file)?))?;
                            let config: ResPackInfo =
                                serde_yaml::from_reader(zip.by_name("info.yml").context("missing info.yml")?).context("invalid info.yml")?;
                            config.verify()?;
                            let mut buffer = Vec::new();
                            for file in [
                                "click.png",
                                "click_mh.png",
                                "drag.png",
                                "drag_mh.png",
                                "flick.png",
                                "flick_mh.png",
                                "hold.png",
                                "hold_mh.png",
                                "hit_fx.png",
                            ] {
                                let mut entry = zip.by_name(file).with_context(|| format!("missing file: {file}"))?;
                                buffer.clear();
                                entry.read_to_end(&mut buffer)?;
                                image::load_from_memory(&buffer).with_context(|| format!("failed to load image: {file}"))?;
                            }

                            for audio in ["click", "drag", "flick", "ending"] {
                                for ext in [".ogg", ".wav", ".mp3"] {
                                    let mut entry = match zip.by_name(format!("{audio}{ext}").as_str()) {
                                        Err(zip::result::ZipError::FileNotFound) => continue,
                                        Err(err) => return Err(err.into()),
                                        Ok(file) => file,
                                    };
                                    buffer.clear();
                                    entry.read_to_end(&mut buffer)?;
                                    AudioClip::new(mem::take(&mut buffer)).with_context(|| format!("failed to load audio: {audio}"))?;
                                    break;
                                }
                            }
                            config
                        };

                        let mut uuid = Uuid::new_v4();
                        while dir.exists(uuid.to_string())? {
                            uuid = Uuid::new_v4();
                        }
                        let id = uuid.to_string();
                        dir.create_dir_all(&id)?;
                        let dir = dir.open_dir(&id)?;
                        dir_id = Some(id.clone());
                        unzip_into(BufReader::new(File::open(file)?), &dir, false).context("failed to unzip")?;
                        get_data_mut().respacks.push(id.clone());
                        save_data()?;
                        Ok(ResPackItem::new(Some(format!("{root}/{id}").into()), config.name))
                    })();
                    match item {
                        Err(err) => {
                            show_error(err.context(itl!("import-respack-failed")));
                            if let Some(id) = &dir_id {
                                dir.remove_dir_all(id)?;
                            }
                            crate::webui::webui_emit("respacks_changed", serde_json::json!({ "ok": false }));
                        }
                        Ok(item) => {
                            RESPACK_ITEM.with(|it| *it.borrow_mut() = Some(item));
                            show_message(itl!("import-respack-success"));
                            crate::webui::webui_emit("respacks_changed", serde_json::json!({ "ok": true }));
                        }
                    }
                }
                _ => return_file(id, file),
            }
        }
        if self.batch_import_confirm.swap(false, Ordering::Relaxed) {
            if let Some((file, _info)) = self.batch_import.take() {
                let (tx, rx) = mpsc::channel();
                self.batch_import_rx = Some(rx);
                self.batch_imported_charts.clear();
                self.batch_import_task = Some(Task::new(async move {
                    let mut archive = zip::ZipArchive::new(BufReader::new(File::open(&file)?))?;
                    let charts_dir = dir::charts()?;
                    for i in 0..archive.len() {
                        let mut file = archive.by_index(i)?;
                        let Some(name) = file.enclosed_name() else {
                            continue;
                        };
                        if name.extension().is_none_or(|it| it != "zip") {
                            continue;
                        }
                        let [Component::Normal(dir), Component::Normal(name)] = name.components().collect::<Vec<_>>()[..] else {
                            continue;
                        };
                        let mut to_tempfile = || -> std::io::Result<_> {
                            let mut tf = tempfile()?;
                            std::io::copy(&mut file, &mut tf)?;
                            tf.seek(SeekFrom::Start(0))?;
                            Ok(tf)
                        };
                        match dir.to_str() {
                            Some("custom") => {
                                let tf = to_tempfile()?;
                                let (chart, warnings) = import_chart(tf)
                                    .await
                                    .with_context(|| itl!("batch-import-failed-chart", "chart" => name.display().to_string()))?;
                                let _ = tx.send(ImportChart::Imported(Box::new(chart), warnings)).ok();
                            }
                            Some("download") => {
                                let Some(id) = name.to_str().and_then(|it| it.strip_suffix(".zip")).and_then(|it| it.parse::<i32>().ok()) else {
                                    warn!("invalid batch import download id: {:?}", name);
                                    continue;
                                };
                                let local_path = format!("download/{id}");
                                let path = PathBuf::from(format!("{charts_dir}/{local_path}"));
                                if std::fs::exists(&path)? {
                                    let info: ChartInfo = serde_yaml::from_reader(File::open(path.join("info.yml"))?)?;
                                    let _ = tx.send(ImportChart::Skipped(info.name));
                                    continue;
                                }
                                std::fs::create_dir(&path)?;
                                let tf = to_tempfile()?;
                                let (chart, warnings) = import_chart_to(&path, local_path, tf)
                                    .await
                                    .with_context(|| itl!("batch-import-failed-chart", "chart" => name.display().to_string()))?;
                                let _ = tx.send(ImportChart::Imported(Box::new(chart), warnings)).ok();
                            }
                            _ => {
                                warn!("invalid batch import dir: {:?}", dir);
                            }
                        }
                    }
                    Ok(())
                }));
            }
        }

        if let Some(rx) = &mut self.batch_import_rx {
            match rx.try_recv() {
                Ok(chart) => {
                    self.batch_imported_charts.push(chart);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    warn!("import thread panicked");
                    self.batch_import_rx = None;
                }
            }
        }

        if let Some(task) = &mut self.batch_import_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        let charts = dir::charts()?;
                        for chart in self.batch_imported_charts.drain(..) {
                            if let ImportChart::Imported(chart, _) = chart {
                                let path = format!("{charts}{}", chart.local_path);
                                let _ = std::fs::remove_dir_all(path);
                            }
                        }
                        show_error(err.context(itl!("batch-import-failed")));
                    }
                    Ok(()) => {
                        let mut warning_messages = vec![];
                        let data = get_data_mut();
                        let mut count = 0;
                        let mut skipped = String::new();
                        for chart in self.batch_imported_charts.drain(..) {
                            match chart {
                                ImportChart::Imported(chart, warnings) => {
                                    if let Some(warn) = parse_warnings_to_string(&warnings) {
                                        warning_messages.push(format!("{}\n{warn}", chart.info.name));
                                    }
                                    data.charts.push(*chart);
                                    count += 1;
                                }
                                ImportChart::Skipped(name) => {
                                    if !skipped.is_empty() {
                                        skipped.push_str(", ");
                                    }
                                    skipped.push_str(&name);
                                }
                            }
                        }
                        save_data()?;
                        self.state.reload_local_charts();
                        NEED_UPDATE.store(true, Ordering::Relaxed);

                        let mut message = itl!("batch-import-success", "count" => count);
                        if !skipped.is_empty() {
                            message.push('\n');
                            message += &itl!("batch-import-downloaded-skipped", "charts" => skipped);
                        }

                        if !warning_messages.is_empty() {
                            message += "\n\n";
                            message += &warning_messages.join("\n\n");
                        }
                        Dialog::simple(message).show();
                    }
                }
                self.batch_import_task = None;
                self.batch_import_rx = None;
            }
        }

        if self.mp_save_pos_at.is_some_and(|it| it < Instant::now()) {
            std::fs::write(position_file()?, format!("{},{}", self.mp_btn_pos.x, self.mp_btn_pos.y))?;
            self.mp_save_pos_at = None;
        }

        Ok(())
    }

    fn render(&mut self, tm: &mut TimeManager, ui: &mut Ui) -> Result<()> {
        // 从原生页返回开始页时，那一帧的可见性标志有时还停在 false（退出是本帧早先发生的），
        // 于是 `render` 走原生分支、把 **Rust 原生首页**画了出来 —— WebView 下一帧才跟上，
        // 观感就是「上一个界面一闪而过」。这里在真正开画之前再判一次：
        // 已经回到根页面、且没有待播的谱面场景时，立刻把可见性补上，
        // 直接走下面的 WebView 分支，原生首页一帧都不画。
        if self.pages.len() == 1
            && self.webui_next_scene.is_none()
            && crate::webui::webui_embedded()
            && !crate::webui::is_webui_visible()
        {
            crate::webui::set_webui_visible(true);
        }

        // WebView 铺在窗口上时整帧都不画（被盖住了），省 GPU。
        // 只有真的嵌入了原生 WebView 且当前可见才跳过；StubHost 下照常渲染 Rust UI。
        if crate::webui::is_webui_visible() {
            // ⚠️ 不能直接 return：`render` 早退就不会清屏，而 WebView 是独立子窗口，
            // 它没盖到的边缘（底部状态栏 / 左侧活动栏那几条）会**残留上一帧的原生 UI**，
            // 观感就是「你的页面 + 我做的界面」混在一起。
            // 所以这里先把整屏填成页面同款深色底，再交还给 WebView。
            set_camera(&ui.camera());
            ui.fill_rect(ui.screen_rect(), Color::new(0.051, 0.055, 0.063, 1.));
            return Ok(());
        }
        set_camera(&ui.camera());

        let m = Metrics::new(ui);
        let th = newui::theme();
        let t = tm.real_time() as f32;
        let base = self.bg_beijing.clone();

        // 开始界面背景 = 设计稿的 `.grid-bg` + `.ambient` + `.vignette`
        // （beijing 底图仍压在最下面，压暗后当纹理用）。
        //
        // 若开启了「背景模糊」且 RT / 着色器可用：把背景渲进半分辨率 RT → 高斯模糊
        // → 把模糊结果整屏铺底，玻璃面板底下透出来的就是真·模糊背景。
        // 任何一步不可用都静默降级：仍走锐利背景（绝不黑屏 / 不 panic）。
        if get_data().config.ui_backdrop_blur && self.backdrop.ensure(ui.viewport.2 as u32, ui.viewport.3 as u32) {
            let ok = self.backdrop.capture_and_blur(ui, |ui: &mut Ui| {
                newui::draw_background(ui, &m, &th, t, Some(base.clone()));
            });
            if ok {
                if let Some(tex) = crate::backdrop::texture() {
                    let sr = ui.screen_rect();
                    ui.fill_rect(sr, (tex, sr, ScaleType::Fit));
                } else {
                    newui::draw_background(ui, &m, &th, t, Some(base));
                }
            } else {
                newui::draw_background(ui, &m, &th, t, Some(base));
            }
        } else {
            crate::backdrop::set_texture(None);
            newui::draw_background(ui, &m, &th, t, Some(base));
        }

        let s = &mut self.state;
        s.update(tm);

        // 状态栏附加信息每帧由页面重填（曲库的曲目计数 / 选中项），先清掉上一帧的。
        newui::set_status_extra(None);

        // 【回开始界面】的过渡特判。
        //
        // 开始界面由原生 WebView（`ui/index.html`）承载，Rust 侧 `pages[0]`（`HomePage`）
        // 只是垫背；而 WebView 是独立子窗口、盖在 GL 画布之上，没法把 Rust 的滑出动画叠在它上面。
        // 所以这一段过渡这样做：
        //   1. 垫背根页**不画**（见下面的守卫）→ 露出的是 Rust 自己的 TriGrid 网格底；
        //   2. 开始界面用的是**同一套 TriGrid 背景**，所以「页面滑走 → 网格底 → 开始界面」
        //      在视觉上是连贯的，不会突兀；
        //   3. Rust 外壳（标题栏 / 状态栏）跟着淡出 —— 开始界面那边没有 Rust 外壳，
        //      不淡出的话它会在最后一帧「啪」地消失。
        // `to_root_p` 是「开始界面（下层页）的入场进度」：0 → 1。
        let st = s.t;
        let to_root = s.fader.transiting() && self.pages.len() == 2 && crate::webui::webui_embedded();
        let to_root_p = if to_root { s.fader.for_sub(|f| f.progress(st)) } else { 0. };

        // 1. page
        if s.fader.transiting() {
            let pos = self.pages.len() - 2;
            // 【跳过垫背的根页】过渡时露出的「下层页」如果正是根页（开始界面），**不要画它**：
            // 开始界面实际由原生 WebView（`ui/index.html`）承载，根页 `HomePage` 只是垫背，
            // 画出来就是用户反馈的「闪过我之前写的老界面」。
            // 只有真的嵌了 WebView 才跳过；没嵌入时根页**就是**开始界面，照常画。
            if !(pos == 0 && crate::webui::webui_embedded()) {
                let old = s.fader.distance;
                s.fader.distance *= -0.6;
                self.pages[pos].render(ui, s)?;
                s.fader.distance = old;
            }
        }
        s.fader.sub = true;
        s.fader.reset();
        crate::hud::begin_frame();
        self.pages.last_mut().unwrap().render(ui, s)?;
        s.fader.sub = false;

        // 2. 标题：New UI 把场景名放进了标题栏，这里不再画旧的「PHIRA vX.Y.Z」大标题
        if s.fader.transiting() {
            s.fader.reset();
        }

        // 3. back —— New UI 里放在内容区左上角（活动栏右边），样式是设计稿的方形图标按钮
        if self.pages.len() >= 2 {
            let base = newui::back_rect(ui, &m);
            // 命中区比可视方块大一圈：返回键是高频操作，点边缘落空会很难受。
            // 命中区按**最终位置**登记（动画期间也点得到，和 0.8.2 一致）。
            self.btn_back.set(ui, newui::back_hit_rect(ui, &m));
            // 照 phira-0.8.2：返回键跟着页面过渡一起**滑入 / 滑出** ——
            // 栈从 1 变 2 时它从下方滑到位，弹回 1 页时反向滑走，
            // 而不是「啪」地凭空出现 / 消失（原来是硬切，观感很跳）。
            let st = s.t;
            let dy = (match self.pages.len() {
                1 => 1.,
                2 => s.fader.for_sub(|f| f.progress(st)),
                _ => 0.,
            } * base.h)
                .clamp(0., base.h);
            let r = Rect::new(base.x, base.y + dy, base.w, base.h);
            newui::rounded(ui, r, m.u(6.), th.wash(0.05));
            newui::rounded_stroke(ui, r, m.u(6.), m.u(1.), th.wash(0.08));
            newui::draw_icon(ui, &m, newui::Icon::ChevronLeft, r.feather(-r.w * 0.3), th.text_dim, 1.8);
        }

        // 页面自绘的顶栏（曲库的搜索框等）画在 fader 之外，跟着回开始界面的淡出一起走。
        ui.alpha(1. - to_root_p, |ui| self.pages.last_mut().unwrap().render_top(ui, s))?;

        // 4. New UI 外壳：最后画，盖在页面之上
        {
            let label = self.pages.last().unwrap().label().to_string();
            self.shell.scene = if self.pages.len() == 1 || label == "PHIRA" {
                "start.scene".to_owned()
            } else {
                label
            };
            self.shell.active = newui::current_activity();
            self.shell.has_new = newui::has_new();
            let me = get_data().me.as_ref();
            self.shell.player = me.map(|it| it.name.clone());
            self.shell.rks = me.map_or(0., |it| it.rks);
            self.shell.online = me.is_some() && !get_data().config.offline_mode;
            // 回开始界面时外壳跟着淡出（`to_root_p`: 0 → 1），
            // 否则它会在 WebView 铺上来的那一帧整块消失。
            let scene = self.shell.scene.clone();
            let st = s.t;
            ui.alpha(1. - to_root_p, |ui| self.shell.render(ui, &m, &th, &scene, st));
        }

        if get_data().config.mp_enabled {
            let r = 0.06;
            self.mp_btn_pos.y = self.mp_btn_pos.y.clamp(-ui.top, ui.top);
            self.mp_btn_pos.x = self.mp_btn_pos.x.clamp(-1., 1.);
            ui.fill_circle(self.mp_btn_pos.x, self.mp_btn_pos.y, r, ui.background());
            let r = Rect::new(self.mp_btn_pos.x, self.mp_btn_pos.y, 0., 0.).feather(r);
            self.mp_btn.set(ui, r);
            let r = r.feather(-0.02);
            ui.fill_rect(r, (*self.mp_icon, r));

            MP_PANEL.with(|it| {
                if let Some(panel) = it.borrow_mut().as_mut() {
                    panel.render(tm, ui);
                }
            });
        }

        // 自定义布局编辑层：画在最上面（页面 / 外壳 / 联机按钮之后）
        if crate::hud::edit_active() {
            crate::hud::editor_render(ui);
        }

        if self.import_task.is_some() {
            ui.full_loading(itl!("importing"), s.t);
        }
        if self.batch_import_task.is_some() {
            let current = self.batch_imported_charts.len();
            let total = self.batch_import_total;
            ui.full_loading(itl!("batch-importing", "current" => current, "total" => total), s.t);
        }

        Ok(())
    }

    fn next_scene(&mut self, _tm: &mut TimeManager) -> NextScene {
        // WebView 请求启动的谱面优先（一次性消费）
        if let Some(scene) = self.webui_next_scene.take() {
            if let Some(bgm) = &mut self.bgm {
                let _ = bgm.fade_out(0.5);
            }
            return scene;
        }
        let res = MP_PANEL
            .with(|it| it.borrow_mut().as_mut().and_then(|it| it.next_scene()))
            .unwrap_or(self.pages.last_mut().unwrap().next_scene(&mut self.state));
        if !matches!(res, NextScene::None) {
            if let Some(bgm) = &mut self.bgm {
                let _ = bgm.fade_out(0.5);
            }
        }
        res
    }
}
