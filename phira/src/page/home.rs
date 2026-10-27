prpr_l10n::tl_file!("home");

use super::{
    load_font_with_cksum, set_bold_font, ChartToolboxPage, EventPage, HistoryPage, LibraryPage, MessagePage, NextPage, Page,
    ResPackPage, SFader, SharedState, BOLD_FONT_CKSUM,
};
use crate::{
    anim::Anim,
    client::{recv_raw, Character, Client, LoginParams, User, UserManager},
    dir, get_data, get_data_mut,
    icons::Icons,
    login::Login,
    newui::{self, Activity, Icon, Metrics, Theme},
    save_data,
    scene::{check_read_tos_and_policy, ProfileScene, JUST_LOADED_TOS},
    sync_data,
    threed::ThreeD,
};
use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use image::DynamicImage;
use macroquad::prelude::*;
use prpr::{
    core::{BOLD_FONT, Matrix, Point},
    ext::{open_url, screen_aspect, RectExt, SafeTexture, ScaleType},
    info::ChartInfo,
    scene::{show_error, NextScene},
    task::Task,
    ui::{button_hit_large, clip_rounded_rect, ClipType, Dialog, FontArc, RectButton, Scroll, Shading, Ui},
};
use prpr_l10n::LANG_IDENTS;
use reqwest::StatusCode;
use serde::Deserialize;
use std::{
    borrow::Cow,
    sync::{atomic::Ordering, Arc},
};
use tracing::{info, warn};

type BoldFontUpdateTask = Task<Result<Option<(FontArc, String)>>>;

/// 双击判定窗口（秒）：两次点击间隔小于它才算双击。
const DOUBLE_CLICK_WINDOW: f32 = 0.4;

#[derive(Deserialize)]
struct Version {
    version: semver::Version,
    date: NaiveDate,
    description: String,
    url: String,
}

/// 开始界面**右侧玻璃面板**的 8 个入口（kaichang 版式，顺序 = 自上而下）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum PanelBtn {
    /// 主按钮「开始游玩 / GAME →」
    Play,
    /// 「资源包」
    ResPack,
    /// 「谱面工具箱」（整宽一行）
    Toolbox,
    /// 「设置」（与「活动」并排）
    Settings,
    /// 「活动」（与「设置」并排）
    Event,
    /// 「上次游玩的谱面」（白底深字）
    LastPlay,
    /// 「消息」（与「关于」并排）
    Messages,
    /// 「关于」（与「消息」并排）
    About,
    /// 「退出」（面板最底部，弱化样式）
    Quit,
}

impl PanelBtn {
    /// 自上而下的排列顺序，也是 `panel_btn` / `panel_rect` 的下标含义。
    const ALL: [PanelBtn; 9] = [
        PanelBtn::Play,
        PanelBtn::LastPlay,
        PanelBtn::ResPack,
        PanelBtn::Toolbox,
        PanelBtn::Settings,
        PanelBtn::Event,
        PanelBtn::Messages,
        PanelBtn::About,
        PanelBtn::Quit,
    ];
}

pub struct HomePage {
    icons: Arc<Icons>,

    /// 右侧玻璃面板的 9 个入口（下标含义见 [`PanelBtn::ALL`]）。
    /// 顺序：开始游玩 / 资源包 / 谱面工具箱 / 设置 / 活动 / 上次游玩 / 消息 / 关于 / 退出。
    panel_btn: [RectButton; 9],
    /// 每个入口的命中矩形（渲染时算出来，鼠标悬停判定用）。
    panel_rect: [Rect; 9],
    /// 鼠标悬停的入口下标（`usize::MAX` = 没有悬停）。
    panel_hover: usize,
    /// 悬停平滑过渡（悬停不再瞬切，0.16s 缓动）。
    panel_hover_p: [Anim<f32>; 9],
    /// 按压回弹：点击后按钮向右冲一下再回落。
    panel_press_i: usize,
    panel_press_p: Anim<f32>,
    /// 左侧立绘区（单击展开角色详情，**双击换立绘**）。
    art_btn: RectButton,
    /// 上一次点立绘的时刻，用来识别双击。
    char_pick_last: Option<f32>,
    /// 菜单里点了「退出」。
    quit: bool,
    /// 进场时刻（设计稿 `.enter` 的交错入场动画）。
    enter_t: f32,
    /// 是否首次进入。仅首次重播完整入场动画，从设置/曲库等子页返回时不再重播。
    first_enter: bool,
    /// 点击闪光时刻（设计稿 `.flash`）。
    flash_t: f32,

    next_page: Option<NextPage>,

    login: Login,
    update_task: Option<Task<Result<User>>>,

    need_back: bool,
    sf: SFader,

    board_task: Option<Task<Result<Option<DynamicImage>>>>,
    /// 已经挑过「上次游玩」的谱面了（挑一次就够，避免每帧重复挑）
    board_last: Option<String>,
    board_tex: Option<SafeTexture>,
    /// 「照搬 kaichang/index.html」的首页立绘贴图 = `assets/3.png`（编译期嵌入）。
    /// 用户名下没有角色立绘时优先用它，观感与 kaichang 原页面一致。
    hero_tex: Option<SafeTexture>,
    /// 开始页 Logo = `phirc.png`（Phirc Mod++ 字标，编译期嵌入）。
    /// 用户指定用它替换原来手绘的「Phirc Mod ++」字标。
    logo_tex: Option<SafeTexture>,
    /// 「上次游玩」卡片上的曲名 / 难度 / 最高分
    last_play_name: Option<String>,
    last_play_diff: Option<String>,
    last_play_score: Option<i32>,

    has_new_task: Option<Task<Result<bool>>>,
    has_new: bool,

    check_update_task: Option<Task<Result<Option<Version>>>>,
    check_bold_font_update_task: Option<BoldFontUpdateTask>,

    character: Character,
    char_appear_p: Anim<f32>,
    char_last_illu: Option<String>,
    char_last_user_id: Option<i32>,
    char_fetch_task: Option<Task<Result<Character>>>,
    char_illu: Option<SafeTexture>,
    char_illu_task: Option<Task<Result<DynamicImage>>>,
    // progress of character screen
    char_screen_p: Anim<f32>,
    char_cached_size: f32,
    char_scroll: Scroll,
    char_edit_btn: RectButton,

    #[cfg(feature = "hykb")]
    beian_btn: RectButton,
}

impl HomePage {
    pub async fn new() -> Result<Self> {
        let update_task = if get_data().config.offline_mode {
            None
        } else if let Some(u) = &get_data().me {
            UserManager::request(u.id);
            Some(Task::new(async {
                Client::login(LoginParams::RefreshToken {
                    token: &get_data().tokens.as_ref().unwrap().1,
                })
                .await?;
                let me = Client::get_me().await?;
                // On HYKB builds a restored session still requires the native SDK
                // to be signed in: silently restore the SDK session and tear the
                // in-game session down if that login fails/cancels (`ok_or_err`).
                // The signed-in HYKB account no longer has to match the restored
                // Phira account — any successful HYKB login is accepted.
                #[cfg(feature = "hykb")]
                crate::obtain_hykb_credential_silent().await?.ok_or_err()?;
                Ok(me)
            }))
        } else {
            None
        };

        let flavor = match load_file("flavor").await.map(String::from_utf8) {
            Ok(Ok(flavor)) => flavor.trim().to_owned(),
            _ => "none".to_owned(),
        };

        let icons = Arc::new(Icons::new().await?);
        super::set_global_icons(Arc::clone(&icons));
        let mut res = Self {
            icons: Arc::clone(&icons),

            panel_btn: std::array::from_fn(|_| RectButton::new()),
            panel_rect: [Rect::new(0., 0., 0., 0.); 9],
            panel_hover: usize::MAX,
            panel_hover_p: std::array::from_fn(|_| Anim::new(0.)),
            panel_press_i: 0,
            panel_press_p: Anim::new(0.),
            art_btn: RectButton::new(),
            char_pick_last: None,
            quit: false,
            enter_t: f32::NEG_INFINITY,
            first_enter: true,
            flash_t: f32::NEG_INFINITY,

            next_page: None,

            login: Login::new(icons),
            update_task,

            need_back: false,
            sf: SFader::new(),

            board_task: None,
            board_last: None,
            board_tex: None,
            // 照搬 kaichang/index.html 的左立绘：assets/3.png（编译期嵌入，不依赖网络/账号）
            // 路径基准是本文件（phira/src/page/），所以指向 phira/art/assets/3.png。
            // （原目录名是 `phira/ui/`，WebView 界面移除后改名为 `phira/art/`，
            //   因为这里剩下的唯一素材就是首页左立绘，跟 WebView 无关了。）
            hero_tex: image::load_from_memory(include_bytes!("../../art/assets/3.png"))
                .ok()
                .map(|it| it.into()),
            logo_tex: image::load_from_memory(include_bytes!("../../art/assets/phirc.png"))
                .ok()
                .map(|it| it.into()),
            last_play_name: None,
            last_play_diff: None,
            last_play_score: None,

            has_new_task: None,
            has_new: false,

            check_update_task: Some(Task::new(async move {
                Ok(recv_raw(Client::get("/check-update").query(&[("version", env!("CARGO_PKG_VERSION")), ("flavor", &flavor)]))
                    .await?
                    .json()
                    .await?)
            })),
            check_bold_font_update_task: {
                let cksum = BOLD_FONT_CKSUM.with(|it| it.borrow().clone());
                Some(Task::new(async move {
                    let resp = Client::get("/font-bold")
                        .query(&[("cksum", cksum)])
                        .query(&[("new_bold_font", "true")])
                        .send()
                        .await?;
                    if resp.status() == StatusCode::NOT_MODIFIED {
                        info!("bold font not modified");
                        return Ok(None);
                    }
                    if !resp.status().is_success() {
                        let status = resp.status().as_str().to_owned();
                        let text = resp.text().await.context("failed to receive text")?;
                        if let Ok(what) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(detail) = what["error"].as_str() {
                                bail!("request failed ({status}): {detail}");
                            }
                        }
                        bail!("request failed ({status}): {text}");
                    }
                    info!("downloading new bold font");
                    let bytes = resp.bytes().await?;
                    std::fs::write(dir::bold_font_path()?, &bytes).context("failed to save font")?;
                    Ok(Some(load_font_with_cksum(bytes.to_vec())?))
                }))
            },

            character: get_data().character.clone().unwrap_or_default(),
            char_appear_p: Anim::new(0.),
            char_last_illu: None,
            char_last_user_id: None,
            char_fetch_task: None,
            char_illu: None,
            char_illu_task: None,
            char_screen_p: Anim::new(0.),
            char_cached_size: 0.,
            char_scroll: Scroll::new().use_clip(ClipType::Clip),
            char_edit_btn: RectButton::new(),

            #[cfg(feature = "hykb")]
            beian_btn: RectButton::new(),
        };
        res.load_char_illu();

        Ok(res)
    }
}

impl HomePage {
    fn load_char_illu(&mut self) {
        let key = if self.character.illust == "@" {
            format!("@{}", self.character.id)
        } else {
            self.character.illust.clone()
        };
        if self.char_last_illu.as_ref() == Some(&key) {
            return;
        }
        self.char_last_illu = Some(key);

        self.char_appear_p.set(0.);

        // 自定义立绘：设置里导入后图片会被写到 <data>/char.img，优先使用它
        if let Ok(root) = dir::root() {
            let path = format!("{root}/char.img");
            if std::path::Path::new(&path).is_file() {
                self.char_illu_task = Some(Task::new(async move { Ok(image::load_from_memory(&std::fs::read(path)?)?) }));
                return;
            }
        }

        #[cfg(closed)]
        if self.character.illust == "@" {
            let id = self.character.id.clone();
            self.char_illu_task =
                Some(Task::new(
                    async move { Ok(image::load_from_memory(&crate::inner::resolve_data(load_file(&format!("res/{id}.char")).await?))?) },
                ));
        } else {
            let file = crate::page::File {
                url: self.character.illust.clone(),
            };
            self.char_illu_task =
                Some(Task::new(async move { Ok(image::load_from_memory(&crate::inner::resolve_data(file.fetch().await?.to_vec()))?) }));
        }
    }

    fn fetch_has_new(&mut self) {
        if get_data().config.offline_mode || get_data().me.is_none() || get_data().tokens.is_none() {
            self.has_new_task = None;
            self.has_new = false;
            return;
        }
        let time = get_data().message_check_time.unwrap_or_default();
        self.has_new_task = Some(Task::new(async move {
            #[derive(Deserialize)]
            struct Resp {
                has: bool,
            }
            let resp: Resp = recv_raw(Client::get("/message/has_new").query(&[("checked", time)]))
                .await?
                .json()
                .await?;
            Ok(resp.has)
        }));
    }

    /// 打开曲库。
    fn open_library(&mut self, s: &mut SharedState) -> Result<()> {
        newui::set_current_activity(Activity::Library);
        self.next_page = Some(NextPage::Overlay(Box::new(LibraryPage::new(Arc::clone(&self.icons), s.icons.clone())?)));
        Ok(())
    }

    /// 「关于」弹窗。
    fn show_about(&mut self) {
        Dialog::plain(
            format!("Phirc Mod++  v{}", env!("CARGO_PKG_VERSION")),
            format!("{}\n\nPhira · GPL-3.0\nhttps://phira.moe", tl!("about-desc")),
        )
        .show();
    }

    /// New UI 开始界面（kaichang 版式）：**左立绘 + 右竖排玻璃按钮面板**。
    ///
    /// 版式参照 `kaichang/rust/index.html`：
    /// 左侧人物立绘（右侧羽化渐隐）、右上品牌字标、右侧一列玻璃按钮、
    /// 右下版本号、左下提示。
    fn render_home(&mut self, ui: &mut Ui, t: f32) {
        let m = Metrics::new(ui);
        let th = newui::theme();
        let c = m.content(ui);

        // 按用户要求：**删掉开始界面的「跟随鼠标 / 陀螺仪视差」**，nx/ny 恒为 0。
        // （保留变量只为不动 `render_art` / `render_panel` 的签名与调用点。）
        let (nx, ny) = (0f32, 0f32);

        // ---------- 版式：左立绘 / 右面板 ----------
        // kaichang：`.left-character{left:-3%;bottom:0;height:100vh}` + `img{height:100vh;width:auto}`。
        // **立绘区取整屏高**（`ui.screen_rect()`，不是内容区）：上缘顶到屏幕顶、
        // 下缘顶到屏幕底（状态栏由外壳最后画、盖在上面），左缘就是屏幕最左 —— 三边零缝。
        // 窄屏（compact）横向放不下两块，立绘索性铺满整屏当背景层，玻璃面板压在它上面居中。
        let sr = ui.screen_rect();
        let art_w = if m.compact { sr.w } else { (sr.w * 0.52).min(m.u(780.)) };
        let art = Rect::new(sr.x, sr.y, art_w, sr.h);
        let panel_margin = m.u(if m.compact { 26. } else { 60. });
        let panel_w = if m.compact {
            (c.w - panel_margin * 2.).max(m.u(240.))
        } else {
            (c.w * 0.42).min(m.u(460.))
        };
        let panel = Rect::new(c.right() - panel_margin - panel_w, c.y, panel_w, c.h);

        // 立绘：**保持平面、不倾斜**（用户明确要求「左边立绘不要改成倾斜的」）。
        // 之前这里套了 `css_perspective(.., -9° ..)` 把立绘「立起来」，观感不对，已移除。
        self.render_art(ui, t, &m, &th, art, nx, ny);
        self.render_panel(ui, t, &m, &th, panel, nx, ny);

        // ---------- 右下角版本号（kaichang `.version`） ----------
        {
            let p = newui::enter_p((t - self.enter_t).max(0.), 0.52);
            ui.scope(|ui| {
                ui.dy((1. - p) * m.u(10.));
                ui.alpha(p, |ui| {
                    // 压在页面底（深色主题是网格 / 浅色主题是浅灰）→ 跟主题墨色
                    let col = if th.dark { th.white(0.62) } else { th.text_mute };
                    newui::text(
                        ui,
                        &m,
                        format!("Version: v {}", env!("CARGO_PKG_VERSION")),
                        c.right() - m.u(28.),
                        c.bottom() - m.u(13.),
                        15.,
                        col,
                        (1., 1.),
                    );
                });
            });
        }

        // 点击闪光（设计稿 `.flash`：0.62s 的径向高光）
        let fp = (t - self.flash_t) / 0.62;
        if (0. ..1.).contains(&fp) {
            let sr = ui.screen_rect();
            let a = (1. - fp) * 0.34;
            ui.fill_rect(
                sr,
                newui::radial(
                    (sr.x + sr.w * 0.42, sr.y + sr.h * 0.52),
                    sr.w * 0.45,
                    th.with_a(th.glow, a),
                    th.with_a(th.glow, 0.),
                ),
            );
        }
    }

    /// 右侧：品牌字标 + 竖排玻璃按钮面板（kaichang 的 `.right-panel`）。
    ///
    /// 自上而下：主按钮「开始游玩 / GAME →」→「资源包」→「设置 | 活动」→
    /// 「上次游玩的谱面」（白底深字）→「消息 | 关于」→「退出」（弱化）。
    /// 原来的 5 个菜单项 + 上次游玩 + 2 个快捷入口**一个不少**，只是换了位置与外观。
    fn render_panel(&mut self, ui: &mut Ui, t: f32, m: &Metrics, th: &Theme, area: Rect, nx: f32, ny: f32) {
        let enter = (t - self.enter_t).max(0.);

        // ---------- 竖向排版 ----------
        // 尺寸取 kaichang 的设计稿 px（主按钮 100 / 次级 76 / 并排 76 / 间距 16）。
        let cs = if m.compact { 0.86 } else { 1. };
        let h_main = 100. * cs;
        let h_row = 76. * cs;
        let h_last = 86. * cs;
        let h_quit = 46. * cs;
        let gap = if m.compact { 13. } else { 16. };
        let row_gap = if m.compact { 12. } else { 14. };
        let brand_h = 64.;
        let brand_gap = 22.;
        let margin = 16.;

        // 分区：主按钮 / 上次游玩 / 次级入口（3 行 × 2 列）/ 退出
        let need = h_main + h_last + h_row * 3. + h_quit + gap * 5.;
        // 窗口矮的时候整列等比收缩（0.62 是下限，再矮就只能贴边了）
        let avail = area.h / m.u(1.);
        let s = ((avail - brand_h - brand_gap - margin * 2.) / need).clamp(0.62, 1.);
        let gy = m.u(gap * s);
        let row_gy = m.u(row_gap * s);
        let total = m.u(brand_h + brand_gap + need * s);
        let y0 = area.center().y - total * 0.5;
        let x = area.x;
        let w = area.w;
        let hpx = |v: f32| m.u(v * s);

        // ---------- 卡片底板（用户要求：「保留左立绘，面板做成卡片」） ----------
        //
        // 品牌字标 + 8 个入口整体放进**一张**磨砂玻璃卡里，和设置页 / 曲库 / 消息页
        // 那些卡片同款（都走 `newui::glass_card`）。里面每个按钮仍是自己的小玻璃块，
        // 形成「卡中卡」的层次 —— 外层给的是「这是一个面板」的整体感，
        // 内层给的是「这一项可以点」的可操作感。
        //
        // 内边距刻意留得比按钮间距大（18/16 vs 16），否则卡片边界和按钮边界会糊在一起。
        {
            let pad_x = m.u(18.);
            let pad_y = m.u(16.);
            let card = Rect::new(x - pad_x, y0 - pad_y, w + pad_x * 2., total + pad_y * 2.);
            let p = newui::enter_p(enter, 0.02);
            ui.scope(|ui| {
                ui.dy((1. - p) * m.u(14.));
                ui.alpha(p, |ui| {
                    newui::glass_card(ui, card, m.u(22.), th, 0.);
                });
            });
        }

        // ---------- 品牌字标（对应 kaichang 右上角的 `.logo`） ----------
        {
            let p = newui::enter_p(enter, 0.05);
            ui.scope(|ui| {
                ui.dx(nx * m.u(8.));
                ui.dy((1. - p) * m.u(18.) + ny * m.u(6.));
                ui.alpha(p, |ui| {
                    // 呼吸小圆点 + SYSTEM READY
                    let blip = newui::pulse(t, 2.6, 0.);
                    let dot = m.u(6.);
                    ui.fill_circle(
                        x + dot * 0.5,
                        y0 + m.u(8.),
                        dot * 0.5 * (0.75 + 0.25 * (1. - blip)),
                        Color {
                            a: 0.28 + 0.72 * (1. - blip),
                            ..th.teal
                        },
                    );
                    newui::text(ui, m, "SYSTEM READY", x + dot + m.u(8.), y0 + m.u(8.), 11., th.text_mute, (0., 0.5));

                    // Logo = `phirc.png`（用户指定）：右对齐到面板右边、等比缩放。
                    // ⚠️ 图是**白字 + 淡蓝辉光**（为深底设计）→ 必须垫一块深色圆角底，
                    // 否则浅色主题下白字压白卡看不见。（之前用主题墨色 tint 会把整个 logo
                    // 染成纯黑，用户否掉。）
                    if let Some(tex) = self.logo_tex.as_ref() {
                        let lh = m.u(40.);
                        let lw = lh * (tex.width() / tex.height().max(1.));
                        let lr = Rect::new(area.right() - lw, y0 + m.u(6.), lw, lh);
                        // 图是白字 + 淡蓝辉光（为深底设计）。**不垫深色底**（用户否掉「黑底」）：
                        // 深色主题直接用白；浅色主题染成强调色（白字压白卡看不见，纯黑又被否掉）。
                        let tint = if th.dark { WHITE } else { th.accent };
                        ui.fill_rect(lr, (**tex, lr, ScaleType::Fit, tint));
                    }

                    // 品牌下的一条细分割线
                    ui.fill_rect(
                        Rect::new(x, y0 + m.u(brand_h - 10.), w, m.u(1.)),
                        if th.dark { th.white(0.10) } else { th.black(0.10) },
                    );
                });
            });
        }

        // ---------- 9 个入口的矩形（顺序必须与 `PanelBtn::ALL` 一致） ----------
        // 分区：主按钮 / 上次游玩 / 次级入口（3 行 × 2 列）/ 退出
        let mut y = y0 + m.u(brand_h + brand_gap);
        let r_main = Rect::new(x, y, w, hpx(h_main));
        y = r_main.bottom() + gy;
        let r_last = Rect::new(x, y, w, hpx(h_last));
        y = r_last.bottom() + gy;

        // 次级入口：3 行 × 2 列（资源包|工具箱 / 设置|活动 / 消息|关于）
        let half = (w - row_gy) * 0.5;
        let grid_row = |yy: f32| Rect::new(x, yy, w, hpx(h_row));
        let r_gr1 = grid_row(y);
        y = r_gr1.bottom() + gy;
        let r_gr2 = grid_row(y);
        y = r_gr2.bottom() + gy;
        let r_gr3 = grid_row(y);
        y = r_gr3.bottom() + gy;

        let left = |r: Rect| Rect::new(r.x, r.y, half, r.h);
        let right = |r: Rect| Rect::new(r.x + half + row_gy, r.y, half, r.h);
        let r_respack = left(r_gr1);
        let r_toolbox = right(r_gr1);
        let r_settings = left(r_gr2);
        let r_event = right(r_gr2);
        let r_messages = left(r_gr3);
        let r_about = right(r_gr3);

        let r_quit = Rect::new(x, y, w, hpx(h_quit));

        let rects = [r_main, r_last, r_respack, r_toolbox, r_settings, r_event, r_messages, r_about, r_quit];
        for (i, r) in rects.iter().enumerate() {
            self.panel_rect[i] = *r;
            self.panel_btn[i].set(ui, *r);
        }

        // ---------- 悬停（面板是平面，鼠标世界坐标直接用） ----------
        let (mx, my) = newui::mouse_world();
        if (-1. ..=1.).contains(&mx) && (-ui.top..=ui.top).contains(&my) {
            let mouse = vec2(mx, my);
            let mut hover = usize::MAX;
            for (i, r) in rects.iter().enumerate() {
                if r.contains(mouse) {
                    hover = i;
                }
            }
            self.panel_hover = hover;
        }
        let mut hp = [0f32; 9];
        for (i, h) in hp.iter_mut().enumerate() {
            *h = newui::hover_step(&mut self.panel_hover_p[i], self.panel_hover == i, t);
        }
        // 按压回弹：0.28s 从 1 回 0，按钮向右冲 5px（沿用原来左侧菜单的动效）
        let press_i = self.panel_press_i;
        let press_p = self.panel_press_p.now(t);

        // ---------- 绘制 ----------
        // 设计稿 `.enter`：各块依次淡入 + 上移
        const DELAY: [f32; 9] = [0.14, 0.20, 0.26, 0.26, 0.30, 0.30, 0.36, 0.36, 0.44];
        for i in 0..PanelBtn::ALL.len() {
            let r = rects[i];
            let p = newui::enter_p(enter, DELAY[i]);
            let pr = if press_i == i { press_p } else { 0. };
            // 按下回弹（kaichang `:active{scale(.985)}` + 松手弹回）：
            // `panel_press_p` 在按下瞬间置 1、0.28s 内缓动回 0，所以
            //   pr = 1（刚按下）→ 缩小到 0.985
            //   pr → 0（弹回）→ 恢复 1.0
            // 即「按下去缩一下、松手弹回来」。不用 `touching()`：`RectButton::touch`
            // 是边沿触发、且一按就切页，拿不到持续按住的状态。
            let ps = newui::press_scale(pr);
            let r = newui::scaled_rect(r, ps, ps);
            ui.scope(|ui| {
                ui.dx(nx * m.u(if i == 0 { 10. } else { 6. }) + pr * m.u(5.));
                ui.dy((1. - p) * m.u(18.) + ny * m.u(if i == 0 { 6. } else { 4. }));
                ui.alpha(p, |ui| {
                    match PanelBtn::ALL[i] {
                        PanelBtn::Play => newui::draw_main_btn_lg(ui, m, th, r, hp[i], &tl!("menu-start"), "GAME"),
                        PanelBtn::ResPack => newui::draw_sec_btn_lg(ui, m, th, r, hp[i], &tl!("respack"), Icon::Download),
                        PanelBtn::Toolbox => newui::draw_row_btn_lg(ui, m, th, r, hp[i], &ttl!("chart-toolbox"), Icon::Gear, false, t),
                        PanelBtn::Settings => newui::draw_row_btn_lg(ui, m, th, r, hp[i], &tl!("menu-settings"), Icon::Gear, false, t),
                        PanelBtn::Event => newui::draw_row_btn_lg(ui, m, th, r, hp[i], &tl!("event"), Icon::Trophy, false, t),
                        PanelBtn::LastPlay => self.draw_last_play(ui, m, th, r, hp[i]),
                        PanelBtn::Messages => newui::draw_row_btn_lg(ui, m, th, r, hp[i], &tl!("menu-messages"), Icon::Chat, self.has_new, t),
                        PanelBtn::About => newui::draw_row_btn_lg(ui, m, th, r, hp[i], &tl!("menu-about"), Icon::Info, false, t),
                        PanelBtn::Quit => newui::draw_quit_btn_lg(ui, m, th, r, hp[i], &tl!("menu-quit")),
                    }
                });
            });
        }
    }

    /// 左侧人物立绘区（kaichang `.left-character`）：**左缘贴屏幕、上下顶满、
    /// 等比 cover 裁切**，右边缘羽化渐隐。点击这块仍然展开角色详情（原功能不变）。
    #[allow(clippy::too_many_arguments)]
    fn render_art(&mut self, ui: &mut Ui, t: f32, m: &Metrics, th: &Theme, area: Rect, nx: f32, ny: f32) {
        // 入场（设计稿 `.enter` 延迟 .10s）
        let enter = newui::enter_p((t - self.enter_t).max(0.), 0.10);

        // 立绘后面的环境光：跟视差反向漂（`translate3d(nx*-30px, ny*-16px, 0)`）
        let halo_c = (
            area.center().x - nx * m.u(30.),
            area.bottom() - area.h * 0.22 - ny * m.u(16.),
        );
        ui.fill_rect(
            area,
            newui::radial(
                halo_c,
                area.w * 0.68,
                if th.dark {
                    Color::new(0., 0.48, 0.8, 0.30)
                } else {
                    Color::new(0., 0.37, 0.72, 0.14)
                },
                Color::new(0., 0., 0., 0.),
            ),
        );

        // 立绘呼吸（设计稿 `breathe` 7s：上浮 9px）
        let breathe = newui::pulse(t, 7., 0.);
        // 右边缘羽化宽度（占立绘区宽度的比例，对应 kaichang 的
        // `mask-image: linear-gradient(to right, black 55%, transparent 100%)`）
        const FEATHER_R: f32 = 0.30;
        const FEATHER_T: f32 = 0.05;

        let p = self.char_appear_p.now(t);
        ui.scope(|ui| {
            ui.dy((1. - enter) * m.u(30.) - breathe * m.u(9.));
            ui.alpha(enter, |ui| {
                if let Some(illu) = &self.char_illu {
                    // 立绘：**cover 等比裁切铺满整列**（不是 contain 居中留白）——
                    // 自带图（`data/char.img`）什么比例都有，所以取「露出的那一块」再画，
                    // 绝不拉伸变形；锚左 / 锚下 = kaichang 的 `object-position: left bottom`。
                    let src = newui::cover_left_bottom(illu.width(), illu.height(), area);
                    draw_art_feathered(
                        ui,
                        **illu,
                        src,
                        area,
                        area,
                        Color::new(1., 1., 1., p),
                        FEATHER_R,
                        FEATHER_T,
                    );
                } else if let Some(tex) = self.hero_tex.as_ref().or(self.board_tex.as_ref()) {
                    // 没拿到角色立绘（本地 `data/char.img` 不存在、账号又没立绘）时，
                    // **优先用照搬 kaichang 的 `assets/3.png`**（观感与原版一致），
                    // 再退回「上次游玩」那张谱面的插图。
                    // ⚠️ 不能乘 `p`（`char_appear_p`）：它只在角色立绘加载完成时才动，
                    // 没立绘时恒为 0 —— 乘上去整块立绘区就空了。
                    let src = newui::cover_left_bottom(tex.width(), tex.height(), area);
                    draw_art_feathered(
                        ui,
                        **tex,
                        src,
                        area,
                        area,
                        Color::new(1., 1., 1., 0.95),
                        FEATHER_R,
                        FEATHER_T,
                    );
                } else {
                    // 设计稿的 `.art-slot`：虚线占位框
                    let sw = (area.w * 0.6).min(m.u(340.));
                    let sh = sw * 4. / 3.;
                    let slot = Rect::new(area.center().x - sw * 0.5, area.bottom() - m.u(96.) - sh, sw, sh);
                    newui::rounded(ui, slot, m.u(16.), th.wash(0.02));
                    newui::rounded_stroke(ui, slot, m.u(16.), m.u(1.), th.with_a(th.teal, 0.22));
                    newui::draw_icon(
                        ui,
                        m,
                        Icon::Music,
                        Rect::new(slot.center().x - m.u(17.), slot.center().y - m.u(32.), m.u(34.), m.u(34.)),
                        th.text_mute,
                        1.5,
                    );
                    newui::text_center(ui, m, "Character Art", slot.center().x, slot.center().y + m.u(16.), 10.5, th.text_mute);
                    newui::text_center(ui, m, "PNG / WebP", slot.center().x, slot.center().y + m.u(32.), 9.5, th.text_mute);
                }

                self.art_btn.set(ui, area);
            });
        });

        // ---------- 左下：品牌徽标 + 操作提示 ----------
        // 提示压在立绘上 → 白字 + 黑投影；没立绘（浅灰底）→ 跟主题墨色。
        // 锚点是**内容区底**（状态栏上沿），不是立绘区底 —— 立绘区现在是整屏高，
        // 直接贴它底下会被状态栏压住。
        let over_art = self.char_illu.is_some() || self.hero_tex.is_some() || self.board_tex.is_some();
        let cb = m.content(ui);
        let p = newui::enter_p((t - self.enter_t).max(0.), 0.46);
        ui.scope(|ui| {
            ui.dy((1. - p) * m.u(14.));
            ui.alpha(p, |ui| {
                let badge = Rect::new(area.x + m.u(24.), cb.bottom() - m.u(22.) - m.u(48.), m.u(152.), m.u(48.));
                newui::acrylic(ui, badge, m.u(9.), th);
                newui::rounded_stroke(ui, badge, m.u(9.), m.u(1.), th.with_a(th.glow, 0.18));
                ui.fill_rect(Rect::new(badge.x, badge.y + m.u(8.), m.u(2.), badge.h - m.u(16.)), th.accent);
                newui::text(ui, m, "Phira", badge.x + m.u(16.), badge.center().y - m.u(8.), 15., th.text_hi, (0., 0.5));
                newui::text(ui, m, "RHYTHM DIVISION", badge.x + m.u(16.), badge.center().y + m.u(9.), 9.5, th.text_mute, (0., 0.5));

                let hint = tl!("hint-character-detail");
                let hx = badge.right() + m.u(18.);
                let hy = badge.center().y;
                if over_art {
                    // 压在立绘 / 图片上：强制纯白 + 黑投影
                    newui::text(ui, m, hint.clone(), hx + m.u(1.), hy + m.u(1.), 13., th.black(0.45), (0., 0.5));
                    newui::text(ui, m, hint.clone(), hx, hy, 13., th.white(0.72), (0., 0.5));
                } else {
                    newui::text(ui, m, hint, hx, hy, 13., if th.dark { th.white(0.50) } else { th.text_mute }, (0., 0.5));
                }
            });
        });
    }

    /// 「上次游玩」卡。
    ///
    /// ⚠️ 2026-10-09 改版：原来它是一张**写死的白卡 + 深色字**（给「深色页面底 +
    /// 玻璃面板」那套服务的）。改成 iOS 平面风格后，右侧面板本身就是白卡，
    /// 再来一张白卡就完全糊在一起了；而且深色主题下一张纯白卡也很扎眼。
    /// 现在改成跟主题走的实色卡：底色比面板**拉开一档明度差**，压在面板上才看得
    /// 出是一张独立的卡（浅色 = 比白深一档的灰，深色 = 比 `#1C1C1E` 亮一档）。
    fn draw_last_play(&self, ui: &mut Ui, m: &Metrics, th: &Theme, r: Rect, hp: f32) {
        let r = newui::hover_scale(r, hp);
        let radius = m.u(14.);
        newui::rounded(ui, r, radius, newui::mix(th.bg_elev, th.bg_hover, 0.55 + 0.35 * hp));
        newui::rounded_stroke(ui, r, radius, m.u(1.), th.border);

        // 封面
        let cover_s = r.h - m.u(22.);
        let cover = Rect::new(r.x + m.u(11.), r.center().y - cover_s * 0.5, cover_s, cover_s);
        clip_rounded_rect(ui, cover, m.u(9.), |ui| {
            newui::rounded(ui, cover, m.u(9.), th.wash(0.08));
            if let Some(tex) = &self.board_tex {
                ui.fill_rect(cover, (**tex, cover, ScaleType::CropCenter, Color::new(1., 1., 1., 0.96)));
            }
        });
        if self.board_tex.is_none() {
            newui::draw_icon(ui, m, Icon::Music, cover.feather(-cover_s * 0.28), th.text_mute, 1.7);
        }

        let tx = cover.right() + m.u(13.);
        let max_w = (r.right() - m.u(64.) - tx).max(m.u(60.));
        newui::text(ui, m, "LAST PLAYED", tx, r.y + m.u(13.), 10.5, th.text_mute, (0., 0.));
        let name = self.last_play_name.clone().unwrap_or_else(|| tl!("last-played-none").into_owned());
        newui::text_ellipsis(ui, m, name, tx, r.y + m.u(28.), 18., max_w, th.text_hi, (0., 0.));

        // 底部一行：难度 chip + 分数
        let my = r.bottom() - m.u(15.);
        let mut mx = tx;
        if let Some(diff) = &self.last_play_diff {
            let cw = newui::measure(ui, m, diff.clone(), 11.5).w + m.u(15.);
            let chip = Rect::new(mx, my - m.u(8.), cw, m.u(16.));
            let kind = th.diff_kind(diff);
            // 难度 chip 用**深底白字**：`text_diff` 不画投影，白字必须自带深底
            newui::rounded(ui, chip, m.u(5.), th.with_a(th.black(1.), if th.dark { 0.30 } else { 0.58 }));
            newui::text_diff(ui, diff, chip.center().x, chip.center().y, m.f(11.), kind, (0.5, 0.5), chip.w - m.u(6.), 0.);
            mx = chip.right() + m.u(10.);
        }
        if let Some(score) = self.last_play_score {
            newui::text(ui, m, fmt_score(score), mx, my, 12.5, th.text_dim, (0., 0.5));
        } else if self.board_tex.is_none() {
            newui::text(ui, m, tl!("random-no-chart"), mx, my, 12., th.text_mute, (0., 0.5));
        }

        // 右侧播放圆钮（跟主题墨色走，悬停变大）
        let pr = m.u(38.) * (1. + 0.06 * hp);
        let pc = (r.right() - m.u(37.), r.center().y);
        ui.fill_circle(pc.0, pc.1, pr * 0.5, th.wash(0.06 + 0.05 * hp));
        ui.stroke_circle(pc.0, pc.1, pr * 0.5, m.u(1.), th.wash(0.22 + 0.20 * hp));
        newui::draw_icon(
            ui,
            m,
            Icon::Play,
            Rect::new(pc.0 - pr * 0.18, pc.1 - pr * 0.20, pr * 0.36, pr * 0.40),
            th.text_hi,
            1.,
        );
    }
}

// ============================================================
// 开始界面的版式件
// ============================================================
//
// 这里**不再自己实现**玻璃底板 / 带字距排版 / 按钮 —— 全部在 `crate::newui` 里，
// 和设置页、曲库、消息页共用同一套（`glass_plate_flat` / `glass_border` /
// `glass_text` / `hover_scale` / `text_tracked` / `draw_*_btn_lg`）。
// 以前这里各留一份，导致「改了 newui 的玻璃质感、开始页却没变」。
//
// 本文件只剩「开始页独有」的东西：立绘羽化着色、上次游玩白卡、面板竖向排版。

// ============================================================
// 立绘羽化（右边缘渐隐）
// ============================================================

/// 立绘的逐顶点 alpha 羽化着色：**右边缘渐隐**。
///
/// 对应 kaichang 的 `mask-image: linear-gradient(to right, black 55%, transparent 100%)`。
/// 为什么不用「盖一条背景色渐变」：背景是网格 + 环境光，不是纯色，
/// 盖上去只会留下一条色带；逐顶点 alpha 才是真透明。
struct ArtFeather {
    tex: Texture2D,
    /// 采样区（归一化 uv）
    src: Rect,
    /// 目标矩形（世界坐标）
    dst: Rect,
    /// 羽化量参考矩形（一般是整块立绘区）
    fade: Rect,
    color: Color,
    /// 右边缘羽化宽度（占 `fade.w` 比例）
    feather_r: f32,
    /// 上边缘羽化宽度（占 `fade.h` 比例）
    feather_t: f32,
}

impl Shading for ArtFeather {
    fn new_vertex(&self, mat: &Matrix, p: &Point, alpha: f32) -> Vertex {
        let t = mat.transform_point(p);
        let ux = ((p.x - self.dst.x) / self.dst.w).clamp(0., 1.);
        let uy = ((p.y - self.dst.y) / self.dst.h).clamp(0., 1.);

        let mut a = self.color.a;
        if self.feather_r > 1e-5 {
            // 越靠右越透明：右边界处 0，往内 `feather_r` 段线性升到 1
            let e = (1. - (p.x - self.fade.x) / self.fade.w) / self.feather_r;
            a *= e.clamp(0., 1.);
        }
        if self.feather_t > 1e-5 {
            // 上边缘同理（立绘从上方自然融进背景）
            let e = ((p.y - self.fade.y) / self.fade.h) / self.feather_t;
            a *= e.clamp(0., 1.);
        }

        Vertex::new(
            t.x,
            t.y,
            0.,
            self.src.x + self.src.w * ux,
            self.src.y + self.src.h * uy,
            Color { a: a * alpha, ..self.color },
        )
    }

    fn texture(&self) -> Option<Texture2D> {
        Some(self.tex)
    }
}

/// 把一张贴图按 `dst` 铺开，右 / 上边缘做**真 alpha 羽化**（顶点要够密才平滑）。
#[allow(clippy::too_many_arguments)]
fn draw_art_feathered(
    ui: &mut Ui,
    tex: Texture2D,
    src: Rect,
    dst: Rect,
    fade: Rect,
    color: Color,
    feather_r: f32,
    feather_t: f32,
) {
    const COLS: u16 = 32;
    const ROWS: u16 = 10;

    let sh = ArtFeather {
        tex,
        src,
        dst,
        fade,
        color,
        feather_r,
        feather_t,
    };
    let mut b = ui.builder(sh);
    for j in 0..=ROWS {
        let y = dst.y + dst.h * (j as f32 / ROWS as f32);
        for i in 0..=COLS {
            b.add(dst.x + dst.w * (i as f32 / COLS as f32), y);
        }
    }
    let stride = COLS + 1;
    for j in 0..ROWS {
        for i in 0..COLS {
            let k = j * stride + i;
            b.triangle(k, k + 1, k + stride);
            b.triangle(k + 1, k + stride, k + stride + 1);
        }
    }
    b.commit();
}

/// 分数加千分位（设计稿里的 `998,420`）。
fn fmt_score(score: i32) -> String {
    let s = score.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if score < 0 {
        format!("-{out}")
    } else {
        out
    }
}

impl Page for HomePage {
    fn label(&self) -> Cow<'static, str> {
        "PHIRA".into()
    }

    fn enter(&mut self, s: &mut SharedState) -> Result<()> {
        // 仅首次进入重播完整入场动画；从设置/曲库等子页返回时保持已就绪状态，
        // 避免每次点返回都从头播放一遍动画（体验突兀）。
        if self.first_enter {
            self.enter_t = s.t;
            self.first_enter = false;
        }
        if self.need_back {
            self.sf.enter(s.t);
            self.need_back = false;
        }
        self.fetch_has_new();
        Ok(())
    }

    fn touch(&mut self, touch: &Touch, s: &mut SharedState) -> Result<bool> {
        if self.sf.transiting() {
            return Ok(true);
        }
        // The HYKB startup check (refresh + verify SDK session) is blocking: keep
        // the home page inert until it resolves.
        #[cfg(feature = "hykb")]
        if self.update_task.is_some() {
            return Ok(true);
        }
        let t = s.t;
        let rt = s.rt;
        if self.login.touch(touch, s.t) {
            return Ok(true);
        }
        if self.char_screen_p.now(rt) < 1e-2 {
            // ---- 右侧玻璃面板的 8 个入口 ----
            // 位置 / 外观全换了，但每个入口的行为与改版前**逐条对应**：
            // 开始游玩 与 上次游玩 → 曲库；资源包 / 设置 / 活动 / 消息 / 关于 / 退出 各自不变。
            for i in 0..PanelBtn::ALL.len() {
                if self.panel_btn[i].touch(touch) {
                    self.panel_hover = i;
                    self.flash_t = t;
                    // 按压回弹动效
                    self.panel_press_i = i;
                    self.panel_press_p.set(1.);
                    self.panel_press_p.goto(0., t, 0.28);
                    button_hit_large();
                    match PanelBtn::ALL[i] {
                        PanelBtn::Play | PanelBtn::LastPlay => self.open_library(s)?,
                        PanelBtn::ResPack => {
                            // WebView 可用时打开资源包页（ResPack.html），否则回退原生 ResPackPage
                            if crate::webui::host_available() {
                                crate::webui::open_respack();
                            } else {
                                self.next_page = Some(NextPage::Overlay(Box::new(ResPackPage::new(Arc::clone(&self.icons))?)));
                            }
                        }
                        PanelBtn::Toolbox => {
                            self.next_page = Some(NextPage::Overlay(Box::new(ChartToolboxPage::new())));
                        }
                        PanelBtn::Settings => {
                            crate::webui::open_settings();
                        }
                        PanelBtn::Event => {
                            if get_data().me.is_none() {
                                self.login.enter(t);
                            } else {
                                self.next_page = Some(NextPage::Overlay(Box::new(EventPage::new(Arc::clone(&self.icons), s.icons.clone()))));
                            }
                        }
                        PanelBtn::Messages => {
                            newui::set_current_activity(Activity::Messages);
                            self.next_page = Some(NextPage::Overlay(Box::new(MessagePage::new(Arc::clone(&self.icons), s.icons.clone()))));
                        }
                        PanelBtn::About => self.show_about(),
                        PanelBtn::Quit => self.quit = true,
                    }
                    return Ok(true);
                }
            }
        } else {
            if self.char_scroll.touch(touch, t) {
                return Ok(true);
            }
            if self.char_edit_btn.touch(touch) {
                let _ = open_url("https://phira.moe/settings/account");
            }
        }
        #[cfg(feature = "hykb")]
        if self.beian_btn.touch(touch) {
            let _ = open_url("https://beian.miit.gov.cn/#/home");
            return Ok(true);
        }
        if self.art_btn.touch(touch) {
            // 立绘区**双击** → 打开相册换立绘（系统文件选择器，id 与设置页一致）。
            // 单击仍是原行为：展开 / 收起角色详情。
            // （同一块矩形只用一个 RectButton 判定，避免两个按钮抢同一次点击。）
            // 按用户要求：**去掉**「点立绘展开角色简介」——单击立绘不再展开/收起简介。
            // （双击 → 打开相册换立绘的行为保留。）
            if let Some(last) = self.char_pick_last {
                if rt - last < DOUBLE_CLICK_WINDOW {
                    self.char_pick_last = None;
                    prpr::scene::request_file("home_char");
                    return Ok(true);
                }
            }
            self.char_pick_last = Some(rt);
            return Ok(true);
        }
        Ok(false)
    }

    fn update(&mut self, s: &mut SharedState) -> Result<()> {
        let t = s.t;

        // 活动栏点过来的跳转请求（MainScene 先把页面栈弹回根页再投递）
        match newui::take_activity() {
            Some(Activity::Library) => self.open_library(s)?,
            Some(Activity::Messages) => {
                self.next_page = Some(NextPage::Overlay(Box::new(MessagePage::new(Arc::clone(&self.icons), s.icons.clone()))));
            }
            Some(Activity::Ranking) => {
                self.next_page = Some(NextPage::Overlay(Box::new(HistoryPage::new())));
            }
            Some(Activity::Settings) => {
                crate::webui::open_settings();
            }
            Some(Activity::Account) => {
                if let Some(me) = &get_data().me {
                    self.need_back = true;
                    self.sf.goto(t, ProfileScene::new(me.id, self.icons.user.clone(), s.icons.clone()));
                } else {
                    self.login.enter(t);
                }
            }
            Some(Activity::Home) | None => {}
        }
        // HYKB builds require an account: while signed out, keep the login panel
        // forced open. Polling here (rather than only on entry) also covers the
        // player manually logging out and popping back to the home page.
        #[cfg(feature = "hykb")]
        if get_data().me.is_none() {
            self.login.force(t);
        }
        self.login.update(t)?;
        let current_user = Some(get_data().me.as_ref().map_or(-1, |it| it.id));
        self.char_scroll.update(t);
        if self.char_last_user_id != current_user {
            let locale = get_data().language.clone().unwrap_or(LANG_IDENTS[0].to_string());
            self.char_last_user_id = current_user;
            if get_data().config.offline_mode || get_data().me.is_none() || get_data().tokens.is_none() {
                self.char_fetch_task = None;
            } else {
                self.char_fetch_task =
                    Some(Task::new(async move { Ok(recv_raw(Client::get("/me/char").query(&[("locale", locale)])).await?.json().await?) }));
            }
        }
        if let Some(task) = &mut self.update_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        // wtf bro
                        if format!("{err:?}").contains("invalid token") {
                            get_data_mut().me = None;
                            get_data_mut().tokens = None;
                            let _ = save_data();
                            sync_data();
                        }
                        // TODO: better error handling
                        show_error(err.context(tl!("failed-to-update") + "\n" + tl!("note-try-login-again")));
                    }
                    Ok(val) => {
                        get_data_mut().me = Some(val);
                        save_data()?;
                    }
                }
                self.update_task = None;
            }
        }
        // 「上次游玩」：优先挑有本地成绩的那张谱面，没有就退回第一张。
        // 只挑一次 —— 卡片是「上次游玩」，不该每隔几秒换一首。
        if self.board_task.is_none() && self.board_last.is_none() {
            let charts = &get_data().charts;
            let pick = charts
                .iter()
                .find(|it| get_data().local_records.get(&it.local_path).is_some_and(|r| r.is_some()))
                .or_else(|| charts.first());
            if let Some(pick) = pick {
                let path = pick.local_path.clone();
                self.last_play_name = Some(pick.info.name.clone());
                self.last_play_diff = Some(format!("{} {}", pick.info.level, pick.info.difficulty as i32));
                self.last_play_score = get_data().local_records.get(&path).and_then(|it| it.as_ref()).map(|it| it.score);
                self.board_last = Some(path.clone());
                let dir = prpr::dir::Dir::new(format!("{}/{}", dir::charts()?, path))?;
                self.board_task = Some(Task::new(async move {
                    let info: ChartInfo = serde_yaml::from_reader(dir.open("info.yml")?)?;
                    let bytes = dir.read(info.illustration)?;
                    Ok(Some(image::load_from_memory(&bytes)?))
                }));
            } else {
                self.board_last = Some(String::new());
            }
        }
        if let Some(task) = &mut self.board_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        warn!(?err, "failed to load illustration for board");
                    }
                    Ok(image) => {
                        if let Some(image) = image {
                            let tex: SafeTexture = image.into();
                            self.board_tex = Some(tex);
                        }
                    }
                }
                self.board_task = None;
            }
        }
        if let Some(task) = &mut self.has_new_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        warn!("fail to load has new {:?}", err);
                    }
                    Ok(has) => {
                        self.has_new = has;
                        newui::set_has_new(has);
                    }
                }
                self.has_new_task = None;
            }
        }
        if let Some(task) = &mut self.check_update_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        warn!("fail to check update {:?}", err);
                    }
                    Ok(Some(ver)) => {
                        if get_data().ignored_version.as_ref().is_none_or(|it| it < &ver.version) {
                            Dialog::plain(
                                tl!("update", "version" => ver.version.to_string()),
                                tl!("update-desc", "date" => ver.date.to_string(), "desc" => ver.description),
                            )
                            .buttons(vec![
                                ttl!("cancel").into_owned(),
                                tl!("update-ignore").into_owned(),
                                tl!("update-go").into_owned(),
                            ])
                            .listener(move |_dialog, pos| {
                                match pos {
                                    1 => {
                                        get_data_mut().ignored_version = Some(ver.version.clone());
                                        let _ = save_data();
                                    }
                                    2 => {
                                        let _ = open_url(&ver.url);
                                    }
                                    _ => {}
                                }
                                false
                            })
                            .show();
                        }
                    }
                    _ => {}
                }
                self.check_update_task = None;
            }
        }
        if let Some(task) = &mut self.check_bold_font_update_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        warn!("fail to check bold font update {:?}", err);
                    }
                    Ok(None) => {}
                    Ok(Some(parsed)) => {
                        info!(cksum = parsed.1, "new bold font");
                        set_bold_font(parsed);
                    }
                }
                self.check_bold_font_update_task = None;
            }
        }
        if let Some(task) = &mut self.char_illu_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        warn!(?err, "fail to load char illu");
                    }
                    Ok(image) => {
                        self.char_appear_p.goto(1., t, 0.5);
                        let tex: SafeTexture = image.into();
                        self.char_illu = Some(tex.with_mipmap());
                    }
                }
                self.char_illu_task = None;
            }
        }
        if let Some(task) = &mut self.char_fetch_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        warn!(?err, "fail to load char");
                    }
                    Ok(char) => {
                        info!(?char, "char loaded");
                        self.character = char;
                        get_data_mut().character = Some(self.character.clone());
                        let _ = save_data();
                        self.char_cached_size = 0.;
                        self.load_char_illu();
                    }
                }
                self.char_fetch_task = None;
            }
        }
        if JUST_LOADED_TOS.fetch_and(false, Ordering::Relaxed) {
            check_read_tos_and_policy(true, true);
        }
        // 立绘双击换图：系统文件选择器返回后，把图复制到 <data>/char.img 再重新加载
        // （与设置页「导入立绘」同一套落盘位置，两边改的是同一份文件）。
        if let Some((id, file)) = prpr::scene::take_file() {
            if id == "home_char" {
                let dst = dir::root().map(|it| format!("{it}/char.img"));
                match dst.and_then(|dst| -> Result<()> {
                    std::fs::copy(&file, &dst)?;
                    Ok(())
                }) {
                    Ok(()) => {
                        self.char_cached_size = 0.;
                        self.load_char_illu();
                        prpr::scene::show_message(tl!("char-imported")).ok();
                    }
                    Err(err) => show_error(err.context(tl!("char-import-failed").into_owned())),
                }
            } else {
                prpr::scene::return_file(id, file);
            }
        }

        Ok(())
    }

    fn render(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        // ⚠️ 原生 Home 页**彻底删掉**：WebView 可用时这里一个字都不画。
        //
        // 用户要求「直接把老界面删了」—— 之前只是把「回到它」的出口全堵上
        //（hide / open_page('home') / 返回回根都改回 WebView），但只要有任何一条
        // 路径把它露出来（比如换全屏时 WebView 那一帧还没跟上窗口尺寸），
        // 用户就会看到那个「开始游戏 / 资源包 / 谱面工具箱 / 设置 / 活动 / 消息 / 关于 / 退出」
        // 的老面板。这里直接不画，它就再也露不出来了。
        //
        // 不画也不影响功能：它的按钮本来就被 WebView 盖住、点不到；`update` / `enter`
        // 照旧跑（导入任务、登录态这些副作用都在那里，不在 render 里）。
        // WebView 不可用（没有 WebView2）时照旧完整渲染，兜底不变。
        if crate::webui::host_available() {
            return Ok(());
        }

        let t = s.t;
        let rt = s.rt;

        let cp = self.char_screen_p.now(rt);

        s.render_fader(ui, |ui| {
            ui.alpha(1. - cp, |ui| {
                self.render_home(ui, t);
            });
        });

        // 角色详情页：点右侧立绘展开
        if cp > 1e-5 {
            let height = 0.8 - ((screen_aspect() - 16. / 9.) * 0.2).min(0.2);
            let r = Rect::new(0.16, (-height - height * cp) / 4., 0.6, height);
            let mat = ThreeD::build(vec2(0., 0.), r, 0.12);
            let gl = unsafe { get_internal_gl() }.quad_gl;
            gl.push_model_matrix(mat);

            ui.alpha(cp, |ui| {
                let mut r = Rect::new(r.x, r.y + 0.14, r.w, r.h - 0.14);
                // 角色详情面板 = 磨砂玻璃（原来铺的是 `semi_black(0.3)`，浅色主题下是一块脏灰砖）
                let th = newui::theme();
                newui::acrylic(ui, r, 0.02, &th);
                ui.fill_rect(Rect::new(r.x, r.y, 0.01, r.h), th.accent);
                let mut t = ui.text(tl!("change-char")).pos(r.x + 0.01, r.bottom() + 0.015).size(0.3);
                let ir = t.measure().feather(0.007);
                t.ui.fill_rect(ir, th.wash(0.10));
                self.char_edit_btn.set(t.ui, ir);
                t.draw();
                let pad = 0.01;

                let mut t = ui
                    .text(self.character.name_en())
                    .pos(r.right() - pad, r.bottom() - pad)
                    .anchor(1., 1.)
                    // 角色详情底现在是磨砂玻璃（跟主题）→ 水印同样跟主题（深色加白 / 浅色加黑）
                    .color(th.wash(0.22));
                if self.char_cached_size < 1e-6 {
                    let mut initial = 2.;
                    loop {
                        t = t.size(initial);
                        if t.measure().w < r.w * 0.7 {
                            break;
                        }
                        initial *= 0.95;
                    }
                    self.char_cached_size = initial;
                } else {
                    t = t.size(self.char_cached_size);
                }
                t.draw();

                r.x += 0.01;
                r.w -= 0.01;

                self.char_scroll.size((r.w, r.h));
                ui.scope(|ui| {
                    ui.dx(r.x);
                    ui.dy(r.y);
                    let ow = r.w;
                    self.char_scroll.render(ui, |ui| {
                        let r = Rect::new(0., 0., r.w, r.h);
                        let r = r.feather(-0.03);
                        let r = ui.text(&self.character.intro).pos(r.x, r.y).max_width(r.w).multiline().size(0.4).draw();
                        (ow, r.h + 0.1)
                    });
                });
            });

            let r = Rect::new(r.x, r.y, 0.4, 0.12);

            ui.alpha(cp, |ui| {
                let r = ui
                    .text(&self.character.name)
                    .pos(r.x + (1. - cp) * 0.12 + 0.01, r.center().y)
                    .anchor(0., 0.5)
                    .size(self.character.name_size.unwrap_or(1.4))
                    .draw_using(&BOLD_FONT);

                let off = if self.character.baseline { 0. } else { 0.01 };
                // 这两行画在 `r` 面板**外面**，直接压在页面底上（浅色主题是浅灰）→ 跟主题墨色
                ui.text(format!("Artist: {}", self.character.artist))
                    .pos(r.right() + (1. - cp) * 0.1 + 0.02, r.bottom() + off - 0.03)
                    .anchor(0., 1.)
                    .size(0.34)
                    .color(prpr::ui::theme::ink(0.7))
                    .draw();
                ui.text(format!("Designer: {}", self.character.designer))
                    .pos(r.right() + (1. - cp) * 0.1 + 0.016, r.bottom() + off)
                    .anchor(0., 1.)
                    .size(0.34)
                    .color(prpr::ui::theme::ink(0.7))
                    .draw();
            });

            gl.pop_model_matrix();
        }

        s.fader.roll_back();

        self.login.render(ui, t);
        // Cover the home page with a blocking loader during the HYKB startup check.
        #[cfg(feature = "hykb")]
        if self.update_task.is_some() {
            ui.full_loading_simple(t);
        }
        self.sf.render(ui, t);

        Ok(())
    }

    fn next_page(&mut self) -> NextPage {
        self.next_page.take().unwrap_or_default()
    }

    fn next_scene(&mut self, s: &mut SharedState) -> NextScene {
        if self.quit {
            self.quit = false;
            return NextScene::Exit;
        }
        self.sf.next_scene(s.t).unwrap_or_default()
    }
}
