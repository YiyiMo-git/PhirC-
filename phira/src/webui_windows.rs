//! Windows 上的原生 WebView 宿主（wry 0.44）。
//!
//! 只在 `cfg(all(target_os = "windows", feature = "webui"))` 下编译。
//! 把 `phira/ui/index.html`（连同 css/js/图片）通过自定义协议 `webui://` 喂给 wry，
//! 挂到 macroquad 游戏窗口（HWND）上作为子窗口；JS 用 `window.ipc.postMessage(json)`
//! 发命令，Rust 用 `window.__phira_recv(...)` 回包 + 推事件。IPC 分发（含网络调用）全在
//! `webui.rs` 的 `handle_ipc`；本文件只负责「起 WebView + 转发」。
//!
//! wry 0.44 自带事件循环（不依赖 tao），WebView 在自己的线程里跑；本文件把 WebView 建在
//! 独立线程，并用一条 mpsc 通道把所有需要在 WebView 里执行的 JS（show/hide/emit/IPC 回包）
//! 统一转成 `evaluate_script`。

use crate::webui::{handle_ipc, WebUiHost};
use serde_json::Value;
use std::borrow::Cow;
use std::sync::mpsc;

// ---- wry 0.44 相关类型（仅本文件用到，且已受 feature 门控） ----
// wry 0.44 把 WebView / WebViewBuilder 直接挂在 crate 根（不是 wry::webview）。
use wry::{
    http::{Request, Response},
    WebView, WebViewBuilder,
};

/// 把 macroquad 窗口的 HWND 包装成 raw-window-handle 0.6 的 `HasWindowHandle`，
/// 供 wry 0.44 的 `WebViewBuilder::new_as_child` 作为父窗口挂子 WebView。
struct HwndHandle(isize);

// raw-window-handle 0.6 里 HasWindowHandle 是安全 trait，不需要 unsafe。
impl raw_window_handle::HasWindowHandle for HwndHandle {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        use raw_window_handle::{RawWindowHandle, Win32WindowHandle, WindowHandle};
        // raw-window-handle 0.6：Win32 句柄用 NonZeroIsize 承载；借用立即结束，可安全返回
        let hwnd = std::num::NonZeroIsize::new(self.0).expect("invalid game HWND");
        let handle = RawWindowHandle::Win32(Win32WindowHandle::new(hwnd));
        Ok(unsafe { WindowHandle::borrow_raw(handle) })
    }
}

/// 主线程 → WebView 线程的命令。
///
/// 之前只传 JS 字符串，靠 `documentElement.style.visibility` 隐藏页面。
/// 但那样**控件本身还在**，WebView 会用自己的不透明黑底盖住游戏画面
/// （点进谱面预选就全黑）。所以隐藏必须走 `WebCmd::Visible(false)`
/// → wry 的 `WebView::set_visible` → `ShowWindow(SW_HIDE)` + `SetIsVisible(false)`。
enum WebCmd {
    /// 在页面里执行一段 JS
    Eval(String),
    /// 真正显示 / 隐藏 WebView 控件（不是页面里的元素）
    Visible(bool),
    Quit,
}

/// 宿主持有：向 WebView 所在线程投递命令的发送端。
pub struct WryHost {
    cmd_tx: mpsc::Sender<WebCmd>,
}

impl WryHost {
    /// 创建并启动 wry WebView（挂到游戏窗口上）。
    pub fn new() -> anyhow::Result<Self> {
        // 1) 拿到 macroquad 窗口 HWND（窗口标题在 lib.rs 设成 "Phira"）
        let hwnd = find_game_hwnd().ok_or_else(|| anyhow::anyhow!("找不到游戏窗口 HWND"))?;

        // 2) 命令通道：JS→Rust 的 IPC 回包、Rust→JS 的 show/hide/emit 都走这里。
        //    `cmd_tx` 给 WryHost（主线程）用；克隆一份给 WebView 线程的 IPC 处理器。
        let (cmd_tx, cmd_rx) = mpsc::channel::<WebCmd>();
        let ipc_cmd_tx = cmd_tx.clone();

        // 3) 在独立线程建 WebView 并跑「泵消息 + 收命令」循环
        std::thread::Builder::new()
            .name("phirc-webui".into())
            .spawn(move || run_webview(hwnd, cmd_rx, ipc_cmd_tx))
            .map_err(|e| anyhow::anyhow!("启动 WebView 线程失败：{e}"))?;

        Ok(Self { cmd_tx })
    }
}

impl WebUiHost for WryHost {
    fn show(&self) {
        // 真正把控件显示出来，再让 JS 恢复背景动画
        let _ = self.cmd_tx.send(WebCmd::Visible(true));
        let _ = self
            .cmd_tx
            .send(WebCmd::Eval("window.__phiraSetHidden&&window.__phiraSetHidden(false)".into()));
    }

    fn hide(&self) {
        // 先停背景 rAF，再把**整个 WebView 控件**隐藏。
        // 只设 CSS visibility 不够：控件还在，会用不透明黑底盖住游戏画面
        // （点进谱面预选就全黑），必须走 set_visible(false)。
        let _ = self
            .cmd_tx
            .send(WebCmd::Eval("window.__phiraSetHidden&&window.__phiraSetHidden(true)".into()));
        let _ = self.cmd_tx.send(WebCmd::Visible(false));
    }

    fn set_page(&self, _page: &str) {
        let _ = self.cmd_tx.send(WebCmd::Visible(true));
        let _ = self
            .cmd_tx
            .send(WebCmd::Eval("window.__phiraSetHidden&&window.__phiraSetHidden(false)".into()));
    }

    fn emit(&self, event: &str, payload: Value) {
        let js = format!(
            "window.__phira_recv({})",
            serde_json::json!({ "event": event, "payload": payload })
        );
        let _ = self.cmd_tx.send(WebCmd::Eval(js));
    }

    fn shutdown(&self) {
        // 通知 WebView 线程退出并释放 WebView
        let _ = self.cmd_tx.send(WebCmd::Quit);
    }
}

/// 注入到**每一个**页面的初始化脚本（`with_initialization_script`）。
///
/// 为什么用注入而不是改 HTML：开始页 `ui/index.html` 是用户给的成品（要求逐字节照搬，
/// SHA256 固定），里面 5 个按钮只 `console.log`（原文件第 479 行「按钮点击（占位）」），
/// 而且**没有**引 `bridge.js`，所以 `window.__PHIRA__` / `window.__phira_recv` 都不存在。
/// 这里在文档创建时（页面自己的脚本之前）补上最小 IPC 实现，并把 5 个按钮接到 Rust 原生逻辑：
///   开始游玩 → open_rust_page{page:library}（原生曲库） / 资源包 / 设置 / 活动 / 上次游玩。
///
/// 注意：
/// - 用 `addEventListener` **追加**监听，绝不 `removeEventListener`，不动原文件自己的占位监听；
/// - 初始化脚本可能在 DOM 之前执行，所以 DOMContentLoaded + 短轮询双重兜底再挂载；
/// - 幂等：重复注入 / 重复挂载都用标记位挡掉；
/// - **IPC 出口按平台自动选**（与 `ui/bridge.js` 的平台分支一致）：
///   Windows/wry 走 `window.ipc`，Android 走 `window.AndroidBridge`，iOS 走 `webkit.messageHandlers`。
pub(crate) const START_PAGE_INIT_SCRIPT: &str = r#"
(function () {
  'use strict';
  if (window.__phiraStartPageBridgeInstalled) return;
  window.__phiraStartPageBridgeInstalled = true;

  var seq = 0;
  var pending = {};

  /* ---------- 按平台选 IPC 出口（与 ui/bridge.js 的分支一致） ---------- */
  function post(json) {
    if (window.ipc && typeof window.ipc.postMessage === 'function') {
      window.ipc.postMessage(json);
      return true;
    }
    if (window.AndroidBridge && typeof window.AndroidBridge.postMessage === 'function') {
      window.AndroidBridge.postMessage(json);
      return true;
    }
    if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.phira
        && typeof window.webkit.messageHandlers.phira.postMessage === 'function') {
      window.webkit.messageHandlers.phira.postMessage(json);
      return true;
    }
    return false;
  }

  /* ---------- 最小 invoke（形状与 ui/bridge.js 一致：{id, cmd, args}） ---------- */
  function invoke(cmd, args) {
    return new Promise(function (resolve, reject) {
      var id = ++seq;
      pending[id] = { resolve: resolve, reject: reject };
      if (!post(JSON.stringify({ id: id, cmd: cmd, args: args || {} }))) {
        delete pending[id];
        reject(new Error('IPC 桥不可用（不在 WebView 里？）'));
        return;
      }
      // 超时兜底，避免 Promise 永远挂着（与 bridge.js 一致：20s）
      setTimeout(function () {
        if (pending[id]) { delete pending[id]; reject(new Error('IPC 超时：' + cmd)); }
      }, 20000);
    });
  }

  /* ---------- Rust → JS 回包入口（页面没引 bridge.js，这里补最小实现） ---------- */
  if (typeof window.__phira_recv !== 'function') {
    window.__phira_recv = function (json) {
      var msg;
      try { msg = typeof json === 'string' ? JSON.parse(json) : json; } catch (e) { return; }
      if (msg && msg.id != null && pending[msg.id]) {
        var p = pending[msg.id];
        delete pending[msg.id];
        if (msg.error != null) p.reject(new Error(String(msg.error)));
        else p.resolve(msg.result);
      }
    };
  }

  /* ---------- 5 个按钮 → Rust 原生逻辑 ---------- */
  var HANDLERS = {
    'btn-start': function () { return invoke('open_rust_page', { page: 'library' }); },   // 原生曲库
    'btn-res': function () { return invoke('open_rust_page', { page: 'respack' }); },     // 资源包
    'btn-settings': function () { return invoke('open_rust_page', { page: 'settings' }); }, // 设置
    'btn-activity': function () { return invoke('open_rust_page', { page: 'event' }); },  // 活动
    'btn-last': function () { return invoke('start_last_chart'); }                        // 上次游玩的谱面
  };

  function bind() {
    Object.keys(HANDLERS).forEach(function (id) {
      var el = document.getElementById(id);
      if (!el || el.__phiraBound) return;
      el.__phiraBound = true;
      // 追加监听：原文件自己的占位监听保持不变
      el.addEventListener('click', function () {
        try {
          HANDLERS[id]().catch(function (e) { console.error('[phirc] ' + id + ' 失败', e); });
        } catch (e) {
          console.error('[phirc] ' + id + ' 异常', e);
        }
      });
    });
  }

  function start() {
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', onReady);
    } else {
      onReady();
    }
    // 兜底：DOM 结构晚于 DOMContentLoaded 就绪（或元素被动态替换）时再挂几次
    var tries = 0;
    var timer = setInterval(function () {
      bind();
      if (++tries >= 50) clearInterval(timer);
    }, 100);
  }

  function onReady() {
    bind();
    installAccount();
    // 拉一次真实的账号信息（未登录会返回 null，头像显示「未登录」）
    refreshMe();
  }

  /* ---------- 右上角：头像 + 名字（登录入口） ----------
   *
   * 为什么用注入而不是写进 HTML：`ui/index.html` 是逐字节冻结的成品（SHA256 固定），
   * 里面**完全没有**账号相关元素 —— 结果开始页没有任何登录入口。
   * 这里在**所有平台共用的注入脚本**里把它补上，于是 Windows / Android / iOS 一起有，
   * 而且 HTML 一个字节都不动。
   *
   * 数据来自已有的 IPC：`get_me` 返回持久化的 `me`（含 avatar / name），
   * 未登录时点击弹输入框，走 `login`；已登录时点击可退出，走 `logout`。
   */
  var ACCOUNT_STYLE = [
    '#phira-account{position:fixed;top:18px;right:20px;z-index:9999;display:flex;',
    'align-items:center;gap:10px;padding:6px 14px 6px 6px;border-radius:999px;',
    'background:rgba(255,255,255,.10);border:1px solid rgba(255,255,255,.18);',
    '-webkit-backdrop-filter:blur(14px);backdrop-filter:blur(14px);cursor:pointer;',
    'transition:background .18s,border-color .18s;user-select:none;',
    'font-family:inherit;-webkit-tap-highlight-color:transparent}',
    '#phira-account:hover{background:rgba(255,255,255,.18);border-color:rgba(255,255,255,.30)}',
    '#phira-account img,#phira-account .ph-ava-fallback{width:38px;height:38px;border-radius:50%;',
    'object-fit:cover;flex:0 0 auto;display:block;background:rgba(255,255,255,.14)}',
    '#phira-account .ph-ava-fallback{display:flex;align-items:center;justify-content:center;',
    'font-size:19px;color:rgba(255,255,255,.75)}',
    '#phira-account .ph-name{color:rgba(255,255,255,.92);font-size:15px;max-width:150px;',
    'overflow:hidden;text-overflow:ellipsis;white-space:nowrap}',
    '#phira-account .ph-sub{color:rgba(255,255,255,.55);font-size:12px}',
    '#phira-account .ph-col{display:flex;flex-direction:column;align-items:flex-start;line-height:1.25}',
  ].join('');

  function installAccount() {
    if (document.getElementById('phira-account')) return;
    var st = document.createElement('style');
    st.id = 'phira-account-style';
    if (!document.getElementById('phira-account-style')) document.head.appendChild(st);
    st.textContent = ACCOUNT_STYLE;

    var el = document.createElement('div');
    el.id = 'phira-account';
    el.title = '账号：点击登录 / 退出';
    document.body.appendChild(el);
    el.addEventListener('click', onAccountClick);
    renderAccount();
  }

  function renderAccount() {
    var el = document.getElementById('phira-account');
    if (!el) return;
    var me = null;
    try { me = JSON.parse(localStorage.getItem('phira_me_cache') || 'null'); } catch (e) { me = null; }
    var name = (me && me.name) ? String(me.name) : '';
    var avatar = (me && me.avatar) ? String(me.avatar) : '';

    var ava;
    if (avatar) {
      ava = document.createElement('img');
      ava.src = avatar;
      ava.alt = '';
    } else {
      ava = document.createElement('div');
      ava.className = 'ph-ava-fallback';
      ava.textContent = name ? name.charAt(0) : '?';
    }
    var col = document.createElement('div');
    col.className = 'ph-col';
    var n1 = document.createElement('div');
    n1.className = name ? 'ph-name' : 'ph-sub';
    n1.textContent = name || '未登录';
    col.appendChild(n1);
    if (name && me && me.rks != null) {
      var n2 = document.createElement('div');
      n2.className = 'ph-sub';
      n2.textContent = 'RKS ' + Number(me.rks).toFixed(2);
      col.appendChild(n2);
    }
    el.textContent = '';
    el.appendChild(ava);
    el.appendChild(col);
  }

  function refreshMe() {
    return invoke('get_me').then(function (me) {
      try { localStorage.setItem('phira_me_cache', JSON.stringify(me || null)); } catch (e) {}
      renderAccount();
      return me;
    }).catch(function () { return null; });
  }

  function onAccountClick() {
    var cached = null;
    try { cached = JSON.parse(localStorage.getItem('phira_me_cache') || 'null'); } catch (e) {}
    if (cached && cached.name) {
      if (window.confirm('已登录：' + cached.name + '\n\n点「确定」退出登录，点「取消」取消。')) {
        invoke('logout').then(refreshMe).catch(function (e) { alert('退出失败：' + e); });
      }
      return;
    }
    var email = window.prompt('登录 Phira\n\n请输入邮箱：', '');
    if (!email) return;
    var pwd = window.prompt('请输入密码：', '');
    if (!pwd) return;
    invoke('login', { username: email, password: pwd }).then(function () {
      return refreshMe();
    }).then(function (me) {
      if (me && me.name) alert('登录成功：' + me.name);
    }).catch(function (e) {
      alert('登录失败：' + e);
    });
  }

  start();
})();
"#;

/// WebView 线程：建 WebView，然后循环把 eval 请求转成 `evaluate_script`。
fn run_webview(hwnd: isize, cmd_rx: mpsc::Receiver<WebCmd>, ipc_cmd_tx: mpsc::Sender<WebCmd>) {
    let parent = HwndHandle(hwnd);

    // 【关键】必须显式指定 bounds，让 WebView 铺满整个游戏窗口客户区。
    //
    // wry 的子窗口默认 bounds 只有 200×200（见 wry `WebViewAttributes::default`），
    // 而主场景一旦认为「WebView 可见」就会整帧跳过 Rust 渲染（main.rs 的
    // `is_webui_visible()` 早退）。两者叠加的结果就是：窗口几乎全黑，只有左上角
    // 一小块 200×200 的网页 —— 也就是用户看到的「界面纯黑」。
    let (w, h) = parent_client_logical_size(hwnd).unwrap_or((973.0, 608.0));
    tracing::info!("[webui] WebView 尺寸（逻辑像素）：{w}×{h}");
    let bounds = wry::Rect {
        position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
        size: wry::dpi::LogicalSize::new(w, h).into(),
    };

    // 自定义协议 `webui://` 加载内嵌资源；IPC 走 `window.ipc.postMessage`；最后加载首页。
    let builder = WebViewBuilder::new_as_child(&parent)
        .with_bounds(bounds)
        .with_asynchronous_custom_protocol(
            "webui".to_string(),
            |request: Request<Vec<u8>>, responder| {
                // local-cover 需要从谱包里按需解出曲绘，放到工作线程处理，别堵 wry 消息泵。
                std::thread::spawn(move || responder.respond(protocol_handler(request)));
            },
        )
        .with_ipc_handler(move |request: Request<String>| {
            // IPC 在 wry 内部线程触发；把真正的（可能阻塞网络的）处理丢到独立线程，
            // 结果再通过 eval_tx 回灌到 WebView 线程执行，避免卡住 wry 的消息循环。
            let tx = ipc_cmd_tx.clone();
            std::thread::spawn(move || {
                let body = request.body();
                if let Some(resp) = handle_ipc(body) {
                    let _ = tx.send(WebCmd::Eval(format!("window.__phira_recv({resp})")));
                }
            });
        })
        // 注入开始页桥接脚本：不改 `ui/index.html`（逐字节照搬）也能让 5 个按钮生效。
        // 初始化脚本每次页面加载都会执行，且早于页面自身的脚本。
        .with_initialization_script(START_PAGE_INIT_SCRIPT)
        .with_url("webui://webui/index.html");

    // 显式标注类型，既清晰又消除「未使用 import」告警。
    let webview: WebView = match builder.build() {
        Ok(w) => w,
        Err(e) => {
            // WebView2 不可用 / 创建失败：保持 WEBUI_EMBEDDED 为 false，
            // 主场景就会照常渲染 Rust 原生 UI，而不是黑屏。
            tracing::error!("[webui] WebView 构建失败，回退到原生 UI：{e:?}");
            crate::webui::set_webui_embedded(false);
            return;
        }
    };

    // 到这里 WebView 才真正创建成功。此时才允许主场景跳过 Rust 渲染、
    // 把画面完全交给 WebView（main.rs 下一帧检测到后会自动 `set_webui_visible(true)`）。
    crate::webui::set_webui_embedded(true);
    tracing::info!("[webui] wry WebView 创建成功，界面已交给 WebView");

    // 主循环：一边泵 Win32 消息、一边把通道里的 JS（来自 WryHost 的 show/hide/emit，
    // 或 IPC 回包）转成 evaluate_script。`__PHIRA_WEBUI_QUIT__` 用于优雅退出。
    //
    // 【关键】绝不能用阻塞的 `recv()` 等在这里：
    //   - wry 源码里**没有任何** PeekMessage/GetMessage/DispatchMessage，它不自带消息循环，
    //     泵消息是宿主的责任；
    //   - WebView2 的容器窗口是 `WS_CHILD` 子窗口且建在本线程上，不泵消息它就永远不绘制
    //     → **黑屏**；
    //   - 主线程（macroquad）一旦 SendMessage 到这个跨线程子窗口，而本线程不泵消息，
    //     主线程就一直等 → **游戏窗口卡死、拖不动**。
    loop {
        unsafe { pump_win32_messages() };
        match cmd_rx.recv_timeout(std::time::Duration::from_millis(8)) {
            Ok(WebCmd::Eval(js)) => {
                let _ = webview.evaluate_script(&js);
            }
            Ok(WebCmd::Visible(v)) => {
                // 真正显示 / 隐藏 WebView 窗口（ShowWindow + SetIsVisible）
                if let Err(e) = webview.set_visible(v) {
                    tracing::error!("[webui] set_visible({v}) 失败：{e:?}");
                }
            }
            Ok(WebCmd::Quit) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // webview 在此随变量 drop 而释放
}

/// 泵一次本线程的 Win32 消息（把队列排空）。
///
/// WebView2 的绘制、以及「主线程 SendMessage 到跨线程子窗口」的应答都依赖这一步。
/// 没有它就会出现「黑屏 + 窗口卡死拖不动」。
///
/// # Safety
/// 只能在本线程（拥有 WebView 容器窗口的线程）调用。
unsafe fn pump_win32_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };

    // hwnd 传 NULL：泵本线程所有窗口的消息（这里只有 WebView 容器窗口及其内部窗口）。
    let mut msg: MSG = std::mem::zeroed();
    while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// 自定义协议处理：`webui://webui/<path>` → 内嵌资源。
fn protocol_handler(request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    use wry::http::{header, StatusCode};

    let path = request.uri().path().trim_start_matches('/').to_owned();
    let asset = if path == "local-cover" {
        request
            .uri()
            .query()
            .and_then(|query| query.strip_prefix("p="))
            .and_then(|encoded| {
                use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
                URL_SAFE_NO_PAD.decode(encoded).ok()
            })
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|chart_path| load_local_cover(&chart_path))
            .map(|bytes| (bytes, "image/jpeg"))
    } else if path == "respack-preview" {
        request
            .uri()
            .query()
            .and_then(load_respack_preview_query)
            .map(|bytes| (bytes, "image/png"))
    } else {
        load_asset(&path)
    };

    match asset {
        Some((data, mime)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, "public, max-age=86400")
            .body(Cow::Owned(data))
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            // Cow::Borrowed 需要 &[u8]（不是 &[u8; N]），用 as_slice() 抹平数组长度。
            .body(Cow::Borrowed(b"not found".as_slice()))
            .unwrap(),
    }
}

/// 给 Web 资源包页读取受限的音符预览贴图。文件名采用白名单，索引只指向 Data 中已登记的资源包。
fn load_respack_preview_query(query: &str) -> Option<Vec<u8>> {
    let mut index = None;
    let mut file = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        match key {
            "index" => index = value.parse::<usize>().ok(),
            "file" => file = Some(value),
            _ => return None,
        }
    }
    let index = index?;
    let file = file?;
    if !matches!(file, "click.png" | "drag.png" | "flick.png" | "hold.png" | "hit_fx.png") || index == 0 {
        return None;
    }
    let id = crate::get_data().respacks.get(index - 1)?;
    let rel = std::path::Path::new(id);
    if rel.components().count() != 1 || !matches!(rel.components().next(), Some(std::path::Component::Normal(_))) {
        return None;
    }
    let path = std::path::Path::new(&crate::dir::respacks().ok()?).join(rel).join(file);
    std::fs::read(path).ok()
}

/// 给 Web 曲库按需提供本地谱包里的曲绘：优先命中现有缩略图缓存，缺失时才解包并生成。
/// WebView 用 loading=lazy，因此只有视口附近卡片会触发；解码运行在协议工作线程，不阻塞游戏帧。
fn load_local_cover(chart_path: &str) -> Option<Vec<u8>> {
    // 仅允许请求当前已导入谱面，防止自定义协议被用来读取任意本地文件。
    if !crate::get_data()
        .charts
        .iter()
        .any(|chart| chart.local_path == chart_path)
    {
        return None;
    }

    let cache_path = crate::page::thumbnail_path(chart_path).ok()?;
    if let Ok(bytes) = std::fs::read(&cache_path) {
        return Some(bytes);
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async {
        let mut fs = crate::scene::fs_from_path(chart_path).ok()?;
        let info = prpr::fs::load_info(fs.as_mut()).await.ok()?;
        let image_data = fs.load_file(&info.illustration).await.ok()?;
        let image = image::load_from_memory(&image_data).ok()?;
        let thumbnail = crate::images::Images::thumbnail(&image);
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        thumbnail
            .save_with_format(&cache_path, image::ImageFormat::Jpeg)
            .ok()?;
        std::fs::read(cache_path).ok()
    })
}

/// 内嵌资源表：把 `phira/ui/` 下的文件打进二进制。
///
/// 资源本身与平台无关，已提到 [`crate::webui_assets`] 共用（Android 侧也走同一张表），
/// 这里只做一次转发，避免两份 `include_str!`。
fn load_asset(path: &str) -> Option<(Vec<u8>, &'static str)> {
    crate::webui_assets::load_asset(path)
}

/// 取游戏窗口客户区的**逻辑**尺寸（CSS 像素），供 `with_bounds` 使用。
///
/// wry 的 `set_bounds` 收的是逻辑坐标，内部会按窗口 DPI 换算成物理像素
/// （`set_bounds` → `set_bounds_inner` → `SetBounds` / `SetWindowPos`）；
/// 而 `GetClientRect` 返回的是物理像素，所以这里要反向除回 DPI 缩放，
/// 否则在高 DPI 屏上 WebView 会被再放大一轮、撑出窗口边界。
fn parent_client_logical_size(hwnd: isize) -> Option<(f64, f64)> {
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;

    let hwnd = hwnd as HWND;
    // 手动初始化，不依赖 windows-sys 是否给 RECT 实现了 Default。
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // 返回 BOOL：非零表示成功
    if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
        return None;
    }
    let phys_w = (rect.right - rect.left) as f64;
    let phys_h = (rect.bottom - rect.top) as f64;
    if phys_w <= 0.0 || phys_h <= 0.0 {
        return None;
    }

    // DPI → 缩放系数（96 = 100%）。进程若非 DPI-aware，这里会返回 96，即缩放 1.0。
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = if dpi > 0 { dpi as f64 / 96.0 } else { 1.0 };
    let scale = if scale > 0.0 { scale } else { 1.0 };

    Some((phys_w / scale, phys_h / scale))
}

/// 用窗口标题找 macroquad 窗口 HWND（窗口标题在 lib.rs 设为 "Phira"）。
fn find_game_hwnd() -> Option<isize> {
    use std::ffi::CString;
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowA;

    // 按精确标题 "Phira" 找
    let title = CString::new("Phira").ok()?;
    // windows-sys 0.59 的 FindWindowA 参数是 PCSTR（*const u8）；CString::as_ptr() 给的是
    // *const i8，需要显式转成 *const u8。返回的 HWND 是 *mut c_void，空指针用 is_null() 判断。
    let hwnd = unsafe { FindWindowA(std::ptr::null::<u8>(), title.as_ptr() as *const u8) };
    if !hwnd.is_null() {
        return Some(hwnd as isize);
    }
    // 兜底：标题以 "Phira" 开头（预留，目前宏quad 默认标题就是 "Phira"）
    find_by_prefix(b"Phira")
}

fn find_by_prefix(_prefix: &[u8]) -> Option<isize> {
    // 简单兜底：若以后标题变化，这里再扩成 EnumWindows 遍历。
    None
}
