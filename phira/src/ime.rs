//! iOS 软键盘桥。
//!
//! 背景：`miniquad` 的 `Context::show_keyboard` **只有 Android 实现了**
//! （`android.rs` 调 `activity.showKeyboard`），其余平台走 `native.rs` 的空实现。
//! 所以 iOS 上光调 `show_keyboard(true)` 什么都不会发生 —— 键盘永远不弹。
//!
//! 这里配合 `ios/ime.m` 里的隐藏 `UITextField`：
//!   * [`show`] → `phira_ime_show(1)` → `becomeFirstResponder` → 系统键盘弹出；
//!   * ObjC 在 `shouldChangeCharactersInRange` 里把每个字符回调到
//!     [`phira_ime_on_char`] / [`phira_ime_on_backspace`]，存进下面的缓冲区；
//!   * 自绘输入框每帧 [`drain`] 一次，把事件并进自己的文本。
//!
//! 缓冲区只在 iOS / Android 上被写入（对应的 ObjC 回调 / JNI 才调 `push_*`），
//! 桌面平台恒为空，所以这个模块可以无条件编进所有平台，`cargo check` 就能覆盖它。

use std::sync::Mutex;

/// 软键盘产生的一个事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImeEvent {
    /// 输入了一个字符。
    Char(char),
    /// 退格。
    Backspace,
    /// 软键盘「完成 / 确定」（Android 的 `performEditorAction` 走这里，
    /// 绕开 macroquad 的键队列 —— 否则 IME 线程 push / 渲染线程 pop 会数据竞争）。
    Confirm,
}

static IME_BUFFER: Mutex<Vec<ImeEvent>> = Mutex::new(Vec::new());


/// ObjC 每输入一个字符回调一次（**只由 iOS 的 `ime.m` 调用**）。
#[no_mangle]
pub extern "C" fn phira_ime_on_char(c: u32) {
    if let Some(ch) = char::from_u32(c) {
        if let Ok(mut buf) = IME_BUFFER.lock() {
            buf.push(ImeEvent::Char(ch));
        }
    }
}

/// ObjC 的退格回调（**只由 iOS 的 `ime.m` 调用**）。
#[no_mangle]
pub extern "C" fn phira_ime_on_backspace() {
    if let Ok(mut buf) = IME_BUFFER.lock() {
        buf.push(ImeEvent::Backspace);
    }
}

/// 取出并清空当前累积的软键盘事件（自绘输入框每帧调一次）。
pub fn drain() -> Vec<ImeEvent> {
    IME_BUFFER.lock().map(|mut it| std::mem::take(&mut *it)).unwrap_or_default()
}

/// 一次压入 N 个退格事件（**Android** 的「删除 N 个字符」走这里）。
///
/// 为什么不让 Java 连发 N 次 `surfaceOnKeyDown(KEYCODE_DEL)`：那 N 个事件会在**同一帧**
/// 入队，而 macroquad 的 `keys_pressed` 是 `HashSet` —— 同一个键在同一帧多次按下会
/// 坍缩成一次，`is_key_pressed(KeyCode::Backspace)` 只真一帧，于是只 `pop()` 一个字符。
/// 表现就是「选中一段文字后粘贴 / 输入」只删掉 1 个旧字符，其余残留。
/// 走这个计数通道就绕开了按键坍缩（自绘输入框每帧 `drain()` 一次，顺序与个数都保真）。
///
/// 只有 Android 的 `QuadNative.imeBackspace` 会调它，其他平台（桌面走 Ctrl+V、
/// iOS 走 ObjC 回调）用不到 —— 用 `cfg_attr` 把非 Android 的 dead_code 告警压掉，
/// 否则会破坏「只允许那 5 条既有告警」的约定。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn push_backspaces(n: usize) {
    if n == 0 {
        return;
    }
    if let Ok(mut buf) = IME_BUFFER.lock() {
        buf.reserve(n);
        for _ in 0..n {
            buf.push(ImeEvent::Backspace);
        }
    }
}

/// 软键盘输入一个字符（**Android** 的 `InputConnection.commitText` 走这里）。
///
/// 与 [`push_backspaces`] 共用同一把 `IME_BUFFER` 锁，所以 IME 线程 push / 渲染线程
/// `drain` pop 之间**不再有数据竞争** —— 这正是「移动端输入框有时把整个软件卡死」的
/// 根因：原先 `commitText` 走 macroquad 的 `chars_pressed` 裸队列（`surfaceOnCharacter`），
/// 那个队列在 IME 线程 push / 渲染线程 pop 之间没有任何同步，偶发会损坏并被 `get_char_pressed`
/// 死转，表现就是整软件冻住。现在所有软键盘字符都和退格一样走这条互斥锁安全通道。
///
/// 只有 Android 的 `QuadNative.imeChar` 会调它；其他平台用不到 —— 用 `cfg_attr` 把
/// 非 Android 的 dead_code 告警压掉，否则会破坏「只允许那 5 条既有告警」的约定。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn push_char(c: u32) {
    if let Some(ch) = char::from_u32(c) {
        if let Ok(mut buf) = IME_BUFFER.lock() {
            buf.push(ImeEvent::Char(ch));
        }
    }
}

/// 软键盘「完成 / 确定」（**Android** 的 `InputConnection.performEditorAction` 走这里）。
///
/// 原先走 `surfaceOnKeyDown/Up(KEYCODE_ENTER)`，那个事件照旧进 macroquad 的裸键队列，
/// 同样有 IME 线程 / 渲染线程竞争；统一走 `IME_BUFFER`，和字符、退格同一条安全路径。
/// 语义上 = 确定（与面板「确定」按钮等价），与 [`ImeEvent::Confirm`] 在
/// `input_dialog::apply_ime_events` 里的处理对应。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn push_confirm() {
    if let Ok(mut buf) = IME_BUFFER.lock() {
        buf.push(ImeEvent::Confirm);
    }
}

/// 弹出 / 收起软键盘。
///
/// * iOS：走本模块的 ObjC 通道（`show_keyboard` 在 iOS 上是空实现）。
/// * 其他平台：仍然交给 `miniquad` 的 `show_keyboard`（Android 有实现），
///   本函数**不做事**，由调用方照常调 `show_keyboard`。
pub fn show(show: bool) {
    // ⚠️ 这里**不做** Rust 侧去重。曾经加过一个 `IME_SHOWN` 原子标记，但那会让
    // 「系统把键盘收起（用户点收起/切换 App）后再也弹不出来」—— 因为状态还是 shown。
    // 去重的正确位置在 ObjC：那边用 `isFirstResponder` 判断，既避免重复
    // `becomeFirstResponder`，又能在真丢了第一响应者时重新弹。
    #[cfg(target_os = "ios")]
    {
        // 分步日志：万一 iOS 上「点了输入框就卡死」，crash-log 里能看到到底走到哪一步。
        // 只有 "请求" 没有 "返回" ⇒ 卡在 ObjC 的 becomeFirstResponder 里。
        crate::diag(&format!("ime::show({show}) 请求"));
        unsafe {
            phira_ime_show(if show { 1 } else { 0 });
        }
        crate::diag(&format!("ime::show({show}) 返回"));
    }
    #[cfg(not(target_os = "ios"))]
    let _ = show;
}

/// 把**已有文本**灌进 iOS 的隐藏输入框（打开输入框时用 `pending.default` 调一次）。
///
/// ⚠️ 必须做：Rust 侧一开场就有文本（例如连击文字原有的 `RECALL`），而隐藏框若是空的，
/// 用户按退格时 UIKit 认为「没东西可删」→ **原有内容删不掉**。两边必须从同一份文本起步。
/// 非 iOS 平台是空操作（那两端的软键盘不依赖隐藏框）。
pub fn set_text(text: &str) {
    #[cfg(target_os = "ios")]
    {
        if let Ok(c) = std::ffi::CString::new(text) {
            unsafe {
                phira_ime_set_text(c.as_ptr());
            }
        }
    }
    #[cfg(not(target_os = "ios"))]
    let _ = text;
}

#[cfg(target_os = "ios")]
unsafe extern "C" {
    /// 由 `phira/ios/ime.m` 导出：创建/唤出隐藏 `UITextField`。
    fn phira_ime_show(show: i32);
    /// 由 `phira/ios/ime.m` 导出：把已有文本灌进隐藏输入框。
    fn phira_ime_set_text(text: *const std::ffi::c_char);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_returns_events_once() {
        phira_ime_on_char('A' as u32);
        phira_ime_on_char('中' as u32);
        phira_ime_on_backspace();
        let evs = drain();
        assert_eq!(
            evs,
            vec![ImeEvent::Char('A'), ImeEvent::Char('中'), ImeEvent::Backspace]
        );
        // 取过一次就空了
        assert!(drain().is_empty());
    }

    #[test]
    fn invalid_scalar_is_ignored() {
        phira_ime_on_char(0xD800); // 代理区，不是合法 char
        assert!(drain().is_empty());
    }

    #[test]
    fn push_char_routes_through_buffer() {
        // 模拟 Android 的 `commitText`：UTF-32 码点，带上中文与 emoji。
        push_char('A' as u32);
        push_char('中' as u32);
        push_char('😀' as u32);
        let evs = drain();
        assert_eq!(
            evs,
            vec![ImeEvent::Char('A'), ImeEvent::Char('中'), ImeEvent::Char('😀')]
        );
        // 取过一次就空了（和 ObjC 回调走同一条缓冲区，语义一致）
        assert!(drain().is_empty());
    }

    #[test]
    fn push_confirm_is_distinct_from_enter() {
        // 软键盘「完成」必须是一个独立事件，不能被当成字符 / 退格。
        push_confirm();
        let evs = drain();
        assert_eq!(evs, vec![ImeEvent::Confirm]);
    }

    #[test]
    fn push_char_and_backspace_interleave_in_order() {
        push_char('X' as u32);
        push_backspaces(1);
        push_char('Y' as u32);
        let evs = drain();
        assert_eq!(evs, vec![ImeEvent::Char('X'), ImeEvent::Backspace, ImeEvent::Char('Y')]);
    }
}
