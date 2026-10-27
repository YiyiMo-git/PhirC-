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
//! # 生效范围
//!
//! 绘制回调由 `phira` 启动时注册到 [`prpr::scene::set_input_dialog_renderer`]，
//! 引擎把它挂在 `prpr::scene::Main::render` 的 `top_level` 分支（和引擎自带 `DIALOG`
//! 同一位置），所以**对所有场景都生效** —— 包括独立场景（`GameScene` 的练习模式
//! 时间输入、`SongScene` 的举报理由 / 稳定度评论、`ProfileScene` 的转移邮箱等）。
//!
//! ⚠️ 模态拦截（触摸不穿透到底下场景）**在引擎层做**：`prpr::scene::Main::touch`
//! 的触摸派发里判 `has_pending_input()`，命中就 `return false`（保留触摸、不派发）。
//! 只写在 `MainScene::touch` 里是不够的 —— 栈顶不是 `MainScene` 时那行根本不会执行，
//! 表现就是「在练习模式 / 举报框里点确定，底下的场景也被点了一下」。
//!
//! # 交互
//!
//! * 鼠标 / 触摸点按钮；键盘 `Enter` 确定、`Esc` 取消、`Backspace` 删一个字符
//!   （按 `char` 删，不会切坏 UTF-8）、`Ctrl+V` 粘贴、其余可打印字符追加。
//! * 打开时弹软键盘（`show_keyboard`），确定 / 取消时收起。桌面平台该调用是空操作。

use crate::newui::{self, Metrics};
use macroquad::prelude::*;
use prpr::{
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
    // ⚠️ 两条路都要走：
    //   * `miniquad::show_keyboard` 只有 **Android** 实现了（调 activity.showKeyboard），
    //     桌面 / iOS 都是 `native.rs` 里的空实现；
    //   * iOS 因此**必须**走 `crate::ime` 那条（ObjC 隐藏 UITextField + becomeFirstResponder）。
    // 不两条都调的话，iOS 上键盘永远不会弹（用户实测「iOS 无法唤出键盘输入」）。
    crate::ime::show(show);
    unsafe { get_internal_gl() }.quad_context.show_keyboard(show);
}

/// 画输入框并处理交互。`t` 是秒级时间，只用来让光标闪烁。
///

/// 把软键盘事件并进输入缓冲。返回 `true` 表示要求「确定」（软键盘按了完成 / 回车）。
///
/// 抽成纯函数是为了**能单测** —— 之前它内联在渲染函数里，嵌错一层都没人发现。
/// 把软键盘事件并进文本。返回 `true` 表示「完成」（= 确定）。
///
/// `multiline` 决定软键盘的「完成 / 换行」键怎么处理：单行 = 确定，多行 = 换行
/// （与物理回车键一致，否则多行输入框永远拿不到换行）。
fn apply_ime_events(text: &mut String, events: Vec<crate::ime::ImeEvent>, multiline: bool) -> bool {
    let mut confirm = false;
    for ev in events {
        match ev {
            crate::ime::ImeEvent::Char('\r') | crate::ime::ImeEvent::Char('\n') => {
                if multiline {
                    text.push('\n');
                } else {
                    confirm = true;
                }
            }
            crate::ime::ImeEvent::Char(c) if !c.is_control() => text.push(c),
            // 其余控制字符（含其它不可见字符）：不写进文本、也不确认，吞掉。
            // ⚠️ 必须有这一臂：带 guard 的 `Char(c) if !c.is_control()` 不计入穷尽性，
            // 没有它编译器会报 `ImeEvent::Char(..)` 未覆盖（控制字符那部分）。
            crate::ime::ImeEvent::Char(_) => {}
            crate::ime::ImeEvent::Backspace => {
                text.pop();
            }
            // 软键盘「完成 / 确定」（Android 的 `performEditorAction`、统一走安全通道）。
            // 语义 = 确定，与面板「确定」按钮等价；无论单行 / 多行都直接确认
            // （多行的换行走软键盘自身的回车键或面板按钮，不靠这个动作键）。
            crate::ime::ImeEvent::Confirm => {
                confirm = true;
            }
        }
    }
    confirm
}

/// 没有待处理请求时**立刻返回**：不画任何东西，也不读键盘 / 鼠标。
pub fn render(ui: &mut Ui, t: f32) {
    let Some(pending) = pending_input() else {
        // 请求在别处（或上一次交互里）已经结束：顺手清掉编辑缓冲。
        // 如果刚才键盘还是弹起的（EDIT 还是 Some），这里一并把它收起 —— 否则外部
        // 取消 / 消费掉请求时，键盘会卡在屏幕上（「输入框关了、软键盘还顶在那儿」）。
        // 用 was_open 做门控，避免每帧（绝大多数帧都没有待处理请求）都去调一次收键盘。
        let was_open = EDIT.with(|it| it.borrow().is_some());
        EDIT.with(|it| *it.borrow_mut() = None);
        if was_open {
            set_keyboard(false);
        }
        // 顺便丢掉残留的软键盘事件 —— 否则输入框关掉期间按的键会漏进**下一次**输入。
        let _ = crate::ime::drain();
        return;
    };

    // ---- 1. 编辑缓冲：新请求才重置 + 弹一次软键盘 ----
    let mut edit = EDIT.with(|it| it.borrow_mut().take());
    if edit.as_ref().is_none_or(|it| it.seq != pending.seq) {
        // macroquad 的字符队列**不随帧清空**（见 macroquad `lib.rs` 的 end_frame：
        // 它只清 keys_pressed / mouse_pressed）。所以打开输入框之前敲的字符会一直
        // 躺在队列里，这里不排掉的话，输入框一打开就会被灌进一堆旧字符。
        while get_char_pressed().is_some() {}
        // 软键盘事件同理：`ime` 的缓冲区也是「攒着等人取」，输入框关着的时候
        // Android 的 deleteSurroundingText / iOS 的回调照样会往里塞。不排掉就会
        // 漏进**下一次**输入（凭空少几个字）。注意 render 在无请求时根本不会被调用，
        // 所以「无请求时清理」那条路指望不上，必须在**开新框**时清。
        let _ = crate::ime::drain();
        set_keyboard(true);
        // 把初始文本也给 iOS 的隐藏输入框一份（顺序无所谓：两者都是
        // `dispatch_async` 到主队列，FIFO 执行）。不做这一步的话，隐藏框是空的，
        // 用户按退格时 UIKit 认为「没东西可删」→ **原有的 `RECALL` 删不掉**。
        crate::ime::set_text(&pending.default);
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
    } else if (is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter)) && !pending.multiline {
        // 单行：回车 = 确定。
        // 多行：回车**换行**（确定走面板上的「确定」按钮，或软键盘的「完成」）——
        // 否则 `InputMode::Multiline` 的调用点（举报理由 / 收藏描述 / 谱面信息）
        // 永远拿不到换行，多行模式形同虚设。
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
        if pending.multiline && (is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter)) {
            edit.text.push('\n');
        }
        while let Some(ch) = get_char_pressed() {
            if !ch.is_control() {
                edit.text.push(ch);
            }
        }
        // iOS 软键盘送来的事件（见 `crate::ime`）。非 iOS 平台这里恒为空。
        //
        // ⚠️⚠️ 这段**必须放在 `while` 循环之外**！
        // 曾经的写法是嵌在 `while let Some(ch) = get_char_pressed()` 的循环体里 ——
        // 而 iOS 的软键盘**不经过 macroquad 的字符队列**，那个 while 一次都不会进入，
        // 于是 `drain()` 永远不被调用，表现就是「键盘能唤出、但一个字也输不进去」。
        let ime_events = crate::ime::drain();
        if !ime_events.is_empty() {
            // 记进 crash-log（设置→日志 / Xcode 下载容器都能看）：
            // 有这行 = ObjC 那条回传通了，问题在别处；没有 = 委托没被调到。
            crate::diag(&format!("软键盘回传 {} 个事件", ime_events.len()));
        }
        if apply_ime_events(&mut edit.text, ime_events, pending.multiline) {
            action = Some(true); // 软键盘的「完成」= 确定
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
    // 输入框 + 右侧两个快捷按钮（**退格 / 清空**）。
    //
    // 为什么必须有它们：移动端软键盘的退格行为**不可靠**（我们自己就踩过 iOS
    // 「空文本框根本不投递删除键」），而且删长文本只能一个个按。
    // 这两个按钮是**纯 Rust 侧**的操作，完全不经过输入法，任何平台都必然可用。
    let quick_w = m.u(84.);
    let quick_gap = m.u(10.);
    let field = Rect::new(x, y, inner_w - quick_w * 2. - quick_gap * 2., m.u(56.));
    let back_r = Rect::new(field.right() + quick_gap, y, quick_w, field.h);
    let clear_r = Rect::new(back_r.right() + quick_gap, y, quick_w, field.h);
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
    // ⚠️ 必须用**引擎本帧的触摸**，不能用 `Judge::get_touches()`：
    // Judge 只在游戏内有数据，主菜单 / 设置 / 曲库里恒为空 → 确定/取消永远点不到
    // → 弹窗卡死 → 模态守卫吃掉全界面点击（用户实测「什么都点不动」）。
    let touches = ui.touches().to_vec();  // 拷一份，避免和下面的可变绘制借用冲突
    let clicked = |r: Rect| touches.iter().any(|it| it.phase == TouchPhase::Started && r.contains(it.position));

    let hp_cancel = if hovered(cancel_r) { 1. } else { 0. };
    let hp_ok = if hovered(ok_r) { 1. } else { 0. };
    newui::draw_sec_btn(ui, &m, &th, cancel_r, hp_cancel, pending.cancel_label.as_str(), newui::Icon::X);
    newui::draw_main_btn(ui, &m, &th, ok_r, hp_ok, pending.ok_label.as_str(), "");
    // 快捷按钮：退格（左尖括号）/ 清空
    let hp_back = if hovered(back_r) { 1. } else { 0. };
    let hp_clear = if hovered(clear_r) { 1. } else { 0. };
    // 文案走 `common.ftl`（本模块不单独建 ftl：`ttl!` 是 crate 根为 `common.ftl`
    // 声明的宏，`ttl!` 的 key 在 `common.ftl` 里）。
    newui::draw_sec_btn(ui, &m, &th, back_r, hp_back, &ttl!("backspace"), newui::Icon::ChevronLeft);
    newui::draw_sec_btn(ui, &m, &th, clear_r, hp_clear, &ttl!("clear"), newui::Icon::X);

    if action.is_none() {
        if clicked(ok_r) {
            action = Some(true);
        } else if clicked(cancel_r) {
            action = Some(false);
        } else if clicked(back_r) {
            // 退格：`String::pop` 按 `char` 删，不会切坏多字节字符。
            edit.text.pop();
            // ⚠️ 必须同步给 iOS 的隐藏输入框：它的文本现在会累积，不同步的话
            // 两边长度会越差越多，之后按软键盘退格就会「删到别的东西」。
            crate::ime::set_text(&edit.text);
        } else if clicked(clear_r) {
            edit.text.clear();
            crate::ime::set_text(&edit.text);
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

#[cfg(test)]
mod ime_apply_tests {
    use super::*;
    use crate::ime::ImeEvent;

    #[test]
    fn chars_append_and_backspace_pops() {
        let mut s = String::from("RE");
        assert!(!apply_ime_events(&mut s, vec![ImeEvent::Char('C'), ImeEvent::Char('A')], false));
        assert_eq!(s, "RECA");
        assert!(!apply_ime_events(&mut s, vec![ImeEvent::Backspace], false));
        assert_eq!(s, "REC");
    }

    #[test]
    fn return_requests_confirm() {
        let mut s = String::new();
        assert!(apply_ime_events(&mut s, vec![ImeEvent::Char('\r')], false));
        assert!(apply_ime_events(&mut s, vec![ImeEvent::Char('\n')], false));
        // 回车本身不写进文本
        assert!(s.is_empty());
    }

    #[test]
    fn control_chars_are_ignored() {
        let mut s = String::new();
        assert!(!apply_ime_events(&mut s, vec![ImeEvent::Char('\u{7}'), ImeEvent::Char('A')], false));
        assert_eq!(s, "A");
    }

    #[test]
    fn backspace_on_empty_is_safe() {
        let mut s = String::new();
        assert!(!apply_ime_events(&mut s, vec![ImeEvent::Backspace], false));
        assert!(s.is_empty());
    }

    #[test]
    fn confirm_event_requests_confirm_and_appends_nothing() {
        // 软键盘「完成」= 确定，且不像回车那样往文本里塞换行。
        let mut s = String::from("hi");
        assert!(apply_ime_events(&mut s, vec![ImeEvent::Confirm], false));
        assert_eq!(s, "hi");
    }

    #[test]
    fn confirm_event_is_distinct_from_return() {
        // Confirm 与 Char('\r') 应产生相同的最终结果（都确认），但走的是
        // 不同事件，确认两者都被正确处理、不会漏。
        let mut a = String::new();
        let mut b = String::new();
        assert!(apply_ime_events(&mut a, vec![ImeEvent::Confirm], false));
        assert!(apply_ime_events(&mut b, vec![ImeEvent::Char('\r')], false));
        assert_eq!(a, b);
    }
}
