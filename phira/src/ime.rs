//! iOS 软键盘桥。
//!
//! 背景：`miniquad` 的 `Context::show_keyboard` **只有 Android 实现了**
//! （`android.rs` 调 `activity.showKeyboard`），其余平台走 `native.rs` 的空实现。
//! 所以 iOS 上光调 `show_keyboard(true)` 什么都不会发生 —— 键盘永远不弹。
//!
//! 这里配合 `ios/webui.m` 里的隐藏 `UITextField`：
//!   * [`show`] → `phira_ime_show(1)` → `becomeFirstResponder` → 系统键盘弹出；
//!   * ObjC 在 `shouldChangeCharactersInRange` 里把每个字符回调到
//!     [`phira_ime_on_char`] / [`phira_ime_on_backspace`]，存进下面的缓冲区；
//!   * 自绘输入框每帧 [`drain`] 一次，把事件并进自己的文本。
//!
//! 缓冲区在非 iOS 平台恒为空（那两个 `#[no_mangle]` 入口只有 ObjC 会调），
//! 所以这个模块可以无条件编进所有平台，`cargo check` 就能覆盖它。

use std::sync::Mutex;

/// 软键盘产生的一个事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImeEvent {
    /// 输入了一个字符。
    Char(char),
    /// 退格。
    Backspace,
}

static IME_BUFFER: Mutex<Vec<ImeEvent>> = Mutex::new(Vec::new());


/// ObjC 每输入一个字符回调一次（**只由 iOS 的 `webui.m` 调用**）。
#[no_mangle]
pub extern "C" fn phira_ime_on_char(c: u32) {
    if let Some(ch) = char::from_u32(c) {
        if let Ok(mut buf) = IME_BUFFER.lock() {
            buf.push(ImeEvent::Char(ch));
        }
    }
}

/// ObjC 的退格回调（**只由 iOS 的 `webui.m` 调用**）。
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
        crate::webui::diag(&format!("ime::show({show}) 请求"));
        unsafe {
            phira_ime_show(if show { 1 } else { 0 });
        }
        crate::webui::diag(&format!("ime::show({show}) 返回"));
    }
    #[cfg(not(target_os = "ios"))]
    let _ = show;
}

#[cfg(target_os = "ios")]
unsafe extern "C" {
    /// 由 `phira/ios/webui.m` 导出：创建/唤出隐藏 `UITextField`。
    fn phira_ime_show(show: i32);
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
}
