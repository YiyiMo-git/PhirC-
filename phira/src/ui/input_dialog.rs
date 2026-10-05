//! 游戏内自绘的文本输入框 —— 取代 `inputbox` crate 的原生对话框。
//!
//! # 为什么要自绘
//!
//! `inputbox` 在 Android 上走 `inputbox-android` 的 `.aar`：那个后端**打不开对话框时**
//! 会把失败提示串（例如 `can't open`）当成「用户输入的文本」回调
//! `QuadNative.setInputText()`，于是脏字符串被写进配置
//! （实测：`comboText` 被存成 `"COMBOcan't open "`）。
//!
//! 现在 [`prpr::scene::request_input`] 不再调那个库，只把请求落成
//! [`prpr::scene::PendingInput`]；本模块负责把它画出来、编辑、收尾：
//!
//! * 确定 → [`prpr::scene::finish_pending_input`]（`take_input()` 就能拿到 `(id, text)`）；
//! * 取消 → [`prpr::scene::cancel_pending_input`]（`take_input_cancelled()` 拿到 id）。
//!
//! 所以 30 个调用点的消费链路一行都不用改。
//!
//! # ⚠️ 生效范围（重要，别当成「全局弹窗」）
//!
//! 引擎每帧**只渲染场景栈顶的那个场景**（`prpr::scene::Main::render` →
//! `scenes.last_mut().render(..)`），而本函数只由 `MainScene::render` 调用。因此：
//!
//! * **生效**：`MainScene` 内部发出的请求 —— 设置 / 曲库 / 收藏 / 历史 / 黑名单 /
//!   标签 / 联机面板 / 登录 / RKS 等页面，以及 `tags.rs`、`mp/panel.rs` 这些模块；
//! * **不生效**：独立场景（`phira` 的 `SongScene` / `ProfileScene` /
//!   `ChallengeScene` / `EventScene` / `ChapterScene` / `UnlockScene`，以及
//!   `prpr` 的 `GameScene`）里发出的 `request_input` —— 那些场景在栈顶时
//!   `MainScene::render` 根本不会被调用，请求会一直挂在 [`prpr::scene::PENDING_INPUT`]
//!   里（既没有原生对话框，也没有自绘框）。
//!
//!   要覆盖它们，得把绘制挂到 `prpr::scene::Main::render` 的顶层（那里对所有场景
//!   都在栈顶之上绘制，引擎自带的 `DIALOG` 就是这么做的）。
//!
//! # 交互
//!
//! * 鼠标 / 触摸点按钮；键盘 `Enter` 确定、`Esc` 取消、`Backspace` 删一个字符
//!   （按 `char` 删，不会切坏 UTF-8）、`Ctrl+V` 粘贴、其余可打印字符追加。
//! * 打开时弹软键盘（`show_keyboard`），确定 / 取消时收起。桌面平台该调用是空操作。

use crate::newui::{self, Metrics};
use macroquad::prelude::*;
use prpr::{
    judge::Judge,
    scene::{cancel_pending_input, finish_pending_input, pending_input},
    ui::Ui,
};
use std::cell::RefCell;

/// 正在编辑的输入缓冲。`seq` 对应 [`prpr::scene::PendingInput::seq`]：序号变了说明是
/// 一次新请求，要重新用 `default` 初始化（同一个 id 连续请求两次时，光看 id 会留下
/// 上次的编辑内容）。
struct Edit {
    seq: u64,
    text: String,
}

thread_local! {
    static EDIT: RefCell<Option<Edit>> = const { RefCell::new(None) };
}

/// 弹 / 收软键盘。
///
/// 桌面 / 未实现该后端的平台是**空操作**（miniquad `native.rs` 里的默认实现），
/// 所以这里不做平台判断；Android 走 `MainActivity.showKeyboard(boolean)`，
/// 内部已经把异常全吃掉（`CrashLog.error("showKeyboard failed", t)`），调不炸。
fn set_keyboard(show: bool) {
    unsafe { get_internal_gl() }.quad_context.show_keyboard(show);
}

/// 画输入框并处理交互。`t` 是秒级时间，只用来让光标闪烁。
///
/// 没有待处理请求时**立刻返回**：不画任何东西，也不读键盘 / 鼠标。
pub fn render(ui: &mut Ui, t: f32) {
    let Some(pending) = pending_input() else {
        // 请求在别处（或上一次交互里）已经结束：顺手清掉编辑缓冲。
        EDIT.with(|it| *it.borrow_mut() = None);
        return;
    };

    // ---- 1. 编辑缓冲：新请求才重置 + 弹一次软键盘 ----
    let mut edit = EDIT.with(|it| it.borrow_mut().take());
    if edit.as_ref().is_none_or(|it| it.seq != pending.seq) {
        // macroquad 的字符队列**不随帧清空**（见 macroquad `lib.rs` 的 end_frame：
        // 它只清 keys_pressed / mouse_pressed）。所以打开输入框之前敲的字符会一直
        // 躺在队列里，这里不排掉的话，输入框一打开就会被灌进一堆旧字符。
        while get_char_pressed().is_some() {}
        set_keyboard(true);
        edit = Some(Edit {
            seq: pending.seq,
            text: pending.default.clone(),
        });
    }
    let mut edit = edit.unwrap();

    // ---- 2. 键盘 ----
    // `Some(true)` = 确定，`Some(false)` = 取消；确定 / 取消这一帧就不再收字符了。
    let mut action: Option<bool> = None;
    if is_key_pressed(KeyCode::Escape) {
        action = Some(false);
    } else if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
        // 多行模式下也允许回车确定（简单优先：回车一律 = 确定）。
        action = Some(true);
    } else {
        if (is_key_down(KeyCode::LeftControl) || is_key_down(KeyCode::RightControl)) && is_key_pressed(KeyCode::V) {
            if let Some(text) = unsafe { get_internal_gl() }.quad_context.clipboard_get() {
                edit.text.extend(text.chars().filter(|it| !it.is_control()));
            }
        }
        if is_key_pressed(KeyCode::Backspace) {
            // `String::pop` 按 `char` 删，不会把多字节字符切坏。
            edit.text.pop();
        }
        while let Some(ch) = get_char_pressed() {
            if !ch.is_control() {
                edit.text.push(ch);
            }
        }
    }

    // ---- 3. 绘制 ----
    let m = Metrics::new(ui);
    let th = newui::theme();
    let screen = ui.screen_rect();
    let pad = m.u(28.);
    // 竖屏 / 窄屏上按设计稿宽度会超出屏幕，一律夹进可视区。
    let pw = m.u(520.).min(screen.w * 0.92);
    // 手机上软键盘从下面盖上来，面板整体上移一点（桌面不动）。
    // 键盘实际高度拿不到，这里只是经验值；上移量会夹在「面板不被推出屏幕」的范围内。
    let mobile = cfg!(any(target_os = "android", target_os = "ios"));
    let ph = m.u(290.).min(screen.h * if mobile { 0.72 } else { 0.9 });
    let shift = if mobile {
        (screen.h * 0.12).min((screen.h - ph).max(0.) * 0.5)
    } else {
        0.
    };
    let panel = Rect::new(-pw * 0.5, -ph * 0.5 - shift, pw, ph);

    // 半透明遮罩 + 居中玻璃面板
    ui.fill_rect(screen, Color::new(0., 0., 0., 0.5));
    newui::glass_card(ui, panel, m.u(18.), &th, 0.);

    let x = panel.x + pad;
    let inner_w = panel.w - pad * 2.;
    let mut y = panel.y + pad;

    // 标题
    newui::text(ui, &m, pending.title.as_str(), x, y, 24., th.text, (0., 0.));
    y += m.u(40.);

    // 提示文字：一行放得下就单行；放不下才多行，且多行也超高时退回单行省略号，
    // 免得压到下面的输入框上。
    let prompt_h = m.u(46.);
    if !pending.prompt.is_empty() {
        if newui::measure(ui, &m, pending.prompt.as_str(), 15.).w <= inner_w {
            newui::text(ui, &m, pending.prompt.as_str(), x, y, 15., th.text_dim, (0., 0.));
        } else {
            let multiline_h = ui
                .text(pending.prompt.as_str())
                .size(m.f(15.))
                .no_baseline()
                .max_width(inner_w)
                .multiline()
                .measure()
                .h;
            if multiline_h <= prompt_h {
                newui::text_multiline(ui, &m, pending.prompt.as_str(), x, y, 15., inner_w, th.text_dim);
            } else {
                newui::text_ellipsis(ui, &m, pending.prompt.as_str(), x, y, 15., inner_w, th.text_dim, (0., 0.));
            }
        }
    }
    y += prompt_h;

    // 输入框
    let field = Rect::new(x, y, inner_w, m.u(56.));
    newui::rounded(ui, field, m.u(10.), th.wash(0.06));
    newui::rounded_stroke(ui, field, m.u(10.), m.u(1.), th.wash(0.16));

    // 显示文本：密码打成 `*`；多行只显示最后一行；过长时从头部裁掉（滚动到尾部）。
    let size_px = 22.;
    let last_line: &str = if pending.multiline {
        edit.text.rsplit('\n').next().unwrap_or_default()
    } else {
        edit.text.as_str()
    };
    let shown: String = if pending.password {
        "*".repeat(last_line.chars().count())
    } else {
        last_line.to_owned()
    };
    let max_w = field.w - m.u(28.);
    // `start` = 可见部分的起始字节位置；0 表示整段都放得下。
    let mut start = 0usize;
    if newui::measure(ui, &m, shown.as_str(), size_px).w > max_w {
        // 从尾部往前累加宽度，多出来的部分整字砍掉（不切坏 UTF-8）。
        let mut acc = 0.;
        for (i, ch) in shown.char_indices().rev() {
            let w = newui::measure(ui, &m, ch.to_string(), size_px).w;
            if acc + w > max_w {
                start = i + ch.len_utf8();
                break;
            }
            acc += w;
            start = i;
        }
    }
    let visible = if start > 0 { format!("…{}", &shown[start..]) } else { shown.clone() };
    let tx = field.x + m.u(14.);
    let vr = newui::text(ui, &m, visible.as_str(), tx, field.center().y, size_px, if pending.password { th.text_dim } else { th.text }, (0., 0.5));
    // 光标（闪烁）
    if (t * 1.6).fract() < 0.55 {
        let cx = (tx + vr.w + m.u(3.)).min(field.right() - m.u(8.));
        ui.fill_rect(Rect::new(cx, field.center().y - m.u(11.), m.u(2.), m.u(22.)), if th.dark { th.accent_hi } else { th.accent });
    }

    // ---- 4. 按钮 ----
    let btn_h = m.u(54.);
    let by = panel.bottom() - pad - btn_h;
    let ok_r = Rect::new(panel.right() - pad - inner_w * 0.48, by, inner_w * 0.48, btn_h);
    let cancel_r = Rect::new(x, by, inner_w * 0.34, btn_h);

    // 悬停（桌面）：`mouse_world` 给世界坐标，`unproject` 抵消可能存在的 gl 变换。
    let (mx, my) = newui::mouse_world();
    let mp = newui::unproject(ui, (mx, my));
    let hovered = |r: Rect| r.contains(vec2(mp.0, mp.1));
    // 点击 / 触摸命中：和 `GameScene` 的暂停面板同一套写法（读本帧的原始触摸）。
    let touches = Judge::get_touches();
    let clicked = |r: Rect| touches.iter().any(|it| it.phase == TouchPhase::Started && r.contains(it.position));

    let hp_cancel = if hovered(cancel_r) { 1. } else { 0. };
    let hp_ok = if hovered(ok_r) { 1. } else { 0. };
    newui::draw_sec_btn(ui, &m, &th, cancel_r, hp_cancel, pending.cancel_label.as_str(), newui::Icon::X);
    newui::draw_main_btn(ui, &m, &th, ok_r, hp_ok, pending.ok_label.as_str(), "");

    if action.is_none() {
        if clicked(ok_r) {
            action = Some(true);
        } else if clicked(cancel_r) {
            action = Some(false);
        }
    }

    // ---- 5. 收尾 ----
    match action {
        Some(true) => {
            set_keyboard(false);
            finish_pending_input(edit.text);
        }
        Some(false) => {
            set_keyboard(false);
            cancel_pending_input();
        }
        None => EDIT.with(|it| *it.borrow_mut() = Some(edit)),
    }
}
