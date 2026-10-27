//! **New UI** —— 按用户提供的设计稿实现的应用外壳与控件库。
//!
//! 设计稿来源：`New UI/初始界面.html`（开始界面）、`New UI/曲库.html`（曲库）。
//! 本模块只做两件事：
//!
//! 1. 把设计稿里的**颜色 / 尺寸 / 圆角 / 描边**翻译成单一真源（[`Theme`] / [`Metrics`]）；
//! 2. 提供**圆角矩形 / 文字 / 矢量图标**这些设计稿里反复出现的基础件。
//!
//! # 坐标系
//!
//! 引擎 UI 世界坐标里 **屏幕宽 = 2 个单位**，所以 `1 单位 = 视口宽 / 2 px`。
//! 设计稿是按「横屏 1440px 宽 / 竖屏 860px 宽」画的，本模块把设计稿像素
//! 等比映射进世界坐标：
//!
//! ```text
//! 长度：u(px) = px * 2 / ref_w      （ref_w = 1440 横屏 / 860 竖屏）
//! 字号：f(px) = px / (0.04 * ref_w) （引擎的字号换算见 ui/text.rs: get_scale）
//! ```
//!
//! 两者都**与真实视口尺寸无关**，所以任何分辨率下版式比例完全一致 —— 这正是
//! 「桌面应用」该有的表现（窗口大小变了，UI 不该跟着忽大忽小）。
//!
//! # 深浅主题
//!
//! 设计稿两套变量表都搬进来了（`.html` 里的 `:root` 与 `[data-theme="light"]`）。
//! 主题开关只改一个全局 bool（[`set_dark`]），页面每次渲染取一次 [`theme`]。

// 这是一个「设计稿工具箱」：图标、控件、配色都先备齐，页面用到哪件算哪件，
// 所以这里允许暂时没用上的条目存在。
#![allow(dead_code)]

use macroquad::prelude::*;
use prpr::{
    core::{Matrix, Point, BOLD_FONT},
    ext::{RectExt, SafeTexture},
    ui::{DrawText, IntoShading, RectButton, Shading, Ui},
};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};

// ============================================================
// 平面材质（iOS 18 风格）
// ============================================================
//
// 【2026-10-09 改版】用户要求把整体观感从「磨砂玻璃卡片」改成
// **平面风格 / iOS 18 风格**。原来那套配方（柔光层 + 半透明底板 + 雾面 +
// 顶部高光 + 细密颗粒）整条退役，换成 iOS 的 inset grouped 语言：
//
//   * 页面底 = `systemGroupedBackground`（浅 `#F2F2F7` / 深纯黑）；
//   * 卡片   = `secondarySystemGroupedBackground`（浅纯白 / 深 `#1C1C1E`），**不透明**；
//   * 层级靠「底色差 + 1px 分隔线」做，**不靠投影、不靠发光、不靠模糊**；
//   * 悬停 = 叠一层极淡的墨色（[`Theme::wash`]），不是提亮玻璃。
//
// 好消息是 `acrylic` / `glass_plate_w` / `glass_card` 这些**函数名和签名全部保留**，
// 只换内部实现 —— 所以全 App 几十个调用点一个都不用改，一次性变平面。
//
// ⚠️ 连带影响：浅色主题的页面底从「恒定深色」变成了浅灰 `#F2F2F7`，于是
// 「压在页面底上的写死白字」在浅色主题下会反过来失效。页面里凡是画在**页面底**
// （而不是卡片、也不是封面图）上的文字/图标，都必须走 `th.text*` / `theme::ink`；
// 只有压在**封面图 / 曲绘 / 视频**上的才继续写死白。

// ============================================================
// 主题
// ============================================================

/// 一整套设计稿颜色变量。字段名与设计稿的 CSS 变量一一对应（`--bg-alt` → `bg_alt`）。
#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub bg_alt: Color,
    pub bg_elev: Color,
    pub bg_hover: Color,
    pub border: Color,
    pub border_soft: Color,

    pub text: Color,
    pub text_hi: Color,
    pub text_dim: Color,
    pub text_mute: Color,

    pub accent: Color,
    pub accent_hi: Color,
    pub glow: Color,

    pub blue: Color,
    pub teal: Color,
    pub yellow: Color,
    pub orange: Color,
    pub red: Color,

    /// 谱面难度色（设计稿 `--ez / --hd / --in / --at`）。
    pub ez: Color,
    pub hd: Color,
    pub insane: Color,
    pub at: Color,

    /// 设计稿里「是深色主题吗」。
    pub dark: bool,
}

/// 谱面难度显示分类。
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DiffKind {
    /// EZ / HD / IN：霓虹色
    Neon(Color),
    /// AT 与未知难度：黑字（配白晕保证可读）
    Black,
    /// SP：逐字流动彩虹
    Rainbow,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::new(r as f32 / 255., g as f32 / 255., b as f32 / 255., 1.)
}

pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::new(r as f32 / 255., g as f32 / 255., b as f32 / 255., a)
}

impl Theme {
    pub const DARK: Theme = Theme {
        // systemGroupedBackground（深）—— 纯黑
        bg: rgb(0x00, 0x00, 0x00),
        bg_alt: rgb(0x1c, 0x1c, 0x1e),
        // secondarySystemGroupedBackground（深）—— **不透明实色卡片**
        bg_elev: rgb(0x1c, 0x1c, 0x1e),
        // systemGray5（深）
        bg_hover: rgb(0x2c, 0x2c, 0x2e),
        // separator / separator（弱）
        border: rgba(84, 84, 88, 0.65),
        border_soft: rgba(84, 84, 88, 0.36),

        // label / secondaryLabel / tertiaryLabel（深）
        text: rgb(0xff, 0xff, 0xff),
        text_hi: rgb(0xff, 0xff, 0xff),
        text_dim: rgba(235, 235, 245, 0.60),
        text_mute: rgba(235, 235, 245, 0.30),

        // systemBlue（深）
        accent: rgb(0x0a, 0x84, 0xff),
        accent_hi: rgb(0x40, 0x9c, 0xff),
        glow: rgb(0x0a, 0x84, 0xff),

        blue: rgb(0x0a, 0x84, 0xff),
        teal: rgb(0x40, 0xc8, 0xe0),
        yellow: rgb(0xff, 0xd6, 0x0a),
        orange: rgb(0xff, 0x9f, 0x0a),
        red: rgb(0xff, 0x45, 0x3a),

        // 难度色（保留 EZ 绿 / HD 蓝 / IN 红 / AT 黑 / SP 彩虹 的既定语义，
        // 色值对齐 iOS 系统色）
        ez: rgb(0x30, 0xd1, 0x58),
        hd: rgb(0x0a, 0x84, 0xff),
        insane: rgb(0xff, 0x45, 0x3a),
        at: rgb(0x0a, 0x0c, 0x10),

        dark: true,
    };

    /// **iOS 18 浅色（Light）**：`#F2F2F7` 分组底 + 白色卡片 + 系统蓝 `#007AFF`。
    ///
    /// 这是现在的默认主题（`Config.ui_dark` 默认 false）。
    pub const LIGHT: Theme = Theme {
        // systemGroupedBackground（浅）
        bg: rgb(0xf2, 0xf2, 0xf7),
        bg_alt: rgb(0xff, 0xff, 0xff),
        // secondarySystemGroupedBackground（浅）—— 纯白实色卡片
        bg_elev: rgb(0xff, 0xff, 0xff),
        // systemGray5（浅）
        bg_hover: rgb(0xe5, 0xe5, 0xea),
        // separator / separator（弱）
        border: rgba(60, 60, 67, 0.29),
        border_soft: rgba(60, 60, 67, 0.18),

        // label / secondaryLabel / tertiaryLabel（浅）
        text: rgb(0x00, 0x00, 0x00),
        text_hi: rgb(0x00, 0x00, 0x00),
        text_dim: rgba(60, 60, 67, 0.60),
        text_mute: rgba(60, 60, 67, 0.30),

        // systemBlue（浅）
        accent: rgb(0x00, 0x7a, 0xff),
        accent_hi: rgb(0x33, 0x96, 0xff),
        glow: rgb(0x00, 0x7a, 0xff),

        blue: rgb(0x00, 0x7a, 0xff),
        teal: rgb(0x30, 0xb0, 0xc7),
        // systemYellow 在白底上做文字太浅，压深一档保证可读
        yellow: rgb(0xf0, 0xa8, 0x00),
        orange: rgb(0xff, 0x95, 0x00),
        red: rgb(0xff, 0x3b, 0x30),

        // 难度色（同深色主题的语义，取浅色版系统色）
        ez: rgb(0x34, 0xc7, 0x59),
        hd: rgb(0x00, 0x7a, 0xff),
        insane: rgb(0xff, 0x3b, 0x30),
        at: rgb(0x1c, 0x1c, 0x1e),

        dark: false,
    };

    /// 谱面难度分类：霓虹色（EZ/HD/IN）/ 黑（AT 与未知）/ 彩虹（SP）。
    pub fn diff_kind(&self, level: &str) -> DiffKind {
        let up = level.to_uppercase();
        if up.contains("SP") {
            DiffKind::Rainbow
        } else if up.contains("EZ") {
            DiffKind::Neon(self.ez)
        } else if up.contains("HD") {
            DiffKind::Neon(self.hd)
        } else if up.contains("IN") {
            DiffKind::Neon(self.insane)
        } else {
            DiffKind::Black
        }
    }

    /// 按谱面难度字符串取色（"IN Lv.14" / "Lv.78" → 难度色；AT / 认不出 → 黑）。
    pub fn diff_color(&self, level: &str) -> Color {
        match self.diff_kind(level) {
            DiffKind::Neon(c) => c,
            DiffKind::Black => self.at,
            DiffKind::Rainbow => self.glow,
        }
    }

    /// 保留 RGB、替换 alpha。
    pub fn with_a(&self, c: Color, a: f32) -> Color {
        Color { a, ..c }
    }

    pub fn black(&self, a: f32) -> Color {
        Color::new(0., 0., 0., a)
    }

    pub fn white(&self, a: f32) -> Color {
        Color::new(1., 1., 1., a)
    }

    /// 设计稿里「深色主题加白 / 浅色主题加黑」的蒙版色（悬停、边框、分隔线都是这个套路）。
    pub fn wash(&self, a: f32) -> Color {
        if self.dark {
            self.white(a)
        } else {
            self.black(a)
        }
    }

    /// 压在图片/深色面板上的文字，用白色透明度。
    pub fn on_image(&self, a: f32) -> Color {
        Color::new(1., 1., 1., a)
    }
}

static DARK: AtomicBool = AtomicBool::new(false);

/// 设定深浅主题（Windows 11 浅色为默认；标题栏右上角的太阳/月亮按钮切换）。
pub fn set_dark(v: bool) {
    DARK.store(v, Ordering::Relaxed);
    prpr::ui::theme::set_light(!v);
}

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// 当前主题：深色 = 原 New UI 深色；浅色 = Windows 11 Light 亚克力色板。
pub fn theme() -> Theme {
    if is_dark() {
        Theme::DARK
    } else {
        Theme::LIGHT
    }
}

// ============================================================
// 尺寸 / 版式
// ============================================================

/// 设计稿参考宽度：横屏 1440px、竖屏 860px。
pub const REF_WIDE: f32 = 1440.;
pub const REF_NARROW: f32 = 860.;

/// 设计稿里的固定尺寸（px）。改版式先改这里。
/// 是否画顶部的「条状任务栏」。
///
/// 用户要求去掉它：窗口本身已经有系统标题栏（最小化/最大化/关闭都在上面），
/// 再叠一条 36px 的自绘条纯属浪费竖向空间。
pub const SHOW_TITLEBAR: bool = false;

/// 是否画底部的「状态栏」。
///
/// 用户要求去掉它（和顶栏同理）：玩家名 / RKS / 版本号 / 编码 / 在线状态这些
/// 都只是装饰信息，却常年占着一条 30px 的横条。关掉后 `Metrics::body()`
/// 直接把内容区放到窗口底缘，页面能多用 30px 竖向空间。
///
/// ⚠️ 原来**只有**状态栏最右侧那个太阳/月亮按钮能切深浅主题，
/// 所以主题开关已经挪到「设置 → 通用 → 深色主题」（`page/settings.rs`）。
pub const SHOW_STATUSBAR: bool = false;

pub mod px {
    pub const TITLEBAR_H: f32 = 36.;
    pub const STATUSBAR_H: f32 = 30.;

    // ⚠️ 这里原来还有 `ACTIVITY_W / ACT_BTN_W / ACT_BTN_H / ACT_ICON` 四个常量，
    // 供**左侧全局竖向图标活动栏**使用。用户要求把那条栏整条删掉，常量随之作废，
    // 一起删除，避免以后有人照着它们又把栏画回来。

    pub const MENU_ITEM_H: f32 = 58.;
    pub const MENU_GAP: f32 = 10.;
    pub const MENU_ICON: f32 = 20.;
    // 圆角按 iOS 的连续圆角观感整体调大一档（原来那套是给玻璃卡片的硬圆角）
    pub const MENU_RADIUS: f32 = 10.;

    pub const CARD_RADIUS: f32 = 12.;
    pub const CHIP_RADIUS: f32 = 6.;
    pub const PANEL_RADIUS: f32 = 12.;
}

/// 缩放系数的上下限。
///
/// 设计稿是按 `ref_w` 宽画的，但**不能**一路等比缩放：窗口小了字号会缩水，
/// 窗口大了版面又会被撑得夸张，所以夹在 `[MIN, MAX]` 之间。
/// 移动端（DPR ≥ 1.5）另有放大底线。
pub const SCALE_MIN: f32 = 1.0;
pub const SCALE_MAX: f32 = 1.8;

/// 一帧的版式换算。由 [`Metrics::new`] 从 `ui` 推出来。
#[derive(Clone, Copy)]
pub struct Metrics {
    /// 视口宽度（屏幕像素）。
    pub w: f32,
    /// 视口高度（屏幕像素）。
    pub h: f32,
    /// 设计稿 px → 屏幕 px 的倍率（已按 [`SCALE_MIN`]/[`SCALE_MAX`] 夹过）。
    pub k: f32,
    /// 竖屏 / 窄屏：设计稿里 `max-width:860px` 那档（隐藏活动栏、立绘做背景）。
    pub compact: bool,
    /// 设计稿里 `max-width:620px` 那档（进一步压缩）。
    pub tiny: bool,
}

impl Metrics {
    pub fn new(ui: &Ui) -> Self {
        let w = (ui.viewport.2 as f32).max(1.);
        let h = (ui.viewport.3 as f32).max(1.);
        let aspect = if h > 0. { w / h } else { 1. };
        let compact = aspect < 1.15;
        let ref_w = if compact { REF_NARROW } else { REF_WIDE };

        // 移动端整体放大：viewport 是**逻辑像素**（macroquad 已除 DPI），
        // 手机上 390~860 逻辑宽跟 1440 参考宽一比，字号只有桌面的一半出头。
        // DPR ≥ 1.5 的设备（手机 / 高缩放的桌面）抬起缩放底线，让全部 UI 变大。
        // 这个 fork 版 macroquad 没有公开 dpi_scale()，用「GL 视口物理宽 / 逻辑宽」自己算。
        let vp = prpr::ext::get_viewport();
        let dpi = ((vp.2.max(1)) as f32 / w).max(1.);
        let mobile = dpi >= 1.5f32;
        let (k_min, k_max) = if mobile { (1.5, 2.2) } else { (SCALE_MIN, SCALE_MAX) };
        let k = (w / ref_w).clamp(k_min, k_max);
        Self {
            w,
            h,
            k,
            compact,
            tiny: aspect < 0.75,
        }
    }

    /// 设计稿 px → 世界坐标长度。
    #[inline]
    pub fn u(&self, p: f32) -> f32 {
        p * self.k * 2. / self.w
    }

    /// 设计稿 px → 字号（喂给 `ui.text(..).size(..)`）。
    #[inline]
    pub fn f(&self, p: f32) -> f32 {
        p * self.k / (0.04 * self.w)
    }

    /// 顶栏矩形（整宽）。[`SHOW_TITLEBAR`] 为 false 时高度是 0。
    pub fn titlebar(&self, ui: &Ui) -> Rect {
        if SHOW_TITLEBAR {
            Rect::new(-1., -ui.top, 2., self.u(px::TITLEBAR_H))
        } else {
            Rect::new(-1., -ui.top, 2., 0.)
        }
    }

    /// 底栏矩形（整宽）。[`SHOW_STATUSBAR`] 为 false 时高度是 0，贴着窗口下缘，
    /// 这样 [`Metrics::body`] 算出来的内容区自然铺到最底。
    pub fn statusbar(&self, ui: &Ui) -> Rect {
        if SHOW_STATUSBAR {
            let h = self.u(px::STATUSBAR_H);
            Rect::new(-1., ui.top - h, 2., h)
        } else {
            Rect::new(-1., ui.top, 2., 0.)
        }
    }

    /// 左侧活动栏矩形。
    ///
    /// **已废弃**：用户要求删掉左侧那条全局竖向图标导航栏（⚡/♪/💬/📊/☇/👤），
    /// 整条栏的绘制与命中都已移除，这里恒返回 `None`。
    /// 保留这个函数只是为了让「内容区 = 整个可视区」这件事有一个唯一入口。
    pub fn activity(&self, _ui: &Ui) -> Option<Rect> {
        None
    }

    /// 外壳包住的中间区域（标题栏与状态栏之间）。
    pub fn body(&self, ui: &Ui) -> Rect {
        let tb = self.titlebar(ui);
        let sb = self.statusbar(ui);
        Rect::new(-1., tb.bottom(), 2., sb.y - tb.bottom())
    }

    /// 内容区。左侧活动栏已删除，内容区直接等于 [`Metrics::body`]（**整宽**）。
    ///
    /// 删栏腾出来的 48 设计稿 px 一律还给内容：页面用 `m.content(ui)` 拿到的
    /// 左缘直接贴到窗口左缘，配合各页自身的边距实现「整体左移」。
    pub fn content(&self, ui: &Ui) -> Rect {
        self.body(ui)
    }
}

// ============================================================
// 矢量图标
// ============================================================

/// 24×24 视口里的一段路径（与设计稿 SVG 的 `d` 一一对应）。
#[derive(Clone, Copy)]
pub enum Seg {
    M(f32, f32),
    L(f32, f32),
    Q(f32, f32, f32, f32),
    C(f32, f32, f32, f32, f32, f32),
    /// 圆（用四段三次贝塞尔逼近）。
    Circle(f32, f32, f32),
    Z,
}

/// 图标 = 若干条子路径 + 填充/描边方式。
pub struct IconDef {
    pub subpaths: &'static [&'static [Seg]],
    pub fill: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Logo,
    Sun,
    Moon,
    Minimize,
    Maximize,
    Close,
    /// 闪电 —— 活动栏「开始」
    Bolt,
    /// 对话气泡 —— 消息
    Chat,
    /// 柱状图 —— 排行 / RKS
    Chart,
    /// 齿轮 —— 设置
    Gear,
    /// 人像 —— 账户
    User,
    /// 播放三角
    Play,
    /// 圆圈 i —— 关于
    Info,
    /// 退出
    Logout,
    /// 音符 —— 上次游玩 / 曲库
    Music,
    Search,
    /// 左尖括号 —— 返回
    ChevronLeft,
    /// 右尖括号 —— 前进
    ChevronRight,
    /// 上下箭头 —— 排序
    Sort,
    /// 波形 —— BPM
    Wave,
    /// 时钟 —— 时长
    Clock,
    /// 层叠 —— 版本
    Layers,
    /// 三条横线 —— 编码
    List,
    Star,
    Heart,
    Cloud,
    Plus,
    Menu,
    Download,
    Trophy,
    Edit,
    Delete,
    Filter,
    X,
}

const I_BOLT: &[Seg] = &[Seg::M(13., 2.), Seg::L(4.5, 13.5), Seg::L(11., 13.5), Seg::L(10., 22.), Seg::L(18.5, 10.5), Seg::L(12., 10.5), Seg::Z];
const I_CHAT: &[Seg] = &[
    Seg::M(20.5, 12.),
    Seg::C(20.5, 16.42, 16.7, 20., 12., 20.),
    Seg::C(10.72, 20., 9.51, 19.62, 8.9, 19.1),
    Seg::L(4., 20.5),
    Seg::L(5.4, 15.8),
    Seg::C(4.83, 14.65, 4.5, 13.36, 4.5, 12.),
    Seg::C(4.5, 7.58, 8.3, 4., 12., 4.),
    Seg::C(15.7, 4., 20.5, 7.58, 20.5, 12.),
    Seg::Z,
];
const I_CHART: &[Seg] = &[Seg::M(7., 20.), Seg::L(7., 10.)];
const I_CHART2: &[Seg] = &[Seg::M(12., 20.), Seg::L(12., 4.)];
const I_CHART3: &[Seg] = &[Seg::M(17., 20.), Seg::L(17., 13.)];
const I_GEAR_RING: &[Seg] = &[Seg::Circle(12., 12., 3.)];
const I_GEAR_SPOKES: &[Seg] = &[
    Seg::M(12., 2.5),
    Seg::L(12., 4.7),
    Seg::M(12., 19.3),
    Seg::L(12., 21.5),
    Seg::M(4.2, 4.2),
    Seg::L(5.8, 5.8),
    Seg::M(18.2, 18.2),
    Seg::L(19.8, 19.8),
    Seg::M(2.5, 12.),
    Seg::L(4.7, 12.),
    Seg::M(19.3, 12.),
    Seg::L(21.5, 12.),
    Seg::M(4.2, 19.8),
    Seg::L(5.8, 18.2),
    Seg::M(18.2, 5.8),
    Seg::L(19.8, 4.2),
];
const I_USER_HEAD: &[Seg] = &[Seg::Circle(12., 8.5, 3.6)];
const I_USER_BODY: &[Seg] = &[
    Seg::M(4.6, 20.),
    Seg::C(4.6, 15.8, 7.9, 12.4, 12., 12.4),
    Seg::C(16.1, 12.4, 19.4, 15.8, 19.4, 20.),
];
const I_PLAY: &[Seg] = &[Seg::M(6., 4.5), Seg::L(6., 19.5), Seg::L(19., 12.), Seg::Z];
const I_INFO: &[Seg] = &[Seg::Circle(12., 12., 9.)];
const I_INFO2: &[Seg] = &[Seg::M(12., 11.), Seg::L(12., 16.5), Seg::M(12., 7.8), Seg::L(12.01, 7.8)];
const I_LOGOUT: &[Seg] = &[
    Seg::M(15., 4.),
    Seg::L(18.5, 4.),
    Seg::C(19.33, 4., 20., 4.67, 20., 5.5),
    Seg::L(20., 18.5),
    Seg::C(20., 19.33, 19.33, 20., 18.5, 20.),
    Seg::L(15., 20.),
];
const I_LOGOUT2: &[Seg] = &[Seg::M(10., 8.), Seg::L(6., 12.), Seg::L(10., 16.), Seg::M(6., 12.), Seg::L(15., 12.)];
const I_MUSIC: &[Seg] = &[Seg::M(9., 18.), Seg::L(9., 5.), Seg::L(19., 3.), Seg::L(19., 16.)];
const I_MUSIC_C1: &[Seg] = &[Seg::Circle(6.5, 18., 2.6)];
const I_MUSIC_C2: &[Seg] = &[Seg::Circle(16.5, 16., 2.6)];
const I_SEARCH: &[Seg] = &[Seg::Circle(11., 11., 6.5)];
const I_SEARCH2: &[Seg] = &[Seg::M(16., 16.), Seg::L(20.5, 20.5)];
const I_CHEV_L: &[Seg] = &[Seg::M(15., 4.5), Seg::L(7.5, 12.), Seg::L(15., 19.5)];
const I_CHEV_R: &[Seg] = &[Seg::M(9., 4.5), Seg::L(16.5, 12.), Seg::L(9., 19.5)];
const I_X: &[Seg] = &[Seg::M(6., 6.), Seg::L(18., 18.), Seg::M(18., 6.), Seg::L(6., 18.)];
const I_WAVE: &[Seg] = &[
    Seg::M(3., 12.),
    Seg::L(6., 12.),
    Seg::L(8.5, 6.),
    Seg::L(12., 18.),
    Seg::L(15., 9.),
    Seg::L(17., 12.),
    Seg::L(21., 12.),
];
const I_CLOCK: &[Seg] = &[Seg::Circle(12., 12., 8.5)];
const I_CLOCK2: &[Seg] = &[Seg::M(12., 7.5), Seg::L(12., 12.), Seg::L(15., 14.)];
const I_LAYERS: &[Seg] = &[Seg::M(12., 3.), Seg::L(3., 7.5), Seg::L(12., 12.), Seg::L(21., 7.5), Seg::Z];
const I_LAYERS2: &[Seg] = &[Seg::M(3., 12.5), Seg::L(12., 17.), Seg::L(21., 12.5)];
const I_LAYERS3: &[Seg] = &[Seg::M(3., 17.), Seg::L(12., 21.5), Seg::L(21., 17.)];
const I_LIST: &[Seg] = &[Seg::M(4., 7.), Seg::L(20., 7.), Seg::M(4., 12.), Seg::L(20., 12.), Seg::M(4., 17.), Seg::L(14., 17.)];
const I_STAR: &[Seg] = &[
    Seg::M(12., 2.6),
    Seg::L(14.9, 8.9),
    Seg::L(21.6, 9.8),
    Seg::L(16.7, 14.5),
    Seg::L(17.9, 21.3),
    Seg::L(12., 18.),
    Seg::L(6.1, 21.3),
    Seg::L(7.3, 14.5),
    Seg::L(2.4, 9.8),
    Seg::L(9.1, 8.9),
    Seg::Z,
];
const I_HEART: &[Seg] = &[
    Seg::M(12., 20.6),
    Seg::C(3., 15., 2.4, 9.2, 5.2, 6.5),
    Seg::C(8., 3.8, 11.4, 5.2, 12., 8.),
    Seg::C(12.6, 5.2, 16., 3.8, 18.8, 6.5),
    Seg::C(21.6, 9.2, 21., 15., 12., 20.6),
    Seg::Z,
];
const I_CLOUD: &[Seg] = &[
    Seg::M(7., 18.),
    Seg::C(3.7, 18., 2., 15.6, 2., 13.),
    Seg::C(2., 10.4, 4.1, 8.6, 6.4, 9.),
    Seg::C(7., 5.6, 9.8, 3.5, 12.8, 4.4),
    Seg::C(15.3, 5.1, 16.7, 7.1, 17., 9.2),
    Seg::C(20., 9.2, 22., 11.3, 22., 13.6),
    Seg::C(22., 16.1, 20., 18., 17.5, 18.),
    Seg::Z,
];
const I_PLUS: &[Seg] = &[Seg::M(12., 5.), Seg::L(12., 19.), Seg::M(5., 12.), Seg::L(19., 12.)];
const I_MENU: &[Seg] = &[Seg::M(4., 7.), Seg::L(20., 7.), Seg::M(4., 12.), Seg::L(20., 12.), Seg::M(4., 17.), Seg::L(20., 17.)];
const I_DOWNLOAD: &[Seg] = &[
    Seg::M(12., 3.5),
    Seg::L(12., 15.),
    Seg::M(7., 10.),
    Seg::L(12., 15.),
    Seg::L(17., 10.),
    Seg::M(4.5, 19.5),
    Seg::L(19.5, 19.5),
];
const I_TROPHY: &[Seg] = &[
    Seg::M(8., 4.),
    Seg::L(16., 4.),
    Seg::L(16., 10.),
    Seg::C(16., 12.8, 14.2, 15., 12., 15.),
    Seg::C(9.8, 15., 8., 12.8, 8., 10.),
    Seg::Z,
];
const I_TROPHY2: &[Seg] = &[
    Seg::M(12., 15.),
    Seg::L(12., 19.),
    Seg::M(8., 20.),
    Seg::L(16., 20.),
    Seg::M(8., 6.),
    Seg::L(5., 6.),
    Seg::L(5., 8.5),
    Seg::C(5., 10.4, 6.2, 11.4, 8., 11.6),
    Seg::M(16., 6.),
    Seg::L(19., 6.),
    Seg::L(19., 8.5),
    Seg::C(19., 10.4, 17.8, 11.4, 16., 11.6),
];
const I_EDIT: &[Seg] = &[
    Seg::M(4., 20.),
    Seg::L(4.6, 15.6),
    Seg::L(16.4, 3.8),
    Seg::C(17.4, 2.8, 19., 2.8, 20., 3.8),
    Seg::C(21., 4.8, 21., 6.4, 20., 7.4),
    Seg::L(8.2, 19.2),
    Seg::Z,
];
const I_DELETE: &[Seg] = &[
    Seg::M(4., 6.5),
    Seg::L(20., 6.5),
    Seg::M(9.5, 6.5),
    Seg::L(9.5, 4.5),
    Seg::L(14.5, 4.5),
    Seg::L(14.5, 6.5),
    Seg::M(6.5, 6.5),
    Seg::L(7.4, 20.),
    Seg::L(16.6, 20.),
    Seg::L(17.5, 6.5),
    Seg::M(10., 10.),
    Seg::L(10.4, 16.5),
    Seg::M(14., 10.),
    Seg::L(13.6, 16.5),
];
const I_FILTER: &[Seg] = &[
    Seg::M(3.5, 5.),
    Seg::L(20.5, 5.),
    Seg::L(14., 12.5),
    Seg::L(14., 20.),
    Seg::L(10., 17.5),
    Seg::L(10., 12.5),
    Seg::Z,
];
const I_SORT: &[Seg] = &[
    Seg::M(8., 5.),
    Seg::L(8., 19.),
    Seg::M(4.5, 8.5),
    Seg::L(8., 5.),
    Seg::L(11.5, 8.5),
    Seg::M(16., 19.),
    Seg::L(16., 5.),
    Seg::M(12.5, 15.5),
    Seg::L(16., 19.),
    Seg::L(19.5, 15.5),
];
const SUN_RAYS: &[Seg] = &[
    Seg::M(12., 2.5),
    Seg::L(12., 4.7),
    Seg::M(12., 19.3),
    Seg::L(12., 21.5),
    Seg::M(4.2, 4.2),
    Seg::L(5.8, 5.8),
    Seg::M(18.2, 18.2),
    Seg::L(19.8, 19.8),
    Seg::M(2.5, 12.),
    Seg::L(4.7, 12.),
    Seg::M(19.3, 12.),
    Seg::L(21.5, 12.),
    Seg::M(4.2, 19.8),
    Seg::L(5.8, 18.2),
    Seg::M(18.2, 5.8),
    Seg::L(19.8, 4.2),
];
const SUN_RING: &[Seg] = &[Seg::Circle(12., 12., 4.2)];
const MIN_BAR: &[Seg] = &[Seg::M(1., 5.5), Seg::L(11., 5.5)];
const MAX_BOX: &[Seg] = &[Seg::M(1.5, 1.5), Seg::L(10.5, 1.5), Seg::L(10.5, 10.5), Seg::L(1.5, 10.5), Seg::Z];
const CLOSE_X: &[Seg] = &[Seg::M(2., 2.), Seg::L(10., 10.), Seg::M(10., 2.), Seg::L(2., 10.)];
const MOON_BODY: &[Seg] = &[Seg::Circle(12., 12., 8.5)];

pub fn icon_def(icon: Icon) -> IconDef {
    match icon {
        Icon::Logo => IconDef {
            subpaths: &[I_LAYERS, I_LAYERS2, I_LAYERS3],
            fill: false,
        },
        Icon::Sun => IconDef {
            subpaths: &[SUN_RING, SUN_RAYS],
            fill: false,
        },
        // 月亮用「圆 + 挖一块」画（见 `draw_icon`），这里给个占位圆。
        Icon::Moon => IconDef {
            subpaths: &[MOON_BODY],
            fill: true,
        },
        Icon::Minimize => IconDef {
            subpaths: &[MIN_BAR],
            fill: true,
        },
        Icon::Maximize => IconDef {
            subpaths: &[MAX_BOX],
            fill: false,
        },
        Icon::Close => IconDef {
            subpaths: &[CLOSE_X],
            fill: false,
        },
        Icon::Bolt => IconDef {
            subpaths: &[I_BOLT],
            fill: false,
        },
        Icon::Chat => IconDef {
            subpaths: &[I_CHAT],
            fill: false,
        },
        Icon::Chart => IconDef {
            subpaths: &[I_CHART, I_CHART2, I_CHART3],
            fill: false,
        },
        Icon::Gear => IconDef {
            subpaths: &[I_GEAR_RING, I_GEAR_SPOKES],
            fill: false,
        },
        Icon::User => IconDef {
            subpaths: &[I_USER_HEAD, I_USER_BODY],
            fill: false,
        },
        Icon::Play => IconDef {
            subpaths: &[I_PLAY],
            fill: true,
        },
        Icon::Info => IconDef {
            subpaths: &[I_INFO, I_INFO2],
            fill: false,
        },
        Icon::Logout => IconDef {
            subpaths: &[I_LOGOUT, I_LOGOUT2],
            fill: false,
        },
        Icon::Music => IconDef {
            subpaths: &[I_MUSIC, I_MUSIC_C1, I_MUSIC_C2],
            fill: false,
        },
        Icon::Search => IconDef {
            subpaths: &[I_SEARCH, I_SEARCH2],
            fill: false,
        },
        Icon::ChevronLeft => IconDef {
            subpaths: &[I_CHEV_L],
            fill: false,
        },
        Icon::ChevronRight => IconDef {
            subpaths: &[I_CHEV_R],
            fill: false,
        },
        Icon::Sort => IconDef {
            subpaths: &[I_SORT],
            fill: false,
        },
        Icon::Wave => IconDef {
            subpaths: &[I_WAVE],
            fill: false,
        },
        Icon::Clock => IconDef {
            subpaths: &[I_CLOCK, I_CLOCK2],
            fill: false,
        },
        Icon::Layers => IconDef {
            subpaths: &[I_LAYERS, I_LAYERS2, I_LAYERS3],
            fill: false,
        },
        Icon::List => IconDef {
            subpaths: &[I_LIST],
            fill: false,
        },
        Icon::Star => IconDef {
            subpaths: &[I_STAR],
            fill: true,
        },
        Icon::Heart => IconDef {
            subpaths: &[I_HEART],
            fill: true,
        },
        Icon::Cloud => IconDef {
            subpaths: &[I_CLOUD],
            fill: false,
        },
        Icon::Plus => IconDef {
            subpaths: &[I_PLUS],
            fill: false,
        },
        Icon::Menu => IconDef {
            subpaths: &[I_MENU],
            fill: false,
        },
        Icon::Download => IconDef {
            subpaths: &[I_DOWNLOAD],
            fill: false,
        },
        Icon::Trophy => IconDef {
            subpaths: &[I_TROPHY, I_TROPHY2],
            fill: false,
        },
        Icon::Edit => IconDef {
            subpaths: &[I_EDIT],
            fill: false,
        },
        Icon::Delete => IconDef {
            subpaths: &[I_DELETE],
            fill: false,
        },
        Icon::Filter => IconDef {
            subpaths: &[I_FILTER],
            fill: false,
        },
        Icon::X => IconDef {
            subpaths: &[I_X],
            fill: false,
        },
    }
}

/// 四段三次贝塞尔逼近一个圆时用的控制点系数。
const KAPPA: f32 = 0.552_284_75;

/// 把 24×24 设计稿坐标的图标路径铺进 `r`（等比、居中）。
pub fn build_icon_path(segs: &[Seg], r: Rect) -> lyon::path::Path {
    let s = (r.w / 24.).min(r.h / 24.);
    let ox = r.x + (r.w - 24. * s) * 0.5;
    let oy = r.y + (r.h - 24. * s) * 0.5;
    let map = |x: f32, y: f32| lyon::math::point(ox + x * s, oy + y * s);

    let mut b = lyon::path::Path::builder();
    let mut open = false;
    for seg in segs {
        match *seg {
            Seg::M(x, y) => {
                if open {
                    b.end(false);
                }
                b.begin(map(x, y));
                open = true;
            }
            Seg::L(x, y) => {
                b.line_to(map(x, y));
            }
            Seg::Q(cx, cy, x, y) => {
                b.quadratic_bezier_to(map(cx, cy), map(x, y));
            }
            Seg::C(c1x, c1y, c2x, c2y, x, y) => {
                b.cubic_bezier_to(map(c1x, c1y), map(c2x, c2y), map(x, y));
            }
            Seg::Circle(cx, cy, rad) => {
                if open {
                    b.end(false);
                }
                let k = rad * KAPPA;
                b.begin(map(cx, cy - rad));
                b.cubic_bezier_to(map(cx + k, cy - rad), map(cx + rad, cy - k), map(cx + rad, cy));
                b.cubic_bezier_to(map(cx + rad, cy + k), map(cx + k, cy + rad), map(cx, cy + rad));
                b.cubic_bezier_to(map(cx - k, cy + rad), map(cx - rad, cy + k), map(cx - rad, cy));
                b.cubic_bezier_to(map(cx - rad, cy - k), map(cx - k, cy - rad), map(cx, cy - rad));
                b.end(true);
                open = false;
            }
            Seg::Z => {
                b.end(true);
                open = false;
            }
        }
    }
    if open {
        b.end(false);
    }
    b.build()
}

/// 画一个图标。`stroke_px` 是设计稿 24×24 视口下的线宽（填充图标忽略）。
pub fn draw_icon(ui: &mut Ui, _m: &Metrics, icon: Icon, r: Rect, color: Color, stroke_px: f32) {
    if icon == Icon::Moon {
        draw_moon(ui, r, color, theme().bg_alt);
        return;
    }
    let def = icon_def(icon);
    let s = (r.w / 24.).min(r.h / 24.);
    if def.fill {
        for sp in def.subpaths {
            ui.fill_path(&build_icon_path(sp, r), color);
        }
    } else {
        ui.stroke_options.start_cap = lyon::path::LineCap::Round;
        ui.stroke_options.end_cap = lyon::path::LineCap::Round;
        ui.stroke_options.line_join = lyon::path::LineJoin::Round;
        let lw = stroke_px * s;
        for sp in def.subpaths {
            ui.stroke_path(&build_icon_path(sp, r), lw, color);
        }
    }
}

/// 月亮：画一个实心圆，再用背景色挖掉一块（设计稿的 `icon-moon`）。
fn draw_moon(ui: &mut Ui, r: Rect, color: Color, bg: Color) {
    let cx = r.x + r.w * 0.5;
    let cy = r.y + r.h * 0.5;
    let rad = r.w * 0.38;
    ui.fill_circle(cx, cy, rad, color);
    ui.fill_circle(cx + rad * 0.72, cy - rad * 0.42, rad * 0.92, bg);
}

// ============================================================
// 基础控件
// ============================================================

/// 圆角实心矩形。
pub fn rounded(ui: &mut Ui, r: Rect, radius: f32, color: Color) {
    if radius <= 1e-6 {
        ui.fill_rect(r, color);
    } else {
        ui.fill_path(&r.rounded(radius), color);
    }
}

/// 圆角描边。
pub fn rounded_stroke(ui: &mut Ui, r: Rect, radius: f32, width: f32, color: Color) {
    ui.stroke_path(&r.rounded(radius.max(1e-6)), width, color);
}

// ============================================================
// 斜切（skew）—— Phigros 设置页的「平行四边形」语言
// ============================================================
//
// 用户指定的参考版式（`D:\Phigros\kaichang\Setting.html`）里，标签页 / 滑块轨道 /
// 勾选框 / 箭头框全都用 CSS `transform: skewX(-15deg)` 画成**平行四边形**。
// 这里把那条规则翻成 lyon 路径：左右边是斜线，顶边与底边水平错开。
//
// 以**矩形中心**为原点斜切 —— 与 CSS 默认 `transform-origin: center` 一致，
// 所以斜切后的图形**中心不变**，文字按未斜切的矩形居中就正好落在视觉中心。

/// 平行四边形路径。
///
/// `skew` = **顶边相对底边**的水平位移（世界坐标，正 = 顶边右移，即 CSS 的
/// `skewX(-15deg)` 观感）。上下各偏 `skew / 2`。
pub fn skew_path(r: Rect, skew: f32) -> lyon::path::Path {
    use lyon::math::point;
    let d = skew * 0.5;
    let (xl, xr, yt, yb) = (r.x, r.right(), r.y, r.bottom());
    let mut p = lyon::path::Path::builder();
    p.begin(point(xl + d, yt));
    p.line_to(point(xr + d, yt));
    p.line_to(point(xr - d, yb));
    p.line_to(point(xl - d, yb));
    p.end(true);
    p.build()
}

/// 把一点按同一条斜切规则变形。
///
/// `cy` = 斜切中心的行，`k` = 每单位高度的水平位移（= `-skew / 高度`）。
pub fn skew_pt(p: (f32, f32), cy: f32, k: f32) -> (f32, f32) {
    (p.0 + (p.1 - cy) * k, p.1)
}

/// 斜切矩形填充。
pub fn skew_rect(ui: &mut Ui, r: Rect, skew: f32, color: Color) {
    ui.fill_path(&skew_path(r, skew), color);
}

/// 斜切矩形描边。
pub fn skew_stroke(ui: &mut Ui, r: Rect, skew: f32, width: f32, color: Color) {
    ui.stroke_path(&skew_path(r, skew), width, color);
}

/// Phigros 风格勾选框：**斜切方块**，`on` 时叠一个黑色对勾。
///
/// 参考图里开 = 白块 + 黑勾、关 = 暗块 + 弱描边，所以这里按 `on` 分两路画，
/// 勾线本身也走同一条斜切规则（否则勾和框会歪斜不一致）。
pub fn skew_check(ui: &mut Ui, r: Rect, skew: f32, on: bool, th: &Theme) {
    let k = -skew / r.h.max(1e-4);
    let cy = r.center().y;
    if on {
        skew_rect(ui, r, skew, th.white(1.));
        use lyon::math::point;
        let a = skew_pt((r.x + r.w * 0.24, cy + r.h * 0.02), cy, k);
        let b = skew_pt((r.x + r.w * 0.43, r.bottom() - r.h * 0.26), cy, k);
        let c = skew_pt((r.x + r.w * 0.78, r.y + r.h * 0.26), cy, k);
        let mut p = lyon::path::Path::builder();
        p.begin(point(a.0, a.1));
        p.line_to(point(b.0, b.1));
        p.line_to(point(c.0, c.1));
        p.end(false);
        ui.stroke_path(&p.build(), r.h * 0.15, Color::new(0., 0., 0., 1.));
    } else {
        // 关：底块压暗（用主题墨色），加一圈弱描边 —— 浅色主题下也是「一个空框」。
        skew_rect(ui, r, skew, th.wash(0.14));
        skew_stroke(ui, r, skew, 0.0016, th.border);
    }
}

// ============================================================
// 开关按钮（替代旧的胶囊开关）
// ============================================================

/// 开关按钮：直接把「开 / 关」写在按钮上。
///
/// 为什么不再用胶囊：胶囊是标准 2:1 的小件（世界坐标下仅约 60×30px，见曾经的
/// `pill_rect`），滑块位置又是个需要"看懂"的隐喻，用户反馈**点不准、也看不出状态**。
/// 按钮把状态写成字，命中区就是整块矩形，不存在误判。
///
/// 按下回弹不用在这里做：`DRectButton::build` 本身就会绕中心做 0.96→1.0 的缩放
/// （见 `prpr/src/ui.rs` 的 `build`），已经够明显。
///
/// `on` 为真：强调色实底 + 亮描边 + 白字；为假：中性底 + 弱描边 + 弱化字。
pub fn toggle_btn(ui: &mut Ui, m: &Metrics, r: Rect, on: bool, th: &Theme) {
    let radius = m.u(8.);
    let (bg, border, fg) = if on {
        (th.accent, th.with_a(th.white(1.), 0.34), Color::new(1., 1., 1., 1.))
    } else {
        (th.wash(0.10), th.border, th.text_mute)
    };
    rounded(ui, r, radius, bg);
    rounded_stroke(ui, r, radius, m.u(1.), border);
    text_center(ui, m, if on { "开" } else { "关" }, r.center().x, r.center().y, 15., fg);
}

/// 按键按下时的缩放倍率（kaichang `:active{scale(.985)}`）。
///
/// `press` 是按下进度 0~1（由调用方自己的按压动画给出，例如开始页的
/// `panel_press_p`）。返回要作用在矩形上的缩放，配合 [`scaled_rect`] 使用。
pub fn press_scale(press: f32) -> f32 {
    1. - 0.015 * press.clamp(0., 1.)
}

/// 给一个矩形算一个稳定的过渡 key（需要按控件记忆状态时用）。
pub fn rect_key(r: Rect) -> u64 {
    let x = (r.x * 4096.) as i64 as u64;
    let y = (r.y * 4096.) as i64 as u64;
    (x << 32) ^ y ^ ((r.w * 4096.) as i64 as u64).rotate_left(17)
}

/// 把一个矩形按**中心**缩放（CSS `transform: scale()` 默认锚点）。
pub fn scaled_rect(r: Rect, sx: f32, sy: f32) -> Rect {
    let w = r.w * sx;
    let h = r.h * sy;
    Rect::new(r.center().x - w * 0.5, r.center().y - h * 0.5, w, h)
}

/// 卡片 / 按钮的**悬停缩放动效**：光标进入时整块轻微「弹」起（CSS `hover` 的 transform）。
///
/// 缩放锚点在矩形中心，所以并排元素之间的间距观感不受影响。
/// 幅度刻意很小（1.4%）—— 这是「卡片有生命感」而不是「抖动」的分界线。
pub fn hover_scale(r: Rect, hp: f32) -> Rect {
    let s = 1. + 0.014 * hp.clamp(0., 1.);
    scaled_rect(r, s, s)
}



/// 竖直渐变（自上而下）。
pub fn vgrad(r: Rect, top: Color, bottom: Color) -> impl IntoShading {
    (top, (r.x, r.y), bottom, (r.x, r.y + r.h))
}

/// 水平渐变（自左而右）。
pub fn hgrad(r: Rect, left: Color, right: Color) -> impl IntoShading {
    (left, (r.x, r.y), right, (r.x + r.w, r.y))
}

/// 径向渐变。
pub fn radial(center: (f32, f32), radius: f32, inner: Color, outer: Color) -> impl IntoShading {
    (inner, center, outer, radius)
}

/// 两个颜色（含 alpha）线性混合，`p ∈ [0,1]`。
pub fn mix(a: Color, b: Color, p: f32) -> Color {
    let p = p.clamp(0., 1.);
    Color::new(a.r + (b.r - a.r) * p, a.g + (b.g - a.g) * p, a.b + (b.b - a.b) * p, a.a + (b.a - a.a) * p)
}

/// 内容区左上角「返回」按钮的矩形。
///
/// 由 `MainScene` 画（它才知道页面栈深度），页面按需避让 —— 曲库的头部
/// 就是把搜索框摆在这个槽位右边的。
pub fn back_rect(ui: &Ui, m: &Metrics) -> Rect {
    let c = m.content(ui);
    // 48 设计稿 px（原来 34px，实际只有约 35px，远低于 44px 的最小可点尺寸，
    // 用户反馈"返回太小、还时常点不中"）。
    let s = m.u(48.);
    Rect::new(c.x + m.u(12.), c.y + m.u(12.), s, s)
}

/// 返回键的**命中区**：比可视方形大一圈。
///
/// 可视件是 48px 的方块，但手指/鼠标落在边缘一两像素就出界；
/// 这里每边各放 8px 容错。注意与相邻元素（页面左上角内容）留有余量：
/// 命中区只向左上扩，不侵占右侧内容太多。
pub fn back_hit_rect(ui: &Ui, m: &Metrics) -> Rect {
    back_rect(ui, m).feather(m.u(8.))
}

/// 一段文字。`anchor` 与 `DrawText::anchor` 同义；`size_px` 是设计稿像素字号。
#[allow(clippy::too_many_arguments)]
pub fn text<'a, 's>(
    ui: &mut Ui<'a>,
    m: &Metrics,
    s: impl Into<Cow<'s, str>>,
    x: f32,
    y: f32,
    size_px: f32,
    color: Color,
    anchor: (f32, f32),
) -> Rect {
    let t: DrawText<'a, 's, '_> = ui.text(s);
    t.pos(x, y)
        .anchor(anchor.0, anchor.1)
        .size(m.f(size_px))
        .color(color)
        .no_baseline()
        .draw()
}

/// 用指定字体（如 `prpr::core::BOLD_FONT`）画一段文字。
#[allow(clippy::too_many_arguments)]
pub fn text_with_font<'a, 's>(
    ui: &mut Ui<'a>,
    m: &Metrics,
    s: impl Into<Cow<'s, str>>,
    x: f32,
    y: f32,
    size_px: f32,
    color: Color,
    anchor: (f32, f32),
    font: &'static std::thread::LocalKey<std::cell::RefCell<Option<prpr::ui::TextPainter>>>,
) -> Rect {
    let t: DrawText<'a, 's, '_> = ui.text(s);
    t.pos(x, y)
        .anchor(anchor.0, anchor.1)
        .size(m.f(size_px))
        .color(color)
        .no_baseline()
        .draw_using(font)
}

/// 鼠标指针在 UI 世界坐标里的位置（与 `Touch::position` 同一套换算）。
///
/// 换算方式抄自 `prpr::judge::Judge::touch_transform` —— 那是对外唯一有保证的
/// 「屏幕像素 → 世界坐标」映射，自己推容易把 y 轴正负号搞反。
pub fn mouse_world() -> (f32, f32) {
    let (mx, my) = mouse_position();
    let vp = prpr::ext::get_viewport();
    (
        (mx - vp.0 as f32) / vp.2 as f32 * 2. - 1.,
        ((my - (screen_height() - (vp.1 + vp.3) as f32)) / vp.3 as f32 * 2. - 1.) / (vp.2 as f32 / vp.3 as f32),
    )
}

/// 居中文字（水平 + 垂直）。
pub fn text_center<'a, 's>(ui: &mut Ui<'a>, m: &Metrics, s: impl Into<Cow<'s, str>>, cx: f32, cy: f32, size_px: f32, color: Color) -> Rect {
    text(ui, m, s, cx, cy, size_px, color, (0.5, 0.5))
}

/// 量一段文字（返回世界坐标矩形），用于布局。
pub fn measure<'a, 's>(ui: &mut Ui<'a>, m: &Metrics, s: impl Into<Cow<'s, str>>, size_px: f32) -> Rect {
    let t = ui.text(s);
    t.size(m.f(size_px)).no_baseline().measure()
}

/// 单行文字，超宽自动截断成 `…`。
#[allow(clippy::too_many_arguments)]
pub fn text_ellipsis<'a, 's>(
    ui: &mut Ui<'a>,
    m: &Metrics,
    s: impl Into<Cow<'s, str>>,
    x: f32,
    y: f32,
    size_px: f32,
    max_w: f32,
    color: Color,
    anchor: (f32, f32),
) -> Rect {
    let t = ui.text(s);
    t.pos(x, y)
        .anchor(anchor.0, anchor.1)
        .size(m.f(size_px))
        .color(color)
        .no_baseline()
        .max_width(max_w)
        .draw()
}

/// 一段可换行文字。
#[allow(clippy::too_many_arguments)]
pub fn text_multiline<'a, 's>(
    ui: &mut Ui<'a>,
    m: &Metrics,
    s: impl Into<Cow<'s, str>>,
    x: f32,
    y: f32,
    size_px: f32,
    max_w: f32,
    color: Color,
) -> Rect {
    let t = ui.text(s);
    t.pos(x, y).size(m.f(size_px)).color(color).no_baseline().max_width(max_w).multiline().draw()
}

/// HSV 色相（s=1 v=1）→ RGB，`h` 归一化后取 6 段线性插值。
pub fn hue_color(h: f32) -> Color {
    let h = h.rem_euclid(1.) * 6.;
    let f = h - h.floor();
    let (r, g, b) = match h as u32 {
        0 => (1., f, 0.),
        1 => (1. - f, 1., 0.),
        2 => (0., 1., f),
        3 => (0., 1. - f, 1.),
        4 => (f, 0., 1.),
        _ => (1., 0., 1. - f),
    };
    Color::new(r, g, b, 1.)
}

/// SP 难度的**彩虹文字**：逐字符色相循环，随时间缓慢流动。
///
/// 世界坐标字号（与 `ui.text().size(...)` 同尺度）；返回排版矩形。
/// `max_w` 超限时截断成 `…`（截断部分按最后一个字符的颜色画）。
pub fn rainbow_text<'a, 's>(
    ui: &mut Ui<'a>,
    s: impl Into<Cow<'s, str>>,
    x: f32,
    y: f32,
    size: f32,
    anchor: (f32, f32),
    max_w: f32,
    t: f32,
) -> Rect {
    let s: Cow<'s, str> = s.into();
    let whole = ui.text(s.clone()).size(size).no_baseline().measure();
    // 超宽截断：逐字追加直到放不下，再补省略号
    let mut used = 0.;
    let mut chars: Vec<char> = Vec::new();
    let mut truncated = false;
    for ch in s.chars() {
        let w = ui.text(ch.to_string()).size(size).no_baseline().measure().w;
        if used + w > max_w && max_w > 1e-6 {
            truncated = true;
            break;
        }
        used += w;
        chars.push(ch);
    }
    if truncated && !chars.is_empty() {
        chars.pop();
        chars.push('…');
    }

    let ty = y - anchor.1 * whole.h;
    let mut tx = x - anchor.0 * used;
    for (i, ch) in chars.into_iter().enumerate() {
        let w = ui.text(ch.to_string()).size(size).no_baseline().measure().w;
        ui.text(ch.to_string())
            .pos(tx, ty)
            .anchor(0., 0.)
            .no_baseline()
            .size(size)
            .color(hue_color(i as f32 * 0.16 + t * 0.05))
            .draw_using(&BOLD_FONT);
        tx += w;
    }
    Rect::new(x - anchor.0 * used, ty, used, whole.h)
}

/// 难度标签文字：**纯白、无投影**。
///
/// ⚠️ 用户要求删掉难度标签（`Lv.78` / `SP Lv.?`）后面的**偏移阴影**：
/// 原来这里会先把同一段文字画一遍黑色、往右下偏移 2.8% 字号当作投影，
/// 视觉上是一层脏边。现在只画文字本身。
///
/// 去掉投影后为了让白字在**浅色封面 / 浅色面板**上照样读得清，调用方必须保证
/// 文字底下有足够深的底衬（曲库卡片是半透明黑胶囊，见 `charts_view.rs`；
/// 歌曲详情页的难度 chip 用的是深色底）。**不要**在浅底上直接调它。
#[allow(clippy::too_many_arguments)]
pub fn text_diff<'a, 's>(
    ui: &mut Ui<'a>,
    s: impl Into<Cow<'s, str>>,
    x: f32,
    y: f32,
    size: f32,
    _kind: DiffKind,
    anchor: (f32, f32),
    max_w: f32,
    _t: f32,
) -> Rect {
    let s: Cow<'s, str> = s.into();
    ui.text(s)
        .pos(x, y)
        .anchor(anchor.0, anchor.1)
        .size(size)
        .no_baseline()
        .max_width(max_w)
        .color(Color::new(1., 1., 1., 0.97))
        .draw_using(&BOLD_FONT)
}

/// 磨砂亚克力面板：半透明底 + 1px 描边 + 顶部高光 + 面板内细横纹（磨砂颗粒感）。
/// 纹理尺寸按世界坐标写死（≈7px 一条 @1080p），不依赖 Metrics。
pub fn acrylic(ui: &mut Ui, r: Rect, radius: f32, th: &Theme) {
    // iOS inset grouped：卡片就是一块**不透明**的圆角实色。
    // 层级由「页面底 → 卡片底」的明度差提供（浅色 `#F2F2F7` → 纯白）。
    let path = r.rounded(radius);
    ui.fill_path(&path, th.bg_elev);
    // 1px 分隔线：iOS 的卡片边界靠它，不靠投影
    if th.border.a > 0.001 {
        ui.stroke_path(&path, 0.001, th.border);
    }
}

/// 卡片左侧那根 2px 竖向渐变条。
pub fn accent_bar(ui: &mut Ui, r: Rect, from: Color, to: Color, radius: f32) {
    ui.fill_path(&r.rounded(radius), (from, (r.x, r.y), to, (r.x, r.y + r.h)));
}

// ============================================================
// kaichang 玻璃件（与开始界面同款，供**其他页面**复用）
// ============================================================
//
// 开始界面（`page/home.rs`）那套「左立绘 + 右玻璃按钮面板」的视觉语言全部提炼在
// 这里：玻璃底板 / 玻璃描边 / 带字距排版 / 主按钮（强调色描边 + 外发光 + 箭头）。
// 其他页面直接调这些函数，就能和开始页保持一致 —— 不许各写各的玻璃。

/// 推进一个悬停进度条（0.16s 缓动，替代瞬切），返回当前值。
pub fn hover_step(a: &mut crate::anim::Anim<f32>, on: bool, t: f32) -> f32 {
    let target = if on { 1. } else { 0. };
    if !a.transiting(t) && (target - a.now(t)).abs() > 1e-3 {
        a.goto(target, t, 0.16);
    }
    a.now(t)
}

/// 玻璃按钮的描边色（kaichang `rgba(255,255,255,.18)` → 深色加白 / 浅色加黑）。
pub fn glass_border(th: &Theme, _hp: f32) -> Color {
    // iOS：卡片边界恒为那条 1px 分隔线；悬停改的是**底色**（见 `glass_plate_w`），
    // 不改描边颜色 —— 所以这里不再按 `hp` 提亮。
    th.border
}

/// 玻璃面板上的文字色：深色主题纯白，浅色主题主题墨色（**绝不写死纯白**）。
pub fn glass_text(th: &Theme, hp: f32) -> Color {
    if th.dark {
        mix(th.white(0.92), th.white(1.), hp)
    } else {
        mix(th.text, th.text_hi, hp)
    }
}

/// 玻璃面板的外发光（悬停时更亮）。
pub fn glass_glow(ui: &mut Ui, r: Rect, radius: f32, th: &Theme, hp: f32) {
    // iOS 平面风格**不用彩色发光**。这个函数保留（调用点不少），但退化成一层
    // 极淡的中性投影，且只在悬停时出现 —— 用来把卡片「抬」起来一点点。
    if hp <= 0.01 {
        return;
    }
    glow_rect(ui, r, radius, th.black(if th.dark { 0.30 } else { 0.08 } * hp), radius * 0.5, 3);
}

/// **iOS 卡片底板**（世界坐标版）：不透明实色 + 悬停淡墨 + 1px 分隔线。
/// `tint` 是叠在底板上的强调色（alpha 为 0 表示不叠）。
#[allow(clippy::too_many_arguments)]
pub fn glass_plate_w(ui: &mut Ui, r: Rect, radius: f32, th: &Theme, hp: f32, tint: Color, border: Color) {
    let radius = radius.min(r.h * 0.5).min(r.w * 0.5);
    acrylic(ui, r, radius, th);
    // 悬停 = 叠一层极淡的墨色（深色加白 / 浅色加黑），不是玻璃提亮
    if hp > 0.001 {
        rounded(ui, r, radius, th.wash(0.055 * hp));
    }
    if tint.a > 1e-3 {
        rounded(
            ui,
            r,
            radius,
            Color {
                a: (tint.a * (0.8 + 0.45 * hp)).min(1.),
                ..tint
            },
        );
    }
    rounded_stroke(ui, r, radius, 0.001, border);
}

/// [`glass_plate_w`] 的「设计稿像素」版（`radius` / 反光高度按 [`Metrics`] 换算）。
#[allow(clippy::too_many_arguments)]
pub fn glass_plate(ui: &mut Ui, m: &Metrics, r: Rect, radius_px: f32, th: &Theme, hp: f32, tint: Color, border: Color) {
    glass_plate_w(ui, r, m.u(radius_px), th, hp, tint, border)
}

/// 统一卡片（列表 / 侧栏 / 内容面板）：实色底板 + 悬停淡墨 + 1px 分隔线。
pub fn glass_card(ui: &mut Ui, r: Rect, radius: f32, th: &Theme, hp: f32) {
    // iOS 卡片不发光：悬停反馈由 `glass_plate_w` 自己那层淡墨色完成。
    glass_plate_w(ui, r, radius, th, hp, Color::new(0., 0., 0., 0.), glass_border(th, hp));
}

/// **带可调描边宽度**的卡片底板（世界坐标半径）。
///
/// 与 [`glass_plate_w`] 的唯一区别是描边宽度由调用方给（`m.u(1.)` 这类设计稿像素，
/// 而不是写死世界坐标）。用在「一堆按钮紧密竖排在同一个面板里」的场景。
#[allow(clippy::too_many_arguments)]
pub fn glass_plate_flat(ui: &mut Ui, r: Rect, radius: f32, th: &Theme, hp: f32, tint: Color, border: Color, edge: f32) {
    let radius = radius.min(r.h * 0.5).min(r.w * 0.5);
    acrylic(ui, r, radius, th);
    if hp > 0.001 {
        rounded(ui, r, radius, th.wash(0.055 * hp));
    }
    if tint.a > 1e-3 {
        rounded(
            ui,
            r,
            radius,
            Color {
                a: (tint.a * (0.8 + 0.45 * hp)).min(1.),
                ..tint
            },
        );
    }
    rounded_stroke(ui, r, radius, edge, border);
}

/// kaichang `.main-arrow` 的箭头（SVG `M5 12h14M13 6l6 6-6 6`）。
pub const ARROW_SEGS: &[Seg] = &[
    Seg::M(5., 12.),
    Seg::L(19., 12.),
    Seg::M(13., 6.),
    Seg::L(19., 12.),
    Seg::L(13., 18.),
];

/// 画那根箭头（`stroke_px` 是 24×24 视口里的线宽）。
pub fn draw_arrow(ui: &mut Ui, r: Rect, color: Color, stroke_px: f32) {
    let s = (r.w / 24.).min(r.h / 24.);
    ui.stroke_options.start_cap = lyon::path::LineCap::Round;
    ui.stroke_options.end_cap = lyon::path::LineCap::Round;
    ui.stroke_options.line_join = lyon::path::LineJoin::Round;
    ui.stroke_path(&build_icon_path(ARROW_SEGS, r), stroke_px * s, color);
}

// ---------- 带字距（letter-spacing）的排版 ----------

thread_local! {
    /// 单字符宽度缓存：`(字符, 字号位模式, 是否粗体, 视口宽)` → 世界坐标宽度。
    ///
    /// 设置页有 ~40 行、每行都要逐字符排版，一帧几百次 `measure` 会明显掉帧；
    /// 字宽只跟字体/字号/视口有关，缓存下来后每帧只需要画，不用再量。
    static CHAR_W: std::cell::RefCell<std::collections::HashMap<(char, u32, bool, i32), f32>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// 量一个字符的宽度（带缓存）。
fn char_w(ui: &mut Ui, ch: char, size: f32, bold: bool) -> f32 {
    let vw = prpr::ext::get_viewport().2;
    let key = (ch, size.to_bits(), bold, vw);
    if let Some(w) = CHAR_W.with(|it| it.borrow().get(&key).copied()) {
        return w;
    }
    let s = ch.to_string();
    let w = if bold {
        ui.text(s).size(size).no_baseline().measure_using(&BOLD_FONT).w
    } else {
        ui.text(s).size(size).no_baseline().measure().w
    };
    CHAR_W.with(|it| {
        let mut m = it.borrow_mut();
        if m.len() > 4096 {
            m.clear(); // 字号/文案变了就重来，别让缓存无限长
        }
        m.insert(key, w);
    });
    w
}

/// 逐字符量一段带字距的文字宽度（世界坐标版）。
pub fn tracked_width_w(ui: &mut Ui, s: &str, size: f32, track: f32, bold: bool) -> f32 {
    let mut w = 0.;
    for ch in s.chars() {
        w += char_w(ui, ch, size, bold) + track;
    }
    (w - track).max(0.)
}

/// [`tracked_width_w`] 的「设计稿像素」版。
pub fn tracked_width(ui: &mut Ui, m: &Metrics, s: &str, size_px: f32, track_px: f32, bold: bool) -> f32 {
    tracked_width_w(ui, s, m.f(size_px), m.u(track_px), bold)
}

/// 画一段带字距的文字（世界坐标版，逐字符画）。返回整段包围盒。
#[allow(clippy::too_many_arguments)]
pub fn text_tracked_w(ui: &mut Ui, s: &str, x: f32, y: f32, size: f32, color: Color, track: f32, anchor: (f32, f32), bold: bool) -> Rect {
    let total = tracked_width_w(ui, s, size, track, bold);
    let start = x - total * anchor.0;
    let mut cx = start;
    let mut out = Rect::new(start, y, total, 0.);
    for ch in s.chars() {
        let r = if bold {
            ui.text(ch.to_string())
                .pos(cx, y)
                .anchor(0., anchor.1)
                .size(size)
                .no_baseline()
                .color(color)
                .draw_using(&BOLD_FONT)
        } else {
            ui.text(ch.to_string())
                .pos(cx, y)
                .anchor(0., anchor.1)
                .size(size)
                .no_baseline()
                .color(color)
                .draw()
        };
        if out.h <= 1e-6 {
            out.y = r.y;
            out.h = r.h;
        }
        cx = r.right() + track;
    }
    out
}

/// 画一段带字距的文字（`track_px` 是设计稿像素）。`anchor` 与 [`text`] 同义。
#[allow(clippy::too_many_arguments)]
pub fn text_tracked(
    ui: &mut Ui,
    m: &Metrics,
    s: &str,
    x: f32,
    y: f32,
    size_px: f32,
    color: Color,
    track_px: f32,
    anchor: (f32, f32),
    bold: bool,
) -> Rect {
    text_tracked_w(ui, s, x, y, m.f(size_px), color, m.u(track_px), anchor, bold)
}

/// **页面标题行**（kaichang 排版）：左侧强调条 + 带字距主标题 + 弱化副标题。
///
/// 返回标题行的包围盒。`cy` 是这一行的垂直中心。
pub fn page_title(ui: &mut Ui, m: &Metrics, th: &Theme, x: f32, cy: f32, title: &str, sub: Option<&str>, size_px: f32) -> Rect {
    // 左侧 2px 强调条（活动栏选中态同款）
    let bar = Rect::new(x, cy - m.u(size_px * 0.42), m.u(3.), m.u(size_px * 0.84));
    accent_bar(ui, bar, th.glow, th.teal, m.u(1.5));
    let tx = x + m.u(12.);
    let t = text_tracked(ui, m, title, tx, cy, size_px, th.text_hi, size_px * 0.10, (0., 0.5), true);
    if let Some(sub) = sub {
        text_tracked(ui, m, sub, t.right() + m.u(12.), cy + m.u(1.), (size_px * 0.52).max(10.), th.text_mute, 1.5, (0., 0.5), false);
    }
    t
}

// ---------- kaichang 三种按钮（主 / 次 / 白底） ----------

/// 主按钮（kaichang `.glass-btn.main`）：强调色描边 + 外发光 + 右侧箭头。
///
/// `label` / `label_en` 对应「开始游玩 / GAME」那种中英双行排版。
pub fn draw_main_btn(ui: &mut Ui, m: &Metrics, th: &Theme, r: Rect, hp: f32, label: &str, label_en: &str) {
    let radius = m.u(12.);
    // iOS filled button：强调色**实底** + 白字。无描边、无发光，
    // 悬停只把底色往亮一档推（`accent → accent_hi`）。
    rounded(ui, r, radius, mix(th.accent, th.accent_hi, hp * 0.5));

    let fg = Color::new(1., 1., 1., 1.);
    let pad = m.u(20.);
    let cn = text_tracked(ui, m, label, r.x + pad, r.center().y, 20., fg, 2., (0., 0.5), true);
    if !label_en.is_empty() {
        text_tracked(ui, m, label_en, cn.right() + m.u(10.), r.center().y + m.u(8.), 11., Color::new(1., 1., 1., 0.78), 1.5, (0., 1.), false);
    }
    let aw = m.u(22.);
    let ar = Rect::new(r.right() - pad - aw, r.center().y - aw * 0.5, aw, aw);
    ui.scope(|ui| {
        ui.dx(hp * m.u(4.));
        draw_arrow(ui, ar, fg, 2.2);
    });
}

/// 次级按钮：iOS **tinted** 样式（强调色 12% 底 + 强调色字），左对齐标签 + 右侧图标。
pub fn draw_sec_btn(ui: &mut Ui, m: &Metrics, th: &Theme, r: Rect, hp: f32, label: &str, icon: Icon) {
    rounded(
        ui,
        r,
        m.u(12.),
        mix(th.with_a(th.accent, 0.12), th.with_a(th.accent, 0.20), hp),
    );
    text_tracked(ui, m, label, r.x + m.u(18.), r.center().y, 16., th.accent, 2., (0., 0.5), true);
    let isz = m.u(16.);
    draw_icon(
        ui,
        m,
        icon,
        Rect::new(r.right() - m.u(20.), r.center().y - isz * 0.5, isz, isz),
        th.accent,
        1.7,
    );
}

/// 中性实底按钮（灰底 + 主题墨色字）——「确定 / 保存」这类不需要强调色的操作。
pub fn draw_white_btn(ui: &mut Ui, m: &Metrics, th: &Theme, r: Rect, hp: f32, label: &str) {
    let radius = m.u(12.).min(r.h * 0.5);
    rounded(ui, r, radius, mix(th.bg_elev, th.bg_hover, 0.45 + 0.55 * hp));
    rounded_stroke(ui, r, radius, m.u(1.), th.border);
    text_tracked(ui, m, label, r.center().x, r.center().y, 16., th.text, 2., (0.5, 0.5), true);
}

/// 面板最底部的「退出」：iOS destructive 样式（平时透明，悬停转红底红字）。
pub fn draw_quit_btn(ui: &mut Ui, m: &Metrics, th: &Theme, r: Rect, hp: f32, label: &str) {
    let radius = m.u(10.);
    rounded(ui, r, radius, mix(Color::new(0., 0., 0., 0.), th.with_a(th.red, 0.12), hp));
    let col = th.red;
    let size = 15.;
    let isz = m.u(15.);
    let tw = tracked_width(ui, m, label, size, 2., false);
    let x0 = r.center().x - (isz + m.u(8.) + tw) * 0.5;
    draw_icon(ui, m, Icon::Logout, Rect::new(x0, r.center().y - isz * 0.5, isz, isz), col, 1.8);
    text_tracked(ui, m, label, x0 + isz + m.u(8.), r.center().y, size, col, 2., (0., 0.5), false);
}

// ---------- 大号（开始页尺度）的同族按钮 ----------
//
// 上面那组（`draw_main_btn` / `draw_sec_btn` / `draw_quit_btn`）是**输入对话框**用的
// 紧凑尺度；开始页右侧那列按钮用的是设计稿 `.glass-btn` 的原始大尺度
// （标签 31px / 内边距 44px / 图标 20px）。
// 两者都保留 —— 不是「两份实现」，而是同一族的两个尺寸档，都调同一套底板
// （`glass_plate_flat` / `glass_border` / `glass_text` / `hover_scale`），
// 所以任何一处改玻璃质感，两个尺度同时生效。
//
// （原来还有一个紧凑尺度的 `draw_row_btn`，因为「图标 + 文字居中」这个版式
//   只出现在开始页，没人用，已删掉 —— 现在只有 `draw_row_btn_lg`。）

/// 大号主按钮「开始游玩 / GAME / →」（kaichang `.glass-btn.main`）。
#[allow(clippy::too_many_arguments)]
pub fn draw_main_btn_lg(ui: &mut Ui, m: &Metrics, th: &Theme, r: Rect, hp: f32, label: &str, label_en: &str) {
    let r = hover_scale(r, hp);
    let radius = m.u(14.);
    // iOS filled button：强调色实底 + 白字（无描边、无发光）
    rounded(ui, r, radius, mix(th.accent, th.accent_hi, hp * 0.5));

    let fg = Color::new(1., 1., 1., 1.);
    let pad = m.u(36.);
    let cn = text_tracked(ui, m, label, r.x + pad, r.center().y, 26., fg, 3., (0., 0.5), true);
    if !label_en.is_empty() {
        // 小号英文贴在中文右下角（kaichang `.main-en{align-self:flex-end}`）
        text_tracked(ui, m, label_en, cn.right() + m.u(12.), r.center().y + m.u(10.), 12., Color::new(1., 1., 1., 0.80), 1.5, (0., 1.), false);
    }
    // 箭头：悬停往右冲一点
    let aw = m.u(24.);
    let ar = Rect::new(r.right() - pad - aw, r.center().y - aw * 0.5, aw, aw);
    ui.scope(|ui| {
        ui.dx(hp * m.u(4.));
        draw_arrow(ui, ar, fg, 2.4);
    });
}

/// 大号次级满宽按钮：iOS grouped 行样式（浅灰实底 + 1px 分隔线 + 主题墨色字）。
pub fn draw_sec_btn_lg(ui: &mut Ui, m: &Metrics, th: &Theme, r: Rect, hp: f32, label: &str, icon: Icon) {
    let r = hover_scale(r, hp);
    let radius = m.u(14.);
    // 底比卡片（`bg_elev`）略深一档，压在卡片上才看得出边界 —— 这就是 iOS 的灰按钮
    rounded(ui, r, radius, mix(th.bg_elev, th.bg_hover, 0.45 + 0.55 * hp));
    rounded_stroke(ui, r, radius, m.u(1.), th.border);
    text_tracked(ui, m, label, r.x + m.u(36.), r.center().y, 20., th.text, 3., (0., 0.5), true);
    let isz = m.u(18.);
    draw_icon(
        ui,
        m,
        icon,
        Rect::new(r.right() - m.u(36.), r.center().y - isz * 0.5, isz, isz),
        mix(th.text_mute, th.accent, hp),
        1.7,
    );
}

/// 大号并排按钮（设置 / 活动 / 消息 / 关于）：图标 + 标签整体居中。
#[allow(clippy::too_many_arguments)]
pub fn draw_row_btn_lg(ui: &mut Ui, m: &Metrics, th: &Theme, r: Rect, hp: f32, label: &str, icon: Icon, badge: bool, t: f32) {
    let r = hover_scale(r, hp);
    let radius = m.u(14.);
    rounded(ui, r, radius, mix(th.bg_elev, th.bg_hover, 0.45 + 0.55 * hp));
    rounded_stroke(ui, r, radius, m.u(1.), th.border);

    let size = 19.;
    let isz = m.u(18.);
    let tw = tracked_width(ui, m, label, size, 2., true);
    let x0 = r.center().x - (isz + m.u(9.) + tw) * 0.5;
    draw_icon(
        ui,
        m,
        icon,
        Rect::new(x0, r.center().y - isz * 0.5, isz, isz),
        mix(th.text_dim, th.accent, hp),
        1.8,
    );
    let lr = text_tracked(ui, m, label, x0 + isz + m.u(9.), r.center().y, size, th.text, 2., (0., 0.5), true);

    if badge {
        // 有新消息：标签右边的呼吸红点（设计稿 `dotPulse` 2.2s）
        let dp = pulse(t, 2.2, 0.);
        ui.fill_circle(
            lr.right() + m.u(9.),
            r.center().y,
            m.u(3.6) * (0.82 + 0.18 * (1. - dp)),
            Color {
                a: 0.55 + 0.45 * (1. - dp),
                ..th.red
            },
        );
    }
}

/// 大号「退出」：弱化样式（只有悬停才转红）。
pub fn draw_quit_btn_lg(ui: &mut Ui, m: &Metrics, th: &Theme, r: Rect, hp: f32, label: &str) {
    let r = hover_scale(r, hp);
    let radius = m.u(10.);
    // iOS destructive：平时透明，悬停铺一层 12% 红底
    rounded(ui, r, radius, mix(Color::new(0., 0., 0., 0.), th.with_a(th.red, 0.12), hp));

    let col = th.red;
    let size = 15.;
    let isz = m.u(15.);
    let tw = tracked_width(ui, m, label, size, 2., false);
    let x0 = r.center().x - (isz + m.u(8.) + tw) * 0.5;
    draw_icon(ui, m, Icon::Logout, Rect::new(x0, r.center().y - isz * 0.5, isz, isz), col, 1.8);
    text_tracked(ui, m, label, x0 + isz + m.u(8.), r.center().y, size, col, 2., (0., 0.5), false);
}

/// 列表行（世界坐标）：玻璃行卡 + 左图标 + 标题 + 副标题（弱化）+ 右值。
///
/// `size` 是**世界坐标**字号（老页面那套尺度，0.35~0.5）；行高由调用方给，
/// 图标大小跟着行高走。所有页面里的「设置项 / 消息行 / 历史行」都该长这样。
#[allow(clippy::too_many_arguments)]
pub fn draw_list_row(
    ui: &mut Ui,
    th: &Theme,
    r: Rect,
    hp: f32,
    icon: Option<Icon>,
    title: &str,
    sub: Option<&str>,
    value: Option<&str>,
    size: f32,
) {
    glass_card(ui, r, r.h.min(0.020) * 0.6, th, hp);
    let pad = r.h * 0.30;
    let mut x = r.x + pad;
    if let Some(ic) = icon {
        let isz = r.h * 0.5;
        draw_icon(
            ui,
            &th_icon_metrics(),
            ic,
            Rect::new(x, r.center().y - isz * 0.5, isz, isz),
            mix(th.text_mute, th.accent_hi, hp),
            1.8,
        );
        x += isz + pad * 0.9;
    }
    let ty = if sub.is_some() { r.center().y - r.h * 0.17 } else { r.center().y };
    ui.text(title.to_owned())
        .pos(x, ty)
        .anchor(0., 0.5)
        .no_baseline()
        .max_width((r.w * 0.6).max(0.05))
        .size(size)
        .color(glass_text(th, hp))
        .draw();
    if let Some(sub) = sub {
        ui.text(sub.to_owned())
            .pos(x, r.center().y + r.h * 0.24)
            .anchor(0., 0.5)
            .no_baseline()
            .max_width((r.w * 0.6).max(0.05))
            .size(size * 0.76)
            .color(th.text_mute)
            .draw();
    }
    if let Some(v) = value {
        ui.text(v.to_owned())
            .pos(r.right() - pad, r.center().y)
            .anchor(1., 0.5)
            .no_baseline()
            .max_width((r.w * 0.3).max(0.03))
            .size(size * 0.9)
            .color(th.text_dim)
            .draw();
    }
}

// ============================================================
// iOS 分组列表（inset grouped）
// ============================================================
//
// iOS 的列表语言：**一整组共用一个圆角容器**，行与行之间用**内缩的发丝线**分隔，
// 只有整组的**上下两端**才有圆角。跟「每行一张独立圆角卡 + 行间距」是两种观感。
//
// 用法（页面自己知道组里有几行时最省事）：
//
// ```ignore
// let n = items.len();
// newui::group_plate(ui, &th, group_rect, m.u(px::CARD_RADIUS)); // ① 先铺容器
// for (i, it) in items.iter().enumerate() {
//     let r = row_rect(i);
//     newui::group_row(ui, &th, r, hp, i == 0, i + 1 == n, radius, m.u(14.)); // ② 再画行
//     // …行内容…
// }
// ```
//
// ⚠️ **容器必须在画任何一行之前铺好**。这是即时模式：晚画就会盖住已经画好的行内容，
// 所以「回头给上一行补下圆角」这条路走不通 —— 上一行的文字已经压在它的填充之上了。
// 结论：整组一个圆角容器**要求页面先知道总行数**。

/// 分组容器：整组的圆角实色底（不画分隔线，线由 [`group_row`] 画）。
pub fn group_plate(ui: &mut Ui, th: &Theme, r: Rect, radius: f32) {
    if flat_groups() {
        // 扁平模式（Phigros 设置页）：不铺卡片底 —— 行直接落在页面底上。
        return;
    }
    let radius = radius.min(r.h * 0.5).min(r.w * 0.5);
    let path = r.rounded(radius);
    ui.fill_path(&path, th.bg_elev);
    if th.border.a > 0.001 {
        ui.stroke_path(&path, 0.001, th.border);
    }
}

/// 分组里的一行。
///
/// **不填底板**（底板来自 [`group_plate`] 或外层面板），只叠悬停/选中高亮，
/// 并在行底画一条**内缩 `divider_inset` 的发丝线**（`last` 为 true 时不画）。
///
/// `radius`：行的外缘圆角。
/// * 行**本身就是容器**（整组 = 这几行）→ 传卡片圆角（如 `m.u(px::CARD_RADIUS)`）；
/// * 行**内嵌在面板里**（行比容器窄）→ 传 `0.`，悬停就是一块方角高亮。
#[allow(clippy::too_many_arguments)]
pub fn group_row(ui: &mut Ui, th: &Theme, r: Rect, hp: f32, first: bool, last: bool, radius: f32, divider_inset: f32, tint: Color) {
    // 两层叠加，顺序固定：先调用方给的强调色（选中态），再悬停墨色。
    for col in [tint, th.wash(0.055 * hp)] {
        if col.a <= 0.001 {
            continue;
        }
        if radius <= 0.0005 {
            ui.fill_rect(r, col);
        } else {
            // 只有整组的首/末行才有圆角；中间行与「朝组内那一侧」的圆角落在行外，
            // 用 `scissor` 裁掉，就不会在行与行之间留下月牙形缺口。
            let big = match (first, last) {
                (true, true) => r,
                (true, false) => Rect::new(r.x, r.y, r.w, r.h + radius),
                (false, true) => Rect::new(r.x, r.y - radius, r.w, r.h + radius),
                (false, false) => r.feather(radius),
            };
            ui.scissor(r, |ui| rounded(ui, big, radius, col));
        }
    }
    if !last && !flat_groups() && th.border.a > 0.001 {
        let y = r.bottom() - 0.0005;
        ui.fill_rect(
            Rect::new(r.x + divider_inset, y, (r.w - divider_inset).max(0.), 0.001),
            th.border,
        );
    }
}

thread_local! {
    /// 分组列表是否走「扁平」模式（Phigros 设置页）。
    ///
    /// 为真时 [`group_plate`] 不铺卡片底、[`group_row`] 不画分隔线 —— 各行直接
    /// 落在页面底上，靠留白分隔。只在该页渲染期间置位，渲染完复位。
    static FLAT_GROUPS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 设置 / 复位「扁平分组」模式（见 [`FLAT_GROUPS`]）。
pub fn set_flat_groups(v: bool) {
    FLAT_GROUPS.with(|c| c.set(v));
}

/// 当前是否处于「扁平分组」模式。
pub fn flat_groups() -> bool {
    FLAT_GROUPS.with(|c| c.get())
}

/// 分组标题：小号、弱化、带字距，落在整组的上方（iOS 的 section header）。
pub fn section_header(ui: &mut Ui, m: &Metrics, th: &Theme, x: f32, y: f32, text: &str) {
    text_tracked(ui, m, text, x, y, 12., th.text_mute, 1.2, (0., 0.5), true);
}

/// 图标绘制需要一份 [`Metrics`]（`draw_icon` 的签名如此），但世界坐标下的
/// 图标大小由调用方给的矩形决定 —— 这份 Metrics 仅用于满足签名。
pub fn th_icon_metrics() -> Metrics {
    Metrics {
        w: 960.,
        h: 540.,
        k: 1.,
        compact: false,
        tiny: false,
    }
}

// ============================================================
// 应用外壳：标题栏 / 活动栏 / 状态栏
// ============================================================

/// 活动栏上的五个入口（设计稿里 `aside.activity` 的那五个按钮）。
///
/// **这条栏本体已被删除**（用户要求）。`Activity` 现在只作为「页面跳转意图」的
/// 枚举保留：开始界面的玻璃按钮（开始游戏 / 消息 / 设置 / 活动 / 关于 / 账户）
/// 依然通过 [`set_current_activity`] / [`take_activity`] 这套通道驱动导航。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Activity {
    Home,
    /// 曲库（设计稿 `曲库.html` 的活动栏里就有这一项）
    Library,
    Messages,
    Ranking,
    Settings,
    Account,
}

impl Activity {
    pub const ALL: [Activity; 6] = [
        Activity::Home,
        Activity::Library,
        Activity::Messages,
        Activity::Ranking,
        Activity::Settings,
        Activity::Account,
    ];

    pub fn icon(self) -> Icon {
        match self {
            Activity::Home => Icon::Bolt,
            Activity::Library => Icon::Music,
            Activity::Messages => Icon::Chat,
            Activity::Ranking => Icon::Chart,
            Activity::Settings => Icon::Gear,
            Activity::Account => Icon::User,
        }
    }
}

/// 外壳上报给场景的动作。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShellAction {
    Activity(Activity),
    ToggleTheme,
    ToggleFullscreen,
    Quit,
}

/// 标题栏 + 状态栏。（左侧活动栏已按用户要求整条删除，本结构不再画它。）
///
/// 渲染与命中都在这里，场景只负责响应 [`ShellAction`]。
pub struct Shell {
    theme_btn: RectButton,
    win_min: RectButton,
    win_max: RectButton,
    win_close: RectButton,

    /// 活动栏高亮项（栏已删除，仅保留为页面状态，供以后需要时使用）。
    pub active: Activity,
    /// 消息红点。
    pub has_new: bool,
    /// 状态栏左下角：玩家名 / RKS / 在线。
    pub player: Option<String>,
    pub rks: f32,
    pub online: bool,
    /// 标题栏中间的场景名（设计稿里的 `start.scene` / `library.scene`）。
    pub scene: String,
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

impl Shell {
    pub fn new() -> Self {
        Self {
            theme_btn: RectButton::new(),
            win_min: RectButton::new(),
            win_max: RectButton::new(),
            win_close: RectButton::new(),
            active: Activity::Home,
            has_new: false,
            player: None,
            rks: 0.,
            online: false,
            scene: "start.scene".to_owned(),
        }
    }

    pub fn touch(&mut self, touch: &Touch) -> Option<ShellAction> {
        // 状态栏整条已按用户要求移除，栏里的主题按钮随之失效 ——
        // 不画了就别再命中，否则按钮矩形会停在上一帧 / 初值上误触。
        // （深浅主题开关已挪到「设置 → 通用 → 深色主题」。）
        if SHOW_STATUSBAR && self.theme_btn.touch(touch) {
            return Some(ShellAction::ToggleTheme);
        }
        // 左侧活动栏的 6 个图标按钮曾在这里命中；整条栏已删除，命中逻辑一并移除。
        // 各页面的导航不受影响：开始界面玻璃面板 / 返回键 / 状态栏都不走这条通道。
        None
    }

    pub fn render(&mut self, ui: &mut Ui, m: &Metrics, th: &Theme, scene_label: &str, t: f32) {
        let _ = scene_label;

        // ---------- 活动栏 ----------
        // 用户要求：把最左侧那条全局竖向图标导航栏**整条删掉**（含绘制）。
        // 删栏后内容区由 `Metrics::content()` 直接返回整宽，页面天然左移。

        // ---------- 状态栏 ----------
        // 用户要求：底部那条状态栏也**整条删掉**。绘制 / 命中 / 布局高度全部关掉，
        // 见 `SHOW_STATUSBAR` 的注释。这里是整个 `render` 的唯一内容，直接提前返回。
        if !SHOW_STATUSBAR {
            return;
        }

        let sb = m.statusbar(ui);
        let (top, bottom) = if th.dark {
            (rgba(20, 64, 104, 0.98), rgba(12, 44, 78, 0.99))
        } else {
            (rgba(255, 255, 255, 0.98), rgba(226, 236, 246, 0.99))
        };
        ui.fill_rect(sb, vgrad(sb, top, bottom));
        // 顶部那条 1px 高光，外加设计稿 `statusSweep` 的 6s 扫光
        let line_y = sb.y;
        ui.fill_rect(
            Rect::new(sb.x, line_y, sb.w, m.u(1.)),
            if th.dark { rgba(120, 200, 255, 0.22) } else { th.white(0.85) },
        );
        {
            let bright = if th.dark { rgba(150, 225, 255, 0.95) } else { rgba(0, 95, 184, 0.8) };
            let clear = Color {
                a: 0.,
                ..bright
            };
            let sw = sb.w * 0.5;
            let sx = sb.x - sw + (sb.w + sw) * pulse(t, 6., 0.);
            let l = Rect::new(sx, line_y, sw * 0.5, m.u(1.));
            let r = Rect::new(sx + sw * 0.5, line_y, sw * 0.5, m.u(1.));
            ui.fill_rect(l, hgrad(l, clear, bright));
            ui.fill_rect(r, hgrad(r, bright, clear));
        }

        // 状态栏文字/图标统一纯白（深色主题），浅色主题维持深色保证可读
        let fg = if th.dark { rgba(0xff, 0xff, 0xff, 0.85) } else { rgb(0x3a, 0x50, 0x68) };
        let sep = m.u(1.);
        let cy = sb.center().y;

        // 最左：页面投放的状态栏信息（曲库的「N 首曲目 · 选中曲目」）。
        // `tiny`（窄屏）时收起，给玩家名 / 在线状态让位。
        if !m.tiny {
            if let Some((count, selected)) = status_extra() {
                let mut lx = sb.x + m.u(10.);
                if !count.is_empty() {
                    sb_left_item(ui, m, sb, &mut lx, Some(Icon::Music), &count, fg, None);
                    ui.fill_rect(Rect::new(lx + m.u(3.), cy - m.u(7.), sep, m.u(14.)), th.wash(0.16));
                    lx += m.u(7.);
                }
                if !selected.is_empty() {
                    sb_left_item(ui, m, sb, &mut lx, None, &selected, if th.dark { rgba(0xff, 0xff, 0xff, 0.9) } else { th.teal }, None);
                }
            }
        }

        // 左：玩家 + RKS
        let mut lx = sb.x + m.u(10.);
        let name = self.player.clone().unwrap_or_else(|| "未登录".to_owned());
        sb_left_item(ui, m, sb, &mut lx, Some(Icon::User), &name, fg, None);
        ui.fill_rect(Rect::new(lx + m.u(3.), cy - m.u(7.), sep, m.u(14.)), th.wash(0.16));
        lx += m.u(7.);
        if self.rks > 0. {
            let rks = format!("{:.2}", self.rks);
            sb_left_item(ui, m, sb, &mut lx, Some(Icon::Chart), "RKS", if th.dark { rgba(0xff, 0xff, 0xff, 0.85) } else { th.teal }, Some(&rks));
        }

        // 右：编码 / 版本 / 在线
        let mut rx = sb.right() - m.u(10.);
        if self.online {
            rx -= m.u(4.);
            sb_online(ui, m, th, sb, &mut rx, t);
        }
        if !m.tiny {
            let ver = format!("v{}", env!("CARGO_PKG_VERSION"));
            sb_right_item(ui, m, sb, &mut rx, Some(Icon::Layers), &ver, fg);
            ui.fill_rect(Rect::new(rx, cy - m.u(7.), sep, m.u(14.)), th.wash(0.16));
            rx -= m.u(7.);
        }
        sb_right_item(ui, m, sb, &mut rx, Some(Icon::List), "UTF-8", fg);

        // 最右：深浅主题切换（深色显示太阳 → 点了变浅色；浅色显示月亮 → 点了变深色）
        {
            let b = m.u(26.);
            let r = Rect::new(rx - b, cy - b * 0.5, b, b);
            rx -= b + m.u(4.);
            if self.theme_btn.touching() {
                rounded(ui, r, m.u(7.), th.wash(0.10));
                rounded_stroke(ui, r, m.u(7.), m.u(1.), th.border);
            }
            draw_icon(ui, m, if th.dark { Icon::Sun } else { Icon::Moon }, r.feather(-b * 0.26), fg, 1.9);
            self.theme_btn.set(ui, r.feather(m.u(3.)));
        }
    }
}

/// 状态栏左侧的一项（自左向右排）。
#[allow(clippy::too_many_arguments)]
fn sb_left_item(
    ui: &mut Ui,
    m: &Metrics,
    sb: Rect,
    lx: &mut f32,
    icon: Option<Icon>,
    label: &str,
    color: Color,
    bold: Option<&str>,
) {
    let pad = m.u(9.);
    let isz = m.u(12.);
    let cy = sb.center().y;
    let tw = if label.is_empty() { 0. } else { measure(ui, m, label.to_owned(), 11.5).w };
    let bw2 = bold.map(|b| measure(ui, m, b.to_owned(), 11.5).w + m.u(6.)).unwrap_or(0.);
    let iw = if icon.is_some() { isz + m.u(6.) } else { 0. };
    let w = pad * 2. + iw + tw + bw2;
    let r = Rect::new(*lx, sb.y, w, sb.h);
    *lx += w + m.u(2.);
    let mut cx = r.x + pad;
    if let Some(ic) = icon {
        draw_icon(ui, m, ic, Rect::new(cx, cy - isz * 0.5, isz, isz), color, 1.9);
        cx += isz + m.u(6.);
    }
    if !label.is_empty() {
        text(ui, m, label.to_owned(), cx, cy, 11.5, color, (0., 0.5));
        cx += tw;
    }
    if let Some(b) = bold {
        text(ui, m, b.to_owned(), cx + m.u(3.), cy, 11.5, color, (0., 0.5));
    }
}

/// 状态栏右侧的一项（自右向左排）。
fn sb_right_item(ui: &mut Ui, m: &Metrics, sb: Rect, rx: &mut f32, icon: Option<Icon>, label: &str, color: Color) {
    let pad = m.u(9.);
    let isz = m.u(12.);
    let cy = sb.center().y;
    let tw = measure(ui, m, label.to_owned(), 11.5).w;
    let iw = if icon.is_some() { isz + m.u(6.) } else { 0. };
    let w = pad * 2. + iw + tw;
    *rx -= w;
    let r = Rect::new(*rx, sb.y, w, sb.h);
    *rx -= m.u(2.);
    let mut cx = r.x + pad;
    if let Some(ic) = icon {
        draw_icon(ui, m, ic, Rect::new(cx, cy - isz * 0.5, isz, isz), color, 1.9);
        cx += isz + m.u(6.);
    }
    text(ui, m, label.to_owned(), cx, cy, 11.5, color, (0., 0.5));
}

/// 状态栏右下角的「在线」指示（绿点 + 文字）。
fn sb_online(ui: &mut Ui, m: &Metrics, th: &Theme, sb: Rect, rx: &mut f32, t: f32) {
    let pad = m.u(9.);
    let dot = m.u(7.);
    let cy = sb.center().y;
    let label = "在线";
    let tw = measure(ui, m, label, 11.5).w;
    let w = pad * 2. + dot + m.u(6.) + tw;
    *rx -= w;
    let r = Rect::new(*rx, sb.y, w, sb.h);
    *rx -= m.u(2.);
    // 设计稿 `livePulse` 2s：呼吸 + 缩放
    let lp = pulse(t, 2., 0.);
    ui.fill_circle(
        r.x + pad + dot * 0.5,
        cy,
        dot * 0.5 * (0.78 + 0.22 * (1. - lp)),
        Color {
            a: 0.5 + 0.5 * (1. - lp),
            ..rgb(0x5e, 0xe0, 0xa0)
        },
    );
    text(
        ui,
        m,
        label,
        r.x + pad + dot + m.u(6.),
        cy,
        11.5,
        if th.dark { rgba(0xff, 0xff, 0xff, 0.85) } else { th.teal },
        (0., 0.5),
    );
}

// ============================================================
// 外壳 ↔ 页面之间的轻量信箱
// ============================================================
//
// 活动栏的跳转要落到「当前最上层那个 Page」手里（比如点「设置」得由 HomePage
// 去 new 一个 SettingsPage，因为只有它拿得到 `Arc<Icons>`）。MainScene 先把栈
// 弹回根页，再把请求塞进信箱，根页在 update 里取走 —— 比在 MainScene 里堆一堆
// 页面构造逻辑干净得多。

use std::cell::Cell;

thread_local! {
    static PENDING_ACTIVITY: Cell<Option<Activity>> = const { Cell::new(None) };
    static HAS_NEW: Cell<bool> = const { Cell::new(false) };
    static CURRENT_ACTIVITY: Cell<Activity> = const { Cell::new(Activity::Home) };
    static STATUS_EXTRA: std::cell::RefCell<Option<(String, String)>> = const { std::cell::RefCell::new(None) };
}

/// 活动栏当前高亮项。
pub fn set_current_activity(a: Activity) {
    CURRENT_ACTIVITY.with(|it| it.set(a));
}

pub fn current_activity() -> Activity {
    CURRENT_ACTIVITY.with(|it| it.get())
}

/// 请求根页跳到活动栏的某一项。
pub fn request_activity(a: Activity) {
    PENDING_ACTIVITY.with(|it| it.set(Some(a)));
}

/// 取走待处理的跳转请求（没有则返回 `None`）。
pub fn take_activity() -> Option<Activity> {
    PENDING_ACTIVITY.with(|it| it.take())
}

/// 根页报告「有没有未读消息」，供活动栏红点用。
pub fn set_has_new(v: bool) {
    HAS_NEW.with(|it| it.set(v));
}

pub fn has_new() -> bool {
    HAS_NEW.with(|it| it.get())
}

/// 页面往状态栏左侧追加两项（`(左边项, 右边项)`，如「N 首曲目」「选中曲目」）。
/// 每帧开头由 `MainScene` 清空，页面在 render/update 里写入。`None` 表示清空。
pub fn set_status_extra(v: Option<(String, String)>) {
    STATUS_EXTRA.with(|it| *it.borrow_mut() = v);
}

pub fn status_extra() -> Option<(String, String)> {
    STATUS_EXTRA.with(|it| it.borrow().clone())
}


// ============================================================
// 页面级背景：**iOS 分组底纯色**
// ============================================================
//
// 【2026-10-09 改版】原来是「恒定深黑灰底 → TriGrid 光波 → 三团环境光 → 暗角」。
// 用户要求改成 iOS 18 平面风格后，这一整条全部退役：
//
//   * 光波（TriGrid）是**加色辉光**，只有在深色底上才成立 —— 压到 `#F2F2F7`
//     这种浅色分组底上什么都看不见，跟平面风格直接冲突；
//   * 三团环境光斑与暗角同样是给「深色氛围」服务的，平面风格里没有位置。
//
// 它是所有页面共用的统一背景入口：`MainScene::render` 每帧调一次，页面内容再画在
// 它上面，所以曲库 / 设置 / 活动 / 消息 / 资源包 / 关于……都走这里。
// 自带背景的场景（打歌 / 结算 / 加载，走 `prpr::scene::draw_background`）
// 与本函数无关，不去动它们。
//
// ⚠️ 浅色主题下页面底现在是**浅色**的（`#F2F2F7`），所以「压在页面底上的写死白字」
// 会失效 —— 那些地方要改用 `th.text*` / `theme::ink`。只有压在封面图 / 曲绘 /
// 视频上的文字才继续写死白。

/// 页面背景：`systemGroupedBackground` 纯色（浅 `#F2F2F7` / 深纯黑）。
///
/// `_m` / `_t` / `base` 三个参数保留只为不改 `MainScene` 的调用点，现在都不使用。
pub fn draw_background(ui: &mut Ui, _m: &Metrics, th: &Theme, _t: f32, base: Option<SafeTexture>) {
    let _ = base;
    // 卡片（`th.bg_elev`）压在它上面，靠明度差分出层级 —— 这就是 iOS 的 inset grouped。
    ui.fill_rect(ui.screen_rect(), th.bg);
}


/// 设计稿的 `.grid-bg`：55px 线网格 + 8s 线性漂移 + 椭圆遮罩。
///
/// **不用 shader 画**。遮罩是二维的，但网格是轴对齐的 —— 把每条线切成若干段、
/// 每段按遮罩取 alpha，精度足够（18 段在 1080p 上每段 34px），而且行为完全可预期。
/// （shader 版试过：`varying lowp` 精度不够、`set_uniform` 还按字节数静默丢弃
/// 类型不符的值，排查成本远高于收益。）
fn draw_grid(ui: &mut Ui, m: &Metrics, th: &Theme, sr: Rect, t: f32) {
    let cell = 55. * m.k; // 屏幕像素
    let drift = (t / 8.).fract() * cell; // 8s 走满一格
    let col = if th.dark {
        Color::new(1., 1., 1., 0.10)
    } else {
        Color::new(0., 0., 0., 0.13)
    };
    let lw = (1.1 * m.k).max(1.1); // 线宽（屏幕像素）
    let k = sr.w / m.w; // 屏幕像素 → 世界坐标
    let sx = |x: f32| sr.x + x * k;
    let sy = |y: f32| sr.y + y * k;

    // 遮罩：radial-gradient(ellipse 88% 80% at 46% 48%, #000 18%, rgba(0,0,0,.45) 62%, transparent 96%)
    let mask = |u: f32, v: f32| -> f32 {
        let dx = (u - 0.46) / 0.88;
        let dy = (v - 0.48) / 0.80;
        let r = (dx * dx + dy * dy).sqrt();
        if r < 0.18 {
            1.
        } else if r < 0.62 {
            1. - 0.55 * (r - 0.18) / 0.44
        } else if r < 0.96 {
            0.45 * (1. - (r - 0.62) / 0.34)
        } else {
            0.
        }
    };

    const SEG: u32 = 18;
    let step = cell * 0.5; // 格内 25% 与 75% 两条 → 间距半格

    // 竖线
    let mut n = ((-cell - cell * 0.25 - drift) / step).floor();
    loop {
        let x = cell * 0.25 + drift + n * step;
        if x >= m.w + cell {
            break;
        }
        if x >= -1. {
            let u = x / m.w;
            for i in 0..SEG {
                let y0 = i as f32 / SEG as f32 * m.h;
                let y1 = (i + 1) as f32 / SEG as f32 * m.h;
                let a = mask(u, (y0 + y1) * 0.5 / m.h);
                if a > 0.004 {
                    ui.fill_rect(
                        Rect::new(sx(x), sy(y0), lw * k, sy(y1) - sy(y0)),
                        Color { a: col.a * a, ..col },
                    );
                }
            }
        }
        n += 1.;
    }

    // 横线
    let mut n = ((-cell - cell * 0.25 - drift) / step).floor();
    loop {
        let y = cell * 0.25 + drift + n * step;
        if y >= m.h + cell {
            break;
        }
        if y >= -1. {
            let v = y / m.h;
            for i in 0..SEG {
                let x0 = i as f32 / SEG as f32 * m.w;
                let x1 = (i + 1) as f32 / SEG as f32 * m.w;
                let a = mask((x0 + x1) * 0.5 / m.w, v);
                if a > 0.004 {
                    ui.fill_rect(
                        Rect::new(sx(x0), sy(y), sx(x1) - sx(x0), lw * k),
                        Color { a: col.a * a, ..col },
                    );
                }
            }
        }
        n += 1.;
    }
}


// ============================================================
// 带羽化边缘的贴图
// ============================================================

/// 一张贴图，边缘按位置做**真正的 alpha 羽化**。
///
/// 设计稿的 `.panel-art` 是靠 CSS `mask-image` 渐隐的。引擎里没有 mask，
/// 常见的「拿背景色盖一条渐变」做法出来是**黑边**而不是羽化（背景是网格+环境光，
/// 不是纯色），所以这里走 `Shading::new_vertex` —— 那是逐顶点回调，
/// 在顶点上按 x / y 位置算 alpha，就能得到真的透明渐变边。
///
/// 顶点要够密才平滑，见 [`draw_image_feathered`]（24×8 网格）。
pub struct FeatherImage {
    tex: Texture2D,
    /// 采样区（归一化 uv）
    src: Rect,
    /// 目标矩形（世界坐标）
    dst: Rect,
    /// 羽化按这个矩形量 —— 一般是**整个立绘区**而不是图片本身，
    /// 这样图片没顶到面板边时不会被误伤。
    fade: Rect,
    color: Color,
    /// 左边缘羽化宽度（占 `fade.w` 比例）
    feather_l: f32,
    /// 上边缘羽化宽度（占 `fade.h` 比例）
    feather_t: f32,
}

impl Shading for FeatherImage {
    fn new_vertex(&self, mat: &Matrix, p: &Point, alpha: f32) -> Vertex {
        let t = mat.transform_point(p);
        let ux = ((p.x - self.dst.x) / self.dst.w).clamp(0., 1.);
        let uy = ((p.y - self.dst.y) / self.dst.h).clamp(0., 1.);

        let mut a = self.color.a;
        if self.feather_l > 1e-5 {
            let e = ((p.x - self.fade.x) / self.fade.w) / self.feather_l;
            a *= e.clamp(0., 1.);
        }
        if self.feather_t > 1e-5 {
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

/// 把一张贴图按 `dst` 铺开，左 / 上边缘做 alpha 羽化。
#[allow(clippy::too_many_arguments)]
pub fn draw_image_feathered(
    ui: &mut Ui,
    tex: Texture2D,
    src: Rect,
    dst: Rect,
    fade: Rect,
    color: Color,
    feather_l: f32,
    feather_t: f32,
) {
    const COLS: u16 = 24;
    const ROWS: u16 = 8;

    let sh = FeatherImage {
        tex,
        src,
        dst,
        fade,
        color,
        feather_l,
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

/// 等比缩放到 `box` 内（`object-fit: contain`），底部对齐 —— 立绘都是「站在地上」的。
pub fn contain_bottom(tex_w: f32, tex_h: f32, b: Rect) -> Rect {
    if tex_w <= 0. || tex_h <= 0. {
        return b;
    }
    let k = (b.w / tex_w).min(b.h / tex_h);
    let w = tex_w * k;
    let h = tex_h * k;
    Rect::new(b.x + (b.w - w) * 0.5, b.bottom() - h, w, h)
}

/// 等比缩放**铺满** `box`（`object-fit: cover` + `object-position: left bottom`），
/// 返回「该从贴图里取哪一块」的归一化 uv 矩形（直接铺到 `box` 上即可，不拉伸、不变形）。
///
/// 对应 kaichang `.left-character{left:-3%; bottom:0; height:100vh}` +
/// `.left-character img{height:100vh; width:auto}` 的语义：竖向铺满、横向从左往右铺开，
/// 右边 / 上边溢出的部分裁掉 —— 于是**左边和上下都顶到边，永远不留缝**。
/// 与 [`contain_bottom`] 的区别：contain 会「留白」（缩放比取 min），cover 会「裁切」（取 max）。
pub fn cover_left_bottom(tex_w: f32, tex_h: f32, b: Rect) -> Rect {
    if tex_w <= 0. || tex_h <= 0. || b.w <= 0. || b.h <= 0. {
        return Rect::new(0., 0., 1., 1.);
    }
    // cover：两个方向都必须盖住 → 缩放比取大的那个
    let k = (b.w / tex_w).max(b.h / tex_h);
    // 贴图里真正露出来的那一块（像素），锚左 + 锚下
    let vis_w = (b.w / k).min(tex_w);
    let vis_h = (b.h / k).min(tex_h);
    Rect::new(0., (tex_h - vis_h) / tex_h, vis_w / tex_w, vis_h / tex_h)
}


// ============================================================
// 外发光 / 透视
// ============================================================

/// 圆角矩形的**外发光**。
///
/// 设计稿里到处都是 `box-shadow: 0 0 Npx rgba(...)`。引擎没有高斯模糊，
/// 这里用「同一形状逐层向外扩 + alpha 递减」近似 —— 对圆角矩形来说观感几乎一致，
/// 成本是 N 个额外的填充（N 取 5~7 就够柔和了）。
pub fn glow_rect(ui: &mut Ui, r: Rect, radius: f32, color: Color, spread: f32, layers: u32) {
    for i in (1..=layers).rev() {
        let t = i as f32 / layers as f32;
        // 平方衰减：外圈淡得快，内圈留一点亮，接近真实辉光
        let a = color.a * (1. - t) * (1. - t) * 0.75;
        if a < 0.005 {
            continue;
        }
        let d = spread * t;
        let rr = r.feather(d);
        ui.fill_path(&rr.rounded(radius + d), Color { a, ..color });
    }
}

/// 矢量图标的**外发光**：先用宽线描一遍低透明度的同色路径。
pub fn glow_icon(ui: &mut Ui, m: &Metrics, icon: Icon, r: Rect, color: Color, stroke_px: f32, spread_px: f32) {
    let def = icon_def(icon);
    if def.fill {
        return;
    }
    let s = (r.w / 24.).min(r.h / 24.);
    ui.stroke_options.start_cap = lyon::path::LineCap::Round;
    ui.stroke_options.end_cap = lyon::path::LineCap::Round;
    ui.stroke_options.line_join = lyon::path::LineJoin::Round;
    for i in (1..=5).rev() {
        let t = i as f32 / 5.;
        // 平方衰减，外圈淡得快
        let a = color.a * (1. - t) * (1. - t) * 0.85;
        if a < 0.004 {
            continue;
        }
        let w = (stroke_px + spread_px * t * 2.) * s;
        for sp in def.subpaths {
            ui.stroke_path(&build_icon_path(sp, r), w, Color { a, ..color });
        }
    }
    let _ = m;
}

/// 文字背后的**柔光**。
///
/// 之前是「按圆周撒采样点、每个点画一份文字」—— 采样点是离散的，出来是一圈
/// **颗粒状斑块**，半径越大越明显（用户原话：「什么狗屎」）。
///
/// 换成：拿文字**包围盒**向外扩几层、alpha 递减。半径到 16px 量级时字形细节
/// 早就糊没了，观感跟真高斯模糊几乎一致，而且完全平滑、零采样噪声。
pub fn text_aura(ui: &mut Ui, bbox: Rect, color: Color, radius: f32, layers: u32) {
    // **层数才是关键**。7 层时相邻圈之间会看出台阶（像一块一块的云），
    // 30+ 层时就是一条连续的 alpha 斜坡，视觉上等价于高斯模糊。
    // 每层 alpha 很低、靠叠加累积，这样外圈自然淡、内圈自然亮。
    let n = layers.max(24) as f32;
    for i in (1..=layers.max(24)).rev() {
        let t = i as f32 / n; // 1 = 最外圈，0 = 贴着文字
        // 二次衰减：越靠外越淡，中心附近叠得最亮
        let a = color.a * (1. - t) * (1. - t) * (1.6 / n);
        if a < 0.0015 {
            continue;
        }
        let d = radius * t;
        let r = bbox.feather(d);
        ui.fill_path(&r.rounded(radius * 0.55 + d), Color { a, ..color });
    }
}

/// 文字柔光：**逐字符**取包围盒向外扩。
///
/// 整段用一个包围盒的话，光晕是个方块 —— 标题这种大字一眼就能看出是块蓝条。
/// 逐字符扩出来才会跟着字形走（字距小的时候相邻字符的柔光自然连成一片）。
#[allow(clippy::too_many_arguments)]
pub fn text_aura_str<'a>(
    ui: &mut Ui<'a>,
    m: &Metrics,
    s: &str,
    x: f32,
    y: f32,
    h: f32,
    size_px: f32,
    color: Color,
    radius: f32,
    _layers: u32,
    font: Option<&'static std::thread::LocalKey<std::cell::RefCell<Option<prpr::ui::TextPainter>>>>,
) {
    let mut cx = x;
    for ch in s.chars() {
        let cs = ch.to_string();
        let w = match font {
            Some(f) => measure_with_font(ui, m, cs.clone(), size_px, f).w,
            None => measure(ui, m, cs.clone(), size_px).w,
        };
        if !ch.is_whitespace() && w > 0. {
            // 每个字画一团**径向渐变**。径向渐变之间是「柔和相加」，
            // 不会像实心方块那样在交叠处留下硬边 —— 这才是柔光的正确做法。
            // （实心多层外扩会糊成一块蓝条，圆周撒点会有颗粒，都试过了。）
            let c = (cx + w * 0.5, y + h * 0.5);
            let r = w * 0.5 + radius;
            let rr = Rect::new(c.0 - r, c.1 - r, r * 2., r * 2.);
            let inner = Color {
                a: color.a * 0.5,
                ..color
            };
            let outer = Color { a: 0., ..color };
            ui.fill_rect(rr, radial(c, r, inner, outer));
        }
        cx += w;
    }
}

/// 用指定字体量一段文字（`text_with_font` 的配套）。
pub fn measure_with_font<'a, 's>(
    ui: &mut Ui<'a>,
    m: &Metrics,
    s: impl Into<Cow<'s, str>>,
    size_px: f32,
    font: &'static std::thread::LocalKey<std::cell::RefCell<Option<prpr::ui::TextPainter>>>,
) -> Rect {
    let t: DrawText<'a, 's, '_> = ui.text(s);
    t.size(m.f(size_px)).no_baseline().measure_using(font)
}

/// 复刻 CSS 的 `perspective(d) + transform-origin:(ox,oy) + rotateY(ry) rotateX(rx)`。
///
/// 设计稿的左右两块面板就是靠这个立起来的（`.panel-left` rotateY(12deg)、
/// `.panel-art` rotateY(-14deg)），少了透视整个版面是平的。
pub fn css_perspective(origin: (f32, f32), ry_deg: f32, rx_deg: f32, d: f32) -> Mat4 {
    let rad = std::f32::consts::PI / 180.;
    let rot = Mat4::from_rotation_y(ry_deg * rad) * Mat4::from_rotation_x(rx_deg * rad);
    let proj = Mat4::from_cols(
        vec4(1., 0., 0., 0.),
        vec4(0., 1., 0., 0.),
        vec4(0., 0., 1., -1. / d.max(0.001)),
        vec4(0., 0., 0., 1.),
    );
    Mat4::from_translation(vec3(origin.0, origin.1, 0.)) * proj * rot * Mat4::from_translation(vec3(-origin.0, -origin.1, 0.))
}

/// 在透视变换下画一段内容。
///
/// 比 `Ui::with_gl` 多做一件事：**同步 `ui.gl_transform`**。
/// `RectButton::set` 是用 `transform * gl_transform` 算命中矩形的，
/// `with_gl` 不改 `gl_transform`，于是按钮的点击区会留在没透视的位置上 ——
/// 透视一明显就会「看着在这、点不着」。
pub fn with_perspective<R>(ui: &mut Ui, mat: Mat4, f: impl FnOnce(&mut Ui) -> R) -> R {
    let old = ui.gl_transform;
    ui.gl_transform = old * mat;
    let gl = unsafe { get_internal_gl() }.quad_gl;
    gl.push_model_matrix(mat);
    let res = f(ui);
    unsafe { get_internal_gl() }.flush();
    gl.pop_model_matrix();
    ui.gl_transform = old;
    res
}


/// 设计稿 `.enter` 的入场进度：`delay` 秒后开始，0.8s 内 0→1，缓出
/// （`cubic-bezier(.2,.7,.3,1)` 用 ease-out-cubic 近似）。
pub fn enter_p(t: f32, delay: f32) -> f32 {
    let p = ((t - delay) / 0.8).clamp(0., 1.);
    1. - (1. - p).powi(3)
}

/// 周期性「呼吸」：返回 0→1→0，周期 `period` 秒，`phase` 用于错开多个元素。
pub fn pulse(t: f32, period: f32, phase: f32) -> f32 {
    ((t / period) * std::f32::consts::TAU + phase).sin() * 0.5 + 0.5
}

/// 把世界坐标的点映射回「透视之前」的坐标。
///
/// 面板套了透视之后，存下来的按钮矩形是**平的**，而鼠标位置是世界坐标 ——
/// 不换算的话悬停高亮会整体偏掉（点击不受影响：`RectButton::set` 自己会乘
/// `ui.gl_transform`）。
pub fn unproject(ui: &Ui, p: (f32, f32)) -> (f32, f32) {
    let inv = ui.gl_transform.inverse();
    let v = inv * vec4(p.0, p.1, 0., 1.);
    if v.w.abs() > 1e-6 {
        (v.x / v.w, v.y / v.w)
    } else {
        p
    }
}
