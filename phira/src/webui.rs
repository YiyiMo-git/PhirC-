//! Phirc Mod++ · WebView 界面（登录 / 开始 / 曲库）↔ Rust IPC 数据桥
//!
//! 这个模块是 JS 侧（`phira/ui/bridge.js` + `app.js`）与 Rust 侧之间的契约层：
//!
//! - JS 通过 `window.__PHIRA__.invoke(cmd, args)` 调用这里定义的命令；
//! - Rust 通过 `webui_emit(event, payload)` 向 JS 推送事件（`toast` / `goto` / `me`）；
//! - 命令处理全部复用已有的 `client::Client` 与 `Data`，不重复任何网络 / 解析逻辑；
//! - 真正的原生 WebView 宿主（wry / Android / iOS）实现 `WebUiHost` trait，
//!   默认构建下使用 `StubHost`（不嵌入 WebView，界面走原有 Rust UI），
//!   启用 `webui` feature（Windows + wry）才真正把界面铺到游戏窗口上。
//!
//! 调用名必须与 `phira/ui/bridge.js`、`app.js` 完全一致，改动任一侧都要同步另一侧。

// 本模块的 IPC 命令实现、WebChart/IpcMessage/PendingOpenPage 等，只有在启用 `webui`
// feature 并被平台宿主（webui_windows）调用时才会被使用；默认构建下会报 dead_code，
// 属正常情况，统一在此忽略。
#![allow(dead_code)]

use crate::{
    client::{Client, LoginParams},
    data::LocalChart,
    get_data, get_data_mut, save_data,
    page::{global_icons, ChartItem, load_local, Page},
    scene::SongScene,
};
use anyhow::{anyhow, Result};
use prpr::{
    config::Mods,
    ext::SafeTexture,
    scene::NextScene,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

/// 曲库里一张谱面在 JS 侧需要的字段（对应 `app.js` 的 `renderCharts`）。
///
/// `renderCharts` 读取：`id` / `name` / `composer` / `charter` / `difficulty`
/// （难度类型，如 `IN`）/ `level`（难度数字，如 `15`）/ `illustration`（封面 URL）
/// / `path`（本地谱面路径，用于 `start_chart`）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WebChart {
    pub id: Option<i32>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub composer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charter: Option<String>,
    /// 难度类型字符串：`EZ` / `HD` / `IN` / `AT` / `SP`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<String>,
    /// 难度数字（字符串形式，保留原始精度）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// 封面图 URL（仅在线谱面有；本地谱面暂留空，JS 显示首字母占位）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub illustration: Option<String>,
    /// 本地谱面路径（用于 `start_chart`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// JS → Rust 的 IPC 消息骨架：`{ id, cmd, args }`。
#[derive(serde::Deserialize)]
struct IpcMessage {
    id: Option<u64>,
    cmd: String,
    #[serde(default)]
    args: Value,
}

/// 待启动的谱面（由 `start_chart` 写入，主场景在 `update` 里消费并推送 `SongScene`）。
pub struct PendingChart {
    pub chart: ChartItem,
    pub local_path: Option<String>,
    /// true = 这份谱面已经由 Web 预选页确认过了，SongScene 直接开玩、不再展示自己那屏。
    pub auto_play: bool,
    /// 预选页设置的单曲 MOD（未设置则使用空 MOD，保持原流程）。
    pub mods: Option<Mods>,
}

/// JS 通过 `open_rust_page` 请求的、需要由 Rust 原生渲染的页面。
#[derive(Clone, Copy)]
pub enum PendingOpenPage {
    Settings,
    Event,
    ResPack,
    /// 原生曲库（开始页「开始游玩」）。`LibraryPage` 需要 `MainScene::state.icons`，
    /// 而 `take_pending_open_page` 只拿得到 `rank_icons`，所以这一支由主场景
    /// 通过 `take_pending_library()` 单独消费。
    Library,
}

/* ============================ 全局状态 ============================ */

/// 当前 WebView 显示的是哪个界面（来自 JS 的 `ui_page_changed`）。
static WEBUI_PAGE: Mutex<String> = Mutex::new(String::new());

/// WebView 当前是否可见（home / library / login 时可见，游玩 / 设置页时隐藏）。
static WEBUI_VISIBLE: AtomicBool = AtomicBool::new(false);

/// 「装了真宿主（不是 StubHost）」。
///
/// 与 [`WEBUI_EMBEDDED`] 的区别**很关键**：
/// * `HOST_REAL` = 有平台宿主可用 → 可以调 `show()` 去建 WebView；
/// * `EMBEDDED`  = WebView **确实已经建好并加载完** → 才能让主场景跳过 Rust 渲染。
///
/// 混用这两个会造成黑屏：提前把 `EMBEDDED` 置 true，但 WebView 还没建好
/// （甚至建失败），主场景就不画了 —— 屏幕全黑。所以：
/// 触发 `show()` 用 `HOST_REAL`，决定「跳过渲染」用 `EMBEDDED && VISIBLE`。
static WEBUI_HOST_REAL: AtomicBool = AtomicBool::new(false);

/// 把 WebView 诊断写进崩溃日志文件（`<data>/phira-crash.log`）。
///
/// 手机上没 adb，`tracing` 的输出看不到；而这个文件在**设置 → 日志**页里能直接看。
/// 移动端 WebView 出问题时，这几行就是唯一的线索。
pub fn diag(msg: &str) {
    tracing::info!("[webui] {msg}");
    crate::crash_log_stage(&format!("[webui] {msg}"));
}

/// 是否真的嵌入了原生 WebView（wry 创建成功才置 true；StubHost 下为 false，
/// 避免在没有 WebView 时把 Rust 首页渲染整个跳过导致黑屏）。
static WEBUI_EMBEDDED: AtomicBool = AtomicBool::new(false);

/// 已创建的 WebView 宿主（Windows + wry 时填充，否则为 `None`）。
static WEBUI_HOST: Mutex<Option<Box<dyn WebUiHost>>> = Mutex::new(None);

/// `start_chart` 写入、主场景消费的待启动谱面。
static PENDING_CHART: Mutex<Option<PendingChart>> = Mutex::new(None);

/// `open_rust_page` 写入、主场景消费的待打开原生页面。
static PENDING_OPEN: Mutex<Option<PendingOpenPage>> = Mutex::new(None);

/* ============================ 宿主 trait ============================ */

/// 原生 WebView 宿主需要实现的接口。所有方法都必须是「即发即忘」的，
/// 不能阻塞调用线程（WebView 在自己的线程上跑）。
pub trait WebUiHost: Send {
    /// 显示 WebView（铺到游戏窗口上）。
    fn show(&self);
    /// 隐藏 WebView（露出下面的 macroquad 画布，用于游玩 / 原生页面）。
    fn hide(&self);
    /// 通知宿主 JS 当前所在界面（可用于调整大小 / 焦点等，默认空实现）。
    fn set_page(&self, _page: &str) {}
    /// 向 JS 推送一个事件：`window.__phira_recv({ event, payload })`。
    fn emit(&self, event: &str, payload: Value);
    /// 关闭并释放 WebView。
    fn shutdown(&self);
}

/// 默认宿主：不嵌入任何 WebView。界面仍走原有 Rust UI。
/// 启用 `webui` feature 时由 `init_host` 替换为真正的 wry 宿主。
struct StubHost;

impl WebUiHost for StubHost {
    fn show(&self) {}
    fn hide(&self) {}
    fn emit(&self, _event: &str, _payload: Value) {}
    fn shutdown(&self) {}
}

/// 创建 WebView 宿主。
///
/// 平台分发：
/// - **Windows**：wry 子窗口挂 HWND（`webui_windows`），走 `webui://` 自定义协议；
/// - **Android**：系统 `WebView` 叠在 QuadSurface 上（`webui_android`），走 JNI；
/// - 其余平台 / 关掉 `webui` feature：`StubHost`，界面走原有 Rust UI。
///
/// 失败一律回退到 `StubHost`，保证启动不崩。
pub fn init_host() {
    #[cfg(all(target_os = "windows", feature = "webui"))]
    {
        match crate::webui_windows::WryHost::new() {
            Ok(host) => {
                *WEBUI_HOST.lock().unwrap() = Some(Box::new(host));
                // 【重要】这里**不能**提前把 WEBUI_EMBEDDED 置 true！
                // `WryHost::new()` 只是启动了 WebView 线程，真正创建 WebView2 的
                // `builder.build()` 是在那个线程里异步执行的。如果这里就置 true，
                // 一旦 WebView2 不可用导致 build() 失败，主场景仍会认为「WebView 可见」
                // 而整帧跳过 Rust 渲染 —— 结果就是纯黑屏。
                // 正确做法：由 webui_windows 在 build() 真正成功后调用
                // `set_webui_embedded(true)`；失败则保持 false，自动回退到原生 UI。
                diag("wry 宿主线程已启动，等待 WebView 创建结果");
            }
            Err(err) => {
                diag(&format!("wry 宿主创建失败，回退到原生 UI：{err:?}"));
                *WEBUI_HOST.lock().unwrap() = Some(Box::new(StubHost));
                WEBUI_EMBEDDED.store(false, Ordering::Relaxed);
            }
        }
        return;
    }
    #[cfg(all(target_os = "android", feature = "webui"))]
    {
        match crate::webui_android::AndroidHost::new() {
            Ok(host) => {
                *WEBUI_HOST.lock().unwrap() = Some(Box::new(host));
                // 「有真宿主」→ 允许主场景去 `show()`（那才会真正建 WebView）。
                // ⚠️ 但**绝不能**在这里就 `set_webui_embedded(true)`！
                // `is_webui_visible()` = EMBEDDED && VISIBLE，一旦提前置位，主场景会
                // 整帧跳过 Rust 渲染；而 Android 的 WebView 还没建好（甚至可能建失败）
                // → **纯黑屏**。必须是等 Java 侧确认 WebView 真的加载完（`webuiOnReady`）
                // 才置位。在此之前 WebView 是叠在 SurfaceView 之上的原生 View，
                // 没它就显示下面的原生 UI，不会黑。
                WEBUI_HOST_REAL.store(true, Ordering::Relaxed);
                diag("Android WebView 宿主已创建，等待首次显示时建 WebView");
            }
            Err(err) => {
                diag(&format!("Android WebView 宿主创建失败，回退到原生 UI：{err:?}"));
                *WEBUI_HOST.lock().unwrap() = Some(Box::new(StubHost));
                WEBUI_EMBEDDED.store(false, Ordering::Relaxed);
                WEBUI_HOST_REAL.store(false, Ordering::Relaxed);
            }
        }
        return;
    }
    #[cfg(all(target_os = "ios", feature = "webui"))]
    {
        match crate::webui_ios::IosHost::new() {
            Ok(host) => {
                *WEBUI_HOST.lock().unwrap() = Some(Box::new(host));
                // 同 Android：只声明「有真宿主」，`EMBEDDED` 等 ObjC 侧确认后再置位
                WEBUI_HOST_REAL.store(true, Ordering::Relaxed);
                diag("iOS WKWebView 宿主已创建，等待首次显示时建 WebView");
            }
            Err(err) => {
                diag(&format!("iOS WebView 宿主创建失败，回退到原生 UI：{err:?}"));
                *WEBUI_HOST.lock().unwrap() = Some(Box::new(StubHost));
                WEBUI_EMBEDDED.store(false, Ordering::Relaxed);
                WEBUI_HOST_REAL.store(false, Ordering::Relaxed);
            }
        }
        return;
    }
    #[cfg(not(any(
        all(target_os = "windows", feature = "webui"),
        all(target_os = "android", feature = "webui"),
        all(target_os = "ios", feature = "webui")
    )))]
    {
        *WEBUI_HOST.lock().unwrap() = Some(Box::new(StubHost));
        WEBUI_EMBEDDED.store(false, Ordering::Relaxed);
    }
}

/// 是否真的嵌入了 WebView（而非 StubHost）。
pub fn host_available() -> bool {
    WEBUI_HOST.lock().unwrap().is_some()
}

/* ============================ 可见性 / 页面 ============================ */

/// 设置 WebView 可见性（同时驱动宿主 show/hide）。
pub fn set_webui_visible(visible: bool) {
    // 只在**变化**时记一行（这个函数每帧都可能被调用，不能刷屏）。
    // 这一行是排查「移动端看不到 WebView」的关键：能看到 `false -> true`
    // 才说明主场景的触发条件真的成立了。
    let prev = WEBUI_VISIBLE.swap(visible, Ordering::Relaxed);
    if prev != visible {
        diag(&format!("WebView 可见性：{prev} -> {visible}"));
    }
    let host = WEBUI_HOST.lock().unwrap();
    if let Some(h) = host.as_ref() {
        if visible {
            h.show();
        } else {
            h.hide();
        }
    }
}

/// 返回 WebView 当前是否可见（供主场景决定是否跳过 Rust 首页渲染）。
/// 只有「真的嵌入了 WebView 且当前可见」时才为 true，StubHost 下恒 false。
pub fn is_webui_visible() -> bool {
    WEBUI_EMBEDDED.load(Ordering::Relaxed) && WEBUI_VISIBLE.load(Ordering::Relaxed)
}

/// 是否真的嵌入了原生 WebView（wry 创建成功）。
pub fn webui_embedded() -> bool {
    WEBUI_EMBEDDED.load(Ordering::Relaxed)
}

/// 是否装了**真宿主**（可用于触发 `show()` 去建 WebView）。
///
/// 与 [`webui_embedded`] 的区别见 [`WEBUI_HOST_REAL`] 的注释 ——
/// **触发建 WebView 用这个，决定跳过 Rust 渲染用 [`is_webui_visible`]**。
pub fn webui_host_real() -> bool {
    WEBUI_HOST_REAL.load(Ordering::Relaxed)
}

/// 标记「原生 WebView 是否真的创建成功」。
///
/// 只由平台宿主（`webui_windows`）在 `builder.build()` **真正成功之后**调用：
/// - 成功 → `true`，主场景下一帧检测到后把画面交给 WebView；
/// - 失败 → `false`，主场景照常渲染 Rust 原生 UI，不会黑屏。
pub fn set_webui_embedded(embedded: bool) {
    WEBUI_EMBEDDED.store(embedded, Ordering::Relaxed);
}

/// 标记「真宿主可用」（平台宿主在能接受 `show()` 的时刻调用）。
///
/// * Windows：在 `builder.build()` **成功之后**调用（与 `set_webui_embedded` 同时），
///   保持原有行为不变；
/// * Android / iOS：在 `init_host` 里就调用 —— 那两个平台的 WebView 是叠在上层的
///   原生 View，提前触发 `show()` 只是"把 WebView 建出来"，**不会**让主场景停止
///   渲染（`EMBEDDED` 仍为 false），所以是安全的。
pub fn set_webui_host_real(real: bool) {
    WEBUI_HOST_REAL.store(real, Ordering::Relaxed);
}

/// 当前界面（login / home / library / 其他）。
pub fn current_page() -> String {
    WEBUI_PAGE.lock().unwrap().clone()
}

/// JS 通过 `ui_page_changed` 上报当前界面。login/home/library 时显示 WebView，
/// 其余（理论上不会由 JS 主动切到，但保险）隐藏。
pub fn set_page(page: &str) {
    *WEBUI_PAGE.lock().unwrap() = page.to_owned();
    // 设置、资源包和谱面预选也由 WebView 承载；不要退回旧 Rust 页面栈。
    let visible = matches!(page, "login" | "home" | "library" | "song" | "settings" | "respacks");
    set_webui_visible(visible);
}

/* ============================ Rust → JS 事件 ============================ */

/// 向 JS 推送事件。宿主为空（StubHost）时静默丢弃。
pub fn webui_emit(event: &str, payload: Value) {
    let host = WEBUI_HOST.lock().unwrap();
    if let Some(h) = host.as_ref() {
        h.emit(event, payload);
    }
}

/// 便捷：弹一条提示。
pub fn toast(msg: impl Into<String>, is_err: bool) {
    webui_emit("toast", json!({ "msg": msg.into(), "err": is_err }));
}

/// 便捷：让 JS 跳到某个界面（同时把可见性打开）。
pub fn goto_page(page: &str) {
    set_page(page);
    webui_emit("goto", json!({ "page": page }));
}

/// 便捷：通知 JS 重新拉取 `me`（登录 / 登出后）。
pub fn notify_me_changed() {
    webui_emit("me", json!({}));
}

/* ============================ 待消费队列 ============================ */

/// 取出待启动谱面（消费一次）。
pub fn take_pending_chart() -> Option<PendingChart> {
    PENDING_CHART.lock().unwrap().take()
}

/// 取出待打开原生页面（消费一次）。
pub fn take_pending_open() -> Option<PendingOpenPage> {
    PENDING_OPEN.lock().unwrap().take()
}

/// 消费「打开原生曲库」这一个待办；不是曲库就原样不动，返回 false。
///
/// 单独开这个函数是因为 `LibraryPage::new` 除了 `global_icons()` 还要
/// `MainScene::state.icons`，而 `take_pending_open_page` 拿不到 `state`。
/// 两处不会互抢：主场景先调本函数，`take_pending_open_page` 遇到 `Library`
/// 也只是把它放回队列、返回 `None`（见下）。
pub fn take_pending_library() -> bool {
    let mut slot = PENDING_OPEN.lock().unwrap();
    if matches!(*slot, Some(PendingOpenPage::Library)) {
        *slot = None;
        true
    } else {
        false
    }
}

/// 主场景在 `update` 里调用：把待启动谱面组装成 `SongScene` overlay。
/// 复用仓库里 100% 原有的游玩入口，不重复任何谱面解析 / 资源加载逻辑。
pub fn take_pending_scene(rank_icons: [SafeTexture; 8]) -> Option<NextScene> {
    let pending = take_pending_chart()?;
    let icons = global_icons()?;
    let mut scene = SongScene::new(
        pending.chart,
        pending.local_path,
        icons,
        rank_icons,
        Mods::default(),
    );
    scene.auto_play = pending.auto_play;
    if let Some(mods) = pending.mods {
        scene.mods = mods;
    }
    Some(NextScene::Overlay(Box::new(scene)))
}

/// 主场景在 `update` 里调用：把待打开原生页面组装成 `Box<dyn Page>`。
pub fn take_pending_open_page(rank_icons: [SafeTexture; 8]) -> Option<Box<dyn Page>> {
    let kind = take_pending_open()?;
    // `Library` 需要 `state.icons`，本函数拿不到：把待办放回队列（不消费），
    // 交给主场景的 `take_pending_library()` 走原生曲库那条路。
    if matches!(kind, PendingOpenPage::Library) {
        *PENDING_OPEN.lock().unwrap() = Some(PendingOpenPage::Library);
        return None;
    }
    let icons = global_icons()?;
    match kind {
        PendingOpenPage::Settings => Some(Box::new(crate::page::SettingsPage::new(
            icons.icon.clone(),
            icons.lang.clone(),
        ))),
        PendingOpenPage::Event => Some(Box::new(crate::page::EventPage::new(
            Arc::clone(&icons),
            rank_icons,
        ))),
        PendingOpenPage::ResPack => crate::page::ResPackPage::new(Arc::clone(&icons))
            .ok()
            .map(|p| Box::new(p) as Box<dyn Page>),
        // 上面已提前放回队列，这里只是让 match 穷尽。
        PendingOpenPage::Library => None,
    }
}

/* ============================ 命令分发 ============================ */

/// JS 调用入口：解析消息、分发到对应命令、拼回 `{id, result}` 或 `{id, error}`。
///
/// 返回值直接作为 `window.__phira_recv(...)` 的参数传给 JS。
pub fn handle_ipc(raw: &str) -> Option<String> {
    let msg: IpcMessage = match serde_json::from_str(raw) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("[webui] 无法解析 IPC 消息 {raw}: {e}");
            return None;
        }
    };
    let id = msg.id?;
    // 命令处理是 async（网络 / 文件），由宿主线程上的运行时驱动；
    // 这里直接在当前线程 block_on，宿主调用点保证有 tokio runtime 可用。
    let result = block_on_dispatch(&msg.cmd, msg.args);
    let out = match result {
        Ok(value) => json!({ "id": id, "result": value }),
        Err(err) => json!({ "id": id, "error": err.to_string() }),
    };
    Some(out.to_string())
}

/// 在「有 tokio runtime」的线程上驱动 async 命令处理。
fn block_on_dispatch(cmd: &str, args: Value) -> Result<Value> {
    // 优先用当前线程已有的 runtime handle；没有就临时建一个（仅命令处理用，开销很小）。
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(dispatch(cmd, args)),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            rt.block_on(dispatch(cmd, args))
        }
    }
}

/// 真正的命令实现。所有命令名必须与 `app.js` 的 `invoke(...)` 完全一致。
async fn dispatch(cmd: &str, args: Value) -> Result<Value> {
    match cmd {
        "get_me" => Ok(get_me().await?),
        "login" => Ok(login(args).await?),
        "logout" => Ok(logout()?),
        "get_local_charts" => Ok(get_local_charts(args)?),
        "get_online_charts" => Ok(get_online_charts(args).await?),
        "start_chart" => Ok(start_chart(args).await?),
        "start_last_chart" => Ok(start_last_chart().await?),
        "get_bg" => Ok(get_bg()?),
        "set_bg" => Ok(set_bg(args)?),
        "get_settings" => Ok(get_settings()?),
        "set_settings" => Ok(set_settings(args)?),
        "get_respacks" => Ok(get_respacks()?),
        "set_respack" => Ok(set_respack(args)?),
        "import_respack" => Ok(import_respack()?),
        "ui_page_changed" => Ok(ui_page_changed(args)),
        "open_rust_page" => Ok(open_rust_page(args)),
        other => Err(anyhow!("未知命令：{other}")),
    }
}

/* ======================= 开始页按钮 → 动作（原生命中共用） ======================= */

/// 开始页 5 个按钮的 id，顺序与 `ui/index.html` 的 DOM 顺序一致。
///
/// * JS 侧：`START_PAGE_INIT_SCRIPT` 的 `HANDLERS`（注入脚本用）；
/// * Rust 侧：[`button_command`] 的动作表 + [`crate::webui_hit::hit`] 的矩形表（iOS 原生触摸命中用）。
///
/// 三处必须描述同一批按钮，靠单元测试焊死
/// （`js_handlers_match_rust_button_table` / `button_ids_match_the_shared_table`）。
pub const START_PAGE_BUTTON_IDS: [&str; 5] = ["btn-start", "btn-res", "btn-settings", "btn-activity", "btn-last"];

/// 按钮 id → `(IPC 命令, 参数)`。
///
/// **与 `START_PAGE_INIT_SCRIPT` 的 `HANDLERS` 一一对应**（有测试盯着）：
///
/// | 按钮 | 命令 | 参数 |
/// |---|---|---|
/// | `#btn-start` 开始游玩 | `open_rust_page` | `{page:"library"}`（原生曲库） |
/// | `#btn-res` 资源包 | `open_rust_page` | `{page:"respack"}` |
/// | `#btn-settings` 设置 | `open_rust_page` | `{page:"settings"}` |
/// | `#btn-activity` 活动 | `open_rust_page` | `{page:"event"}` |
/// | `#btn-last` 上次游玩 | `start_last_chart` | `{}` |
pub fn button_command(btn: &str) -> Option<(&'static str, Value)> {
    let (cmd, args) = match btn {
        "btn-start" => ("open_rust_page", json!({ "page": "library" })),
        "btn-res" => ("open_rust_page", json!({ "page": "respack" })),
        "btn-settings" => ("open_rust_page", json!({ "page": "settings" })),
        "btn-activity" => ("open_rust_page", json!({ "page": "event" })),
        "btn-last" => ("start_last_chart", json!({})),
        _ => return None,
    };
    Some((cmd, args))
}

/// 由**原生触摸命中**（iOS，见 [`crate::webui_hit`]）直接触发开始页按钮动作。
///
/// 走的是与 JS 侧 `window.__PHIRA__.invoke(...)` **完全相同**的一条路：
/// 按 [`button_command`] 拼出 `{ id, cmd, args }` 交给 [`handle_ipc`] ——
/// 消息解析、异步运行时驱动、`open_rust_page` / `start_last_chart` 的实现
/// 全部复用，这里**没有**第二份动作分发逻辑。
///
/// `id` 用 0：原生侧没有 Promise 要配对，只需要一个能过 `handle_ipc` 的占位。
///
/// 返回 `true` 表示按钮已知、命令已派发（命令自身的成功 / 失败只写诊断日志，
/// 因为原生路径没有 JS 回调可以接收错误）。
pub fn invoke_button_action(btn: &str) -> bool {
    let Some((cmd, args)) = button_command(btn) else {
        diag(&format!("原生触摸命中了未知按钮 {btn}，已忽略"));
        return false;
    };
    // 这一行是手机上排查「按钮到底有没有被 Rust 收到」的唯一线索（设置 → 日志里能看）。
    diag(&format!("原生触摸命中 {btn} → {cmd} {args}"));
    let Some(resp) = handle_ipc(&json!({ "id": 0, "cmd": cmd, "args": args }).to_string()) else {
        diag(&format!("按钮 {btn} 的 IPC 消息未能解析"));
        return true;
    };
    // 原生路径没有 JS 的 Promise 去接错误，只能记日志，不然失败是「静默」的。
    if let Ok(value) = serde_json::from_str::<Value>(&resp) {
        if let Some(err) = value.get("error").and_then(Value::as_str) {
            diag(&format!("按钮 {btn} 执行失败：{err}"));
        }
    }
    true
}

/* ---------------------------- 各命令实现 ---------------------------- */

async fn get_me() -> Result<Value> {
    // 直接返回持久化的 `me`（登录后写入 data.json，重启仍在）。
    Ok(json!(get_data().me))
}

async fn login(args: Value) -> Result<Value> {
    let username = args
        .get("username")
        .and_then(|it| it.as_str())
        .ok_or_else(|| anyhow!("缺少 username"))?
        .to_owned();
    let password = args
        .get("password")
        .and_then(|it| it.as_str())
        .ok_or_else(|| anyhow!("缺少 password"))?
        .to_owned();

    Client::login(LoginParams::Password {
        email: username.as_str(),
        password: password.as_str(),
    })
    .await
    .map_err(|e| anyhow!("登录失败：{e}"))?;

    // 拉取并保存用户信息
    let me = Client::get_me().await.map_err(|e| anyhow!("获取用户信息失败：{e}"))?;
    get_data_mut().me = Some(me);
    save_data()?;
    notify_me_changed();
    Ok(json!({ "ok": true }))
}

fn logout() -> Result<Value> {
    get_data_mut().me = None;
    get_data_mut().tokens = None;
    save_data()?;
    crate::sync_data();
    notify_me_changed();
    Ok(json!({ "ok": true }))
}

/// 本地谱面：来自 `Data.charts`（已导入 / 下载的 `.pez`/`.zip`）。
fn get_local_charts(args: Value) -> Result<Value> {
    let search = args
        .get("search")
        .and_then(|it| it.as_str())
        .unwrap_or("")
        .to_lowercase();
    let list: Vec<WebChart> = get_data()
        .charts
        .iter()
        .filter(|lc| matches_search(&lc.info.name, &lc.info.charter, &lc.info.composer, &search))
        .map(local_to_web_chart)
        .collect();
    Ok(json!(list))
}

async fn get_online_charts(args: Value) -> Result<Value> {
    let chart_type = args
        .get("type")
        .and_then(|it| it.as_i64())
        .unwrap_or(0);
    let search = args
        .get("search")
        .and_then(|it| it.as_str())
        .unwrap_or("")
        .to_owned();
    let order = args
        .get("order")
        .and_then(|it| it.as_str())
        .unwrap_or("")
        .to_owned();
    let page = args.get("page").and_then(|it| it.as_u64()).unwrap_or(0);

    let mut qb = Client::query::<crate::client::Chart>();
    if chart_type >= 0 {
        qb = qb.query("type", chart_type.to_string());
    }
    if !search.is_empty() {
        qb = qb.search(search);
    }
    if !order.is_empty() {
        qb = qb.order(order);
    }
    let (charts, _) = qb.page(page).send().await.map_err(|e| anyhow!("加载在线谱面失败：{e}"))?;

    let list: Vec<WebChart> = charts.iter().map(remote_to_web_chart).collect();
    Ok(json!(list))
}

async fn start_chart(args: Value) -> Result<Value> {
    let id = args.get("id").and_then(|it| it.as_i64());
    let path = args
        .get("path")
        .and_then(|it| it.as_str())
        .map(|it| it.to_owned());

    let (chart, local_path) = if let Some(path) = path {
        // 本地谱面：在已导入列表里按 local_path 找，复用 load_local 的完整构造（含封面任务）
        let item = load_local()
            .into_iter()
            .find(|it| it.local_path.as_deref() == Some(path.as_str()))
            .ok_or_else(|| anyhow!("本地谱面不存在：{path}"))?;
        (item, Some(path))
    } else if let Some(id) = id {
        // 在线谱面：拉取后构造 ChartItem（load 返回 Arc<Chart>，&Arc<Chart> 经 Deref 协变即可当 &Chart 用）
        let chart = Client::load::<crate::client::Chart>(id as i32)
            .await
            .map_err(|e| anyhow!("加载谱面 {id} 失败：{e}"))?;
        (ChartItem::from_remote(&chart), None)
    } else {
        return Err(anyhow!("start_chart 需要 id 或 path"));
    };

    // Web 预选页点「开始游玩」会带 autoPlay=true：SongScene 随即直接开玩，
    // 不再弹自己的预选界面（在线谱面还没本地路径，auto_play 不会触发，自动退回原流程）。
    let auto_play = args
        .get("autoPlay")
        .and_then(|it| it.as_bool())
        .unwrap_or(false);
    let mods = args.get("mods").and_then(parse_selected_mods);

    *PENDING_CHART.lock().unwrap() = Some(PendingChart {
        chart,
        local_path,
        auto_play,
        mods,
    });
    // 隐藏 WebView，露出下面的游戏画面（主场景消费 pending 后推送 SongScene）
    set_webui_visible(false);
    Ok(json!({ "ok": true }))
}

async fn start_last_chart() -> Result<Value> {
    // 取「最近更新」的本地谱面路径（先克隆出来，释放对全局 data 的借用，
    // 避免与后续 load_local / save_data 的全局锁重入冲突）
    let path = get_data()
        .charts
        .iter()
        .max_by_key(|it| it.info.chart_updated)
        .map(|it| it.local_path.clone());
    let Some(path) = path else {
        return Ok(json!({ "ok": false, "error": "没有上次游玩记录" }));
    };
    let item = load_local()
        .into_iter()
        .find(|it| it.local_path.as_deref() == Some(path.as_str()))
        .ok_or_else(|| anyhow!("本地谱面不存在"))?;
    *PENDING_CHART.lock().unwrap() = Some(PendingChart {
        chart: item,
        local_path: Some(path),
        auto_play: true,   // 开始页点的「上次游玩」，直接开玩
        mods: None,
    });
    set_webui_visible(false);
    Ok(json!({ "ok": true }))
}

/* ------------------------- 动态背景开关 ------------------------- */

/// 读取「TriGrid 动态背景是否开启」（存在 `Data.webui_bg_disabled`，持久化到 data.json）。
fn get_bg() -> Result<Value> {
    Ok(json!({ "enabled": !get_data().webui_bg_disabled }))
}

/// 设置动态背景开关并落盘。关掉后 JS 侧会停掉 rAF + 清屏，不再跑全屏着色器。
fn set_bg(args: Value) -> Result<Value> {
    let enabled = args
        .get("enabled")
        .and_then(|it| it.as_bool())
        .unwrap_or(true);
    get_data_mut().webui_bg_disabled = !enabled;
    save_data()?;
    Ok(json!({ "ok": true, "enabled": enabled }))
}

/// 读取 Web 设置页需要的常用打歌设置；只输出 UI 明确要编辑的字段。
fn get_settings() -> Result<Value> {
    let config = &get_data().config;
    Ok(json!({
        "speed": config.speed,
        "offset": config.offset,
        "noteScale": config.note_scale,
        "volumeMusic": config.volume_music,
        "volumeSfx": config.volume_sfx,
        "volumeBgm": config.volume_bgm,
        "autoplay": config.autoplay(),
        "showAcc": config.show_acc,
        "showAvgFps": config.show_avg_fps,
        "judgeText": config.judge_text,
        "noteTrail": config.note_trail,
        "disableEffect": config.disable_effect,
        "particle": config.particle,
        "mods": mods_to_value(config.mods),
        "respackId": get_data().respack_id,
    }))
}

fn set_settings(args: Value) -> Result<Value> {
    let patch = args.get("patch").and_then(Value::as_object).ok_or_else(|| anyhow!("缺少设置内容"))?;
    let config = &mut get_data_mut().config;
    for (key, value) in patch {
        match key.as_str() {
            "speed" => config.speed = value.as_f64().ok_or_else(|| anyhow!("speed 必须是数字"))?.clamp(0.5, 2.0) as f32,
            "offset" => config.offset = value.as_f64().ok_or_else(|| anyhow!("offset 必须是数字"))?.clamp(-300.0, 300.0) as f32,
            "noteScale" => config.note_scale = value.as_f64().ok_or_else(|| anyhow!("noteScale 必须是数字"))?.clamp(0.5, 2.0) as f32,
            "volumeMusic" => config.volume_music = value.as_f64().ok_or_else(|| anyhow!("volumeMusic 必须是数字"))?.clamp(0.0, 1.0) as f32,
            "volumeSfx" => config.volume_sfx = value.as_f64().ok_or_else(|| anyhow!("volumeSfx 必须是数字"))?.clamp(0.0, 1.0) as f32,
            "volumeBgm" => {
                config.volume_bgm = value.as_f64().ok_or_else(|| anyhow!("volumeBgm 必须是数字"))?.clamp(0.0, 1.0) as f32;
                crate::scene::BGM_VOLUME_UPDATED.store(true, Ordering::Relaxed);
            }
            "autoplay" => config.set_autoplay(value.as_bool().ok_or_else(|| anyhow!("autoplay 必须是布尔值"))?),
            "showAcc" => config.show_acc = value.as_bool().ok_or_else(|| anyhow!("showAcc 必须是布尔值"))?,
            "showAvgFps" => config.show_avg_fps = value.as_bool().ok_or_else(|| anyhow!("showAvgFps 必须是布尔值"))?,
            "judgeText" => config.judge_text = value.as_bool().ok_or_else(|| anyhow!("judgeText 必须是布尔值"))?,
            "noteTrail" => config.note_trail = value.as_bool().ok_or_else(|| anyhow!("noteTrail 必须是布尔值"))?,
            "disableEffect" => config.disable_effect = value.as_bool().ok_or_else(|| anyhow!("disableEffect 必须是布尔值"))?,
            "particle" => config.particle = value.as_bool().ok_or_else(|| anyhow!("particle 必须是布尔值"))?,
            other => return Err(anyhow!("不支持修改的设置：{other}")),
        }
    }
    save_data()?;
    Ok(json!({ "ok": true }))
}

/// MOD 按键只接收白名单 ID，按 Rust 的冲突规则逐项 toggle。
fn parse_selected_mods(value: &Value) -> Option<Mods> {
    let selected = value.as_array()?;
    let mut mods = Mods::empty();
    for id in selected.iter().filter_map(Value::as_str) {
        let flag = match id {
            "flipX" => Mods::FLIP_X,
            "fadeIn" => Mods::FADE_IN,
            "fadeOut" => Mods::FADE_OUT,
            "nightcore" => Mods::NIGHTCORE,
            "rainbow" => Mods::RAINBOW,
            "noShader" => Mods::NO_SHADER,
            "instantDeathAp" => Mods::INSTANT_DEATH_AP,
            "instantDeathFc" => Mods::INSTANT_DEATH_FC,
            "noComboScore" => Mods::NO_COMBO_SCORE,
            "fullScreenJudge" => Mods::FULL_SCREEN_JUDGE,
            "healthMode" => Mods::HEALTH_MODE,
            _ => continue,
        };
        mods.toggle_mod(flag);
    }
    Some(mods)
}

fn mods_to_value(mods: Mods) -> Value {
    let all = [
        ("flipX", Mods::FLIP_X),
        ("fadeIn", Mods::FADE_IN),
        ("fadeOut", Mods::FADE_OUT),
        ("nightcore", Mods::NIGHTCORE),
        ("rainbow", Mods::RAINBOW),
        ("noShader", Mods::NO_SHADER),
        ("instantDeathAp", Mods::INSTANT_DEATH_AP),
        ("instantDeathFc", Mods::INSTANT_DEATH_FC),
        ("noComboScore", Mods::NO_COMBO_SCORE),
        ("fullScreenJudge", Mods::FULL_SCREEN_JUDGE),
        ("healthMode", Mods::HEALTH_MODE),
    ];
    json!(all.iter().filter_map(|(id, flag)| mods.contains(*flag).then_some(*id)).collect::<Vec<_>>())
}

fn respack_path(index: usize) -> Result<Option<std::path::PathBuf>> {
    if index == 0 {
        return Ok(None);
    }
    let id = get_data()
        .respacks
        .get(index - 1)
        .ok_or_else(|| anyhow!("资源包索引无效"))?;
    let rel = std::path::Path::new(id);
    if rel.components().count() != 1 || !matches!(rel.components().next(), Some(std::path::Component::Normal(_))) {
        return Err(anyhow!("资源包路径无效"));
    }
    Ok(Some(std::path::Path::new(&crate::dir::respacks()?).join(rel)))
}

fn get_respacks() -> Result<Value> {
    let selected = get_data().respack_id;
    let mut packs = vec![json!({
        "id": 0,
        "name": "默认资源包",
        "author": "Phira",
        "description": "使用游戏内置音效、音符和特效。",
        "selected": selected == 0,
        "preview": null
    })];
    for index in 1..=get_data().respacks.len() {
        let Ok(Some(path)) = respack_path(index) else { continue };
        let Ok(raw) = std::fs::read(path.join("info.yml")) else { continue };
        let Ok(info) = serde_yaml::from_slice::<serde_yaml::Value>(&raw) else { continue };
        let get = |key: &str| info.get(key).and_then(serde_yaml::Value::as_str).unwrap_or("");
        packs.push(json!({
            "id": index,
            "name": get("name"),
            "author": get("author"),
            "description": get("description"),
            "selected": selected == index,
            "preview": format!("./respack-preview?index={index}&file=click.png")
        }));
    }
    Ok(json!({ "packs": packs, "selected": selected }))
}

fn set_respack(args: Value) -> Result<Value> {
    let id = args.get("id").and_then(Value::as_u64).ok_or_else(|| anyhow!("资源包 ID 无效"))? as usize;
    if id > get_data().respacks.len() {
        return Err(anyhow!("资源包不存在"));
    }
    if id > 0 {
        let path = respack_path(id)?.ok_or_else(|| anyhow!("资源包不存在"))?;
        if !path.join("info.yml").is_file() {
            return Err(anyhow!("资源包信息文件不存在"));
        }
    }
    get_data_mut().respack_id = id;
    save_data()?;
    Ok(json!({ "ok": true, "id": id }))
}

fn import_respack() -> Result<Value> {
    #[cfg(target_arch = "wasm32")]
    return Err(anyhow!("当前平台不支持导入资源包"));
    #[cfg(not(target_arch = "wasm32"))]
    {
        prpr::scene::request_file("_import_respack");
        Ok(json!({ "ok": true }))
    }
}

fn ui_page_changed(args: Value) -> Value {
    if let Some(page) = args.get("page").and_then(|it| it.as_str()) {
        set_page(page);
    }
    json!(null)
}

fn open_rust_page(args: Value) -> Value {
    let page = args.get("page").and_then(|it| it.as_str()).unwrap_or("");
    // 【重要】这里**不能**把 settings / respack 再转回 Web 页面（`goto_page`）。
    //
    // 开始页 `ui/index.html` 是用户给的成品（逐字节照搬，SHA256 固定），里面
    // **没有任何 `page-*` 容器**（grep 全文件只有 `btn-*` 这 5 个 id），也没引
    // `bridge.js` / `app.js`。而 `goto()`（app.js:229）第一件事就是
    // `$('page-' + page)`，取不到就直接 `return` —— 所以 `goto_page("settings")`
    // 什么都不会发生，点「设置」/「资源包」会毫无反应。
    //
    // 正确行为：两个按钮都落到 Rust 原生页面（与 `event` / `library` 同一条路：
    // 登记 PENDING_OPEN + 隐藏 WebView，由主场景 `take_pending_open_page` 构造）。
    let pending = match page {
        "settings" => Some(PendingOpenPage::Settings),
        "respack" => Some(PendingOpenPage::ResPack),
        "event" => Some(PendingOpenPage::Event),
        // 开始页「开始游玩」→ 原生曲库。与 event 同一处理路径：登记待办 + 藏起 WebView，
        // 由主场景 `take_pending_library()` 构造 `LibraryPage`。
        "library" => Some(PendingOpenPage::Library),
        _ => None,
    };
    if let Some(p) = pending {
        *PENDING_OPEN.lock().unwrap() = Some(p);
        // 隐藏 WebView，露出下面的 Rust 原生页面
        set_webui_visible(false);
        json!({ "ok": true })
    } else {
        json!({ "ok": false, "error": format!("未知页面：{page}") })
    }
}

/* ---------------------------- 映射辅助 ---------------------------- */

/// 难度数字格式化：去掉多余的小数点。
/// `15.0` → `"15"`，`15.50` → `"15.5"`，`15.25` → `"15.25"`（真有小数才保留）。
fn format_level(v: f64) -> String {
    if !v.is_finite() || v == 0. {
        return String::new();
    }
    if v.fract() == 0. {
        return format!("{}", v as i64);
    }
    // 最多两位小数，再去掉尾随的 0 和小数点
    format!("{:.2}", v)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn matches_search(name: &str, charter: &str, composer: &str, search: &str) -> bool {
    if search.is_empty() {
        return true;
    }
    let name = name.to_lowercase();
    let charter = charter.to_lowercase();
    let composer = composer.to_lowercase();
    name.contains(search) || charter.contains(search) || composer.contains(search)
}

fn local_to_web_chart(lc: &LocalChart) -> WebChart {
    let info = &lc.info;
    WebChart {
        id: info.id,
        name: info.name.clone(),
        composer: if info.composer.is_empty() {
            None
        } else {
            Some(info.composer.clone())
        },
        charter: if info.charter.is_empty() {
            None
        } else {
            Some(info.charter.clone())
        },
        difficulty: if info.level.is_empty() {
            None
        } else {
            Some(info.level.clone())
        },
        level: if info.difficulty == 0. {
            None
        } else {
            Some(format_level(info.difficulty as f64))
        },
        // 本地谱面封面在 zip 内部，这里先留空（JS 显示首字母占位）；
        // 后续可经自定义协议按 local_path 解析出来再补。
        illustration: None,
        path: Some(lc.local_path.clone()),
    }
}

fn remote_to_web_chart(chart: &crate::client::Chart) -> WebChart {
    WebChart {
        id: Some(chart.id),
        name: chart.name.clone(),
        composer: if chart.composer.is_empty() {
            None
        } else {
            Some(chart.composer.clone())
        },
        charter: if chart.charter.is_empty() {
            None
        } else {
            Some(chart.charter.clone())
        },
        difficulty: if chart.level.is_empty() {
            None
        } else {
            Some(chart.level.clone())
        },
        level: if chart.difficulty == 0. {
            None
        } else {
            Some(format_level(chart.difficulty as f64))
        },
        illustration: Some(chart.illustration.url.clone()),
        path: None,
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
pub const START_PAGE_INIT_SCRIPT: &str = r#"
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
    // 先装视口（原因见 installViewport 的注释），再绑定按钮。
    installViewport();
    bind();
  }

  /* ---------- 视口：固定设计宽度 ----------
   *
   * `ui/index.html` 是**宽屏桌面设计**，尺寸全是写死的 px：
   *   `.right-panel { right:60px; width:460px }`、`.logo { right:30px; width:200px }`
   * 而它自己**没有** `<meta name="viewport">`（逐字节冻结的原件，不能加）。
   *
   * 若按手机宽度（约 800 CSS px）布局，460px 的面板要占 58% 屏宽，会直接压住
   * 右上角的 logo —— 用户报的「开始菜单按钮太大完全遮住 logo」。
   *
   * 指定固定设计宽度后，整页按设计稿比例排版，再由 WebView 缩放到屏幕宽度，
   * 各元素的相对位置就与设计稿一致（面板 36% / logo 16%，不再重叠）。
   */
  var DESIGN_WIDTH = 1280;

  function installViewport() {
    var head = document.head || document.documentElement;
    if (!head) return;
    var m = document.querySelector('meta[name="viewport"]');
    if (!m) {
      m = document.createElement('meta');
      m.name = 'viewport';
      head.appendChild(m);
    }
    m.content = 'width=' + DESIGN_WIDTH + ', user-scalable=no, minimum-scale=0.2, maximum-scale=5';
  }

  /* ---------- （已停用）右上角：头像 + 名字 ----------
   *
   * 曾经在开始页右上角注入「头像 + 名字」当登录入口，但 `index.html` 的 `.logo`
   * 本来就占着 `top:30px; right:30px` —— 注入的元素**把用户的 logo 挡住了**。
   *
   * 现在账号管理完整落在「设置 → 账号」页（原生 UI，添加 / 切换 / 删除），
   * 所以这里**彻底删掉**了注入代码，不再往 DOM 里加任何东西。
   *
   * ⚠️ 教训：删这段时曾留下半截函数体（孤立 `}` + 已删掉的 `ACCOUNT_STYLE`），
   * 那是 **JS 语法错误** —— 注入脚本一报错，整个 bridge（含 5 个按钮的监听）
   * 都不再执行。改这个字符串后**务必确认 JS 括号配平**（Rust 编译不检查 JS！）。
   */

  start();
})();
"#;

/* ============================ 测试 ============================ */

#[cfg(test)]
mod tests {
    use super::*;

    /// 从 `START_PAGE_INIT_SCRIPT` 里把 `HANDLERS` 表抠出来，解析成
    /// `(按钮 id, 命令, page 参数)` 三元组。
    ///
    /// 解析对象就是注入脚本里那 5 行 `'btn-x': function () { return invoke('cmd', { page: 'y' }); },`
    /// （`btn-last` 没有参数）。任何格式变动都会在这里炸出来 —— 这正是目的。
    fn js_handlers() -> Vec<(String, String, String)> {
        let script = START_PAGE_INIT_SCRIPT;
        let start = script.find("var HANDLERS = {").expect("START_PAGE_INIT_SCRIPT 里找不到 HANDLERS 表");
        let end = start + script[start..].find("};").expect("HANDLERS 表没有闭合的 };");
        let block = &script[start..end];

        let mut out = Vec::new();
        for line in block.lines() {
            let Some(rest) = line.trim().strip_prefix('\'') else { continue };
            let Some((id, rest)) = rest.split_once('\'') else { continue };
            let Some((_, rest)) = rest.split_once("invoke('") else { continue };
            let Some((cmd, rest)) = rest.split_once('\'') else { continue };
            let arg = rest
                .split_once("page: '")
                .and_then(|(_, it)| it.split_once('\''))
                .map(|(v, _)| v.to_owned())
                .unwrap_or_default();
            out.push((id.to_owned(), cmd.to_owned(), arg));
        }
        out.sort();
        out
    }

    fn rust_handlers() -> Vec<(String, String, String)> {
        let mut out: Vec<_> = START_PAGE_BUTTON_IDS
            .iter()
            .map(|id| {
                let (cmd, args) = button_command(id).unwrap_or_else(|| panic!("Rust 动作表里缺 {id}"));
                let page = args.get("page").and_then(Value::as_str).unwrap_or("").to_owned();
                ((*id).to_owned(), cmd.to_owned(), page)
            })
            .collect();
        out.sort();
        out
    }

    /// 注入脚本（JS）与 [`button_command`]（Rust 原生命中）描述的动作必须完全一致。
    ///
    /// 这两张表一旦漂移，就会出现「iOS 上点『设置』开了资源包」这类最难查的错，
    /// 所以用测试把它们焊在一起，而不是靠注释提醒。
    #[test]
    fn js_handlers_match_rust_button_table() {
        let js = js_handlers();
        assert_eq!(js.len(), 5, "HANDLERS 应该正好 5 个按钮，解析到：{js:?}");
        assert_eq!(js, rust_handlers(), "注入脚本的 HANDLERS 与 Rust 的 button_command 漂移了");
    }

    /// 每个按钮都必须能在动作表里查到，且未知 id 要干净地返回 `None`。
    #[test]
    fn button_command_covers_every_button_id() {
        for id in START_PAGE_BUTTON_IDS {
            let (cmd, args) = button_command(id).unwrap_or_else(|| panic!("{id} 没有对应命令"));
            assert!(!cmd.is_empty());
            if cmd == "open_rust_page" {
                assert!(
                    matches!(
                        args.get("page").and_then(Value::as_str),
                        Some("library" | "respack" | "settings" | "event")
                    ),
                    "{id} 的 page 参数不合法：{args}"
                );
            } else {
                assert_eq!(cmd, "start_last_chart");
            }
        }
        assert!(button_command("btn-nope").is_none());
        assert!(button_command("").is_none());
    }
}
