prpr_l10n::tl_file!("settings");

use super::{NextPage, OffsetPage, Page, SharedState};
use crate::newui::Metrics;
use crate::{
    dir, get_data, get_data_mut,
    popup::ChooseButton,
    save_data,
    scene::BGM_VOLUME_UPDATED,
    sync_data,
    tabs::{Tabs, TitleFn},
};use anyhow::Result;
use bytesize::ByteSize;
use inputbox::InputBox;
use macroquad::prelude::*;
use once_cell::sync::Lazy;
use prpr::{
    config::{Config, NoteMapTarget},
    ext::{open_url, poll_future, LocalTask, RectExt, SafeTexture, ScaleType},
    scene::{request_file, request_input, return_file, return_input, show_error, show_message, take_file, take_input},
    task::Task,
    ui::{DRectButton, Dialog, Scroll, Slider, Ui, PREFER_REDUCED_MOTION, UI_SFX_VOLUME},
};
use prpr_l10n::{LanguageIdentifier, LANG_IDENTS, LANG_NAMES};
use reqwest::Url;
use serde::Deserialize;
use std::{
    borrow::Cow,
    fs, io,
    net::ToSocketAddrs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

const ITEM_HEIGHT: f32 = 0.168;
const INTERACT_WIDTH: f32 = 0.26;
const STATUS_PAGE: &str = "https://status.phira.cn";

/// 设置页列表行高（HUD 自定义布局参数，可调 0.1 ~ 0.3）。
#[inline]
fn row_h() -> f32 {
    crate::hud::param("settings", "row_h", ITEM_HEIGHT).clamp(0.1, 0.3)
}

thread_local! {
    /// 一行里「标题 + 副标题」能用的宽度（世界坐标）。
    ///
    /// 每行右边还有开关 / 按钮 / 滑块，文字必须在这个宽度内截断 ——
    /// 原来的副标题 `max_width(1.4)`（≈ 全屏 70%）会横穿到开关底下，
    /// 在玻璃行卡上就是一行压住控件的糊字。各 `*List::render` 进门时设置它。
    static ROW_TEXT_MAX: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}

/// 由各设置列表的 `render` 调用：告诉 [`render_title`] 每行文字能用多宽。
#[inline]
fn set_row_text_max(w: f32) {
    ROW_TEXT_MAX.with(|it| it.set(w.max(0.2)));
}

#[inline]
fn row_text_max() -> f32 {
    ROW_TEXT_MAX.with(|it| it.get())
}

struct NameList(String);
impl<'de> Deserialize<'de> for NameList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = Vec::<String>::deserialize(deserializer)?;
        Ok(Self(s.join(", ")))
    }
}

#[derive(Deserialize)]
struct LocalizationListRaw {
    #[serde(rename = "en-US")]
    en_us: NameList,
    #[serde(rename = "fr-FR")]
    fr_fr: NameList,
    #[serde(rename = "de-DE")]
    de_de: NameList,
    #[serde(rename = "id-ID")]
    id_id: NameList,
    #[serde(rename = "ja-JP")]
    ja_jp: NameList,
    #[serde(rename = "ko-KR")]
    ko_kr: NameList,
    #[serde(rename = "pl-PL")]
    pl_pl: NameList,
    #[serde(rename = "pt-BR")]
    pt_br: NameList,
    #[serde(rename = "ru-RU")]
    ru_ru: NameList,
    #[serde(rename = "th-TH")]
    th_th: NameList,
    #[serde(rename = "zh-TW")]
    zh_tw: NameList,
    #[serde(rename = "tr-TR")]
    tr_tr: NameList,
    #[serde(rename = "vi-VN")]
    vi_vn: NameList,
}

struct LocalizationList(String);
impl<'de> Deserialize<'de> for LocalizationList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = LocalizationListRaw::deserialize(deserializer)?;
        Ok(Self(format!(
            "\
English (en-US)\n{}\n
French (fr-FR)\n{}\n
German (de-DE)\n{}\n
Indonesian (id-ID)\n{}\n
Japanese (ja-JP)\n{}\n
Korean (ko-KR)\n{}\n
Polish (pl-PL)\n{}\n
Portuguese (pt-BR)\n{}\n
Russian (ru-RU)\n{}\n
Thai (th-TH)\n{}\n
Traditional Chinese (zh-TW)\n{}\n
Turkish (tr-TR)\n{}\n
Vietnamese (vi-VN)\n{}",
            raw.en_us.0,
            raw.fr_fr.0,
            raw.de_de.0,
            raw.id_id.0,
            raw.ja_jp.0,
            raw.ko_kr.0,
            raw.pl_pl.0,
            raw.pt_br.0,
            raw.ru_ru.0,
            raw.th_th.0,
            raw.zh_tw.0,
            raw.tr_tr.0,
            raw.vi_vn.0
        )))
    }
}

#[derive(Deserialize)]
struct StaffList {
    development: NameList,
    operations: NameList,
    documentation: NameList,
    art: NameList,
    music: NameList,
    audio: NameList,
    community: NameList,
    localization: LocalizationList,
}

static STAFF_LIST: Lazy<StaffList> = Lazy::new(|| {
    let data = include_str!("../../staff.yml");
    serde_yaml::from_str(data).unwrap()
});

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingListType {
    General,
    Account,
    Audio,
    Chart,
    ChartUi,
    Debug,
    /// **日志**：把 `<data>/phira-crash.log` 显示在屏幕上，手机端不用 adb 也能看。
    Log,
    About,
}

pub struct SettingsPage {
    list_general: GeneralList,
    list_account: AccountList,
    list_audio: AudioList,
    list_chart: ChartList,
    list_chart_ui: ChartUiList,
    list_debug: DebugList,
    list_log: LogList,

    tabs: Tabs<SettingListType>,

    scroll: Scroll,
    save_time: f32,

    icon: SafeTexture,
}

/// 处理所有「设置里的文本输入框」回调（`request_input` 的返回）。
///
/// ⚠️ 必须是**页级**的自由函数，不能放进某个 `*List::update` 里。
/// `take_input()` 是**全局消费式**的，而 `SettingsPage::update` 只会调用
/// **当前选中标签页**的 `update` —— 历史上这段在 `GeneralList` 里，于是：
///   * 「连击文字」的按钮在**谱面UI**页 → 在那一页输入的内容**永远被丢弃**（输不进去）；
///   * 「自定义 RKS」「血量上限」「课题等级」同样是别的标签页的项，也一律失效。
/// 提到页级后与当前标签页无关，全部恢复。
fn handle_settings_inputs() -> Result<bool> {
    let Some((id, text)) = take_input() else {
        return Ok(false);
    };
    let data = get_data_mut();
        // （「账号」页原来在这里做两步输入框，已改用 Phira 自带的登录界面，
        //  见 `AccountList::ensure_login`；相关槽位与任务已一并删除。）
        if id == "mp_addr" {
            if let Err(err) = text.to_socket_addrs() {
                show_error(anyhow::Error::new(err).context(tl!("item-mp-addr-invalid")));
                return Ok(false);
            } else {
                data.config.mp_address = text;
                return Ok(true);
            }
        } else if id == "anys_gateway" {
            if let Err(err) = Url::parse(&text) {
                show_error(anyhow::Error::new(err).context(tl!("item-anys-gateway-invalid")));
                return Ok(false);
            } else {
                data.anys_gateway = text.trim_end_matches('/').to_string();
                return Ok(true);
            }
        } else if id == "rks" {
            let text = text.trim();
            // 留空 = 跟随账号真实 rks
            if text.is_empty() {
                data.config.rks_override = None;
                return Ok(true);
            }
            match text.parse::<f32>() {
                Ok(v) if (0. ..=100.).contains(&v) => {
                    data.config.rks_override = Some(v);
                    return Ok(true);
                }
                _ => {
                    show_error(anyhow::anyhow!("{}", tl!("rks-invalid")));
                    return Ok(false);
                }
            }
        } else if id == "health_max" {
            match text.trim().parse::<f32>() {
                Ok(v) if (10. ..=10000.).contains(&v) => {
                    data.config.health_max = v;
                    return Ok(true);
                }
                _ => {
                    show_error(anyhow::anyhow!("{}", tl!("health-max-invalid")));
                    return Ok(false);
                }
            }
        } else if id == "combo_text" {
            // 留空 = 默认（Autoplay 时 AUTOPLAY，否则 COMBO）
            data.config.combo_text = text.trim().chars().take(Config::COMBO_TEXT_MAX_CHARS).collect();
            return Ok(true);
        } else if id == "challenge_rank" {
            match text.trim().parse::<u32>() {
                Ok(v) if v <= 999 => {
                    data.config.challenge_rank = v;
                    return Ok(true);
                }
                _ => {
                    show_error(anyhow::anyhow!("{}", tl!("challenge-rank-invalid")));
                    return Ok(false);
                }
            }
        } else {
            return_input(id, text);
        }
    Ok(false)
}

impl SettingsPage {
    const SAVE_TIME: f32 = 0.5;

    pub fn new(icon: SafeTexture, icon_lang: SafeTexture) -> Self {
        Self {
            list_general: GeneralList::new(icon_lang),
            list_account: AccountList::new(),
            list_audio: AudioList::new(),
            list_chart: ChartList::new(),
            list_chart_ui: ChartUiList::new(),
            list_debug: DebugList::new(),
            list_log: LogList::new(),

            tabs: Tabs::new([
                (SettingListType::General, || tl!("general")),
                (SettingListType::Account, || tl!("account")),
                (SettingListType::Audio, || tl!("audio")),
                (SettingListType::Chart, || tl!("chart")),
                (SettingListType::ChartUi, || tl!("chart-ui")),
                (SettingListType::Debug, || tl!("debug")),
                (SettingListType::Log, || tl!("log")),
                (SettingListType::About, || tl!("about")),
            ] as [(SettingListType, TitleFn); 8]),

            scroll: Scroll::new(),
            save_time: f32::INFINITY,

            icon,
        }
    }
}

impl Page for SettingsPage {
    fn label(&self) -> Cow<'static, str> {
        tl!("label")
    }

    fn exit(&mut self) -> Result<()> {
        BGM_VOLUME_UPDATED.store(true, Ordering::Relaxed);
        if self.save_time.is_finite() {
            save_data()?;
        }
        Ok(())
    }

    fn touch(&mut self, touch: &Touch, s: &mut SharedState) -> Result<bool> {
        let t = s.t;
        if match self.tabs.selected() {
            SettingListType::General => self.list_general.top_touch(touch, t),
            SettingListType::Account => self.list_account.top_touch(touch, t),
            SettingListType::Audio => self.list_audio.top_touch(touch, t),
            SettingListType::Chart => self.list_chart.top_touch(touch, t),
            SettingListType::ChartUi => self.list_chart_ui.top_touch(touch, t),
            SettingListType::Debug => self.list_debug.top_touch(touch, t),
            SettingListType::Log => self.list_log.top_touch(touch, t),
            SettingListType::About => false,
        } {
            return Ok(true);
        }

        if self.tabs.touch(touch, s.rt) {
            // 切页时回到顶部：谱面 / 谱面UI 两页长度差很大，不重置会停在空白处
            self.scroll.y_scroller.reset();
            return Ok(true);
        }

        if self.scroll.touch(touch, t) {
            return Ok(true);
        }
        if let Some(p) = match self.tabs.selected() {
            SettingListType::General => self.list_general.touch(touch, t)?,
            SettingListType::Account => self.list_account.touch(touch, t)?,
            SettingListType::Audio => self.list_audio.touch(touch, t)?,
            SettingListType::Chart => self.list_chart.touch(touch, t)?,
            SettingListType::ChartUi => self.list_chart_ui.touch(touch, t)?,
            SettingListType::Debug => self.list_debug.touch(touch, t)?,
            SettingListType::Log => self.list_log.touch(touch, t)?,
            SettingListType::About => None,
        } {
            if p {
                self.save_time = t;
            }
            self.scroll.y_scroller.halt();
            return Ok(true);
        }
        Ok(false)
    }

    fn update(&mut self, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        // ⚠️ 输入回调必须在**页级**处理，不能只靠当前标签页的 `update`。
        //
        // 这里必须**先**于下面的 `match` 执行：`take_input()` 是全局消费式的，
        // 而被消费的输入可能来自**任意**标签页（比如「连击文字」的按钮在谱面UI页、
        // 处理逻辑却在通用页）——只调用当前页的 `update` 会让输入直接丢失。
        let input_changed = handle_settings_inputs()?;

        let changed = match self.tabs.selected() {
            SettingListType::General => self.list_general.update(t)?,
            SettingListType::Account => self.list_account.update(t)?,
            SettingListType::Audio => self.list_audio.update(t)?,
            SettingListType::Chart => self.list_chart.update(t)?,
            SettingListType::ChartUi => self.list_chart_ui.update(t)?,
            SettingListType::Debug => self.list_debug.update(t)?,
            SettingListType::Log => self.list_log.update(t)?,
            SettingListType::About => false,
        } || input_changed;
        self.scroll.update(t);
        if changed {
            self.save_time = t;
        }
        if t > self.save_time + Self::SAVE_TIME {
            save_data()?;
            self.save_time = f32::INFINITY;
        }
        Ok(())
    }

    fn render(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        let rt = s.rt;

        s.fader.render(ui, s.t, |ui| {
            let m = crate::newui::Metrics::new(ui);
            let th = crate::newui::theme();
            let sb = m.statusbar(ui);
            let bottom = sb.y - m.u(4.);

            let cr = ui.content_rect();
            // ---- Phigros 版式（参考 `kaichang/Setting.html`）----
            // 标签栏**始终横排在顶部**（与返回键同一行、整体居中），内容区在它下面铺满整宽。
            // 原来的「横屏 = 左侧竖栏」版式整条退役。
            self.tabs.set_style(crate::tabs::TabStyle::Phigros);
            self.tabs.set_horizontal(true);
            let back = crate::newui::back_rect(ui, &m);
            let sh = m.u(66.);
            let strip = Rect::new(cr.x, back.center().y - sh * 0.5, cr.w, sh);
            let cy = strip.bottom() + m.u(8.);
            let r = Rect::new(cr.x, cy, cr.w, (bottom - cy).max(m.u(60.)));

            // HUD 自定义布局：设置列表大框
            const SLOT_LIST: crate::hud::SlotDef = crate::hud::SlotDef::centered("list", [0., 0., 1.65, 1.1], crate::hud::Cap(true, true, true));
            let r = if crate::hud::has("settings", "list") {
                crate::hud::slot(ui, "settings", SLOT_LIST)
            } else {
                crate::hud::register("settings", SLOT_LIST, r);
                r
            };
            let _ = th;

            // 内容区走「扁平」分组：不铺卡片底、不画分隔线 —— Phigros 版式里
            // 各行是直接落在页面底上的（间隔靠留白）。渲染完立刻复位，别影响别的页面。
            crate::newui::set_flat_groups(true);
            prpr::ui::set_skew_sliders(true);
            let tabs_res = self.tabs.render(ui, rt, "settings", strip, r, |ui, item| {
                let r = r.feather(-0.01);
                self.scroll.size((r.w, r.h));
                ui.scope(|ui| {
                    ui.dx(r.x);
                    ui.dy(r.y);
                    self.scroll.render(ui, |ui| match item {
                        SettingListType::General => self.list_general.render(ui, r, t),
            SettingListType::Account => self.list_account.render(ui, r, t),
                        SettingListType::Audio => self.list_audio.render(ui, r, t),
                        SettingListType::Chart => self.list_chart.render(ui, r, t),
                        SettingListType::ChartUi => self.list_chart_ui.render(ui, r, t),
                        SettingListType::Debug => self.list_debug.render(ui, r, t),
            SettingListType::Log => self.list_log.render(ui, r, t),
                        SettingListType::About => render_about(ui, r, &self.icon),
                    });
                });

                Ok(())
            });
            crate::newui::set_flat_groups(false);
            prpr::ui::set_skew_sliders(false);

            // 右侧两条细斜线（参考图里的装饰件）
            if tabs_res.is_ok() {
                let th2 = crate::newui::theme();
                let lw = m.u(150.);
                for i in 0..2 {
                    let rr = Rect::new(cr.right() - m.u(20.) - lw, cr.y + cr.h * (0.44 + 0.07 * i as f32), lw, m.u(2.));
                    crate::newui::skew_rect(ui, rr, m.u(9.), th2.with_a(th2.border, 0.55));
                }
            }

            tabs_res
        })?;

        // Phira 自带登录界面必须在**全屏**上画。
        //
        // 它按全屏面板设计（`home.rs` 也是在页面最外层渲染它），而我一开始把它放在
        // `AccountList::render` 里 —— 那是在设置列表的**滚动区**内，于是被裁剪、
        // 布局按列表宽度算 → 用户看到的「登录界面变形」。
        // 这里提到滚动区之外、页面最上层画。
        if matches!(self.tabs.selected(), SettingListType::Account) {
            self.list_account.render_login_overlay(ui, t);
        }

        Ok(())
    }

    fn next_page(&mut self) -> NextPage {
        if self.list_chart_ui.timing_edit_requested {
            self.list_chart_ui.timing_edit_requested = false;
            return NextPage::Overlay(Box::new(super::TimingEditor::new()));
        }
        if self.list_general.open_blacklist {
            self.list_general.open_blacklist = false;
            return NextPage::Overlay(Box::new(super::BlacklistPage::new()));
        }
        if matches!(self.tabs.selected(), SettingListType::Audio) {
            return self.list_audio.next_page().unwrap_or_default();
        }
        if self.list_chart.open_history {
            self.list_chart.open_history = false;
            return NextPage::Overlay(Box::new(super::HistoryPage::new()));
        }
        NextPage::None
    }
}

fn render_about(ui: &mut Ui, mut r: Rect, icon: &SafeTexture) -> (f32, f32) {
    r.x = 0.;
    r.y = 0.;
    let ow = r.w;
    let r = r.feather(-0.02);
    let th = crate::newui::theme();
    let m = crate::newui::Metrics::new(ui);

    let staff = &*STAFF_LIST;
    let text = tl!(
        "about-content",
        "version" => format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH")),

        "development" => &staff.development.0,
        "operations" => &staff.operations.0,
        "documentation" => &staff.documentation.0,
        "art" => &staff.art.0,
        "music" => &staff.music.0,
        "audio" => &staff.audio.0,
        "community" => &staff.community.0,
        "localization" => &staff.localization.0
    );
    // 版本行 = 正文里**第一行非空文本**，其余是正文。
    // （l10n 值开头可能带空行 / 不同解析器行为不一致，直接 `split_once('\n')`
    //  拿到空串时版本行就整行画不出来 —— 这里按「第一行非空」取，稳。）
    let mut lines = text.lines();
    let first = loop {
        match lines.next() {
            Some(l) if l.trim().is_empty() => continue,
            Some(l) => break l.trim().to_owned(),
            None => break String::new(),
        }
    };
    let body = lines.collect::<Vec<_>>().join("\n");
    let body = body.trim();

    // ---- 头部：Logo + 竖分隔线 + 版本信息（并排；参考 `kaichang/Setting.html` 的「关于」页）----
    let head_cy = r.y + m.u(76.);
    let logo_h = m.u(56.);
    let lw = (logo_h * (icon.width() / icon.height().max(1.))).max(m.u(40.));
    let gap = m.u(34.);
    let ver = [format!("Version : {first}"), format!("Device : {}", std::env::consts::OS)];
    let vw = ver
        .iter()
        .map(|s| ui.text(s.as_str()).size(m.f(15.)).no_baseline().measure().w)
        .fold(0f32, f32::max);
    let div_w = m.u(1.5);
    let total = lw + gap + div_w + gap + vw;
    let x0 = r.center().x - total * 0.5;

    // Logo 是**白字 + 蓝辉光**、为深底设计 → 恒用白色 tint，不再跟主题墨色（否则浅色下变黑块）。
    let logo_r = Rect::new(x0, head_cy - logo_h * 0.5, lw, logo_h);
    ui.fill_rect(logo_r, (**icon, logo_r, ScaleType::Fit, WHITE));

    let div_x = logo_r.right() + gap;
    ui.fill_rect(Rect::new(div_x, head_cy - m.u(26.), div_w, m.u(52.)), th.with_a(th.border, 0.85));

    let tx = div_x + div_w + gap;
    for (i, s) in ver.iter().enumerate() {
        ui.text(s.as_str())
            .pos(tx, head_cy - m.u(11.) + i as f32 * m.u(24.))
            .anchor(0., 0.5)
            .no_baseline()
            .size(m.f(15.))
            .color(th.text_dim)
            .draw();
    }

    // ---- 正文：两列（工作人员名单很长，单列会拖出一大截空白）----
    let body_lines: Vec<&str> = body.lines().collect();
    let half = body_lines.len().div_ceil(2);
    let top = head_cy + m.u(66.);
    let col_w = (r.w - m.u(72.)) * 0.5;
    for (i, l) in body_lines.iter().enumerate() {
        let (c, row) = if i < half { (0, i) } else { (1, i - half) };
        let cx = r.x + m.u(36.) + col_w * c as f32;
        ui.text(*l)
            .pos(cx, top + row as f32 * m.u(19.))
            .anchor(0., 0.5)
            .no_baseline()
            .max_width(col_w - m.u(14.))
            .size(m.f(13.))
            .color(th.text)
            .draw();
    }

    (ow, r.bottom())
}

fn render_title<'a>(ui: &mut Ui, title: impl Into<Cow<'a, str>>, subtitle: Option<Cow<'a, str>>) -> f32 {
    const TITLE_SIZE: f32 = 0.6;
    const SUBTITLE_SIZE: f32 = 0.35;
    const LEFT: f32 = 0.06;
    const PAD: f32 = 0.01;
    const SUB_MAX_WIDTH: f32 = 1.4;
    let _ = SUB_MAX_WIDTH;
    // 行宽上限由列表 render 设置（右侧要留给开关/按钮/滑块）
    let text_max = (row_text_max() - LEFT - 0.02).max(0.2);
    let th = crate::newui::theme();
    // 标题左侧 3px 强调条（New UI 语言）
    crate::newui::rounded(
        ui,
        Rect::new(LEFT - 0.035, (row_h() - 0.062) / 2., 0.0045, 0.062),
        0.002,
        th.with_a(th.glow, 0.55),
    );
    // 标题**加字距**（CSS letter-spacing 的对应物）：kaichang 的排版靠字距 + 层级，
    // 不靠加粗堆叠。副标题小而弱，正文与标题拉开对比。
    let track = 0.012;
    if let Some(subtitle) = subtitle {
        let title = title.into();
        let r1 = ui.text(Cow::clone(&title)).size(TITLE_SIZE).measure();
        let r2 = ui
            .text(Cow::clone(&subtitle))
            .size(SUBTITLE_SIZE)
            .max_width(text_max)
            .no_baseline()
            .measure();
        let h = r1.h + PAD + r2.h;
        let r1 = ui
            .text(subtitle)
            .pos(LEFT, (row_h() + h) / 2.)
            .anchor(0., 1.)
            .size(SUBTITLE_SIZE)
            .max_width(text_max)
            .color(th.text_dim)
            .draw()
            .right();
        let r2 = crate::newui::text_tracked_w(
            ui,
            &title,
            LEFT,
            (row_h() - h) / 2.,
            TITLE_SIZE,
            th.text_hi,
            track,
            (0., 0.),
            false,
        )
        .right();
        r1.max(r2)
    } else {
        crate::newui::text_tracked_w(
            ui,
            &title.into(),
            LEFT,
            row_h() / 2.,
            TITLE_SIZE,
            th.text_hi,
            track,
            (0., 0.5),
            false,
        )
        .right()
    }
}

// 原来的 `row_card`（每行一张独立圆角卡）已经删掉：2026-10-09 起设置页的 7 个 section
// 全部改成 iOS 分组列表 —— 整组一个圆角容器（`newui::group_plate`）+ 行只叠悬停与
// 内缩发丝线（`newui::group_row`），所以不再需要「每行一张卡」这个函数。

#[inline]
fn render_switch(ui: &mut Ui, r: Rect, t: f32, btn: &mut DRectButton, on: bool) {
    // Phigros 版式：开关 = **斜切白方块**（`on` 时叠一个黑勾），命中区仍是整行。
    // 原来的「开 / 关」文字按钮退役（参考图里没有文字，靠勾选状态表达）。
    let th = crate::newui::theme();
    let m = crate::newui::Metrics::new(ui);
    btn.build(ui, t, r, |ui, _path| {
        let s = m.u(30.);
        let br = Rect::new(r.right() - s, r.center().y - s * 0.5, s, s);
        crate::newui::skew_check(ui, br, s * 0.34, on, &th);
    });
}

#[inline]
fn right_rect(w: f32) -> Rect {
    let rh = row_h() * 2. / 3.;
    Rect::new(w - 0.3, (row_h() - rh) / 2., INTERACT_WIDTH, rh)
}

/// 音符映射目标类型的显示名（设置页那 4 个循环按钮上写的字）。
fn note_map_label(t: NoteMapTarget) -> Cow<'static, str> {
    match t {
        NoteMapTarget::Keep => tl!("note-map-keep"),
        NoteMapTarget::Tap => tl!("note-map-tap"),
        NoteMapTarget::Drag => tl!("note-map-drag"),
        NoteMapTarget::Flick => tl!("note-map-flick"),
        NoteMapTarget::Hold => tl!("note-map-hold"),
    }
}

struct GeneralList {
    icon_lang: SafeTexture,

    lang_btn: ChooseButton,
    /// 深浅主题开关。原来只有外壳底部状态栏最右那个太阳/月亮按钮能做这件事，
    /// 状态栏被移除后挪到这里。
    theme_btn: DRectButton,

    #[cfg(all(any(target_os = "windows", target_os = "linux"), not(target_env = "ohos")))]
    fullscreen_btn: DRectButton,

    cache_btn: DRectButton,
    offline_btn: DRectButton,
    server_status_btn: DRectButton,
    mp_btn: DRectButton,
    mp_addr_btn: DRectButton,
    #[cfg(not(target_env = "ohos"))]
    lowq_btn: DRectButton,
    prefer_reduced_motion_btn: DRectButton,
    insecure_btn: DRectButton,
    enable_anys_btn: DRectButton,
    anys_gateway_btn: DRectButton,

    // 结算画面：手动指定 rks / 挑战徽章数字与颜色
    rks_btn: DRectButton,
    challenge_rank_btn: DRectButton,
    challenge_color_btn: DRectButton,

    // 外观：自定义字体 / 主页立绘 / 恢复默认
    font_btn: DRectButton,
    font_reset_btn: DRectButton,
    char_btn: DRectButton,
    char_reset_btn: DRectButton,
    reset_btn: DRectButton,
    has_custom_font: bool,
    has_custom_char: bool,
    /// 「恢复默认设置」的确认框结果
    reset_confirm: Option<Arc<AtomicBool>>,

    /// 打开「玩家黑名单」管理页
    blacklist_btn: DRectButton,
    open_blacklist: bool,

    /// 进入 HUD 自定义布局编辑模式
    hud_btn: DRectButton,

    cache_size: Option<u64>,
    cache_task: Option<Task<Result<u64>>>,
}

/// 自定义字体存放位置（启动时优先加载）
fn custom_font_path() -> Result<String> {
    Ok(format!("{}/font.ttf", dir::root()?))
}

/// 自定义主页立绘存放位置
fn custom_char_path() -> Result<String> {
    Ok(format!("{}/char.img", dir::root()?))
}

impl GeneralList {
    pub fn new(icon_lang: SafeTexture) -> Self {
        let mut this = Self {
            icon_lang,

            lang_btn: ChooseButton::new()
                .with_options(LANG_NAMES.iter().map(|s| s.to_string()).collect())
                .with_selected(
                    get_data()
                        .language
                        .as_ref()
                        .and_then(|it| it.parse::<LanguageIdentifier>().ok())
                        .and_then(|ident| LANG_IDENTS.iter().position(|it| *it == ident))
                        .unwrap_or_default(),
                ),

            theme_btn: DRectButton::new(),

            #[cfg(all(any(target_os = "windows", target_os = "linux"), not(target_env = "ohos")))]
            fullscreen_btn: DRectButton::new(),

            cache_btn: DRectButton::new(),
            offline_btn: DRectButton::new(),
            server_status_btn: DRectButton::new(),
            mp_btn: DRectButton::new(),
            mp_addr_btn: DRectButton::new(),
            #[cfg(not(target_env = "ohos"))]
            lowq_btn: DRectButton::new(),
            prefer_reduced_motion_btn: DRectButton::new(),
            insecure_btn: DRectButton::new(),
            enable_anys_btn: DRectButton::new(),
            anys_gateway_btn: DRectButton::new(),

            rks_btn: DRectButton::new(),
            challenge_rank_btn: DRectButton::new(),
            challenge_color_btn: DRectButton::new(),

            font_btn: DRectButton::new(),
            font_reset_btn: DRectButton::new(),
            char_btn: DRectButton::new(),
            char_reset_btn: DRectButton::new(),
            reset_btn: DRectButton::new(),
            has_custom_font: custom_font_path().map(|it| std::path::Path::new(&it).is_file()).unwrap_or(false),
            has_custom_char: custom_char_path().map(|it| std::path::Path::new(&it).is_file()).unwrap_or(false),
            reset_confirm: None,

            blacklist_btn: DRectButton::new(),
            open_blacklist: false,

            hud_btn: DRectButton::new(),

            cache_size: None,
            cache_task: None,
        };
        let _ = this.update_cache_size();
        this
    }

    pub fn top_touch(&mut self, touch: &Touch, t: f32) -> bool {
        if self.lang_btn.top_touch(touch, t) {
            return true;
        }
        false
    }

    fn dir_size(path: impl Into<PathBuf>) -> io::Result<u64> {
        fn inner(mut dir: fs::ReadDir) -> io::Result<u64> {
            dir.try_fold(0, |acc, file| {
                let file = file?;
                let size = match file.metadata()? {
                    data if data.is_dir() => inner(fs::read_dir(file.path())?)?,
                    data => data.len(),
                };
                Ok(acc + size)
            })
        }

        inner(fs::read_dir(path.into())?)
    }

    fn update_cache_size(&mut self) -> Result<()> {
        self.cache_size = None;

        let cache_dir = dir::cache()?;
        self.cache_task = Some(Task::new(async { Ok(Self::dir_size(cache_dir)?) }));
        Ok(())
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        let data = get_data_mut();
        let config = &mut data.config;
        if self.lang_btn.touch(touch, t) {
            return Ok(Some(false));
        }

        if self.theme_btn.touch(touch, t) {
            // 深浅主题切换。原来只有外壳底部状态栏最右那个太阳/月亮按钮能做这件事，
            // 状态栏已按用户要求整条移除，开关挪到这里（`Config::ui_dark` 会持久化）。
            // `set_dark` 必须当场调用：主题是从这个全局读的，不会每帧回读 config。
            let dark = !crate::newui::is_dark();
            crate::newui::set_dark(dark);
            config.ui_dark = dark;
            return Ok(Some(true));
        }


        #[cfg(all(any(target_os = "windows", target_os = "linux"), not(target_env = "ohos")))]
        if self.fullscreen_btn.touch(touch, t) {
            config.fullscreen_mode ^= true;

            crate::webui::set_fullscreen(config.fullscreen_mode);

            return Ok(Some(true));
        }

        if self.cache_btn.touch(touch, t) {
            fs::remove_dir_all(dir::cache()?)?;
            self.update_cache_size()?;
            show_message(tl!("item-cache-cleared")).ok();
            return Ok(Some(false));
        }
        if self.offline_btn.touch(touch, t) {
            config.offline_mode ^= true;
            return Ok(Some(true));
        }
        if self.server_status_btn.touch(touch, t) {
            let _ = open_url(STATUS_PAGE);
            return Ok(Some(true));
        }
        if self.mp_btn.touch(touch, t) {
            config.mp_enabled ^= true;
            return Ok(Some(true));
        }
        if self.mp_addr_btn.touch(touch, t) {
            request_input("mp_addr", InputBox::new().default_text(&config.mp_address));
            return Ok(Some(true));
        }
        #[cfg(not(target_env = "ohos"))]
        if self.lowq_btn.touch(touch, t) {
            config.sample_count = if config.sample_count == 1 { 2 } else { 1 };
            return Ok(Some(true));
        }
        if self.prefer_reduced_motion_btn.touch(touch, t) {
            data.prefer_reduced_motion ^= true;
            PREFER_REDUCED_MOTION.store(data.prefer_reduced_motion, Ordering::Relaxed);
            return Ok(Some(true));
        }
        if self.insecure_btn.touch(touch, t) {
            data.accept_invalid_cert ^= true;
            return Ok(Some(true));
        }
        if self.enable_anys_btn.touch(touch, t) {
            data.enable_anys ^= true;
            return Ok(Some(true));
        }
        if self.anys_gateway_btn.touch(touch, t) {
            request_input("anys_gateway", InputBox::new().default_text(&data.anys_gateway));
            return Ok(Some(true));
        }
        if self.rks_btn.touch(touch, t) {
            request_input("rks", InputBox::new().default_text(&config.rks_override.map(|it| format!("{it}")).unwrap_or_default()));
            return Ok(Some(true));
        }
        if self.challenge_rank_btn.touch(touch, t) {
            request_input("challenge_rank", InputBox::new().default_text(&config.challenge_rank.to_string()));
            return Ok(Some(true));
        }
        if self.challenge_color_btn.touch(touch, t) {
            config.challenge_color = config.challenge_color.next();
            return Ok(Some(true));
        }
        if self.font_btn.touch(touch, t) {
            request_file("ui_font");
            return Ok(Some(false));
        }
        if self.font_reset_btn.touch(touch, t) {
            if let Ok(path) = custom_font_path() {
                let _ = fs::remove_file(path);
            }
            self.has_custom_font = false;
            show_message(tl!("font-reset-done")).ok();
            return Ok(Some(false));
        }
        if self.char_btn.touch(touch, t) {
            request_file("home_char");
            return Ok(Some(false));
        }
        if self.char_reset_btn.touch(touch, t) {
            if let Ok(path) = custom_char_path() {
                let _ = fs::remove_file(path);
            }
            self.has_custom_char = false;
            show_message(tl!("char-reset-done")).ok();
            return Ok(Some(false));
        }
        if self.blacklist_btn.touch(touch, t) {
            self.open_blacklist = true;
            return Ok(Some(false));
        }
        if self.hud_btn.touch(touch, t) {
            // 进入编辑模式并直接切到谱面库页（槽位最多的页面；切页由 MainScene 处理）
            crate::hud::set_edit(true);
            crate::hud::request_goto(crate::hud::PageId::Library);
            return Ok(Some(true));
        }
        if self.reset_btn.touch(touch, t) {
            let res = Arc::new(AtomicBool::new(false));
            // 确认后由 update() 轮询处理
            crate::scene::confirm_dialog(tl!("item-reset"), tl!("item-reset-confirm"), res.clone());
            self.reset_confirm = Some(res);
            return Ok(Some(false));
        }
        Ok(None)
    }

    pub fn update(&mut self, t: f32) -> Result<bool> {
        self.lang_btn.update(t);
        let data = get_data_mut();
        if self.lang_btn.changed() {
            data.language = Some(LANG_IDENTS[self.lang_btn.selected()].to_string());
            sync_data();
            return Ok(true);
        }
        // 「恢复默认设置」确认框
        if let Some(res) = &self.reset_confirm {
            if res.load(Ordering::SeqCst) {
                self.reset_confirm = None;
                data.config = Config::default();
                data.config.init();
                // 运行时镜像量也要跟着刷新，否则音量 / 全屏要等到再动一次滑块才生效
                UI_SFX_VOLUME.store(data.config.volume_sfx.to_bits(), Ordering::Relaxed);
                BGM_VOLUME_UPDATED.store(true, Ordering::Relaxed);
                show_message(tl!("reset-done")).ok();
                return Ok(true);
            }
        }
        // 导入的字体 / 主页立绘（iOS 选中的文件在临时目录，必须复制到数据目录才能长期使用）
        if let Some((id, file)) = take_file() {
            match id.as_str() {
                "ui_font" => {
                    match custom_font_path().and_then(|dst| -> Result<()> {
                        fs::copy(&file, dst)?;
                        Ok(())
                    }) {
                        Ok(_) => {
                            self.has_custom_font = true;
                            show_message(tl!("font-imported")).ok();
                        }
                        Err(err) => show_error(err.context(tl!("font-import-failed"))),
                    }
                    return Ok(false);
                }
                "home_char" => {
                    match custom_char_path().and_then(|dst| -> Result<()> {
                        fs::copy(&file, dst)?;
                        Ok(())
                    }) {
                        Ok(_) => {
                            self.has_custom_char = true;
                            show_message(tl!("char-imported")).ok();
                        }
                        Err(err) => show_error(err.context(tl!("char-import-failed"))),
                    }
                    return Ok(false);
                }
                _ => return_file(id, file),
            }
        }
        if let Some(task) = &mut self.cache_task {
            if let Some(size) = task.take() {
                self.cache_size = size.ok();
                self.cache_task = None;
            }
        }
        Ok(false)
    }

    pub fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let w = r.w;
        // 行内文字宽度上限：右侧还要放开关 / 按钮 / 滑块
        set_row_text_max(w - 0.38);
        let mut h = 0.;
        // ---- iOS 分组列表（inset grouped）----
        // 整组**共用一个圆角容器**，行与行紧贴、靠内缩发丝线分隔（原来是「每行一张
        // 独立圆角卡 + 4 单位间隙」，那是卡片语言的观感）。
        // ⚠️ 容器必须在画第一行**之前**铺好 —— 即时模式下晚画会盖住已画好的行内容，
        //    所以这里必须**先知道行数**。
        // ⚠️ `n_items` 是**手工核对**出来的：21 个固定项 + 2 个 `#[cfg]` 门控项。
        //    以后加/删 `item!` 一定要同步改这里，否则容器高度会和内容对不上。
        //    门控项用 `cfg!` 宏在运行时计入，不用跟着编译目标写两套常量。
        let th = crate::newui::theme();
        let group_radius = 0.014;
        let n_items = 21usize
            + usize::from(cfg!(all(any(target_os = "windows", target_os = "linux"), not(target_env = "ohos"))))
            + usize::from(cfg!(not(target_env = "ohos")));
        // 容器高度 = 行数 × 行高，**再加上末尾那 0.04 的额外间距**
        // （section 尾部有一句 `ui.dy(0.04); h += 0.04;` 把「重置」单独推开一段）。
        // 行本身不带底色，所以容器里那段空隙是看不见的，但容器必须够高。
        crate::newui::group_plate(ui, &th, Rect::new(0., 0., w, row_h() * n_items as f32 + 0.04), group_radius);
        let mut gi = 0usize;
        macro_rules! item {
            ($($b:tt)*) => {{
                // 每个设置项 = 分组里的一行：**不填底板**（底板是整组的圆角容器），
                // 只叠悬停墨色 + 底部内缩发丝线（内缩到 `LEFT`，对齐标题左缘）。
                let row = Rect::new(0., 0., w, row_h());
                let (mx, my) = crate::newui::mouse_world();
                let hp = if ui.rect_to_global(row).contains(vec2(mx, my)) { 1. } else { 0. };
                crate::newui::group_row(
                    ui,
                    &th,
                    row,
                    hp,
                    gi == 0,
                    gi + 1 == n_items,
                    group_radius,
                    0.06,
                    Color::new(0., 0., 0., 0.),
                );
                gi += 1;
                $($b)*
                ui.dy(row_h());
                h += row_h();
            }}
        }
        let rr = right_rect(w);

        let data = get_data();
        let config = &data.config;
        item! {
            let rt = render_title(ui, tl!("item-lang"), None);
            let w = 0.06;
            let r = Rect::new(rt + 0.02, (ITEM_HEIGHT - w) / 2., w, w);
            // 语言图标是**白色矢量贴图**：直接铺在浅色玻璃卡上等于白底白图（看不见），
            // 必须按主题墨色着色；深色主题下 ink 是白色，观感不变。
            ui.fill_rect(r, (*self.icon_lang, r, ScaleType::Fit, prpr::ui::theme::ink(0.72)));
            self.lang_btn.render(ui, rr, t);
        }

        item! {
            render_title(ui, tl!("item-theme"), Some(tl!("item-theme-sub")));
            render_switch(ui, rr, t, &mut self.theme_btn, crate::newui::is_dark());
        }

        #[cfg(all(any(target_os = "windows", target_os = "linux"), not(target_env = "ohos")))]
        item! {
            render_title(ui, tl!("item-fullscreen"), None);
            render_switch(ui, rr, t, &mut self.fullscreen_btn, config.fullscreen_mode);
        }

        item! {
            render_title(ui, tl!("item-offline"), Some(tl!("item-offline-sub")));
            render_switch(ui, rr, t, &mut self.offline_btn, config.offline_mode);
        }
        item! {
            render_title(ui, tl!("item-server-status"), Some(tl!("item-server-status-sub")));
            self.server_status_btn.render_text(ui, rr, t, tl!("check-status"), 0.5, true);
        }
        item! {
            render_title(ui, tl!("item-mp"), Some(tl!("item-mp-sub")));
            render_switch(ui, rr, t, &mut self.mp_btn, config.mp_enabled);
        }
        item! {
            render_title(ui, tl!("item-mp-addr"), Some(tl!("item-mp-addr-sub")));
            self.mp_addr_btn.render_text(ui, rr, t, &config.mp_address, 0.4, false);
        }
        item! {
            render_title(ui, tl!("item-prefer-reduced-motion"), Some(tl!("item-prefer-reduced-motion-sub")));
            render_switch(ui, rr, t, &mut self.prefer_reduced_motion_btn, data.prefer_reduced_motion);
        }
        #[cfg(not(target_env = "ohos"))]
        item! {
            render_title(ui, tl!("item-lowq"), Some(tl!("item-lowq-sub")));
            render_switch(ui, rr, t, &mut self.lowq_btn, config.sample_count == 1);
        }
        item! {
            let cache_size = if let Some(size) = self.cache_size {
                Cow::Owned(tl!("item-cache-size", "size" => ByteSize(size).to_string()))
            } else {
                tl!("item-cache-size-loading")
            };
            render_title(ui, tl!("item-clear-cache"), Some(cache_size));
            self.cache_btn.render_text(ui, rr, t, tl!("item-clear-cache-btn"), 0.5, true);
        }
        ui.dy(0.04);
        h += 0.04;
        item! {
            render_title(ui, tl!("item-insecure"), Some(tl!("item-insecure-sub")));
            render_switch(ui, rr, t, &mut self.insecure_btn, data.accept_invalid_cert);
        }
        item! {
            render_title(ui, tl!("item-enable-anys"), Some(tl!("item-enable-anys-sub")));
            render_switch(ui, rr, t, &mut self.enable_anys_btn, data.enable_anys);
        }
        item! {
            render_title(ui, tl!("item-anys-gateway"), Some(tl!("item-anys-gateway-sub")));
            self.anys_gateway_btn.render_text(ui, rr, t, &data.anys_gateway, 0.4, false);
        }
        ui.dy(0.04);
        h += 0.04;
        // 结算画面相关：rks / 挑战徽章
        item! {
            let rks_text = match config.rks_override {
                Some(rks) => format!("{rks:.2}"),
                None => tl!("rks-follow").to_string(),
            };
            render_title(ui, tl!("item-rks"), Some(tl!("item-rks-sub")));
            self.rks_btn.render_text(ui, rr, t, rks_text, 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-challenge-rank"), Some(tl!("item-challenge-rank-sub")));
            self.challenge_rank_btn
                .render_text(ui, rr, t, config.challenge_rank.to_string(), 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-challenge-color"), Some(tl!("item-challenge-color-sub")));
            let color_text = format!("{:?}", config.challenge_color);
            self.challenge_color_btn.render_text(ui, rr, t, color_text, 0.5, false);
        }
        ui.dy(0.04);
        h += 0.04;
        // 外观：自定义字体 / 主页立绘 / 恢复默认
        item! {
            render_title(ui, tl!("item-font"), Some(tl!("item-font-sub")));
            let text = if self.has_custom_font {
                tl!("imported").to_string()
            } else {
                tl!("import-font").to_string()
            };
            self.font_btn.render_text(ui, rr, t, text, 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-font-reset"), Some(tl!("item-font-reset-sub")));
            self.font_reset_btn.render_text(ui, rr, t, tl!("reset"), 0.5, true);
        }
        item! {
            render_title(ui, tl!("item-home-char"), Some(tl!("item-home-char-sub")));
            let text = if self.has_custom_char {
                tl!("imported").to_string()
            } else {
                tl!("import-image").to_string()
            };
            self.char_btn.render_text(ui, rr, t, text, 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-home-char-reset"), Some(tl!("item-home-char-reset-sub")));
            self.char_reset_btn.render_text(ui, rr, t, tl!("reset"), 0.5, true);
        }
        item! {
            render_title(ui, tl!("item-blacklist"), Some(tl!("item-blacklist-sub")));
            self.blacklist_btn.render_text(ui, rr, t, tl!("item-blacklist-open"), 0.45, false);
        }
        item! {
            render_title(ui, "自定义布局".to_owned(), Some("拖动、缩放各页面组件的位置与大小".into()));
            self.hud_btn.render_text(ui, rr, t, "进入编辑模式", 0.38, true);
        }
        ui.dy(0.04);
        h += 0.04;
        item! {
            render_title(ui, tl!("item-reset"), Some(tl!("item-reset-sub")));
            self.reset_btn.render_text(ui, rr, t, tl!("reset"), 0.5, true);
        }
        self.lang_btn.render_top(ui, t, 1.);
        // 用一下 `gi`：既避免「最后一次自增没人读」的告警，也顺手校验 `n_items`
        // 和实际条数一致 —— debug 构建下不一致会直接 panic，比肉眼看布局靠谱得多。
        debug_assert_eq!(gi, n_items, "settings 分组：n_items 与实际 item! 条数不一致");
        (w, h)
    }
}

struct AudioList {
    adjust_btn: DRectButton,
    music_slider: Slider,
    sfx_slider: Slider,
    bgm_slider: Slider,
    cali_btn: DRectButton,
    #[cfg(not(target_os = "android"))]
    preferred_sample_rate_btn: DRectButton,
    #[cfg(target_env = "ohos")]
    audio_buffer_size_btn: DRectButton,
    cali_task: LocalTask<Result<OffsetPage>>,
    next_page: Option<NextPage>,
}

impl AudioList {
    pub fn new() -> Self {
        Self {
            adjust_btn: DRectButton::new(),
            music_slider: Slider::new(0.0..2.0, 0.05),
            sfx_slider: Slider::new(0.0..2.0, 0.05),
            bgm_slider: Slider::new(0.0..2.0, 0.05),
            cali_btn: DRectButton::new(),
            #[cfg(not(target_os = "android"))]
            preferred_sample_rate_btn: DRectButton::new(),
            #[cfg(target_env = "ohos")]
            audio_buffer_size_btn: DRectButton::new(),

            cali_task: None,
            next_page: None,
        }
    }

    pub fn top_touch(&mut self, _touch: &Touch, _t: f32) -> bool {
        false
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        let data = get_data_mut();
        let config = &mut data.config;
        if self.adjust_btn.touch(touch, t) {
            config.adjust_time ^= true;
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.music_slider.touch(touch, t, &mut config.volume_music) {
            return Ok(wt);
        }
        if let wt @ Some(_) = self.sfx_slider.touch(touch, t, &mut config.volume_sfx) {
            UI_SFX_VOLUME.store(config.volume_sfx.to_bits(), Ordering::Relaxed);
            return Ok(wt);
        }
        let old = config.volume_bgm;
        if let wt @ Some(_) = self.bgm_slider.touch(touch, t, &mut config.volume_bgm) {
            if (config.volume_bgm - old).abs() > 0.001 {
                BGM_VOLUME_UPDATED.store(true, Ordering::Relaxed);
            }
            return Ok(wt);
        }
        if self.cali_btn.touch(touch, t) {
            self.cali_task = Some(Box::pin(OffsetPage::new()));
            return Ok(Some(false));
        }
        #[cfg(not(target_os = "android"))]
        if self.preferred_sample_rate_btn.touch(touch, t) {
            let options = [None, Some(44100), Some(48000), Some(88200), Some(96000), Some(192000)];
            let current = config.preferred_sample_rate;
            let selected = options.iter().position(|&r| r == current).unwrap_or(0);
            config.preferred_sample_rate = options[(selected + 1) % options.len()];
            return Ok(Some(true));
        }
        #[cfg(target_env = "ohos")]
        if self.audio_buffer_size_btn.touch(touch, t) {
            let options = [128u32, 256u32, 512u32];
            let current = config.audio_buffer_size.unwrap_or(256);
            let selected = options.iter().position(|&r| r == current).unwrap_or(1);
            config.audio_buffer_size = Some(options[(selected + 1) % options.len()]);
            return Ok(Some(true));
        }
        Ok(None)
    }

    pub fn update(&mut self, _t: f32) -> Result<bool> {
        if let Some(task) = &mut self.cali_task {
            if let Some(res) = poll_future(task.as_mut()) {
                match res {
                    Err(err) => show_error(err.context(tl!("load-cali-failed"))),
                    Ok(page) => {
                        self.next_page = Some(NextPage::Overlay(Box::new(page)));
                    }
                }
                self.cali_task = None;
            }
        }
        Ok(false)
    }

    pub fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let w = r.w;
        // 行内文字宽度上限：右侧还要放开关 / 按钮 / 滑块
        set_row_text_max(w - 0.38);
        let mut h = 0.;
        // ---- iOS 分组列表（inset grouped）---- 见 `GeneralList::render` 的说明。
        // ⚠️ `n_items` 手工核对：5 个固定项 + 2 个 `#[cfg]` 门控项。
        let th = crate::newui::theme();
        let group_radius = 0.014;
        let n_items = 5usize
            + usize::from(cfg!(not(target_os = "android")))
            + usize::from(cfg!(target_env = "ohos"));
        crate::newui::group_plate(ui, &th, Rect::new(0., 0., w, row_h() * n_items as f32), group_radius);
        let mut gi = 0usize;
        macro_rules! item {
            ($($b:tt)*) => {{
                // 每个设置项 = 分组里的一行：**不填底板**（底板是整组的圆角容器），
                // 只叠悬停墨色 + 底部内缩发丝线（内缩到 `LEFT`，对齐标题左缘）。
                let row = Rect::new(0., 0., w, row_h());
                let (mx, my) = crate::newui::mouse_world();
                let hp = if ui.rect_to_global(row).contains(vec2(mx, my)) { 1. } else { 0. };
                crate::newui::group_row(
                    ui,
                    &th,
                    row,
                    hp,
                    gi == 0,
                    gi + 1 == n_items,
                    group_radius,
                    0.06,
                    Color::new(0., 0., 0., 0.),
                );
                gi += 1;
                $($b)*
                ui.dy(row_h());
                h += row_h();
            }}
        }
        let rr = right_rect(w);

        let data = get_data();
        let config = &data.config;
        item! {
            render_title(ui, tl!("item-adjust"), Some(tl!("item-adjust-sub")));
            render_switch(ui, rr, t, &mut self.adjust_btn, config.adjust_time);
        }
        item! {
            render_title(ui, tl!("item-music"), None);
            self.music_slider.render(ui, rr, t, config.volume_music, format!("{:.2}", config.volume_music));
        }
        item! {
            render_title(ui, tl!("item-sfx"), None);
            self.sfx_slider.render(ui, rr, t, config.volume_sfx, format!("{:.2}", config.volume_sfx));
        }
        item! {
            render_title(ui, tl!("item-bgm"), None);
            self.bgm_slider.render(ui, rr, t, config.volume_bgm, format!("{:.2}", config.volume_bgm));
        }
        item! {
            render_title(ui, tl!("item-cali"), None);
            self.cali_btn.render_text(ui, rr, t, format!("{:.0}ms", config.offset * 1000.), 0.5, true);
        }
        #[cfg(not(target_os = "android"))]
        item! {
            render_title(ui, tl!("item-preferred-sample-rate"), None);
            let text = if let Some(rate) = config.preferred_sample_rate {
                format!("{} Hz", rate)
            } else {
                tl!("preferred-sample-rate-default").to_string()
            };
            self.preferred_sample_rate_btn.render_text(ui, rr, t, text, 0.5, false);
        }
        #[cfg(target_env = "ohos")]
        item! {
            render_title(ui, tl!("item-audio-buffer-size"), None);
            let buf_size = config.audio_buffer_size.unwrap_or(256);
            self.audio_buffer_size_btn.render_text(ui, rr, t, format!("{}", buf_size), 0.5, false);
        }
        debug_assert_eq!(gi, n_items, "settings 分组：n_items 与实际 item! 条数不一致");
        (w, h)
    }

    pub fn next_page(&mut self) -> Option<NextPage> {
        self.next_page.take()
    }
}

struct ChartList {
    /// 全局「自动游玩」开关 —— 所有谱面统一生效，两处 UI 共用同一个值
    autoplay_btn: DRectButton,
    show_acc_btn: DRectButton,
    ap_fc_indicator_btn: DRectButton,
    show_avg_fps_btn: DRectButton,
    dc_pause_btn: DRectButton,
    dhint_btn: DRectButton,
    opt_btn: DRectButton,
    use_keyboard_btn: DRectButton,
    drag_protect_btn: DRectButton,
    flick_protect_btn: DRectButton,
    speed_slider: Slider,
    size_slider: Slider,
    judge_slider: Slider,
    judge_good_slider: Slider,
    judge_bad_slider: Slider,
    combo_text_btn: DRectButton,
    line_ref_btn: DRectButton,
    late_leniency_slider: Slider,
    debug_line_btn: DRectButton,
    debug_note_btn: DRectButton,
    pause_offset_btn: DRectButton,
    early_late_btn: DRectButton,
    judge_chart_btn: DRectButton,
    health_max_btn: DRectButton,
    health_scale_slider: Slider,
    /// 噪区精细度（掩码分辨率除数）。
    block_mask_slider: Slider,
    block_low_perf_btn: DRectButton,
    /// 音符流速（`Config.note_flow_speed`）：全局**视觉**流速倍率。
    note_flow_speed_slider: Slider,
    health_color_btn: DRectButton,
    health_len_slider: Slider,
    motion_btn: DRectButton,
    motion_threshold_slider: Slider,
    motion_still_slider: Slider,

    // ---- 音符映射（把某一类音符整体改成另一类）----
    /// 源：蓝键（Tap / Click）。
    note_map_tap_btn: DRectButton,
    /// 源：黄键（Drag）。
    note_map_drag_btn: DRectButton,
    /// 源：红键（Flick）。
    note_map_flick_btn: DRectButton,
    /// 源：长条（Hold）。
    note_map_hold_btn: DRectButton,
    /// 映射成「长条」时，新长条的时长（毫秒）。
    note_map_hold_ms_slider: Slider,

    /// 上传成绩开关 + 查看协议
    upload_btn: DRectButton,
    upload_consent_btn: DRectButton,
    /// 一键切换成「可上传成绩」的配置
    upload_profile_btn: DRectButton,
    /// 打开「成绩历史」页
    history_btn: DRectButton,
    open_history: bool,
}

/// 「谱面UI」页：打歌时的**表现与显示**，与「谱面」页的判定/玩法参数分开。
///
/// 注意：「成绩上传 / 成绩历史」留在 `ChartList` —— 那是上传链路的 UI，
/// 不随打歌表现走。
struct ChartUiList {
    // ---- 打击表现 ----
    in_game_font_btn: DRectButton,
    text_scale_slider: Slider,
    judge_text_btn: DRectButton,
    judge_text_style_btn: DRectButton,
    judge_text_size_slider: Slider,
    miss_marker_btn: DRectButton,
    miss_marker_time_slider: Slider,
    note_trail_btn: DRectButton,
    note_trail_len_slider: Slider,
    note_trail_alpha_slider: Slider,
    note_trail_count_slider: Slider,
    note_trail_click_btn: DRectButton,
    note_trail_drag_btn: DRectButton,
    note_trail_flick_btn: DRectButton,
    line_afterimage_btn: DRectButton,
    line_afterimage_count_slider: Slider,
    line_afterimage_alpha_slider: Slider,
    music_spectrum_btn: DRectButton,
    music_spectrum_gain_slider: Slider,
    line_glow_btn: DRectButton,
    line_glow_strength_slider: Slider,

    // ---- 实时 HUD ----
    hud_btn: DRectButton,
    hud_acc_btn: DRectButton,
    hud_counts_btn: DRectButton,
    hud_max_combo_btn: DRectButton,
    hud_time_btn: DRectButton,
    hud_delta_btn: DRectButton,
    hud_rks_btn: DRectButton,
    hud_fps_btn: DRectButton,
    hud_corner_btn: DRectButton,
    hud_size_slider: Slider,
    hud_alpha_slider: Slider,
    hud_outline_btn: DRectButton,

    // ---- 准度条（Timing Bar） ----
    /// 打歌时显示准度条。
    timing_bar_enabled_btn: DRectButton,
    /// 准度条下方的 P / G / B / M 计数。
    timing_bar_counts_btn: DRectButton,
    /// 打开可视化编辑器。
    timing_bar_edit_btn: DRectButton,
    /// 本帧请求打开编辑器（由 `SettingsPage::next_page` 取走）。
    timing_edit_requested: bool,

    // ---- 判定范围可视化（从 PhiraiAd 移植） ----
    /// 打歌时把判定窗口画成框（只读叠加层，不参与判定）。
    judge_range_enabled_btn: DRectButton,
    /// 贴着判定线画。
    judge_range_line_btn: DRectButton,
    /// 贴着音符画。
    judge_range_note_btn: DRectButton,
}

impl ChartList {
    pub fn new() -> Self {
        Self {
            autoplay_btn: DRectButton::new(),
            show_acc_btn: DRectButton::new(),
            ap_fc_indicator_btn: DRectButton::new(),
            show_avg_fps_btn: DRectButton::new(),
            dc_pause_btn: DRectButton::new(),
            dhint_btn: DRectButton::new(),
            opt_btn: DRectButton::new(),
            use_keyboard_btn: DRectButton::new(),
            drag_protect_btn: DRectButton::new(),
            flick_protect_btn: DRectButton::new(),
            speed_slider: Slider::new(0.5..2., 0.05),
            size_slider: Slider::new(0.8..1.2, 0.005),
            judge_slider: Slider::new(Config::JUDGE_WINDOW_MIN..Config::JUDGE_WINDOW_MAX, 5.),
            judge_good_slider: Slider::new(Config::JUDGE_WINDOW_MIN..Config::GOOD_WINDOW_MAX, 5.),
            judge_bad_slider: Slider::new(Config::JUDGE_WINDOW_MIN..Config::BAD_WINDOW_MAX, 5.),
            combo_text_btn: DRectButton::new(),
            line_ref_btn: DRectButton::new(),
            late_leniency_slider: Slider::new(0. ..Config::LATE_LENIENCY_MAX + 1., 5.),
            debug_line_btn: DRectButton::new(),
            debug_note_btn: DRectButton::new(),
            pause_offset_btn: DRectButton::new(),
            early_late_btn: DRectButton::new(),
            judge_chart_btn: DRectButton::new(),
            health_max_btn: DRectButton::new(),
            health_scale_slider: Slider::new(0.1..5., 0.1),
            block_mask_slider: Slider::new(
                // `Slider::new` 收的是**排他**范围 `Range<f32>`；滑块右端 p=1 时
                // 算出的值正好等于 MAX，所以这里写 `MIN..MAX` 而不是 `..=MAX`。
                prpr::core::MIN_MASK_DIVISOR as f32..prpr::core::MAX_MASK_DIVISOR as f32,
                1.0,
            ),
            // 音符流速：范围与 `Config::global_note_flow_speed` 的 clamp(0.1, 5.) 对齐。
            // `Slider::new` 收**排他**范围，p=1 时算出的值正好是右端点。
            note_flow_speed_slider: Slider::new(0.1..5., 0.05),
            block_low_perf_btn: DRectButton::new(),
            health_color_btn: DRectButton::new(),
            health_len_slider: Slider::new(Config::HEALTH_BAR_LEN_MIN..Config::HEALTH_BAR_LEN_MAX, 0.05),
            motion_btn: DRectButton::new(),
            motion_threshold_slider: Slider::new(Config::MOTION_THRESHOLD_MIN..Config::MOTION_THRESHOLD_MAX, 0.01),
            motion_still_slider: Slider::new(Config::MOTION_STILL_MIN..Config::MOTION_STILL_MAX, 0.1),

            note_map_tap_btn: DRectButton::new(),
            note_map_drag_btn: DRectButton::new(),
            note_map_flick_btn: DRectButton::new(),
            note_map_hold_btn: DRectButton::new(),
            note_map_hold_ms_slider: Slider::new(Config::NOTE_MAP_HOLD_MS_MIN..Config::NOTE_MAP_HOLD_MS_MAX, 10.),

            upload_btn: DRectButton::new(),
            upload_consent_btn: DRectButton::new(),
            upload_profile_btn: DRectButton::new(),
            history_btn: DRectButton::new(),
            open_history: false,
        }
    }

    pub fn top_touch(&mut self, _touch: &Touch, _t: f32) -> bool {
        false
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        let data = get_data_mut();
        let config = &mut data.config;
        if self.autoplay_btn.touch(touch, t) {
            config.set_autoplay(!config.autoplay());
            return Ok(Some(true));
        }
        if self.show_acc_btn.touch(touch, t) {
            config.show_acc ^= true;
            return Ok(Some(true));
        }
        if self.ap_fc_indicator_btn.touch(touch, t) {
            config.ap_fc_indicator ^= true;
            return Ok(Some(true));
        }
        if self.show_avg_fps_btn.touch(touch, t) {
            config.show_avg_fps ^= true;
            return Ok(Some(true));
        }
        if self.dc_pause_btn.touch(touch, t) {
            config.double_click_to_pause ^= true;
            return Ok(Some(true));
        }
        if self.dhint_btn.touch(touch, t) {
            config.double_hint ^= true;
            return Ok(Some(true));
        }
        if self.opt_btn.touch(touch, t) {
            config.aggressive ^= true;
            return Ok(Some(true));
        }
        if self.use_keyboard_btn.touch(touch, t) {
            config.use_keyboard ^= true;
            return Ok(Some(true));
        }
        if self.drag_protect_btn.touch(touch, t) {
            config.drag_protect ^= true;
            return Ok(Some(true));
        }
        if self.flick_protect_btn.touch(touch, t) {
            config.flick_protect ^= true;
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.speed_slider.touch(touch, t, &mut config.speed) {
            return Ok(wt);
        }
        if let wt @ Some(_) = self.size_slider.touch(touch, t, &mut config.note_scale) {
            return Ok(wt);
        }
        if let wt @ Some(_) = self.judge_slider.touch(touch, t, &mut config.judge_window) {
            config.judge_window = config.judge_window.clamp(Config::JUDGE_WINDOW_MIN, Config::JUDGE_WINDOW_MAX);
            return Ok(wt);
        }
        {
            // 拖动即改为「单独指定」，不再跟随 Perfect 的比例
            let mut v = config.judge_window_good_ms();
            if let wt @ Some(_) = self.judge_good_slider.touch(touch, t, &mut v) {
                config.judge_window_good = Some(v.clamp(Config::JUDGE_WINDOW_MIN, Config::GOOD_WINDOW_MAX));
                return Ok(wt);
            }
        }
        {
            let mut v = config.judge_window_bad_ms();
            if let wt @ Some(_) = self.judge_bad_slider.touch(touch, t, &mut v) {
                config.judge_window_bad = Some(v.clamp(Config::JUDGE_WINDOW_MIN, Config::BAD_WINDOW_MAX));
                return Ok(wt);
            }
        }
        if self.combo_text_btn.touch(touch, t) {
            request_input("combo_text", InputBox::new().default_text(&config.combo_text));
            return Ok(Some(true));
        }
        if self.line_ref_btn.touch(touch, t) {
            config.line_ref_y_axis ^= true;
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.late_leniency_slider.touch(touch, t, &mut config.late_leniency_ms) {
            config.late_leniency_ms = config.late_leniency_ms.clamp(0., Config::LATE_LENIENCY_MAX);
            return Ok(wt);
        }
        if self.debug_line_btn.touch(touch, t) {
            config.chart_debug_line ^= true;
            return Ok(Some(true));
        }
        if self.debug_note_btn.touch(touch, t) {
            config.chart_debug_note ^= true;
            return Ok(Some(true));
        }
        if self.pause_offset_btn.touch(touch, t) {
            config.pause_offset_adjust ^= true;
            return Ok(Some(true));
        }
        if self.early_late_btn.touch(touch, t) {
            config.early_late_hint ^= true;
            return Ok(Some(true));
        }
        if self.judge_chart_btn.touch(touch, t) {
            config.ending_judge_chart ^= true;
            return Ok(Some(true));
        }
        if self.health_max_btn.touch(touch, t) {
            request_input("health_max", InputBox::new().default_text(&format!("{:.0}", config.health_max)));
            return Ok(Some(true));
        }
        // 噪区精细度：改完立刻生效（`render_displaced` 每帧读原子量，无需重启）。
        let mut block_mask_divisor = config.block_mask_divisor as f32;
        if let wt @ Some(_) = self.block_mask_slider.touch(touch, t, &mut block_mask_divisor) {
            config.block_mask_divisor = (block_mask_divisor.round() as usize)
                .clamp(prpr::core::MIN_MASK_DIVISOR, prpr::core::MAX_MASK_DIVISOR);
            prpr::core::set_mask_divisor(config.block_mask_divisor);
            return Ok(wt);
        }
        // 音符流速：改完**下一局**生效（`Game` 构造时读 config，局内由
        // `sync_note_flow_speed` 同步到 `Resource`）。
        if let wt @ Some(_) = self.note_flow_speed_slider.touch(touch, t, &mut config.note_flow_speed) {
            config.note_flow_speed = config.note_flow_speed.clamp(0.1, 5.);
            return Ok(wt);
        }
        // 噪区低性能模式：改完立刻生效（绘制路径每帧读原子量）。
        if self.block_low_perf_btn.touch(touch, t) {
            config.block_area_low_performance ^= true;
            prpr::core::set_low_performance(config.block_area_low_performance);
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.health_scale_slider.touch(touch, t, &mut config.health_scale) {
            config.health_scale = config.health_scale.clamp(0.1, 5.);
            return Ok(wt);
        }
        if self.health_color_btn.touch(touch, t) {
            config.health_bar_color = config.health_bar_color.next();
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.health_len_slider.touch(touch, t, &mut config.health_bar_len) {
            config.health_bar_len = config.health_bar_len.clamp(Config::HEALTH_BAR_LEN_MIN, Config::HEALTH_BAR_LEN_MAX);
            return Ok(wt);
        }
        if self.motion_btn.touch(touch, t) {
            config.motion_pause ^= true;
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.motion_threshold_slider.touch(touch, t, &mut config.motion_threshold) {
            config.motion_threshold = config.motion_threshold.clamp(Config::MOTION_THRESHOLD_MIN, Config::MOTION_THRESHOLD_MAX);
            return Ok(wt);
        }
        if let wt @ Some(_) = self.motion_still_slider.touch(touch, t, &mut config.motion_still_time) {
            config.motion_still_time = config.motion_still_time.clamp(Config::MOTION_STILL_MIN, Config::MOTION_STILL_MAX);
            return Ok(wt);
        }

        // 音符映射：四个按钮点一下循环「保持不变 → 蓝键 → 黄键 → 红键 → 长条」
        if self.note_map_tap_btn.touch(touch, t) {
            config.note_map.tap = config.note_map.tap.next();
            return Ok(Some(true));
        }
        if self.note_map_drag_btn.touch(touch, t) {
            config.note_map.drag = config.note_map.drag.next();
            return Ok(Some(true));
        }
        if self.note_map_flick_btn.touch(touch, t) {
            config.note_map.flick = config.note_map.flick.next();
            return Ok(Some(true));
        }
        if self.note_map_hold_btn.touch(touch, t) {
            config.note_map.hold = config.note_map.hold.next();
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.note_map_hold_ms_slider.touch(touch, t, &mut config.note_map_hold_ms) {
            config.note_map_hold_ms = config
                .note_map_hold_ms
                .clamp(Config::NOTE_MAP_HOLD_MS_MIN, Config::NOTE_MAP_HOLD_MS_MAX);
            return Ok(wt);
        }

        if self.upload_btn.touch(touch, t) {
            if config.upload_record {
                // 已经开着：直接关掉
                config.upload_record = false;
            } else if config.upload_agreed {
                // 之前同意过：直接打开
                config.upload_record = true;
            } else {
                // 第一次打开：先看协议，同意了才真正开
                show_upload_consent(true, false);
            }
            return Ok(Some(true));
        }
        if self.upload_profile_btn.touch(touch, t) {
            // 一键可上传配置：没同意过协议就先弹协议，同意了再真正切换
            if config.upload_agreed {
                let turned = apply_upload_profile_to(config);
                show_message(tl!("upload-profile-done", "mods" => turned.to_string()));
            } else {
                show_upload_consent(true, true);
            }
            return Ok(Some(true));
        }
        if self.upload_consent_btn.touch(touch, t) {
            show_upload_consent(false, false);
            return Ok(Some(false));
        }
        if self.history_btn.touch(touch, t) {
            self.open_history = true;
            return Ok(Some(false));
        }
        Ok(None)
    }

    pub fn update(&mut self, _t: f32) -> Result<bool> {
        Ok(false)
    }

    pub fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let w = r.w;
        // 行内文字宽度上限：右侧还要放开关 / 按钮 / 滑块
        set_row_text_max(w - 0.38);
        let mut h = 0.;
        // ---- iOS 分组列表（inset grouped）---- 见 `GeneralList::render` 的说明。
        // ⚠️ `n_items` 手工核对：本 section 全部是固定项，没有 `#[cfg]` 门控。
        let th = crate::newui::theme();
        let group_radius = 0.014;
        let n_items = 42usize;
        crate::newui::group_plate(ui, &th, Rect::new(0., 0., w, row_h() * n_items as f32), group_radius);
        let mut gi = 0usize;
        macro_rules! item {
            ($($b:tt)*) => {{
                // 每个设置项 = 分组里的一行：**不填底板**（底板是整组的圆角容器），
                // 只叠悬停墨色 + 底部内缩发丝线（内缩到 `LEFT`，对齐标题左缘）。
                let row = Rect::new(0., 0., w, row_h());
                let (mx, my) = crate::newui::mouse_world();
                let hp = if ui.rect_to_global(row).contains(vec2(mx, my)) { 1. } else { 0. };
                crate::newui::group_row(
                    ui,
                    &th,
                    row,
                    hp,
                    gi == 0,
                    gi + 1 == n_items,
                    group_radius,
                    0.06,
                    Color::new(0., 0., 0., 0.),
                );
                gi += 1;
                $($b)*
                ui.dy(row_h());
                h += row_h();
            }}
        }
        let rr = right_rect(w);

        let data = get_data();
        let config = &data.config;
        item! {
            render_title(ui, tl!("item-autoplay"), Some(tl!("item-autoplay-sub")));
            render_switch(ui, rr, t, &mut self.autoplay_btn, config.autoplay());
        }
        item! {
            render_title(ui, tl!("item-show-acc"), None);
            render_switch(ui, rr, t, &mut self.show_acc_btn, config.show_acc);
        }
        item! {
            render_title(ui, tl!("item-ap-fc-indicator"), Some(tl!("item-ap-fc-indicator-sub")));
            render_switch(ui, rr, t, &mut self.ap_fc_indicator_btn, config.ap_fc_indicator);
        }
        item! {
            render_title(ui, tl!("item-show-avg-fps"), Some(tl!("item-show-avg-fps-sub")));
            render_switch(ui, rr, t, &mut self.show_avg_fps_btn, config.show_avg_fps);
        }
        item! {
            render_title(ui, tl!("item-dc-pause"), None);
            render_switch(ui, rr, t, &mut self.dc_pause_btn, config.double_click_to_pause);
        }
        item! {
            render_title(ui, tl!("item-dhint"), Some(tl!("item-dhint-sub")));
            render_switch(ui, rr, t, &mut self.dhint_btn, config.double_hint);
        }
        item! {
            render_title(ui, tl!("item-opt"), Some(tl!("item-opt-sub")));
            render_switch(ui, rr, t, &mut self.opt_btn, config.aggressive);
        }
        item! {
            render_title(ui, tl!("item-use-keyboard"), Some(tl!("item-use-keyboard-sub")));
            render_switch(ui, rr, t, &mut self.use_keyboard_btn, config.use_keyboard);
        }
        item! {
            render_title(ui, tl!("item-drag-protect"), Some(tl!("item-drag-protect-sub")));
            render_switch(ui, rr, t, &mut self.drag_protect_btn, config.drag_protect);
        }
        item! {
            render_title(ui, tl!("item-flick-protect"), Some(tl!("item-flick-protect-sub")));
            render_switch(ui, rr, t, &mut self.flick_protect_btn, config.flick_protect);
        }
        // ---- 音符映射 ----
        item! {
            render_title(ui, tl!("item-note-map-tap"), Some(tl!("item-note-map-tap-sub")));
            self.note_map_tap_btn.render_text(ui, rr, t, note_map_label(config.note_map.tap), 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-note-map-drag"), Some(tl!("item-note-map-drag-sub")));
            self.note_map_drag_btn.render_text(ui, rr, t, note_map_label(config.note_map.drag), 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-note-map-flick"), Some(tl!("item-note-map-flick-sub")));
            self.note_map_flick_btn.render_text(ui, rr, t, note_map_label(config.note_map.flick), 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-note-map-hold"), Some(tl!("item-note-map-hold-sub")));
            self.note_map_hold_btn.render_text(ui, rr, t, note_map_label(config.note_map.hold), 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-note-map-hold-ms"), Some(tl!("item-note-map-hold-ms-sub")));
            self.note_map_hold_ms_slider.render(
                ui,
                rr,
                t,
                config.note_map_hold_ms,
                format!("{:.0}ms", config.note_map_hold_ms),
            );
        }
        item! {
            render_title(ui, tl!("item-judge"), Some(tl!("item-judge-sub")));
            self.judge_slider
                .render(ui, rr, t, config.judge_window, format!("±{:.0}ms", config.judge_window));
        }
        item! {
            let good = config.judge_window_good_ms();
            render_title(ui, tl!("item-judge-good"), Some(tl!("item-judge-good-sub")));
            self.judge_good_slider.render(ui, rr, t, good, format!("±{good:.0}ms"));
        }
        item! {
            let bad = config.judge_window_bad_ms();
            render_title(ui, tl!("item-judge-bad"), Some(tl!("item-judge-bad-sub")));
            self.judge_bad_slider.render(ui, rr, t, bad, format!("±{bad:.0}ms"));
        }
        item! {
            render_title(ui, tl!("item-late-leniency"), Some(tl!("item-late-leniency-sub")));
            self.late_leniency_slider
                .render(ui, rr, t, config.late_leniency_ms, format!("{:.0}ms", config.late_leniency_ms));
        }
        item! {
            let combo_text = if config.combo_text.is_empty() {
                tl!("combo-text-default").to_string()
            } else {
                config.combo_text.clone()
            };
            render_title(ui, tl!("item-combo-text"), Some(tl!("item-combo-text-sub")));
            self.combo_text_btn.render_text(ui, rr, t, &combo_text, 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-line-ref-y-axis"), Some(tl!("item-line-ref-y-axis-sub")));
            render_switch(ui, rr, t, &mut self.line_ref_btn, config.line_ref_y_axis);
        }
        item! {
            render_title(ui, tl!("item-debug-line"), Some(tl!("item-debug-line-sub")));
            render_switch(ui, rr, t, &mut self.debug_line_btn, config.chart_debug_line);
        }
        item! {
            render_title(ui, tl!("item-debug-note"), Some(tl!("item-debug-note-sub")));
            render_switch(ui, rr, t, &mut self.debug_note_btn, config.chart_debug_note);
        }
        item! {
            render_title(ui, tl!("item-pause-offset"), Some(tl!("item-pause-offset-sub")));
            render_switch(ui, rr, t, &mut self.pause_offset_btn, config.pause_offset_adjust);
        }
        item! {
            render_title(ui, tl!("item-early-late"), Some(tl!("item-early-late-sub")));
            render_switch(ui, rr, t, &mut self.early_late_btn, config.early_late_hint);
        }
        item! {
            render_title(ui, tl!("item-judge-chart"), Some(tl!("item-judge-chart-sub")));
            render_switch(ui, rr, t, &mut self.judge_chart_btn, config.ending_judge_chart);
        }
        item! {
            render_title(ui, tl!("item-health-max"), Some(tl!("item-health-max-sub")));
            self.health_max_btn
                .render_text(ui, rr, t, format!("{:.0}", config.health_max), 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-block-mask-divisor"), Some(tl!("item-block-mask-divisor-sub")));
            self.block_mask_slider.render(
                ui,
                rr,
                t,
                config.block_mask_divisor as f32,
                format!("{}", config.block_mask_divisor),
            );
        }
        item! {
            render_title(ui, tl!("item-block-low-perf"), Some(tl!("item-block-low-perf-sub")));
            render_switch(ui, rr, t, &mut self.block_low_perf_btn, config.block_area_low_performance);
        }
        item! {
            render_title(ui, tl!("item-note-flow-speed"), Some(tl!("item-note-flow-speed-sub")));
            self.note_flow_speed_slider
                .render(ui, rr, t, config.note_flow_speed, format!("{:.2}x", config.note_flow_speed));
        }
        item! {
            render_title(ui, tl!("item-health-scale"), Some(tl!("item-health-scale-sub")));
            self.health_scale_slider
                .render(ui, rr, t, config.health_scale, format!("{:.1}x", config.health_scale));
        }
        item! {
            let color = match config.health_bar_color {
                prpr::config::HealthBarColor::White => tl!("health-color-white"),
                prpr::config::HealthBarColor::Green => tl!("health-color-green"),
                prpr::config::HealthBarColor::Blue => tl!("health-color-blue"),
                prpr::config::HealthBarColor::Red => tl!("health-color-red"),
                prpr::config::HealthBarColor::Gold => tl!("health-color-gold"),
                prpr::config::HealthBarColor::Purple => tl!("health-color-purple"),
            };
            render_title(ui, tl!("item-health-color"), Some(tl!("item-health-color-sub")));
            self.health_color_btn.render_text(ui, rr, t, color, 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-health-len"), Some(tl!("item-health-len-sub")));
            self.health_len_slider
                .render(ui, rr, t, config.health_bar_len, format!("{:.0}%", config.health_bar_len * 100.));
        }
        item! {
            render_title(ui, tl!("item-motion"), Some(tl!("item-motion-sub")));
            render_switch(ui, rr, t, &mut self.motion_btn, config.motion_pause);
        }
        item! {
            render_title(ui, tl!("item-motion-threshold"), Some(tl!("item-motion-threshold-sub")));
            self.motion_threshold_slider
                .render(ui, rr, t, config.motion_threshold, format!("{:.2}g", config.motion_threshold));
        }
        item! {
            render_title(ui, tl!("item-motion-still"), Some(tl!("item-motion-still-sub")));
            self.motion_still_slider
                .render(ui, rr, t, config.motion_still_time, format!("{:.1}s", config.motion_still_time));
        }
        item! {
            render_title(ui, tl!("item-speed"), None);
            self.speed_slider.render(ui, rr, t, config.speed, format!("{:.2}", config.speed));
        }
        item! {
            render_title(ui, tl!("item-note-size"), None);
            self.size_slider.render(ui, rr, t, config.note_scale, format!("{:.3}", config.note_scale));
        }
        item! {
            render_title(ui, tl!("item-upload"), Some(tl!("item-upload-sub")));
            render_switch(ui, rr, t, &mut self.upload_btn, config.upload_record);
        }
        item! {
            render_title(ui, tl!("item-upload-profile"), Some(tl!("item-upload-profile-sub")));
            self.upload_profile_btn.render_text(ui, rr, t, tl!("item-upload-profile-apply"), 0.42, false);
        }
        item! {
            render_title(ui, tl!("item-upload-consent"), None);
            self.upload_consent_btn.render_text(ui, rr, t, tl!("item-upload-consent-open"), 0.45, false);
        }
        item! {
            render_title(ui, tl!("item-history"), Some(tl!("item-history-sub")));
            self.history_btn.render_text(ui, rr, t, tl!("item-history-open"), 0.45, false);
        }
        debug_assert_eq!(gi, n_items, "settings 分组：n_items 与实际 item! 条数不一致");
        (w, h)
    }
}

impl ChartUiList {
    pub fn new() -> Self {
        Self {
            // ---- 打击表现 ----
            in_game_font_btn: DRectButton::new(),
            // 打歌字号倍率：0.6 ~ 2.2，步进 0.02（1.0 = 上游大小 / 1.78 = Phi-Recorder 精确值）
            text_scale_slider: Slider::new(Config::TEXT_SCALE_MIN..Config::TEXT_SCALE_MAX, 0.02),
            judge_text_btn: DRectButton::new(),
            judge_text_style_btn: DRectButton::new(),
            judge_text_size_slider: Slider::new(0.2..1.5, 0.02),
            miss_marker_btn: DRectButton::new(),
            miss_marker_time_slider: Slider::new(0.2..10., 0.1),
            note_trail_btn: DRectButton::new(),
            note_trail_len_slider: Slider::new(0.05..1., 0.05),
            note_trail_alpha_slider: Slider::new(0.05..1., 0.05),
            note_trail_count_slider: Slider::new(1. ..256., 1.),
            note_trail_click_btn: DRectButton::new(),
            note_trail_drag_btn: DRectButton::new(),
            note_trail_flick_btn: DRectButton::new(),
            line_afterimage_btn: DRectButton::new(),
            line_afterimage_count_slider: Slider::new(1. ..256., 1.),
            line_afterimage_alpha_slider: Slider::new(0.05..1., 0.05),
            music_spectrum_btn: DRectButton::new(),
            music_spectrum_gain_slider: Slider::new(0.2..4., 0.1),
            line_glow_btn: DRectButton::new(),
            line_glow_strength_slider: Slider::new(0.05..1., 0.05),

            // ---- 实时 HUD ----
            hud_btn: DRectButton::new(),
            hud_acc_btn: DRectButton::new(),
            hud_counts_btn: DRectButton::new(),
            hud_max_combo_btn: DRectButton::new(),
            hud_time_btn: DRectButton::new(),
            hud_delta_btn: DRectButton::new(),
            hud_rks_btn: DRectButton::new(),
            hud_fps_btn: DRectButton::new(),
            hud_corner_btn: DRectButton::new(),
            hud_size_slider: Slider::new(0.5..2., 0.05),
            hud_alpha_slider: Slider::new(0.1..1., 0.05),
            hud_outline_btn: DRectButton::new(),

            // ---- 准度条 ----
            timing_bar_enabled_btn: DRectButton::new(),
            timing_bar_counts_btn: DRectButton::new(),
            timing_bar_edit_btn: DRectButton::new(),
            timing_edit_requested: false,

            // ---- 判定范围可视化 ----
            judge_range_enabled_btn: DRectButton::new(),
            judge_range_line_btn: DRectButton::new(),
            judge_range_note_btn: DRectButton::new(),
        }
    }

    pub fn top_touch(&mut self, _touch: &Touch, _t: f32) -> bool {
        false
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        let data = get_data_mut();
        let config = &mut data.config;

        // ---- 准度条 ----
        if self.timing_bar_enabled_btn.touch(touch, t) {
            config.timing_bar.enabled ^= true;
            return Ok(Some(true));
        }
        if self.timing_bar_counts_btn.touch(touch, t) {
            config.timing_bar.record_counts ^= true;
            return Ok(Some(true));
        }
        if self.timing_bar_edit_btn.touch(touch, t) {
            self.timing_edit_requested = true;
            return Ok(Some(true));
        }

        // ---- 判定范围可视化 ----
        if self.judge_range_enabled_btn.touch(touch, t) {
            config.judgement_range_debug.enabled ^= true;
            return Ok(Some(true));
        }
        {
            use prpr::config::JudgementRangeAnchor;
            let anchor = &mut config.judgement_range_debug.anchor;
            if self.judge_range_line_btn.touch(touch, t) {
                // 单独点「判定线」：已是 JudgeLine → 关掉（变成 Note）；否则切到 JudgeLine
                *anchor = if *anchor == JudgementRangeAnchor::JudgeLine {
                    JudgementRangeAnchor::Note
                } else {
                    JudgementRangeAnchor::JudgeLine
                };
                return Ok(Some(true));
            }
            if self.judge_range_note_btn.touch(touch, t) {
                *anchor = if *anchor == JudgementRangeAnchor::Note {
                    JudgementRangeAnchor::JudgeLine
                } else {
                    JudgementRangeAnchor::Note
                };
                return Ok(Some(true));
            }
        }

        // ---- 打击表现 ----
        if self.in_game_font_btn.touch(touch, t) {
            config.phi_recorder_font ^= true;
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.text_scale_slider.touch(touch, t, &mut config.in_game_text_scale) {
            config.in_game_text_scale = config.in_game_text_scale.clamp(Config::TEXT_SCALE_MIN, Config::TEXT_SCALE_MAX);
            return Ok(wt);
        }
        if self.judge_text_btn.touch(touch, t) {
            config.judge_text ^= true;
            return Ok(Some(true));
        }
        if self.judge_text_style_btn.touch(touch, t) {
            config.judge_text_style = config.judge_text_style.next();
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.judge_text_size_slider.touch(touch, t, &mut config.judge_text_size) {
            config.judge_text_size = config.judge_text_size.clamp(0.2, 1.5);
            return Ok(wt);
        }
        if self.miss_marker_btn.touch(touch, t) {
            config.miss_marker ^= true;
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.miss_marker_time_slider.touch(touch, t, &mut config.miss_marker_time) {
            config.miss_marker_time = config.miss_marker_time.clamp(0.2, 10.);
            return Ok(wt);
        }
        if self.note_trail_btn.touch(touch, t) {
            config.note_trail ^= true;
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.note_trail_len_slider.touch(touch, t, &mut config.note_trail_len) {
            config.note_trail_len = config.note_trail_len.clamp(0., 1.);
            return Ok(wt);
        }
        if let wt @ Some(_) = self.note_trail_alpha_slider.touch(touch, t, &mut config.note_trail_alpha) {
            config.note_trail_alpha = config.note_trail_alpha.clamp(0., 1.);
            return Ok(wt);
        }
        {
            let mut v = config.note_trail_count_value() as f32;
            if let wt @ Some(_) = self.note_trail_count_slider.touch(touch, t, &mut v) {
                config.note_trail_count = (v.round() as u32).clamp(1, Config::AFTERIMAGE_COUNT_MAX);
                return Ok(wt);
            }
        }
        if self.note_trail_click_btn.touch(touch, t) {
            config.note_trail_click ^= true;
            return Ok(Some(true));
        }
        if self.note_trail_drag_btn.touch(touch, t) {
            config.note_trail_drag ^= true;
            return Ok(Some(true));
        }
        if self.note_trail_flick_btn.touch(touch, t) {
            config.note_trail_flick ^= true;
            return Ok(Some(true));
        }
        if self.line_afterimage_btn.touch(touch, t) {
            config.line_afterimage ^= true;
            return Ok(Some(true));
        }
        {
            let mut v = config.line_afterimage_count_value() as f32;
            if let wt @ Some(_) = self.line_afterimage_count_slider.touch(touch, t, &mut v) {
                config.line_afterimage_count = (v.round() as u32).clamp(1, Config::AFTERIMAGE_COUNT_MAX);
                return Ok(wt);
            }
        }
        if let wt @ Some(_) = self.line_afterimage_alpha_slider.touch(touch, t, &mut config.line_afterimage_alpha) {
            config.line_afterimage_alpha = config.line_afterimage_alpha.clamp(0., 1.);
            return Ok(wt);
        }
        if self.music_spectrum_btn.touch(touch, t) {
            config.music_spectrum ^= true;
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.music_spectrum_gain_slider.touch(touch, t, &mut config.music_spectrum_gain) {
            config.music_spectrum_gain = config.music_spectrum_gain.clamp(0.2, 4.);
            return Ok(wt);
        }
        if self.line_glow_btn.touch(touch, t) {
            config.line_glow ^= true;
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.line_glow_strength_slider.touch(touch, t, &mut config.line_glow_strength) {
            config.line_glow_strength = config.line_glow_strength.clamp(0., 1.);
            return Ok(wt);
        }

        // ---- 实时 HUD ----
        if self.hud_btn.touch(touch, t) {
            config.hud ^= true;
            return Ok(Some(true));
        }
        if self.hud_acc_btn.touch(touch, t) {
            config.hud_acc ^= true;
            return Ok(Some(true));
        }
        if self.hud_counts_btn.touch(touch, t) {
            config.hud_counts ^= true;
            return Ok(Some(true));
        }
        if self.hud_max_combo_btn.touch(touch, t) {
            config.hud_max_combo ^= true;
            return Ok(Some(true));
        }
        if self.hud_time_btn.touch(touch, t) {
            config.hud_time ^= true;
            return Ok(Some(true));
        }
        if self.hud_delta_btn.touch(touch, t) {
            config.hud_delta ^= true;
            return Ok(Some(true));
        }
        if self.hud_rks_btn.touch(touch, t) {
            config.hud_rks ^= true;
            return Ok(Some(true));
        }
        if self.hud_fps_btn.touch(touch, t) {
            config.hud_fps ^= true;
            return Ok(Some(true));
        }
        if self.hud_corner_btn.touch(touch, t) {
            config.hud_corner = config.hud_corner.next();
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.hud_size_slider.touch(touch, t, &mut config.hud_size) {
            config.hud_size = config.hud_size.clamp(0.5, 2.);
            return Ok(wt);
        }
        if let wt @ Some(_) = self.hud_alpha_slider.touch(touch, t, &mut config.hud_alpha) {
            config.hud_alpha = config.hud_alpha.clamp(0.1, 1.);
            return Ok(wt);
        }
        if self.hud_outline_btn.touch(touch, t) {
            config.hud_outline ^= true;
            return Ok(Some(true));
        }

        Ok(None)
    }

    pub fn update(&mut self, _t: f32) -> Result<bool> {
        Ok(false)
    }

    pub fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let w = r.w;
        // 行内文字宽度上限：右侧还要放开关 / 按钮 / 滑块
        set_row_text_max(w - 0.38);
        let mut h = 0.;
        // ---- iOS 分组列表（inset grouped）---- 见 `GeneralList::render` 的说明。
        // ⚠️ `n_items` 手工核对：本 section 全部是固定项，没有 `#[cfg]` 门控。
        // 本 section 里有 4 条 `// ---- 打击表现 / 实时 HUD / 准度条 / 判定范围可视化 ----`
        // 的子分组注释，但**当前代码并没有给它们任何视觉分隔**，所以这里仍然只铺**一个**
        // 容器 —— 保持与既有观感一致。以后想按子分组分开，就再拆成多个 `group_plate`。
        let th = crate::newui::theme();
        let group_radius = 0.014;
        let n_items = 39usize;
        crate::newui::group_plate(ui, &th, Rect::new(0., 0., w, row_h() * n_items as f32), group_radius);
        let mut gi = 0usize;
        macro_rules! item {
            ($($b:tt)*) => {{
                // 每个设置项 = 分组里的一行：**不填底板**（底板是整组的圆角容器），
                // 只叠悬停墨色 + 底部内缩发丝线（内缩到 `LEFT`，对齐标题左缘）。
                let row = Rect::new(0., 0., w, row_h());
                let (mx, my) = crate::newui::mouse_world();
                let hp = if ui.rect_to_global(row).contains(vec2(mx, my)) { 1. } else { 0. };
                crate::newui::group_row(
                    ui,
                    &th,
                    row,
                    hp,
                    gi == 0,
                    gi + 1 == n_items,
                    group_radius,
                    0.06,
                    Color::new(0., 0., 0., 0.),
                );
                gi += 1;
                $($b)*
                ui.dy(row_h());
                h += row_h();
            }}
        }
        let rr = right_rect(w);

        let data = get_data();
        let config = &data.config;

        // ---- 打击表现 ----
        item! {
            render_title(ui, tl!("item-in-game-font"), Some(tl!("item-in-game-font-sub")));
            render_switch(ui, rr, t, &mut self.in_game_font_btn, config.phi_recorder_font);
        }
        item! {
            render_title(ui, tl!("item-text-scale"), Some(tl!("item-text-scale-sub")));
            self.text_scale_slider
                .render(ui, rr, t, config.in_game_text_scale, format!("{:.2}x", config.in_game_text_scale_value()));
        }
        item! {
            render_title(ui, tl!("item-judge-text"), Some(tl!("item-judge-text-sub")));
            render_switch(ui, rr, t, &mut self.judge_text_btn, config.judge_text);
        }
        item! {
            let style = match config.judge_text_style {
                prpr::config::JudgeTextStyle::Fade => tl!("judge-text-style-fade"),
                prpr::config::JudgeTextStyle::Rise => tl!("judge-text-style-rise"),
                prpr::config::JudgeTextStyle::Pop => tl!("judge-text-style-pop"),
                prpr::config::JudgeTextStyle::RisePop => tl!("judge-text-style-rise-pop"),
            };
            render_title(ui, tl!("item-judge-text-style"), Some(tl!("item-judge-text-style-sub")));
            self.judge_text_style_btn.render_text(ui, rr, t, style, 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-judge-text-size"), Some(tl!("item-judge-text-size-sub")));
            self.judge_text_size_slider
                .render(ui, rr, t, config.judge_text_size, format!("{:.2}", config.judge_text_size));
        }
        item! {
            render_title(ui, tl!("item-miss-marker"), Some(tl!("item-miss-marker-sub")));
            render_switch(ui, rr, t, &mut self.miss_marker_btn, config.miss_marker);
        }
        item! {
            render_title(ui, tl!("item-miss-marker-time"), Some(tl!("item-miss-marker-time-sub")));
            self.miss_marker_time_slider
                .render(ui, rr, t, config.miss_marker_time, format!("{:.1}s", config.miss_marker_time));
        }
        item! {
            render_title(ui, tl!("item-note-trail"), Some(tl!("item-note-trail-sub")));
            render_switch(ui, rr, t, &mut self.note_trail_btn, config.note_trail);
        }
        item! {
            render_title(ui, tl!("item-note-trail-len"), Some(tl!("item-note-trail-len-sub")));
            self.note_trail_len_slider
                .render(ui, rr, t, config.note_trail_len, format!("{:.0}%", config.note_trail_len * 100.));
        }
        item! {
            render_title(ui, tl!("item-note-trail-alpha"), Some(tl!("item-note-trail-alpha-sub")));
            self.note_trail_alpha_slider
                .render(ui, rr, t, config.note_trail_alpha, format!("{:.0}%", config.note_trail_alpha * 100.));
        }
        item! {
            render_title(ui, tl!("item-note-trail-count"), Some(tl!("item-note-trail-count-sub")));
            self.note_trail_count_slider
                .render(ui, rr, t, config.note_trail_count as f32, format!("{}", config.note_trail_count_value()));
        }
        item! {
            render_title(ui, tl!("item-note-trail-click"), None);
            render_switch(ui, rr, t, &mut self.note_trail_click_btn, config.note_trail_click);
        }
        item! {
            render_title(ui, tl!("item-note-trail-drag"), None);
            render_switch(ui, rr, t, &mut self.note_trail_drag_btn, config.note_trail_drag);
        }
        item! {
            render_title(ui, tl!("item-note-trail-flick"), None);
            render_switch(ui, rr, t, &mut self.note_trail_flick_btn, config.note_trail_flick);
        }
        item! {
            render_title(ui, tl!("item-line-afterimage"), Some(tl!("item-line-afterimage-sub")));
            render_switch(ui, rr, t, &mut self.line_afterimage_btn, config.line_afterimage);
        }
        item! {
            render_title(ui, tl!("item-line-afterimage-count"), Some(tl!("item-line-afterimage-count-sub")));
            self.line_afterimage_count_slider
                .render(ui, rr, t, config.line_afterimage_count as f32, format!("{}", config.line_afterimage_count_value()));
        }
        item! {
            render_title(ui, tl!("item-line-afterimage-alpha"), Some(tl!("item-line-afterimage-alpha-sub")));
            self.line_afterimage_alpha_slider
                .render(ui, rr, t, config.line_afterimage_alpha, format!("{:.0}%", config.line_afterimage_alpha * 100.));
        }
        item! {
            render_title(ui, tl!("item-music-spectrum"), Some(tl!("item-music-spectrum-sub")));
            render_switch(ui, rr, t, &mut self.music_spectrum_btn, config.music_spectrum);
        }
        item! {
            render_title(ui, tl!("item-music-spectrum-gain"), Some(tl!("item-music-spectrum-gain-sub")));
            self.music_spectrum_gain_slider
                .render(ui, rr, t, config.music_spectrum_gain, format!("{:.1}x", config.music_spectrum_gain));
        }
        item! {
            render_title(ui, tl!("item-line-glow"), Some(tl!("item-line-glow-sub")));
            render_switch(ui, rr, t, &mut self.line_glow_btn, config.line_glow);
        }
        item! {
            render_title(ui, tl!("item-line-glow-strength"), Some(tl!("item-line-glow-strength-sub")));
            self.line_glow_strength_slider
                .render(ui, rr, t, config.line_glow_strength, format!("{:.0}%", config.line_glow_strength * 100.));
        }

        // ---- 实时 HUD ----
        item! {
            render_title(ui, tl!("item-hud"), Some(tl!("item-hud-sub")));
            render_switch(ui, rr, t, &mut self.hud_btn, config.hud);
        }
        item! {
            render_title(ui, tl!("item-hud-acc"), None);
            render_switch(ui, rr, t, &mut self.hud_acc_btn, config.hud_acc);
        }
        item! {
            render_title(ui, tl!("item-hud-counts"), None);
            render_switch(ui, rr, t, &mut self.hud_counts_btn, config.hud_counts);
        }
        item! {
            render_title(ui, tl!("item-hud-max-combo"), None);
            render_switch(ui, rr, t, &mut self.hud_max_combo_btn, config.hud_max_combo);
        }
        item! {
            render_title(ui, tl!("item-hud-time"), None);
            render_switch(ui, rr, t, &mut self.hud_time_btn, config.hud_time);
        }
        item! {
            render_title(ui, tl!("item-hud-delta"), None);
            render_switch(ui, rr, t, &mut self.hud_delta_btn, config.hud_delta);
        }
        item! {
            render_title(ui, tl!("item-hud-rks"), Some(tl!("item-hud-rks-sub")));
            render_switch(ui, rr, t, &mut self.hud_rks_btn, config.hud_rks);
        }
        item! {
            render_title(ui, tl!("item-hud-fps"), None);
            render_switch(ui, rr, t, &mut self.hud_fps_btn, config.hud_fps);
        }
        item! {
            let corner = match config.hud_corner {
                prpr::config::HudCorner::TopLeft => tl!("hud-corner-tl"),
                prpr::config::HudCorner::TopCenter => tl!("hud-corner-tc"),
                prpr::config::HudCorner::TopRight => tl!("hud-corner-tr"),
                prpr::config::HudCorner::LeftCenter => tl!("hud-corner-ml"),
                prpr::config::HudCorner::Center => tl!("hud-corner-mc"),
                prpr::config::HudCorner::RightCenter => tl!("hud-corner-mr"),
                prpr::config::HudCorner::BottomLeft => tl!("hud-corner-bl"),
                prpr::config::HudCorner::BottomCenter => tl!("hud-corner-bc"),
                prpr::config::HudCorner::BottomRight => tl!("hud-corner-br"),
            };
            render_title(ui, tl!("item-hud-corner"), Some(tl!("item-hud-corner-sub")));
            self.hud_corner_btn.render_text(ui, rr, t, corner, 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-hud-size"), Some(tl!("item-hud-size-sub")));
            self.hud_size_slider
                .render(ui, rr, t, config.hud_size, format!("{:.2}x", config.hud_size));
        }
        item! {
            render_title(ui, tl!("item-hud-alpha"), Some(tl!("item-hud-alpha-sub")));
            self.hud_alpha_slider
                .render(ui, rr, t, config.hud_alpha, format!("{:.0}%", config.hud_alpha * 100.));
        }
        item! {
            render_title(ui, tl!("item-hud-outline"), Some(tl!("item-hud-outline-sub")));
            render_switch(ui, rr, t, &mut self.hud_outline_btn, config.hud_outline);
        }

        // ---- 准度条（Timing Bar） ----
        item! {
            render_title(ui, tl!("item-timing-bar"), Some(tl!("item-timing-bar-sub")));
            render_switch(ui, rr, t, &mut self.timing_bar_enabled_btn, config.timing_bar.enabled);
        }
        item! {
            render_title(ui, tl!("item-timing-counts"), Some(tl!("item-timing-counts-sub")));
            render_switch(ui, rr, t, &mut self.timing_bar_counts_btn, config.timing_bar.record_counts);
        }
        item! {
            render_title(ui, tl!("item-timing-edit"), Some(tl!("item-timing-edit-sub")));
            self.timing_bar_edit_btn.render_text(ui, rr, t, tl!("timing-edit-open"), 0.5, true);
        }

        // ---- 判定范围可视化 ----
        item! {
            render_title(
                ui,
                "判定范围可视化",
                Some("把判定窗口画成框（只读叠加，不影响判定）".into()),
            );
            render_switch(ui, rr, t, &mut self.judge_range_enabled_btn, config.judgement_range_debug.enabled);
        }
        {
            use prpr::config::JudgementRangeAnchor;
            let on_line = matches!(
                config.judgement_range_debug.anchor,
                JudgementRangeAnchor::JudgeLine | JudgementRangeAnchor::Both
            );
            let on_note = matches!(
                config.judgement_range_debug.anchor,
                JudgementRangeAnchor::Note | JudgementRangeAnchor::Both
            );
            item! {
                render_title(ui, "贴判定线显示", None);
                render_switch(ui, rr, t, &mut self.judge_range_line_btn, on_line);
            }
            item! {
                render_title(ui, "贴音符显示", None);
                render_switch(ui, rr, t, &mut self.judge_range_note_btn, on_note);
            }
        }
        debug_assert_eq!(gi, n_items, "settings 分组：n_items 与实际 item! 条数不一致");
        (w, h)
    }
}

// ============================ 「账号」页 ============================
//
// 多账号管理：列出 `Data::accounts` 里保存过的账号，可**一键切换**、**添加**、**删除**。
//
// 为什么做这一页：Phira 只有「邮箱 + 密码」登录，原生界面里**没有任何账号入口**
// —— 移动端因此根本没法登录 / 换号。账号管理放在设置里，就不用另开入口了。
//
// 切换账号 = 用保存的凭据重新登录（Phira 没有长期 token，这是唯一可靠做法），
// 所以切换是**异步**的：点了以后起一个 Task，跑完显示提示。

/// 「账号」页里正在进行的异步操作。
enum AccountAction {
    /// 用保存的凭据切换账号
    Switch(String),
}

struct AccountList {
    /// 每个账号一行「切换」按钮
    switch_btns: Vec<DRectButton>,
    /// 每个账号一行「删除」按钮
    remove_btns: Vec<DRectButton>,
    /// 顶部「登录 / 添加账号」按钮
    add_btn: DRectButton,
    /// 未登录时的登录入口
    login_btn: DRectButton,
    /// 退出当前账号
    logout_btn: DRectButton,
    /// 正在跑的异步操作
    task: Option<Task<Result<String>>>,
    /// 正在跑的操作类型
    action: Option<AccountAction>,
    /// 刚点过「删除」、等下一次点击确认的邮箱（两步删除，避免误删）
    confirm_remove: Option<String>,
    /// **Phira 自带的登录界面**（`crate::login::Login`）。
    ///
    /// 用户明确要求「用 phira 自带的登录界面」—— 那套在桌面端一直能用，
    /// 而我之前自造的两步 `request_input` 流程坏过。这里直接复用同一个组件。
    ///
    /// 延迟创建：`Login::new` 需要 `Arc<Icons>`，而 `SettingsPage` 构造时
    /// 全局 icons 不一定已就绪（用 `super::global_icons()` 取）。
    /// 该组件**自己管理可见性**：`enter(t)` 打开，`touch`/`update` 不可见时是 no-op。
    login: Option<crate::login::Login>,
}

impl AccountList {
    /// 确保自带登录界面已创建（拿不到全局 icons 就返回 false）。
    fn ensure_login(&mut self) -> bool {
        if self.login.is_none() {
            let Some(icons) = super::global_icons() else {
                tracing::warn!("[account] 全局 icons 未就绪，无法打开自带登录界面");
                return false;
            };
            self.login = Some(crate::login::Login::new(icons));
        }
        true
    }

    fn new() -> Self {
        Self {
            switch_btns: Vec::new(),
            remove_btns: Vec::new(),
            add_btn: DRectButton::new(),
            login_btn: DRectButton::new(),
            logout_btn: DRectButton::new(),
            task: None,
            action: None,
            confirm_remove: None,
            login: None,
        }
    }

    /// 起一个异步操作（同一时刻只允许一个）。
    fn start(&mut self, action: AccountAction) {
        if self.task.is_some() {
            return;
        }
        let fut: std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>> = match &action {
            AccountAction::Switch(email) => {
                let email = email.clone();
                Box::pin(async move { crate::page::user::switch_account(&email).await })
            }
        };
        self.action = Some(action);
        self.task = Some(Task::new(fut));
    }

    fn top_touch(&mut self, _touch: &Touch, _t: f32) -> bool {
        false
    }

    fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        // Phira 自带登录界面盖在最上层：它可见时优先吃掉触摸（与 home.rs 一致）
        if let Some(login) = &mut self.login {
            if login.touch(touch, t) {
                return Ok(Some(true));
            }
        }
        let accounts = get_data().accounts.clone();
        for (i, acc) in accounts.iter().enumerate() {
            if let Some(btn) = self.switch_btns.get_mut(i) {
                if btn.touch(touch, t) {
                    // 已经是当前账号就不重复登录（省一次网络往返）
                    if crate::page::user::current_email().as_deref() == Some(acc.email.as_str()) {
                        show_message(tl!("account-already-current")).ok();
                    } else {
                        self.confirm_remove = None;
                        self.start(AccountAction::Switch(acc.email.clone()));
                    }
                    return Ok(Some(true));
                }
            }
            if let Some(btn) = self.remove_btns.get_mut(i) {
                if btn.touch(touch, t) {
                    // 两步删除：第一次点只是「待确认」，再点一次才真删，避免误触。
                    if self.confirm_remove.as_deref() == Some(acc.email.as_str()) {
                        self.confirm_remove = None;
                        crate::page::user::forget_account(&acc.email)?;
                        show_message(tl!("account-removed")).ok();
                    } else {
                        self.confirm_remove = Some(acc.email.clone());
                        show_message(tl!("account-remove-confirm")).ok();
                    }
                    return Ok(Some(true));
                }
            }
        }
        // 点别处就取消「待确认」
        self.confirm_remove = None;

        // 「登录 / 添加账号」→ 打开 **Phira 自带的登录界面**（不再用自造的输入框）
        if self.add_btn.touch(touch, t) {
            if self.ensure_login() {
                if let Some(login) = &mut self.login {
                    login.enter(t);
                }
            } else {
                show_error(anyhow::anyhow!("{}", tl!("account-login-unavailable")));
            }
            return Ok(Some(true));
        }
        if self.login_btn.touch(touch, t) {
            if self.ensure_login() {
                if let Some(login) = &mut self.login {
                    login.enter(t);
                }
            }
            return Ok(Some(true));
        }
        if self.logout_btn.touch(touch, t) && self.task.is_none() {
            crate::page::user::logout()?;
            show_message(tl!("account-logged-out")).ok();
            return Ok(Some(true));
        }
        Ok(None)
    }

    fn update(&mut self, t: f32) -> Result<bool> {
        // 自带登录界面（它自己判断是否可见；登录成功后由它写 `data.me`）
        if let Some(login) = &mut self.login {
            login.update(t)?;
        }
        // 异步任务收结果
        if let Some(task) = &mut self.task {
            if let Some(res) = task.take() {
                self.task = None;
                self.action = None;
                match res {
                    Ok(name) => {
                        show_message(format!("{} {}", tl!("account-switched"), name)).ok();
                    }
                    Err(err) => show_error(err.context(tl!("account-failed"))),
                }
                return Ok(true); // 账号变了，列表要重画
            }
        }
        Ok(false)
    }

    /// 渲染成一行行「账号卡」。
    ///
    /// 不复用 `item!`（那是「标题 + 开关」的单行卡），账号行要放头像 + 名字 + 两个按钮，
    /// 所以这里自己排。
    fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let m = Metrics::new(ui);
        let th = crate::newui::theme();
        let w = r.w;
        // 与其它设置列表完全一致：文字宽度上限 + 行高走 `row_h()`。
        // ⚠️ 之前这里自己写了 `row_h = m.u(64.)` 当行高，和 `row_h()` 不是同一个值，
        // 卡片高度与推进量对不上 —— 表现就是「所有卡片叠在一起」。
        set_row_text_max(w - 0.38);
        let mut h = 0.;
        // ---- iOS 分组列表（inset grouped）---- 见 `GeneralList::render` 的说明。
        // ⚠️ `n_items` 手工核对：本 section 全部是固定项，没有 `#[cfg]` 门控。
        // （`th` 这个 section 上面已经有了，不重复声明。）
        let group_radius = 0.014;
        let n_items = 4usize;
        crate::newui::group_plate(ui, &th, Rect::new(0., 0., w, row_h() * n_items as f32), group_radius);
        let mut gi = 0usize;
        macro_rules! item {
            ($($b:tt)*) => {{
                // 每个设置项 = 分组里的一行：**不填底板**（底板是整组的圆角容器），
                // 只叠悬停墨色 + 底部内缩发丝线（内缩到 `LEFT`，对齐标题左缘）。
                let row = Rect::new(0., 0., w, row_h());
                let (mx, my) = crate::newui::mouse_world();
                let hp = if ui.rect_to_global(row).contains(vec2(mx, my)) { 1. } else { 0. };
                crate::newui::group_row(
                    ui,
                    &th,
                    row,
                    hp,
                    gi == 0,
                    gi + 1 == n_items,
                    group_radius,
                    0.06,
                    Color::new(0., 0., 0., 0.),
                );
                gi += 1;
                $($b)*
                ui.dy(row_h());
                h += row_h();
            }}
        }
        let rr = right_rect(w);
        // 先把要用的值都取出来（`current_email()` 内部也会读 data，读锁是共享的，
        // 但下面 `forget_account` 之类的写操作要等这里读完 —— NLL 会在最后一次使用后
        // 自动结束借用，不需要手动 drop）。
        let data = get_data();
        let accounts = data.accounts.clone();
        let current = crate::page::user::current_email();
        let logged_in = data.me.is_some();
        let me_name = data.me.as_ref().map(|it| it.name.clone());

        // ---- 当前账号状态 ----
        item! {
            render_title(
                ui,
                if logged_in { tl!("account-current") } else { tl!("account-none") },
                Some(
                    if logged_in {
                        me_name.clone().unwrap_or_else(|| "—".to_owned()).into()
                    } else {
                        tl!("account-none-sub")
                    },
                ),
            );
            if logged_in {
                self.logout_btn.render_text(ui, rr, t, tl!("account-logout"), 0.5, false);
            }
        }

        // ---- 添加 / 登录 ----
        // 用**一个**入口：点了先要邮箱、再要密码，最后登录并保存。
        item! {
            if self.task.is_some() {
                render_title(ui, tl!("account-working"), None);
            } else {
                render_title(ui, tl!("account-add"), Some(tl!("account-add-sub")));
                self.add_btn.render_text(ui, rr, t, tl!("account-login-btn"), 0.5, true);
            }
        }

        // ---- 已保存的账号 ----
        if accounts.is_empty() {
            item! {
                render_title(ui, tl!("account-empty"), Some(tl!("account-empty-sub")));
            }
        } else {
            for (i, acc) in accounts.iter().enumerate() {
                let is_cur = current.as_deref() == Some(acc.email.as_str());
                let name = if acc.name.is_empty() { acc.email.clone() } else { acc.name.clone() };
                // 副标题显示邮箱（切换的依据）+ 当前标记
                let sub = if is_cur {
                    format!("{} · {}", acc.email, tl!("account-current-tag"))
                } else {
                    acc.email.clone()
                };
                // 一行里要放两个按钮：切换 + 删除。分左右两半排，不重叠。
                let (sw_r, del_r) = {
                    let half = rr.w / 2.;
                    let gap = 0.012;
                    (
                        Rect::new(rr.x, rr.y, half - gap, rr.h),
                        Rect::new(rr.x + half + gap, rr.y, half - gap, rr.h),
                    )
                };
                item! {
                    render_title(ui, name.clone(), Some(sub.into()));
                    if is_cur {
                        crate::newui::text_with_font(
                            ui,
                            &m,
                            tl!("account-in-use"),
                            sw_r.center().x,
                            sw_r.center().y,
                            14.,
                            th.text_dim,
                            (0.5, 0.5),
                            &prpr::core::BOLD_FONT,
                        );
                    } else {
                        self.switch_btns
                            .get_or_insert_with_none(i)
                            .render_text(ui, sw_r, t, tl!("account-switch"), 0.45, true);
                    }
                    self.remove_btns.get_or_insert_with_none(i).render_text(
                        ui,
                        del_r,
                        t,
                        if self.confirm_remove.as_deref() == Some(acc.email.as_str()) {
                            tl!("account-remove-again")
                        } else {
                            tl!("account-remove")
                        },
                        0.45,
                        false,
                    );
                }
            }
        }

        // 「待确认删除」的行给个提示色（这里只画提示文字，不做弹窗）
        if self.confirm_remove.is_some() {
            crate::newui::text(
                ui,
                &m,
                tl!("account-remove-confirm"),
                r.x + m.u(10.),
                0.,
                13.,
                crate::newui::theme().text_dim,
                (0., 0.5),
            );
        }
        // 登录界面**不在这里画** —— 它按全屏设计，必须由 `SettingsPage::render`
        // 在滚动区之外调用 `render_login_overlay`，否则会被裁剪/变形。
        debug_assert_eq!(gi, n_items, "settings 分组：n_items 与实际 item! 条数不一致");
        (w, h)
    }

    /// 在全屏层上绘制 Phira 自带登录界面（由 `SettingsPage::render` 调用）。
    ///
    /// 组件内部自己判断可见性：没点开登录时 `render` 是不画东西的。
    fn render_login_overlay(&mut self, ui: &mut Ui, t: f32) {
        if let Some(login) = &mut self.login {
            login.render(ui, t);
        }
    }
}

/// 小工具：`Vec` 按下标取按钮，不够就补一个（账号数量会变）。
trait GetOrInsertBtn {
    fn get_or_insert_with_none(&mut self, i: usize) -> &mut DRectButton;
}
impl GetOrInsertBtn for Vec<DRectButton> {
    fn get_or_insert_with_none(&mut self, i: usize) -> &mut DRectButton {
        while self.len() <= i {
            self.push(DRectButton::new());
        }
        &mut self[i]
    }
}

// ============================ 「日志」页 ============================
//
// 手机上没有 adb，出问题只能靠截图。这里把 `<data>/phira-crash.log` 直接显示出来。
//
// 这个文件和 Java 侧 `CrashLog`、Rust 的 panic hook、以及 `crate::diag` 共用
// （都在 `dir::root()` 下），所以里面既有 Java 的启动阶段/异常，也有 Rust 的
// 启动阶段与 `[diag]` 诊断。

struct LogList {
    /// 「刷新」按钮（重新读文件）
    refresh_btn: DRectButton,
    /// 「清空」按钮
    clear_btn: DRectButton,
    /// 当前显示的内容（按行切好）
    lines: Vec<String>,
}

impl LogList {
    fn new() -> Self {
        let mut it = Self {
            refresh_btn: DRectButton::new(),
            clear_btn: DRectButton::new(),
            lines: Vec::new(),
        };
        it.reload();
        it
    }

    /// 崩溃日志文件路径（`dir::root()` 就是 Java `getFilesDir()/data`）。
    fn log_path() -> Option<std::path::PathBuf> {
        let root = dir::root().ok()?;
        Some(std::path::Path::new(&root).join("phira-crash.log"))
    }

    /// 重新读取日志（只保留最后 200 行，避免超大文件卡住渲染）。
    fn reload(&mut self) {
        self.lines.clear();
        // 先把「噪区着色器编译错误」放最前面 —— 它是「噪区变成一片纯红方块」的
        // 直接原因（MATERIAL 建不出来 → 走兜底画红方块），而且以前手机上完全看不到。
        // `force_block_shader_init()` 会触发一次编译尝试，这样没打过含噪区的谱面也能读到。
        prpr::core::force_block_shader_init();
        match prpr::core::block_shader_error() {
            Some(err) => {
                self.lines.push("[block-shader] 噪区着色器编译失败！噪区会退化成纯红方块：".to_owned());
                for l in err.lines().take(12) {
                    self.lines.push(format!("[block-shader] {l}"));
                }
            }
            None => self.lines.push("[block-shader] 噪区着色器编译成功 ✅".to_owned()),
        }
        let Some(path) = Self::log_path() else {
            self.lines.push("（无法确定数据目录）".to_owned());
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let all: Vec<&str> = text.lines().collect();
                let start = all.len().saturating_sub(200);
                self.lines.extend(all[start..].iter().map(|s| (*s).to_owned()));
                if self.lines.is_empty() {
                    self.lines.push("（日志为空）".to_owned());
                }
            }
            Err(err) => {
                self.lines.push(format!("（读取失败：{err}）"));
                self.lines.push(format!("路径：{}", path.display()));
            }
        }
    }

    fn top_touch(&mut self, _touch: &Touch, _t: f32) -> bool {
        false
    }

    fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        if self.refresh_btn.touch(touch, t) {
            self.reload();
            show_message(tl!("log-refreshed")).ok();
            return Ok(Some(true));
        }
        if self.clear_btn.touch(touch, t) {
            if let Some(path) = Self::log_path() {
                let _ = std::fs::write(&path, b"");
            }
            self.reload();
            show_message(tl!("log-cleared")).ok();
            return Ok(Some(true));
        }
        Ok(None)
    }

    fn update(&mut self, _t: f32) -> Result<bool> {
        Ok(false)
    }

    /// 一行日志一行卡片地铺开（复用页面自身的滚动，不用再做嵌套滚动）。
    fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let w = r.w;
        set_row_text_max(w - 0.05);
        let mut h = 0.;
        // ---- iOS 分组列表（inset grouped）---- 见 `GeneralList::render` 的说明。
        // 本 section 有**两个**组：① 顶部「日志 + 刷新 / 清空」那一行；② 下面的日志行。
        // 两组的边界在这里是**显式**的，所以各自铺一个容器就行，不用靠 `n_items` 猜。
        let group_radius = 0.014;
        let rr = right_rect(w);
        let m = Metrics::new(ui);
        let th = crate::newui::theme();

        // 组 ①：只有一行
        crate::newui::group_plate(ui, &th, Rect::new(0., 0., w, row_h()), group_radius);
        let mut gi = 0usize;
        let n_items = 1usize;
        macro_rules! item {
            ($($b:tt)*) => {{
                let row = Rect::new(0., 0., w, row_h());
                let (mx, my) = crate::newui::mouse_world();
                let hp = if ui.rect_to_global(row).contains(vec2(mx, my)) { 1. } else { 0. };
                crate::newui::group_row(ui, &th, row, hp, gi == 0, gi + 1 == n_items, group_radius, 0.06, Color::new(0., 0., 0., 0.));
                gi += 1;
                $($b)*
                ui.dy(row_h());
                h += row_h();
            }}
        }

        item! {
            render_title(ui, tl!("log-title"), Some(tl!("log-sub")));
            let half = rr.w / 2.;
            let gap = 0.012;
            self.refresh_btn.render_text(
                ui, Rect::new(rr.x, rr.y, half - gap, rr.h), t, tl!("log-refresh"), 0.45, true);
            self.clear_btn.render_text(
                ui, Rect::new(rr.x + half + gap, rr.y, half - gap, rr.h), t, tl!("log-clear"), 0.45, false);
        }

        // 每行日志占一个窄行（比设置项矮，才能在一屏里看更多）
        let line_h = row_h() * 0.62;
        // 组 ②：所有日志行共用一个容器
        let lines_n = self.lines.len();
        crate::newui::group_plate(ui, &th, Rect::new(0., 0., w, line_h * lines_n as f32), group_radius);
        for (li, line) in self.lines.iter().enumerate() {
            let row = Rect::new(0., 0., w, line_h);
            let (mx, my) = crate::newui::mouse_world();
            let hp = if ui.rect_to_global(row).contains(vec2(mx, my)) { 1. } else { 0. };
            crate::newui::group_row(ui, &th, row, hp, li == 0, li + 1 == lines_n, group_radius, 0.06, Color::new(0., 0., 0., 0.));
            // `[diag]` / 失败 / 异常行高亮，方便一眼找到
            let color = if line.contains("[diag") || line.contains("失败") || line.contains("Exception") {
                Color::new(1., 0.55, 0.45, 0.95)
            } else {
                th.text_mute
            };
            crate::newui::text_ellipsis(
                ui,
                &m,
                line.clone(),
                0.012,
                (line_h - 0.004) / 2.,
                11.,
                w - 0.03,
                color,
                (0., 0.5),
            );
            ui.dy(line_h);
            h += line_h;
        }
        debug_assert_eq!(gi, n_items, "settings 分组：n_items 与实际 item! 条数不一致");
        (w, h)
    }
}

struct DebugList {
    chart_debug_btn: DRectButton,
    touch_style_btn: DRectButton,
    touch_color_btn: DRectButton,
    touch_alpha_slider: Slider,
    touch_debug_btn: DRectButton,
}

impl DebugList {
    pub fn new() -> Self {
        Self {
            chart_debug_btn: DRectButton::new(),
            touch_debug_btn: DRectButton::new(),
            touch_style_btn: DRectButton::new(),
            touch_color_btn: DRectButton::new(),
            touch_alpha_slider: Slider::new(0.05..1., 0.05),
        }
    }

    pub fn top_touch(&mut self, _touch: &Touch, _t: f32) -> bool {
        false
    }

    pub fn touch(&mut self, touch: &Touch, t: f32) -> Result<Option<bool>> {
        let data = get_data_mut();
        let config = &mut data.config;
        if self.chart_debug_btn.touch(touch, t) {
            config.chart_debug ^= true;
            return Ok(Some(true));
        }
        if self.touch_debug_btn.touch(touch, t) {
            config.touch_debug ^= true;
            return Ok(Some(true));
        }
        if self.touch_style_btn.touch(touch, t) {
            config.touch_marker_style = config.touch_marker_style.next();
            return Ok(Some(true));
        }
        if self.touch_color_btn.touch(touch, t) {
            config.touch_marker_color = config.touch_marker_color.next();
            return Ok(Some(true));
        }
        if let wt @ Some(_) = self.touch_alpha_slider.touch(touch, t, &mut config.touch_marker_alpha) {
            config.touch_marker_alpha = config.touch_marker_alpha.clamp(0.05, 1.);
            return Ok(wt);
        }
        Ok(None)
    }

    pub fn update(&mut self, _t: f32) -> Result<bool> {
        Ok(false)
    }

    pub fn render(&mut self, ui: &mut Ui, r: Rect, t: f32) -> (f32, f32) {
        let w = r.w;
        // 行内文字宽度上限：右侧还要放开关 / 按钮 / 滑块
        set_row_text_max(w - 0.38);
        let mut h = 0.;
        // ---- iOS 分组列表（inset grouped）---- 见 `GeneralList::render` 的说明。
        // ⚠️ `n_items` 手工核对：本 section 全部是固定项，没有 `#[cfg]` 门控。
        let th = crate::newui::theme();
        let group_radius = 0.014;
        let n_items = 5usize;
        crate::newui::group_plate(ui, &th, Rect::new(0., 0., w, row_h() * n_items as f32), group_radius);
        let mut gi = 0usize;
        macro_rules! item {
            ($($b:tt)*) => {{
                // 每个设置项 = 分组里的一行：**不填底板**（底板是整组的圆角容器），
                // 只叠悬停墨色 + 底部内缩发丝线（内缩到 `LEFT`，对齐标题左缘）。
                let row = Rect::new(0., 0., w, row_h());
                let (mx, my) = crate::newui::mouse_world();
                let hp = if ui.rect_to_global(row).contains(vec2(mx, my)) { 1. } else { 0. };
                crate::newui::group_row(
                    ui,
                    &th,
                    row,
                    hp,
                    gi == 0,
                    gi + 1 == n_items,
                    group_radius,
                    0.06,
                    Color::new(0., 0., 0., 0.),
                );
                gi += 1;
                $($b)*
                ui.dy(row_h());
                h += row_h();
            }}
        }
        let rr = right_rect(w);

        let data = get_data();
        let config = &data.config;
        item! {
            render_title(ui, tl!("item-chart-debug"), Some(tl!("item-chart-debug-sub")));
            render_switch(ui, rr, t, &mut self.chart_debug_btn, config.chart_debug);
        }
        item! {
            render_title(ui, tl!("item-touch-debug"), Some(tl!("item-touch-debug-sub")));
            render_switch(ui, rr, t, &mut self.touch_debug_btn, config.touch_debug);
        }
        item! {
            let style = match config.touch_marker_style {
                prpr::config::TouchMarkerStyle::Circle => tl!("touch-style-circle"),
                prpr::config::TouchMarkerStyle::HitFx => tl!("touch-style-hitfx"),
                prpr::config::TouchMarkerStyle::Avatar => tl!("touch-style-avatar"),
            };
            render_title(ui, tl!("item-touch-style"), Some(tl!("item-touch-style-sub")));
            self.touch_style_btn.render_text(ui, rr, t, style, 0.5, false);
        }
        item! {
            let color = match config.touch_marker_color {
                prpr::config::TouchMarkerColor::White => tl!("touch-color-white"),
                prpr::config::TouchMarkerColor::Red => tl!("touch-color-red"),
                prpr::config::TouchMarkerColor::Green => tl!("touch-color-green"),
                prpr::config::TouchMarkerColor::Blue => tl!("touch-color-blue"),
                prpr::config::TouchMarkerColor::Gold => tl!("touch-color-gold"),
                prpr::config::TouchMarkerColor::Purple => tl!("touch-color-purple"),
            };
            render_title(ui, tl!("item-touch-color"), Some(tl!("item-touch-color-sub")));
            self.touch_color_btn.render_text(ui, rr, t, color, 0.5, false);
        }
        item! {
            render_title(ui, tl!("item-touch-alpha"), Some(tl!("item-touch-alpha-sub")));
            self.touch_alpha_slider
                .render(ui, rr, t, config.touch_marker_alpha, format!("{:.0}%", config.touch_marker_alpha * 100.));
        }
        debug_assert_eq!(gi, n_items, "settings 分组：n_items 与实际 item! 条数不一致");
        (w, h)
    }
}

/// 把配置切成「可以被上传」的状态：
/// - 关掉所有不计分（UNRATED）或影响公平性的选项；
/// - 速度回到 1.0、判定窗口回到官方 ±80 / 160 / 220、late 补偿归零；
/// - 打开成绩上传。
///
/// 返回被关掉的 UNRATED Mod 数量（用于提示里显示）。
/// 直接吃现成的 `&mut Config`，避免在已经借了 config 的地方再 get_data_mut()。
fn apply_upload_profile_to(config: &mut Config) -> usize {
    use prpr::config::Mods;
    // 以前这里只清了 5 个 mod（AUTOPLAY / NO_SHADER / NO_COMBO_SCORE /
    // FULL_SCREEN_JUDGE / HEALTH_MODE），别的 mod（FLIP_X、NIGHTCORE、FADE_IN/OUT、
    // RAINBOW、INSTANT_DEATH_* …）还留着 —— 引擎照样判定"这张谱面被改过"，
    // 于是按了「一键切换到可上传配置」之后依然弹「不能上传成绩」。直接全清。
    let turned = config.mods.bits().count_ones() as usize;
    config.mods = Mods::empty();
    // 同步清掉旧的 autoplay 兼容字段，否则下次启动 init() 会把它又按回 true
    config.set_autoplay(false);
    // 音符映射 + 流速也复位（它们各自也会挡上传）
    config.note_map = Default::default();
    config.note_flow_speed = 1.;
    // 每张谱面还各自记了一份 mod（LocalChart.mods），不清的话单独某张谱面照样传不了
    for chart in &mut get_data_mut().charts {
        chart.mods = Mods::default();
    }
    config.speed = 1.;
    // 判定窗口回到官方默认；good / bad 用 None 表示"跟着 perfect 派生"
    config.judge_window = Config::DEFAULT_JUDGE_WINDOW;
    config.judge_window_good = None;
    config.judge_window_bad = None;
    config.late_leniency_ms = 0.;
    config.drag_protect = false;
    config.flick_protect = false;
    // 音符映射改了谱面数据本身，上传配置里必须清掉（否则成绩和原谱面对不上）。
    config.note_map = Default::default();
    // 下面两个是引擎判断「能否上传」的直接条件（见 prpr/src/scene/game.rs 里构造
    // record_data 那一段），之前一直漏着 —— 开了离线模式或键盘游玩时，就算 mod 全清、
    // 速度 1.0，照样不上传，而且只弹一句「不能上传」，极难排查。
    config.offline_mode = false;
    config.use_keyboard = false;
    config.upload_record = true;
    turned
}

/// 成绩上传协议。
///
/// `ask` = true：这是「要打开上传 / 一键配置」的流程，两个按钮，点了同意才继续；
/// `ask` = false：只是查看协议。
/// `apply_profile` = true：同意之后顺带切换成可上传配置。
fn show_upload_consent(ask: bool, apply_profile: bool) {
    let mut dialog = Dialog::plain(tl!("upload-consent-title"), tl!("upload-consent-text").into_owned());
    if ask {
        dialog = dialog
            .buttons(vec![tl!("upload-consent-deny").into_owned(), tl!("upload-consent-accept").into_owned()])
            .listener(move |_dialog, pos| {
                if pos == 1 {
                    let config = &mut get_data_mut().config;
                    config.upload_agreed = true;
                    let turned = if apply_profile { apply_upload_profile_to(config) } else { 0 };
                    config.upload_record = true;
                    let _ = save_data();
                    if apply_profile {
                        show_message(tl!("upload-profile-done", "mods" => turned.to_string()));
                    }
                }
                false
            });
    } else {
        dialog = dialog.buttons(vec![tl!("ok").into_owned()]);
    }
    dialog.show();
}
