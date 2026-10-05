//! iOS 上的原生 WebView 宿主（`WKWebView` + Objective-C 胶水）。
//!
//! 与 [`crate::webui_windows`]（wry 挂 HWND）、[`crate::webui_android`]（系统 WebView + JNI）
//! **职责一一对应**，实现方式各有不同：
//!
//! | | Windows | Android | iOS |
//! |---|---|---|---|
//! | 宿主 | wry 子窗口挂 HWND | `WebView` 叠在 QuadSurface | `WKWebView` 加到根 view |
//! | 资源 | `webui://` 自定义协议 | `https://webui.local/` + 内存表 | **内联单文件 HTML**（base64 data URI） |
//! | IPC | `window.ipc.postMessage` | `window.AndroidBridge.postMessage` | `window.webkit.messageHandlers.phira.postMessage` |
//! | 桥 | Rust (wry) | Rust + JNI | `phira/ios/webui.m`（Objective-C） |
//!
//! # 为什么 iOS 走 ObjC 而不是 objc2
//!
//! 工程里 iOS 已有先例（`ios/motion.m` 包 CoreMotion）：用一组 C 函数做边界最省事；
//! 而 `WKScriptMessageHandler` 是协议实现，用 objc2 写协议一致性冗长且易错。
//!
//! # 为什么资源要内联
//!
//! iOS 上没法像 Windows/Android 那样给 WebView 喂自定义来源的资源；
//! `file://` 又会让 `index.html` 里的 `gl.texImage2D(img)` 抛 SecurityError（背景光波全没）。
//! 所以由 [`crate::webui_assets::build_inlined_index_html`] 把整页内联成单文件，
//! 这里一次性交给 `loadHTMLString`。内联逻辑在 Rust 侧，**能在 Windows 上编译验证**。
//!
//! ⚠️ `ios/webui.m` 无法在 Windows 上编译验证（没有 Xcode 工具链），
//! 所以本文件与那份 ObjC 都需要在 macOS/CI 上首次构建时确认。

use crate::webui::WebUiHost;
use serde_json::Value;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicBool, Ordering};

/// ObjC 侧提供的回调类型：JS 一条 IPC 消息进来时调用。
pub type IpcFn = extern "C" fn(*const c_char);

extern "C" {
    fn phira_webui_create(html: *const c_char, ipc_fn: IpcFn);
    fn phira_webui_set_visible(visible: c_int);
    fn phira_webui_eval(js: *const c_char);
    fn phira_webui_ready() -> c_int;
    fn phira_webui_destroy();
}

/// C 字符串里有内嵌 `\0` 时返回 `None`（不该发生，但要挡住 UB）。
fn cstring(s: &str) -> Option<CString> {
    match CString::new(s) {
        Ok(c) => Some(c),
        Err(_) => {
            tracing::warn!("[webui-ios] 字符串含 NUL，无法传给 ObjC");
            None
        }
    }
}

/// JS → Rust：真正处理 IPC 并回包。
///
/// 由 ObjC 的消息处理器调用（主线程）。
extern "C" fn on_ipc_cb(json: *const c_char) {
    if json.is_null() {
        return;
    }
    // SAFETY: ObjC 侧保证传进来的是合法 UTF-8 C 字符串，且只在这次调用期间有效。
    let Ok(text) = unsafe { CStr::from_ptr(json) }.to_str() else {
        return;
    };
    if let Some(resp) = crate::webui::handle_ipc(text) {
        // 回包 eval 回页面：与 Windows 的 with_ipc_handler 行为一致
        if let Some(js) = cstring(&format!("window.__phira_recv({resp})")) {
            unsafe { phira_webui_eval(js.as_ptr()) };
        }
    }
}

/// iOS WebView 宿主。
pub struct IosHost {
    /// 是否已经建过 WebView（只建一次）。
    created: AtomicBool,
}

impl IosHost {
    /// 创建宿主。此时**不**建 WebView —— 那必须在主线程做，而且内联 HTML
    /// （含 ~18MB base64）也没必要在启动时就算出来；推迟到第一次 [`WebUiHost::show`]。
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            created: AtomicBool::new(false),
        })
    }

    /// 真正建 WebView 并加载内联好的单文件 HTML（幂等）。
    fn ensure_created(&self) {
        if self.created.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(html) = crate::webui_assets::build_inlined_index_html() else {
            tracing::error!("[webui-ios] 内联开始页失败：index.html 取不到");
            return;
        };
        let Some(html) = cstring(&html) else {
            return;
        };
        unsafe { phira_webui_create(html.as_ptr(), on_ipc_cb) };
        // 与 Windows/Android 同语义：请求已发出，主场景可以把渲染交给 WebView
        crate::webui::set_webui_embedded(true);
        tracing::info!("[webui-ios] 已请求在 UI 线程创建 WKWebView");
    }

    fn eval(js: &str) {
        if let Some(js) = cstring(js) {
            unsafe { phira_webui_eval(js.as_ptr()) };
        }
    }
}

impl WebUiHost for IosHost {
    fn show(&self) {
        self.ensure_created();
        unsafe { phira_webui_set_visible(1) };
    }

    fn hide(&self) {
        unsafe { phira_webui_set_visible(0) };
    }

    fn emit(&self, event: &str, payload: Value) {
        // 与其它平台同形：调用页面里的 window.__phira_recv({event, payload})
        Self::eval(&format!(
            "window.__phira_recv({})",
            serde_json::json!({ "event": event, "payload": payload })
        ));
    }

    fn shutdown(&self) {
        unsafe { phira_webui_destroy() };
    }
}

/// WebView 是否已经建好并加载过页面。
pub fn is_ready() -> bool {
    unsafe { phira_webui_ready() != 0 }
}
