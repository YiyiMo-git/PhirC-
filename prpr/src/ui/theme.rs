//! 视觉 token —— 颜色 / 字号 / 间距 / 圆角的**单一真源**。
//!
//! 改风格只改这一个文件；页面里不要再散写 `Color::new(...)` 的字面量。
//! 从 §8.1 的教训来：不做模糊 / 后处理 / 玻璃，只做**纯色 + 层级**。
//!
//! ## 坐标系
//!
//! UI 世界坐标里**屏幕宽 = 2 个单位**，所以 `1 单位 = 视口宽/2 px`
//! （1920 宽时 1 单位 = 960px）。下面所有长度都按这个换算写。
//!
//! ## 两套字号尺度，别混
//!
//! * **菜单 / 页面 UI** → 本模块的 [`text_size`]（0.3 ~ 0.6 量级）；
//! * **打歌界面内部** → `prpr::scene::game` 的 `IN_GAME_TEXT_H`（另一套尺度，
//!   有它自己的换算注释）。判定文字 / HUD 那套不要引用本模块的字号。

use macroquad::prelude::Color;

/// 底色 / 面板色（不透明的实色）。
///
/// 命名按"从下往上"的层级来：`BASE` 是最底、`SURFACE` 是常规面板、
/// `SURFACE_HI` 是次级卡片与悬停态。三级之间只差明度，不差色相 —— 
/// 层级靠明度差做，不靠边框和阴影堆。
pub mod color {
    use macroquad::prelude::Color;

    /// 最底层深底 `#0D0D0D`（和 New UI 的 `--bg` 对齐）。
    pub const BASE: Color = Color::new(0.051, 0.051, 0.051, 1.);

    /// 常规面板 / 按钮底 `#191C1F`。`Ui::background()` 用的就是它，
    /// 全 App 的面板（设置、登录、评分、联机面板、弹窗…）都走这里。
    pub const SURFACE: Color = Color::new(0.098, 0.110, 0.122, 1.);

    /// 次级卡片 / 悬停 / 选中的面板 `#23272B`。比 `SURFACE` 亮一档。
    pub const SURFACE_HI: Color = Color::new(0.153, 0.176, 0.212, 1.);

    /// 强调色 `#0086E8`（和 New UI 的 `--accent` 对齐，整体提亮过一档）。
    ///
    /// 注意：App 里真正的"主题色"其实是**谱面难度色 / 章节色**
    /// （`res_pack.info.color_perfect()` 那套），这个 accent 只是兜底。
    pub const ACCENT: Color = Color::new(0., 0.525, 0.910, 1.);
}

/// 文字的三级灰阶 —— 都是**白色的透明度**，不是灰色实色。
///
/// 为什么用白色透明度而不是灰值：绝大部分文字直接压在插图背景上（不在面板里），
/// 灰值会随背景明暗一起崩掉对比度；白色透明度至少保证"越亮越清楚"。
/// 面板里的文字用同一组值也够（`SURFACE` 明度很低，白字透过率等效于高对比）。
pub mod text_alpha {
    /// 主文字（标题、正文、数值）。不要用 1.0 —— 纯白在深底上会"炸"，
    /// 0.94 既干净又留一点柔度。
    pub const PRIMARY: f32 = 0.94;
    /// 次级文字（副标题、单位、非重点数值）。
    pub const SECONDARY: f32 = 0.66;
    /// 说明 / 弱化 / 禁用（提示语、时间戳、占位符）。
    pub const FAINT: f32 = 0.40;
}

/// 描边 / 分隔线的透明度（同样按白色算）。
pub mod stroke_alpha {
    /// 面板外框。只做"有边界感"，不要变成亮边框。
    pub const BORDER: f32 = 0.10;
    /// 分隔线（列表行之间）。
    pub const DIVIDER: f32 = 0.06;
}

/// 菜单 / 页面 UI 的字号三档。
///
/// 取值来自项目里已有的习惯值，不是拍脑袋：
/// `TITLE` = 设置页 `render_title` 的 `TITLE_SIZE`，
/// `CAPTION` = 它的 `SUBTITLE_SIZE`，`BUTTON` = `Ui::button`。
pub mod text_size {
    /// 页面标题 / 卡片主标题。
    pub const TITLE: f32 = 0.6;
    /// 正文 / 列表项 / 主要数值。
    pub const BODY: f32 = 0.5;
    /// 按钮文字。
    pub const BUTTON: f32 = 0.42;
    /// 说明 / 副标题 / 单位。
    pub const CAPTION: f32 = 0.35;
}

/// 间距刻度 —— 8px 栅格。
///
/// 1920 宽下 1 单位 = 960px，所以 `0.008 ≈ 8px`。页面留白只用这四档，
/// 别再出现 0.013 / 0.027 这种"再调调"出来的数。
pub mod space {
    /// 8px：图标与文字之间、紧凑行内。
    pub const XS: f32 = 0.008;
    /// 16px：控件之间。
    pub const S: f32 = 0.016;
    /// 24px：卡片内边距 / 分组之间。
    pub const M: f32 = 0.024;
    /// 40px：大分区之间 / 页面外边距。
    pub const L: f32 = 0.040;
}

/// 圆角三档（单位同坐标换算，`0.01` ≈ 9.6px @1920）。
pub mod radius {
    /// 小控件（chip / 输入框 / 滑条）。
    pub const CHIP: f32 = 0.008;
    /// 卡片。
    pub const CARD: f32 = 0.012;
    /// 大面板 / 弹窗。
    pub const PANEL: f32 = 0.020;
}

/// UI 的「墨色」：深色主题是白、浅色主题是近黑。
///
/// 老页面里满屏的 `semi_white(x)` / 不设色的 `ui.text()` 都是按「压在深底上」
/// 设计的；浅色主题下必须整批翻成深色，否则就是白字压白底。
#[inline]
pub fn ink(alpha: f32) -> Color {
    if is_light() {
        Color::new(0.055, 0.078, 0.106, alpha)
    } else {
        Color::new(1., 1., 1., alpha)
    }
}

/// 主文字色（[`ink`] × [`text_alpha::PRIMARY`]）。
#[inline]
pub fn text_primary() -> Color {
    ink(text_alpha::PRIMARY)
}

/// 次级文字色。
#[inline]
pub fn text_secondary() -> Color {
    ink(text_alpha::SECONDARY)
}

/// 说明 / 弱化文字色。
#[inline]
pub fn text_faint() -> Color {
    ink(text_alpha::FAINT)
}


/// **kaichang 玻璃语言**（`kaichang/index.html` 的 `.glass-btn`）。
///
/// 引擎里没有 `backdrop-filter: blur()`，但页面背景已经是「纯色底 + 光波」这种
/// 低频、柔和的图案，所以「半透明面板 + 顶部高光 + 1px 浅边 + 柔和阴影」四件套
/// 出来就是磨砂玻璃观感，这也是开始界面（`phira/src/page/home.rs`）用的同一套配方。
///
/// `hp` = 悬停进度（0~1）：悬停时底板提亮、描边变亮，配合调用方的轻微上浮。
pub mod glass {
    use macroquad::prelude::Color;

    /// 玻璃底板。浅色主题 = 72% 白（Win11 亚克力白），深色主题 = 6% 白。
    #[inline]
    pub fn plate(hp: f32) -> Color {
        let hp = hp.clamp(0., 1.);
        if super::is_light() {
            Color::new(1., 1., 1., 0.62 + 0.16 * hp)
        } else {
            Color::new(1., 1., 1., 0.055 + 0.05 * hp)
        }
    }

    /// 选中 / 主操作态的玻璃底板：强调色薄涂 + 玻璃（**浅色主题下也够深，白字读得出**）。
    #[inline]
    pub fn plate_accent(hp: f32) -> Color {
        let hp = hp.clamp(0., 1.);
        let a = if super::is_light() { 0.86 + 0.06 * hp } else { 0.30 + 0.10 * hp };
        let c = super::accent();
        Color::new(c.r, c.g, c.b, a)
    }

    /// 玻璃描边：深色主题加白、浅色主题加黑（1px 的「玻璃边」）。
    #[inline]
    pub fn border(hp: f32) -> Color {
        let hp = hp.clamp(0., 1.);
        if super::is_light() {
            Color::new(0.055, 0.078, 0.106, 0.10 + 0.10 * hp)
        } else {
            Color::new(1., 1., 1., 0.16 + 0.18 * hp)
        }
    }

    /// 选中态的描边：强调色。
    #[inline]
    pub fn border_accent(hp: f32) -> Color {
        let hp = hp.clamp(0., 1.);
        let c = super::accent();
        Color::new(c.r, c.g, c.b, 0.62 + 0.30 * hp)
    }

    /// 顶部那一道玻璃反光。
    #[inline]
    pub fn highlight(hp: f32) -> Color {
        let hp = hp.clamp(0., 1.);
        if super::is_light() {
            Color::new(1., 1., 1., 0.55 + 0.25 * hp)
        } else {
            Color::new(1., 1., 1., 0.10 + 0.08 * hp)
        }
    }

    /// 玻璃面板的柔和阴影强度（`box-shadow: 0 4px 18px rgba(0,0,0,.12)`）。
    #[inline]
    pub fn shadow(hp: f32) -> f32 {
        let hp = hp.clamp(0., 1.);
        if super::is_light() {
            0.26 + 0.14 * hp
        } else {
            0.55 + 0.15 * hp
        }
    }
}

/// 当前主题的强调色（浅色 = Win11 蓝 `#0B6BCB`，深色 = `#0086E8`）。
#[inline]
pub fn accent() -> Color {
    if is_light() {
        light::ACCENT
    } else {
        color::ACCENT
    }
}

/// 浅色主题下的面板色。
///
/// 设置页 / 曲库这些老页面全走 [`super::Ui::background`]，而那个方法原来
/// 写死返回深色 `SURFACE` —— 切到浅色主题时整块卡片还是深灰，压在浅底上像块补丁。
/// 这里给一套浅色值，由客户端的 New UI 主题开关（`phira::newui::set_dark`）驱动。
pub mod light {
    use macroquad::prelude::Color;

    /// 面板底：纯白，和页面底 (#f2f4f7) 拉开一档。
    pub const SURFACE: Color = Color::new(1., 1., 1., 1.);
    /// 次级卡片 / 悬停：浅灰蓝。
    pub const SURFACE_HI: Color = Color::new(0.914, 0.937, 0.961, 1.);
    /// 强调色 `#0B6BCB`。
    pub const ACCENT: Color = Color::new(0.043, 0.42, 0.796, 1.);
}

use std::sync::atomic::{AtomicBool, Ordering};

static LIGHT_UI: AtomicBool = AtomicBool::new(false);

/// 由客户端的主题开关写入。
pub fn set_light(v: bool) {
    LIGHT_UI.store(v, Ordering::Relaxed);
}

pub fn is_light() -> bool {
    LIGHT_UI.load(Ordering::Relaxed)
}
