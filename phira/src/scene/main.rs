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
    ext::{open_url, unzip_into, RectExt, SafeTexture},
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

/// 正等用户走完原生「服务条款」流程（走完把用户送回曲库页）。
///
/// 为什么是状态机而不是一次性调用：`check_read_tos_and_policy` 在条款文本还没拉到
/// 时**只启动一次后台拉取、不弹窗**；只调一次的话弹窗永远不会出现，用户就会一直
/// 卡在 `must-accept-tos`。所以要每帧重试到「拉到条款 → 弹窗 → 用户接受」。
static TOS_PENDING: AtomicBool = AtomicBool::new(false);
/// 原生条款对话框是否已经弹出过（避免反复弹）。
static TOS_DIALOG_SHOWN: AtomicBool = AtomicBool::new(false);
/// 拉条款期间的重试计数（每 30 帧推一次，别每帧都调）。
static TOS_RETRY: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

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
        // 回到根页时，过渡期间露出的是 Rust 自己的 TriGrid 网格底，与开始页背景
        // 是同一套，视觉连贯。
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
                // 走自管版本：miniquad 的全屏 style 会丢掉 WS_CLIPCHILDREN，
                // 导致父窗口的 blit 盖住 WebView 子窗口
                crate::webui::set_fullscreen(fs);
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
        // 目标页是否交给 WebView 呈现（这时没有压原生页，下面的 enter() 要跳过）
        let mut webview_page = false;
        match p {
            PageId::Home => {
                // 原生 Home 页已「删掉」：WebView 可用时回开始页，不再露出那个面板。
                if crate::webui::host_available() {
                    crate::webui::show_start_page();
                }
            }
            PageId::Library => {
                // 曲库现在由 WebView 加载 library.html 呈现；
                // WebView2 不可用时回退到原生 Rust 曲库页。
                //
                // ⚠️ 例外：带「要展开的浮层」（B0 / RKS / 课题）时必须走**原生**页 ——
                // 这三个浮层只有原生实现，WebView 版曲库页没有它们的 UI。
                // 之前无脑走 `open_library()` 会把 WebView 又显示回来，
                // `LibraryPage::enter` 永远不会跑 → 浮层请求烂在 `PENDING_OVERLAY` 里，
                // 用户看到的就是「B0 / RKS / 课题点了没反应」。
                let has_overlay = crate::page::library::PENDING_OVERLAY
                    .lock()
                    .map(|g| g.is_some())
                    .unwrap_or(false);
                if crate::webui::host_available() && !has_overlay {
                    crate::webui::open_library();
                    webview_page = true;
                } else {
                    let page = crate::page::LibraryPage::new(icons("library")?, self.state.icons.clone())?;
                    self.pages.push(Box::new(page));
                }
            }
            PageId::Settings => {
                // 设置界面现在由 WebView 加载 Setting.html 呈现；
                // WebView2 不可用时 `open_settings` 无操作，这里回退到原生 Rust 设置页。
                if crate::webui::host_available() {
                    crate::webui::open_settings();
                } else {
                    let icons = icons("settings")?;
                    let page = crate::page::SettingsPage::new(icons.icon.clone(), icons.lang.clone());
                    self.pages.push(Box::new(page));
                }
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
        // 只在真的压了原生页时 enter（Home 与交给 WebView 的页都没有新页可 enter）
        if p != PageId::Home && p != PageId::Settings && !webview_page {
            self.pages.last_mut().unwrap().enter(&mut self.state)?;
        }
        Ok(())
    }

    /// 从 WebView 打开「工具覆盖页」（谱面工具箱 / 活动）。
    ///
    /// 这两页在原生 Home 面板里是 `NextPage::Overlay`（压在 Home 之上），**不是**
    /// `PageId` 的一员，所以不能走 [`Self::goto_hud_page`]：这里把 Home 当底座，
    /// 再把覆盖页压到页面栈上（切页副作用与 `goto_hud_page` 保持一致）。
    fn open_web_tool(&mut self, tool: crate::webui::WebTool) -> Result<()> {
        use crate::webui::WebTool;

        self.pages.truncate(1); // 只留 Home 底座
        self.state.fader.reset();
        crate::hud::set_cur_page(crate::hud::PageId::Home);

        match tool {
            WebTool::Toolbox => {
                self.pages.push(Box::new(crate::page::ChartToolboxPage::new()));
            }
            WebTool::Event => {
                // 活动页要账号数据。原生 Home 面板那里是弹登录框，但从 WebView
                // 进来时拿不到 HomePage 的登录态，所以给一条提示更稳。
                if get_data().me.is_none() {
                    show_message("请先登录后再查看活动").warn();
                    return Ok(());
                }
                let icons = crate::page::global_icons().with_context(|| "UI icons are not ready (event)")?;
                self.pages.push(Box::new(crate::page::EventPage::new(icons, self.state.icons.clone())));
            }
            WebTool::Timing => {
                self.pages.push(Box::new(crate::page::TimingEditor::new()));
            }
        }
        self.pages.last_mut().unwrap().enter(&mut self.state)?;
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
        // 自绘输入框是**模态**的：它存在时把触摸全部吃掉，不让底下的页面 / 外壳跟着
        // 响应（否则点「取消」会顺带点到它背后的按钮）。输入框自己的按钮命中读的是
        // 本帧的原始触摸（`Judge::get_touches()`），不经过这里，所以照样点得到 ——
        // 和引擎自带 `DIALOG` 的拦截方式一致（`prpr::scene::Main::update` 里那个分支）。
        // ⚠️ 返回 **`Ok(false)`**，不是 `Ok(true)`！
        //
        // 引擎派发触摸用的是 `touches.retain_mut(|touch| match scene.touch(touch) { Ok(val) => !val, .. })`
        // —— 即**返回 `true` 的触摸会被从列表里移除**，之后 `self.touches` 就少了它，
        // `ui.set_touches()` 自然也拿不到，而自绘输入框的按钮命中正是读 `ui.touches()`。
        //
        // 之前这里写 `Ok(true)`：触摸被守卫"消费"掉并从列表移除 → 输入框的
        // **确定 / 取消按钮全部失效**（用户实测「确认按钮经常失效」，只有键盘回车还能用）。
        //
        // `Ok(false)` 的效果恰好是我们想要的：**提前 return 所以底下的页面收不到**
        // （模态拦截照旧），但触摸**保留在列表里**，引擎层的输入框能读到它。
        if prpr::scene::has_pending_input() {
            return Ok(false);
        }

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
                // 原生页面栈要回根了 —— 但原生 Home 页已经「删掉」，所以改回 WebView：
                //  • 这一页是 WebView 推过来的（收藏夹点 PLAY / 工具面板）→ 回 WebView 原来那页
                //  • 其它情况（原生设置等）→ 回开始页
                if self.pages.len() == 2 && crate::webui::host_available() {
                    self.pages.truncate(1);
                    s.fader.reset();
                    crate::hud::set_cur_page(crate::hud::PageId::Home);
                    if crate::webui::take_webview_handoff() {
                        crate::webui::show();
                    } else {
                        crate::webui::show_start_page();
                    }
                    return Ok(true);
                }
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
        // 「为浮层（B0 / RKS / 课题）而来」的原生曲库页：浮层关了 → 弹回 WebView 曲库。
        // 与左上角返回键完全同一条路（页面自己出不去，只能在这截断页面栈 + 显示 WebView）。
        if crate::page::library::take_overlay_return() && self.pages.len() >= 2 {
            self.pages.truncate(1);
            self.state.fader.reset();
            crate::hud::set_cur_page(crate::hud::PageId::Home);
            if crate::webui::take_webview_handoff() {
                crate::webui::show();
            } else {
                crate::webui::show_start_page();
            }
        }
        // 原生 Home 页已「删掉」：只要原生页面栈回到根、而 WebView 又被藏着，
        // 就说明马上要露出那个不要的原生 Home 了 → 立刻把 WebView 顶回来。
        // （打完一局回 MainScene、或从原生页用非返回箭头的方式退到根，都靠这条兜住。）
        if self.pages.len() == 1
            && crate::webui::host_available()
            && !crate::webui::webview_visible()
        {
            if crate::webui::take_webview_handoff() {
                crate::webui::show();
            } else {
                crate::webui::show_start_page();
            }
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
        // WebView 已移除：所有页面切换都走原生 `NextPage` / `NextScene`。

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
                "_import" if crate::phic::is_pic_file(std::path::Path::new(&file)) => {
                    // PhiC 剧情包 `.pic`：完整解析（头 + 目录 + 各分块）并导入为「章节」。
                    match crate::phic::import_pic(std::path::Path::new(&file)) {
                        Ok((_id, title, scenes)) => {
                            show_message(format!("已导入章节《{title}》（{scenes} 个场景）"));
                        }
                        Err(err) => {
                            show_error(err.context(itl!("import-failed")));
                        }
                    }
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
                            // 先把调试串取出来：`err.context(..)` 会 move 掉 `err`。
                            let detail = format!("{err:?}");
                            show_error(err.context(itl!("import-respack-failed")));
                            crate::webui::notify_respack_import_error(&detail);
                            if let Some(id) = &dir_id {
                                dir.remove_dir_all(id)?;
                            }
                        }
                        Ok(item) => {
                            RESPACK_ITEM.with(|it| *it.borrow_mut() = Some(item));
                            show_message(itl!("import-respack-success"));
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
        set_camera(&ui.camera());

        let m = Metrics::new(ui);
        let th = newui::theme();
        let t = tm.real_time() as f32;
        let base = self.bg_beijing.clone();

        // 开始界面背景 = 设计稿的 `.grid-bg` + `.ambient` + `.vignette`
        // （beijing 底图仍压在最下面，压暗后当纹理用）。
        //
        // ⚠️「真·背景模糊」管线**已按用户要求删除**。
        //
        // 它原来每帧要：把背景渲进一张半分辨率 RenderTarget → 水平高斯 → 垂直高斯
        // → 再把结果整屏铺底，也就是**两张全屏 RT + 两遍全屏模糊 + 一次整屏绘制**。
        // 现在恒定使用锐利背景（`draw_background`）。
        //
        // `Config::ui_backdrop_blur` 字段仍保留，只为兼容旧存档的反序列化，
        // 不再有任何行为（设置里对应的开关行也已删除）。
        newui::draw_background(ui, &m, &th, t, Some(base));

        let s = &mut self.state;
        s.update(tm);

        // 状态栏附加信息每帧由页面重填（曲库的曲目计数 / 选中项），先清掉上一帧的。
        newui::set_status_extra(None);

        // WebView 已移除：开始界面就是 Rust 原生 `HomePage`，是正常的栈底页面，
        // 不再需要「跳过垫背根页 / 让外壳跟着淡出」那套为独立子窗口做的特判。

        // 1. page
        if s.fader.transiting() {
            let pos = self.pages.len() - 2;
            let old = s.fader.distance;
            s.fader.distance *= -0.6;
            self.pages[pos].render(ui, s)?;
            s.fader.distance = old;
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
            // 页面要求深色（如谱面工具箱）时，返回键也跟着用深色 —— 否则浅色图标压在深底上看不见
            let bth = if self.pages.last().is_some_and(|p| p.prefers_dark()) {
                newui::Theme::DARK
            } else {
                newui::theme()
            };
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
            newui::rounded(ui, r, m.u(6.), bth.wash(0.05));
            newui::rounded_stroke(ui, r, m.u(6.), m.u(1.), bth.wash(0.08));
            newui::draw_icon(ui, &m, newui::Icon::ChevronLeft, r.feather(-r.w * 0.3), bth.text_dim, 1.8);
        }

        // 页面自绘的顶栏（曲库的搜索框等）画在 fader 之外。
        self.pages.last_mut().unwrap().render_top(ui, s)?;

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
            let scene = self.shell.scene.clone();
            let st = s.t;
            self.shell.render(ui, &m, &th, &scene, st);
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

        // 自绘文本输入框：取代 `inputbox` 的原生对话框（理由见 `ui::input_dialog` 的模块注释）。
        // 画在本帧最上层（页面 / 外壳 / 联机面板 / 导入进度之后），这样多页面叠加时也能盖住它们；
        // 没有待处理请求时它什么都不画。
        // ⚠️ 只有 MainScene 在栈顶时才会走到这里：独立场景（SongScene / ProfileScene /
        // GameScene 等）里发出的 request_input 不会被渲染（见模块注释的「生效范围」）。
        // 自绘输入框现在由**引擎层**（`prpr::scene::Main::render` 的 `top_level` 分支）
// 调用，这样在 `SongScene` / `GameScene` 等场景在栈顶时请求的输入也能画出来
// （例如「谱面信息编辑」曾经完全弹不出来）。这里只负责**注册一次**回调。
static INPUT_DIALOG_REGISTERED: std::sync::Once = std::sync::Once::new();
INPUT_DIALOG_REGISTERED.call_once(|| {
    prpr::scene::set_input_dialog_renderer(crate::ui::input_dialog::render);
});

        Ok(())
    }

    fn next_scene(&mut self, _tm: &mut TimeManager) -> NextScene {
        // WebView 请求打开原生页面（设置 / 收藏 / 曲库）：先隐藏 WebView，再在游戏线程切页
        // （页面要 icons，必须回游戏线程建，照 `PENDING_CHART` 的跨线程模式）。
        if let Some(page) = crate::webui::take_pending_page() {
            let _ = self.goto_hud_page(page);
            return NextScene::None;
        }
        // WebView 请求打开「工具覆盖页」（谱面工具箱 / 活动）：这两页不是 `PageId`，
        // 走另一条通道压在 Home 之上。
        if let Some(tool) = crate::webui::take_pending_tool() {
            let _ = self.open_web_tool(tool);
            return NextScene::None;
        }
        // WebView 请求打开外部链接（QQ 频道等）：用系统浏览器打开。
        if let Some(url) = crate::webui::take_pending_url() {
            let _ = open_url(&url);
            return NextScene::None;
        }
        // WebView 请求走「服务条款」流程：**必须在主线程**跑 —— 它会弹原生对话框
        // 并写盘；在 WebView 线程跑会把消息泵卡死（「点在线分类就卡死」的根因）。
        //
        // ⚠️ 原生对话框画在游戏窗口上，而 WebView 是盖在它上面的子窗口 → 必须先
        // 让位给原生，否则用户根本看不到弹窗。
        if crate::webui::take_pending_tos() {
            TOS_PENDING.store(true, std::sync::atomic::Ordering::Relaxed);
            TOS_DIALOG_SHOWN.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        if TOS_PENDING.load(std::sync::atomic::Ordering::Relaxed) {
            use std::sync::atomic::Ordering;
            if crate::scene::tos_ok_for_online() {
                // 已通过（用户刚点了接受，或本来就接受过）→ 回曲库，清状态
                TOS_PENDING.store(false, Ordering::Relaxed);
                TOS_DIALOG_SHOWN.store(false, Ordering::Relaxed);
                crate::webui::open_library();
            } else if !TOS_DIALOG_SHOWN.load(Ordering::Relaxed) {
                if crate::scene::tos_terms_loaded() {
                    // 条款已拉到 → 这次调用一定会弹窗。**先让位给原生再弹**，
                    // 否则 WebView 会盖住对话框（用户点不到「接受」）。
                    TOS_DIALOG_SHOWN.store(true, Ordering::Relaxed);
                    crate::webui::handoff_to_native();
                    let _ = crate::scene::check_read_tos_and_policy(true, false);
                } else {
                    // 条款还没拉到：`check_read_tos_and_policy` 只会启动一次后台拉取
                    // 并返回 false（**不弹窗**）。所以这里要**反复重试**推动它，
                    // 且此时**不隐藏 WebView**（免得用户对着黑屏干等）。
                    let n = TOS_RETRY.fetch_add(1, Ordering::Relaxed);
                    if n % 30 == 0 {
                        let _ = crate::scene::check_read_tos_and_policy(true, false);
                    }
                }
            }
        }
        // WebView 曲库选好 3 张的课题模式：在这里构造 `ChallengeScene`
        // （`ChartItem` 带曲绘纹理，必须在游戏线程用 `load_local()` 建）。
        if let Some(paths) = crate::webui::take_pending_challenge() {
            let all = crate::page::load_local();
            let mut items: Vec<crate::page::ChartItem> = Vec::new();
            for p in &paths {
                if let Some(it) = all.iter().find(|it| it.local_path.as_deref() == Some(p.as_str())) {
                    items.push(it.clone());
                }
            }
            if items.len() == 3 {
                let mut it = items.into_iter();
                let arr = [it.next().unwrap(), it.next().unwrap(), it.next().unwrap()];
                return NextScene::Overlay(Box::new(crate::scene::ChallengeScene::new(arr)));
            }
            crate::scene::show_message("课题谱面已失效，请重新选择").error();
            return NextScene::None;
        }
        // WebView 曲库发起的段位考核：构造 DanScene（共享血条 + 禁重试/禁倒流）。
        if let Some((paths, name)) = crate::webui::take_pending_dan() {
            let all = crate::page::load_local();
            let mut items: Vec<crate::page::ChartItem> = Vec::new();
            for p in &paths {
                if let Some(it) = all.iter().find(|it| it.local_path.as_deref() == Some(p.as_str())) {
                    items.push(it.clone());
                }
            }
            if items.len() == 3 {
                let mut it = items.into_iter();
                let arr = [it.next().unwrap(), it.next().unwrap(), it.next().unwrap()];
                return NextScene::Overlay(Box::new(crate::scene::DanScene::new(arr, name)));
            }
            crate::scene::show_message("考核谱面已失效，请重新选择").error();
            return NextScene::None;
        }
        // WebView 设置页（Setting.html）改了配置：合并 / 存档 / 触发生效。
        crate::webui::apply_pending_config();
        // WebView 资源包页（ResPack.html）的写操作：选中 / 导入 / 导出 / 删除。
        crate::webui::apply_pending_respack();
        // WebView（收藏夹里的 PLAY）请求开局：场景必须在这里构造 —— 贴图只能在游戏线程建。
        let res = match crate::webui::take_next_scene(&mut self.state) {
            Some(res) => res,
            None => MP_PANEL
                .with(|it| it.borrow_mut().as_mut().and_then(|it| it.next_scene()))
                .unwrap_or(self.pages.last_mut().unwrap().next_scene(&mut self.state)),
        };
        if !matches!(res, NextScene::None) {
            if let Some(bgm) = &mut self.bgm {
                let _ = bgm.fade_out(0.5);
            }
        }
        res
    }
}
