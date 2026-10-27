//! WebView 开始界面宿主（Windows / wry 0.44）。
//!
//! 用自定义协议 `phirc://web/<path>` 把 **`kaichang/` 目录**当作 Web 根喂给 wry，
//! 再把 WebView 挂到 macroquad 游戏窗口（HWND）上作为子窗口 —— 于是开始界面
//! 就是 `kaichang/index.html` 本身。
//!
//! ⚠️ `kaichang/index.html` **逐字节不改**：全屏触点靠
//! `with_initialization_script` 注入（每次页面加载都会执行，且早于页面自身的脚本）。
//!
//! 踩坑记录（改之前先读，这几条都是花时间换来的）：
//! * wry 0.44 **不自带消息泵**，宿主必须自己 `PeekMessage/DispatchMessage`：
//!   否则 WebView2 子窗口永远不绘制（黑屏），且主线程 `SendMessage` 过来会死锁
//!   （游戏窗口卡死、拖不动）。
//! * 子窗口默认 bounds 只有 200×200，必须显式 `with_bounds` 铺满客户区，
//!   否则界面只有左上角一小块、其余全黑。
//! * `with_bounds` 收**逻辑**像素，而 `GetClientRect` 给**物理**像素 → 要按 DPI 除回去，
//!   否则高 DPI 屏上 WebView 会被再放大一轮、撑出窗口。
//! * 隐藏 WebView 后必须把键盘焦点还给游戏窗口，否则游戏内自绘输入框一个字也打不进。

use anyhow::{anyhow, Result};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};

use crate::page::{request_export, resolve_export, take_export};
use crate::scene::{compress_folder, MainScene};
// `request_file` / `show_error` 定义在 `prpr::scene`；`crate::scene` 只是私有 `use` 进来，
// 从别处不能当路径用（E0603），所以这里直接引原始路径。
use prpr::scene::{request_file, show_error};

/// 安卓 JNI 桥所用类型（windows 构建不引 jni，避免未用告警）。
#[cfg(target_os = "android")]
use jni::{
    objects::{Global, JClass, JObject, JString},
    EnvUnowned,
};

/// 默认首页（相对 Web 根；自定义协议 `phirc` 见 [`serve_asset`]）。
const DEFAULT_ENTRY: &str = "index.html";

/// 首页 URL。可用 `PHIRC_WEBUI_ENTRY` 覆盖（填相对路径或完整 `phirc://…`）——
/// 这样想单独调某个页面时**不用改 `index.html`**。
fn entry_url() -> String {
    let raw = std::env::var("PHIRC_WEBUI_ENTRY").unwrap_or_else(|_| DEFAULT_ENTRY.to_owned());
    let raw = raw.trim();
    if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("phirc://web/{}", raw.trim_start_matches('/'))
    }
}

/// 主线程 / IPC 线程 → WebView 线程 的命令。
enum WebCmd {
    /// 在页面里执行一段 JS（IPC 回包 / 推事件用）
    Eval(String),
    /// 真正显示 / 隐藏 WebView 控件（不是页面里的元素）
    Visible(bool),
    /// 让 WebView 跳转到某个地址（设置页用它从开始页 / 原生界面切过去）
    Navigate(String),
    /// 把 WebView 重新绑定到父窗口（全屏切换 / 改了父窗口 style 之后，
    /// WebView2 的 DComp 表面（`Intermediate D3D Window`）可能失活，
    /// 表现为「live 视图白屏、Alt+Tab 缩略图里还能看到界面」）。
    /// 重设 bounds + 可见 + 焦点，逼它重新上屏。
    Refresh,
}

/// 宿主持有：向 WebView 所在线程投递命令的发送端。
struct Host {
    cmd_tx: mpsc::Sender<WebCmd>,
}

static HOST: OnceLock<Host> = OnceLock::new();
/// 游戏主窗口 HWND（给 rfd 文件对话框当 owner —— 没有 owner 时对话框会弹到
/// 全屏游戏窗口后面，表现就是「点了选择音乐没反应」）。
#[cfg(target_os = "windows")]
static GAME_HWND: OnceLock<isize> = OnceLock::new();

/// 启动 WebView 宿主（幂等）。失败只记日志，界面回退到 macroquad 原生 UI。
///
/// Windows：wry + WebView2 线程；安卓：见下方安卓段（JNI → Java `WebViewHost`）。
#[cfg(target_os = "windows")]
pub fn init_host() {
    if HOST.get().is_some() {
        return;
    }
    match start() {
        Ok(host) => {
            let _ = HOST.set(host);
            // Windows 上 wry 建出来就是可见的（`start()` 里 `with_url` 直接导航到首页、
            // 子窗口默认显示），所以这里把「可见」标记置位 —— 与安卓 / iOS 默认隐藏
            // （要靠 `show_start_page` 显式显示）区分开。`start()` 失败时 HOST 没建立，
            // 不置位，`host_available()` 返回 false，界面自然回退原生 UI。
            WEBVIEW_VISIBLE.store(true, Ordering::Relaxed);
        }
        Err(e) => tracing::error!("[webui] 启动失败，回退原生界面：{e:?}"),
    }
}

/// 显示 WebView。
#[allow(dead_code)]
pub fn show() {
    if let Some(h) = HOST.get() {
        WEBVIEW_VISIBLE.store(true, Ordering::Relaxed);
        let _ = h.cmd_tx.send(WebCmd::Visible(true));
    }
}

/// WebView 当前是不是显示着的。
///
/// 用来兜住「原生 Home 页已删掉」这条：只要原生页面栈回到根、而 WebView 又被藏着，
/// 就说明马上要露出那个不要的原生 Home 了 → 立刻把开始页显示出来。
pub fn webview_visible() -> bool {
    WEBVIEW_VISIBLE.load(Ordering::Relaxed)
}

/// 隐藏 WebView（把界面还给 macroquad 原生 UI）。
#[allow(dead_code)]
pub fn hide() {
    if let Some(h) = HOST.get() {
        let _ = h.cmd_tx.send(WebCmd::Visible(false));
    }
}

/// 让位给原生界面（**正确的隐藏方式**）。
///
/// ⚠️ 不要用 [`hide()`]：它只发 `WebCmd::Visible(false)`，**不更新 `WEBVIEW_VISIBLE`**，
/// 而 keepalive 每 ~1s 会按 `webview_visible()` **幂等重断言 `set_visible(true)`**
/// —— 于是 WebView 一秒后又被显示回来，盖住原生弹窗（用户实测「一直跳条约、点不到接受」）。
/// 这里两个标志位一起改，再发命令，keepalive 才不会把界面抢回去。
pub fn handoff_to_native() {
    if let Some(h) = HOST.get() {
        WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
        WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
        let _ = h.cmd_tx.send(WebCmd::Visible(false));
    }
}

/// 让 WebView 回到开始页（`index.html`）并显示。
///
/// 用户要求把 macroquad 的原生 Home 页「删掉」：原生页面栈回到根时**不再露出
/// `HomePage`**（那个「开始游戏 / 资源包 / 谱面工具箱 / 设置 / 活动 / 消息 / 关于 / 退出」
/// 的面板），而是把开始页显示出来 —— 这个 mod 的主页就是 WebView 的 `index.html`。
///
/// 所有原本会「露出原生 Home」的出口都改走这里：
/// * 原生页面栈回根（返回箭头）
/// * `open_page('home')`
/// * 页面发来的 `hide`（「我完事了」）
///
/// WebView 不可用（没有 WebView2）时什么都不做，调用方自然回退到原生 Home 兜底。
pub fn show_start_page() {
    let Some(h) = HOST.get() else { return };
    WEBVIEW_HANDOFF.store(false, Ordering::Relaxed);
    WEBVIEW_VISIBLE.store(true, Ordering::Relaxed);
    let _ = h.cmd_tx.send(WebCmd::Navigate(entry_url()));
    let _ = h.cmd_tx.send(WebCmd::Visible(true));
}

/// 把游戏窗口的**所有子窗口**（类名 / 可见性 / 尺寸）拼成一行，给诊断用。
///
/// WebView2 的容器是游戏窗口的 `WS_CHILD`；只要看这一行就能判断
/// 「WebView 到底有没有被创建 / 有没有被隐藏 / 尺寸对不对」。
#[cfg(target_os = "windows")]
fn dump_child_windows(hwnd: isize) -> String {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, GetClassNameW, GetWindowRect, IsWindowVisible,
    };

    unsafe extern "system" fn cb(child: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam as *mut Vec<String>);
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(child, buf.as_mut_ptr(), 256) };
        let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let mut rc: RECT = unsafe { std::mem::zeroed() };
        unsafe { GetWindowRect(child, &mut rc) };
        out.push(format!(
            "{class} vis={} {}x{}",
            unsafe { IsWindowVisible(child) } != 0,
            rc.right - rc.left,
            rc.bottom - rc.top
        ));
        TRUE
    }

    let mut v: Vec<String> = Vec::new();
    unsafe {
        EnumChildWindows(hwnd as HWND, Some(cb), &mut v as *mut Vec<String> as LPARAM);
    }
    if v.is_empty() {
        "(无子窗口)".to_owned()
    } else {
        v.join(" | ")
    }
}

/// 给窗口补上 `WS_CLIPCHILDREN`（少了它父窗口的 blit 会盖住 WebView 子窗口）。
///
/// ⚠️ 根因：miniquad 的 `get_win_style()` 在**全屏**分支只给
/// `WS_POPUP | WS_SYSMENU | WS_VISIBLE` —— **丢掉了 `WS_CLIPCHILDREN`**
/// （窗口模式那份是 `WS_CLIPSIBLINGS | WS_CLIPCHILDREN | WS_CAPTION | …`）。
/// 少了这一位，父窗口每帧把画面 blit 到客户区时会**盖住子窗口**，而 WebView 正是
/// 挂在游戏窗口上的 `WS_CHILD` → 一切全屏 WebView 就被游戏自己的画面糊掉，
/// 看上去「界面又变回 macroquad 原生 UI 了」。
#[cfg(target_os = "windows")]
fn ensure_clip_children(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_STYLE, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
        WS_POPUP,
    };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        // 补上 WS_CLIPCHILDREN|WS_CLIPSIBLINGS（少了前者父窗口 blit 会盖住 WebView 子窗口），
        // 同时**剥掉 WS_POPUP**：miniquad 全屏窗口是 WS_POPUP，它会让 WebView2 的 DComp 表面
        // 在 live 视图失活（全屏白屏）。改成无边框顶层窗口。
        let mut want = style | WS_CLIPCHILDREN as isize | WS_CLIPSIBLINGS as isize;
        want &= !(WS_POPUP as isize);
        if want != style {
            SetWindowLongPtrW(hwnd, GWL_STYLE, want);
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

/// 切全屏 / 回窗口。
///
/// ⚠️ **刻意不走 `macroquad::window::set_fullscreen`**：miniquad 的全屏分支会把 style
/// 换成 `WS_POPUP | WS_SYSMENU | WS_VISIBLE`，**丢掉 `WS_CLIPCHILDREN`** —— 后果见
/// [`ensure_clip_children`]。所以这里自己改 style + 尺寸，把那两位保留住。
///
/// 改完再让 WebView 重画一遍（它的像素刚被上一帧的父窗口 blit 覆盖过，不重画会花屏）：
/// 先藏再显，两条命令同一轮循环里连发，实际看不到闪烁。
/// 非 Windows 平台没有这个问题，照旧交给 macroquad。
pub fn set_fullscreen(fullscreen: bool) {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SetWindowLongPtrW, SetWindowPos, GWL_STYLE, SM_CXSCREEN, SM_CYSCREEN,
            SWP_DEFERERASE, SWP_DRAWFRAME, SWP_FRAMECHANGED, SWP_NOACTIVATE, WS_CAPTION,
            WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SIZEBOX,
            WS_SYSMENU, WS_VISIBLE,
        };

        if let Some(raw) = find_game_hwnd() {
            let hwnd = raw as HWND;
            let style: isize = if fullscreen {
                // ⚠️ 全屏用「无边框顶层窗口」，**不要** `WS_POPUP`：
                // `WS_POPUP` 父窗口会让 WebView2 的 DComp 表面（`Intermediate D3D Window`）
                // 在 live 视图里失活 —— 表现就是全屏下一片白（Alt+Tab 缩略图里还能看到界面）。
                // 无边框顶层窗口 DWM 能正常把子窗口（WebView）合成进 live 视图。
                (WS_CLIPSIBLINGS | WS_CLIPCHILDREN | WS_VISIBLE) as isize
            } else {
                (WS_CLIPSIBLINGS | WS_CLIPCHILDREN | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX
                    | WS_MAXIMIZEBOX | WS_SIZEBOX) as isize
            };
            // 进全屏前记下当前窗口矩形；退出全屏时**原样还原**。
            // ⚠️ 以前退出全屏是把窗口硬编码拉成 973x608，用户自己拉过的窗口
            // 大小/位置直接丢 —— 看着就是「窗口变形」。
            let (mut px, mut py, mut pw, mut ph) = if fullscreen {
                unsafe { (0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
            } else {
                // 没有记录时的兜底尺寸
                (0, 0, 973, 608)
            };
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;
                let mut rc: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
                if fullscreen {
                    if GetWindowRect(hwnd, &mut rc) != 0 {
                        let mut g = PRE_FULLSCREEN_RECT.lock().unwrap();
                        if g.is_none() {
                            *g = Some((rc.left, rc.top, rc.right, rc.bottom));
                        }
                    }
                } else if let Some((l, t, r, b)) = *PRE_FULLSCREEN_RECT.lock().unwrap() {
                    px = l;
                    py = t;
                    pw = r - l;
                    ph = b - t;
                }
                SetWindowLongPtrW(hwnd, GWL_STYLE, style);
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    px,
                    py,
                    pw,
                    ph,
                    SWP_FRAMECHANGED | SWP_DEFERERASE | SWP_DRAWFRAME | SWP_NOACTIVATE,
                );
            }
            let size = unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;
                let mut rc: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                (rc.right - rc.left, rc.bottom - rc.top)
            };
            crate::diag(&format!(
                "[webui] 全屏={fullscreen} style=0x{style:x} 客户区={}x{}",
                size.0, size.1
            ));
        }
    }
    #[cfg(not(target_os = "windows"))]
    macroquad::window::set_fullscreen(fullscreen);

    if webview_visible() {
        if let Some(h) = HOST.get() {
            // 改了父窗口 style / 尺寸后，逼 WebView 重新绑定表面（见 `WebCmd::Refresh`）。
            let _ = h.cmd_tx.send(WebCmd::Refresh);
        }
    }
}

/// 打开设置界面：把 WebView 导航到 `Setting.html` 并显示。
///
/// - WebView2 可用时：导航（带 `?from=native` 标记，返回时让位给原生界面）+ 显示。
/// - WebView2 不可用时（`HOST` 未初始化）：静默无操作，调用方（主场景
///   `goto_hud_page`）会据此回退到原生 Rust 设置页。
pub fn open_settings() {
    if let Some(h) = HOST.get() {
        // 反过来要显示 WebView 了，之前那次「推给原生」的标记作废。
        WEBVIEW_HANDOFF.store(false, Ordering::Relaxed);
        WEBVIEW_VISIBLE.store(true, Ordering::Relaxed);
        let _ = h.cmd_tx.send(WebCmd::Navigate("phirc://web/Setting.html?from=native".to_owned()));
        let _ = h.cmd_tx.send(WebCmd::Visible(true));
    }
}

// ============================================================
//  谱面管理面板（ChartPanel.html）
// ============================================================

fn chart_dir_of(local_path: &str) -> anyhow::Result<std::path::PathBuf> {
    Ok(std::path::PathBuf::from(crate::dir::charts()?).join(local_path))
}

/// 读谱面目录里的 `info.yml`（面板要拿全字段做编辑页）。
fn read_info_yml(dir: &std::path::Path) -> anyhow::Result<prpr::info::ChartInfo> {
    let path = dir.join("info.yml");
    let f = std::fs::File::open(&path).map_err(|e| anyhow::anyhow!("打开 {} 失败：{e}", path.display()))?;
    serde_yaml::from_reader(f).map_err(|e| anyhow::anyhow!("解析 info.yml 失败：{e}"))
}

/// 面板一次拿到的整包数据。
fn chart_panel_payload(local_path: &str) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    let dir = chart_dir_of(local_path)?;
    let info = read_info_yml(&dir)?;

    let data = crate::get_data();
    let chart = data.charts.iter().find(|c| c.local_path == local_path);
    let mods = chart.map(|c| c.mods.bits()).unwrap_or(0);
    let cfg = &data.config;

    Ok(json!({
        "info": {
            "id": info.id,
            "name": info.name,
            "charter": info.charter,
            "composer": info.composer,
            "illustrator": info.illustrator,
            "level": info.level,
            "difficulty": info.difficulty,
            "intro": info.intro,
            "tip": info.tip,
            "previewStart": info.preview_start,
            "previewEnd": info.preview_end,
            "offset": info.offset,
            "aspectRatio": info.aspect_ratio,
            "backgroundDim": info.background_dim,
            "forceAspectRatio": info.force_aspect_ratio,
            "useRpe170Speed": info.use_rpe_170_speed,
            "useAttachUiFix": info.use_attach_ui_fix,
            "useRpen": info.use_rpen,
            "enableUnlock": info.unlock_video.is_some(),
            "unlockVideo": info.unlock_video,
            "chart": info.chart,
            "music": info.music,
            "illustration": info.illustration,
            "tags": info.tags,
        },
        "mods": mods,
        "config": {
            "offset": cfg.offset,
            "healthMax": cfg.health_max,
            "healthScale": cfg.health_scale,
            "healthBarLen": cfg.health_bar_len,
            "healthBarColor": format!("{:?}", cfg.health_bar_color).to_lowercase(),
        },
        "rating": serde_json::Value::Null,
        "typeText": if info.id.is_some() { "在线谱面" } else { "本地谱面" },
        "tags": info.tags,
    }))
}

/// 全局 Mod 目录：`(id, 名称, 说明, 可上传, 互斥组)`。顺序即页面显示顺序。
const MOD_CATALOG: &[(&str, &str, &str, bool, &str)] = &[
    ("autoplay", "自动游玩", "电脑代打（不上传成绩）", false, ""),
    ("flipX", "X 轴反转", "谱面左右镜像", true, ""),
    ("fadeOut", "上隐", "音符靠近判定线才显现", true, "fade"),
    ("fadeIn", "下隐", "音符靠近判定线时隐藏", true, "fade"),
    ("nightcore", "夜店", "1.5 倍速游玩（改速不上传）", false, ""),
    ("rainbow", "彩虹", "音符变色", true, ""),
    ("instantDeathAp", "突然死亡（AP）", "非 Perfect 立即失败", true, "sd"),
    ("instantDeathFc", "突然死亡（FC）", "连击中断立即失败", true, "sd"),
    ("noShader", "无着色器", "关掉谱面特效（不上传）", false, ""),
    ("noComboScore", "去连击分", "分数 = 准确率 × 1,000,000（不上传）", false, ""),
    ("fullScreenJudge", "全屏判定", "屏幕任意位置都能判定（不上传）", false, ""),
    ("healthMode", "血条模式", "Perfect 回血 / Bad 扣血，归零失败（不上传）", false, ""),
];

fn mod_flag(id: &str) -> Option<prpr::config::Mods> {
    use prpr::config::Mods;
    Some(match id {
        "autoplay" => Mods::AUTOPLAY,
        "flipX" => Mods::FLIP_X,
        "fadeIn" => Mods::FADE_IN,
        "fadeOut" => Mods::FADE_OUT,
        "nightcore" => Mods::NIGHTCORE,
        "rainbow" => Mods::RAINBOW,
        "instantDeathAp" => Mods::INSTANT_DEATH_AP,
        "instantDeathFc" => Mods::INSTANT_DEATH_FC,
        "noShader" => Mods::NO_SHADER,
        "noComboScore" => Mods::NO_COMBO_SCORE,
        "fullScreenJudge" => Mods::FULL_SCREEN_JUDGE,
        "healthMode" => Mods::HEALTH_MODE,
        _ => return None,
    })
}

/// 给 Mod.html 的完整状态：目录 + 开关 + 位标志 + 是否吃上传。
fn mods_state_json(mods: prpr::config::Mods) -> serde_json::Value {
    use serde_json::json;
    let list: Vec<serde_json::Value> = MOD_CATALOG
        .iter()
        .map(|(id, name, desc, upload, group)| {
            let flag = mod_flag(id).unwrap_or_else(prpr::config::Mods::empty);
            json!({
                "id": id,
                "name": name,
                "desc": desc,
                "upload": upload,
                "group": group,
                "bit": flag.bits(),
                "enabled": mods.contains(flag),
            })
        })
        .collect();
    json!({
        "ok": true,
        "bits": mods.bits(),
        "unrated": mods.intersects(prpr::config::Mods::UNRATED),
        "mods": list,
    })
}

/// 面板改 Mods：写回 `data.charts[].mods` 并通知原生详情页刷新。
fn set_chart_mods(local_path: &str, bits: i32) -> anyhow::Result<()> {
    let mods = prpr::config::Mods::from_bits_truncate(bits);
    {
        let data = crate::get_data_mut();
        let Some(chart) = data.charts.iter_mut().find(|c| c.local_path == local_path) else {
            anyhow::bail!("曲库里找不到这张谱面");
        };
        chart.mods = mods;
    }
    crate::save_data()?;
    *PENDING_MODS.lock().unwrap() = Some((local_path.to_owned(), bits));
    Ok(())
}

/// 面板保存「编辑谱面」：把 patch 合并进 `info.yml`。
fn chart_panel_save(local_path: &str, patch: &serde_json::Value) -> anyhow::Result<()> {
    let dir = chart_dir_of(local_path)?;
    let mut info = read_info_yml(&dir)?;
    let as_str = |k: &str| patch.get(k).and_then(|v| v.as_str()).map(|v| v.to_owned());
    let as_f64 = |k: &str| patch.get(k).and_then(|v| v.as_f64());
    let as_bool = |k: &str| patch.get(k).and_then(|v| v.as_bool());

    if let Some(v) = as_str("name") {
        info.name = v;
    }
    if let Some(v) = as_str("charter") {
        info.charter = v;
    }
    if let Some(v) = as_str("composer") {
        info.composer = v;
    }
    if let Some(v) = as_str("illustrator") {
        info.illustrator = v;
    }
    if let Some(v) = as_str("level") {
        info.level = v;
    }
    if let Some(v) = as_str("intro") {
        info.intro = v;
    }
    if let Some(v) = as_str("tip") {
        info.tip = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = as_f64("difficulty") {
        info.difficulty = v as f32;
    }
    if let Some(v) = as_f64("backgroundDim") {
        info.background_dim = v as f32;
    }
    if let Some(v) = as_f64("previewStart") {
        info.preview_start = v as f32;
    }
    if let Some(v) = as_f64("previewEnd") {
        info.preview_end = Some(v as f32);
    }
    if let Some(v) = as_f64("offset") {
        info.offset = v as f32;
    }
    if let Some(v) = as_f64("aspectRatio") {
        if v > 0. {
            info.aspect_ratio = v as f32;
        }
    }
    if let Some(v) = as_bool("forceAspectRatio") {
        info.force_aspect_ratio = v;
    }
    // 三态：null = 未设置
    if let Some(v) = patch.get("useRpe170Speed") {
        info.use_rpe_170_speed = if v.is_null() { None } else { v.as_bool() };
    }
    if let Some(v) = patch.get("useAttachUiFix") {
        info.use_attach_ui_fix = if v.is_null() { None } else { v.as_bool() };
    }
    // RPEN 特性开关：false = 回退旧版行为（忽略曲线轨迹 / 分层噪域 / shader 事件 / globalEvents…）
    if let Some(v) = patch.get("useRpen") {
        info.use_rpen = if v.is_null() { None } else { v.as_bool() };
    }
    if let Some(v) = as_bool("enableUnlock") {
        if v {
            if info.unlock_video.is_none() {
                info.unlock_video = Some("unlock.mp4".to_owned());
            }
        } else {
            info.unlock_video = None;
        }
    }
    // 预览区间兜底：至少 1 秒、最多 20 秒
    let end = info.preview_end.unwrap_or(info.preview_start + 15.);
    if end - info.preview_start < 1. {
        info.preview_end = Some(info.preview_start + 1.);
    } else if end - info.preview_start > 20. {
        info.preview_end = Some(info.preview_start + 20.);
    }

    let yml = serde_yaml::to_string(&info).map_err(|e| anyhow::anyhow!("序列化 info.yml 失败：{e}"))?;
    std::fs::write(dir.join("info.yml"), yml)?;
    // 曲绘可能换了 → 缩略图缓存作废
    if let Ok(p) = crate::page::thumbnail_path(local_path) {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// 面板替换谱面文件（谱面 / 音乐 / 曲绘 / 解锁动画）。
fn copy_chart_file(local_path: &str, kind: &str, src: &str) -> anyhow::Result<String> {
    let dir = chart_dir_of(local_path)?;
    let src_path = std::path::Path::new(src);
    let name = src_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .ok_or_else(|| anyhow::anyhow!("无效的文件路径"))?;
    std::fs::copy(src_path, dir.join(&name))?;

    let mut info = read_info_yml(&dir)?;
    match kind {
        "chart" => info.chart = name.clone(),
        "music" => info.music = name.clone(),
        "illustration" => info.illustration = name.clone(),
        "unlock" => info.unlock_video = Some(name.clone()),
        other => anyhow::bail!("未知的文件类型 {other}"),
    }
    std::fs::write(dir.join("info.yml"), serde_yaml::to_string(&info)?)?;
    if let Ok(p) = crate::page::thumbnail_path(local_path) {
        let _ = std::fs::remove_file(p);
    }
    Ok(name)
}

/// 打开「谱面管理」面板（Mod / 全局设置 / 编辑谱面 / 谱面信息）。
///
/// 从原生谱面详情页的 Mod 按钮进入；`local_path` 用 URL 查询参数带进页面。
pub fn open_chart_panel(local_path: &str) {
    if let Some(h) = HOST.get() {
        WEBVIEW_HANDOFF.store(false, Ordering::Relaxed);
        WEBVIEW_VISIBLE.store(true, Ordering::Relaxed);
        let encoded = urlencode(local_path);
        let _ = h.cmd_tx.send(WebCmd::Navigate(format!("phirc://web/ChartPanel.html?path={encoded}")));
        let _ = h.cmd_tx.send(WebCmd::Visible(true));
    }
}

/// 最小 URL 编码（只保留 unreserved 字符）。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 面板改了 Mods 后，原生谱面详情页要跟着更新（它把 mods 存在内存里）。
static PENDING_MODS: Mutex<Option<(String, i32)>> = Mutex::new(None);

/// 取走「面板改过的 Mods」（`(local_path, bits)`），由 SongScene 每帧调用。
pub fn take_pending_mods() -> Option<(String, i32)> {
    PENDING_MODS.lock().ok().and_then(|mut m| m.take())
}

/// WebView2 宿主是否可用（决定设置入口走 WebView 还是原生回退）。
#[cfg(not(target_os = "android"))]
pub fn host_available() -> bool {
    HOST.get().is_some() && !WEBVIEW_DEAD.load(Ordering::Relaxed)
}

/// 安卓版：除了 Rust 侧命令通道 `HOST`，**还必须有 Java 侧 `WebViewHost` 实例**
/// （`nativeRegisterHost` 注册进 `WEBVIEW_HOST`）才能算「可用」。
///
/// 否则会出现这种白屏：Java 的 `WebViewHost.init()` 在 `nativeRegisterHost` 之前抛了
/// 异常（被 MainActivity 的 try/catch 吞掉、把 `webHost` 置 null），而 Rust 侧
/// `init_host()` 照样把 `HOST` 建好了 → `host_available()` 返回 `true` → 原生 Home 页
/// 被「删掉」早返回、WebView 命令却因 `WEBVIEW_HOST` 为 None 被转发线程静默丢弃 →
/// 画面既没原生 Home、WebView 也永不显示 = 完全白屏，且没有任何回退。
///
/// 加上 `WEBVIEW_HOST` 这个判据后，Java 侧没成功注册就 `host_available()==false`，
/// 所有入口自动回退到原生 UI（不会白屏）。正常启动时 `nativeRegisterHost` 一定早于
/// Rust 游戏线程（`MainActivity` 里 `webHost.init()` 在 `activityOnCreate` 之前），所以
/// 不会误伤。
#[cfg(target_os = "android")]
pub fn host_available() -> bool {
    HOST.get().is_some()
        && !WEBVIEW_DEAD.load(Ordering::Relaxed)
        && WEBVIEW_HOST.get().is_some()
}

/// 打开资源包界面：把 WebView 导航到 `ResPack.html` 并显示。
///
/// - WebView2 可用时：导航（带 `?from=native` 标记，返回时让位给原生界面）+ 显示。
/// - WebView2 不可用时（`HOST` 未初始化）：静默无操作，调用方（库页 / 首页）
///   会据此回退到原生 Rust 资源包页 `ResPackPage`。
pub fn open_respack() {
    if let Some(h) = HOST.get() {
        // 同上：显示 WebView 了，作废「推给原生」的标记。
        WEBVIEW_HANDOFF.store(false, Ordering::Relaxed);
        WEBVIEW_VISIBLE.store(true, Ordering::Relaxed);
        let _ = h.cmd_tx.send(WebCmd::Navigate("phirc://web/ResPack.html?from=native".to_owned()));
        let _ = h.cmd_tx.send(WebCmd::Visible(true));
    }
}

/// 打开曲库界面：把 WebView 导航到 `library.html` 并显示。
///
/// - WebView2 可用时：导航（带 `?from=native` 标记，页面底部「返回」键回原生界面）
///   + 显示。曲库列表 / 曲绘 / 开局全部走页面里的 `listCharts` / `startGame` 桥。
/// - WebView2 不可用时（`HOST` 未初始化）：静默无操作，调用方（HUD 曲库按钮 /
///   原生菜单）据此回退到原生 Rust 曲库页 `LibraryPage`。
pub fn open_library() {
    if let Some(h) = HOST.get() {
        // 同上：显示 WebView 了，作废「推给原生」的标记。
        WEBVIEW_HANDOFF.store(false, Ordering::Relaxed);
        WEBVIEW_VISIBLE.store(true, Ordering::Relaxed);
        let _ = h.cmd_tx.send(WebCmd::Navigate("phirc://web/library.html?from=native".to_owned()));
        let _ = h.cmd_tx.send(WebCmd::Visible(true));
    }
}

// ============================================================
//  收藏夹 → 开局：跨线程的「待开局谱面」
// ============================================================

/// IPC 线程只把谱面路径放这里；**真正建场景必须回到游戏线程**（贴图要在那边建）。
static PENDING_CHART: Mutex<Option<String>> = Mutex::new(None);

/// 在线谱面开局：IPC 线程按 id 从服务器取到 `Chart` 后暂存这里，
/// 由 `take_next_scene` 在游戏线程建 `SongScene`（`local_path = None`，
/// 用户按 play 时它会自己下载 / 解压 / 登记进 `data.charts`）。
static PENDING_ONLINE_CHART: Mutex<Option<std::sync::Arc<crate::client::Chart>>> = Mutex::new(None);

/// 课题模式：WebView 曲库选好的 3 个 local_path，等游戏线程构造 `ChallengeScene`。
static PENDING_CHALLENGE: Mutex<Option<Vec<String>>> = Mutex::new(None);
/// 段位考核：路径 + 段位名。
static PENDING_DAN: Mutex<Option<(Vec<String>, String)>> = Mutex::new(None);

pub fn take_pending_dan() -> Option<(Vec<String>, String)> {
    PENDING_DAN.lock().ok()?.take()
}

/// 游戏线程取走课题请求（`ChartItem` 带纹理，只能在这边建）。
pub fn take_pending_challenge() -> Option<Vec<String>> {
    PENDING_CHALLENGE.lock().ok()?.take()
}

/// WebView 请求走一次「服务条款」流程。
///
/// ⚠️ **绝不能在 WebView 线程调 `check_read_tos_and_policy`**：它可能
/// `Dialog::show()`（非主线程操作 prpr UI，会死锁）并 `get_data_mut() + save_data()`
/// （写盘）——两者都会把 WebView 的消息泵卡住，表现就是「点在线分类就卡死」。
/// 所以桥里只做线程安全的只读判断，真要弹窗就把请求丢给游戏线程。
static PENDING_TOS: AtomicBool = AtomicBool::new(false);

/// 游戏线程每帧调一次：有请求就在**主线程**跑一次条款检查（必要时弹原生对话框）。
pub fn take_pending_tos() -> bool {
    PENDING_TOS.swap(false, Ordering::Relaxed)
}

/// 线程安全的「条款是否已接受」只读判断（不碰 UI、不写盘）。
///
/// 口径与 `scene::check_read_tos_and_policy` 的**早退分支**一致。
/// 没通过时不在这里弹窗，而是置 `PENDING_TOS` 交给游戏线程。
fn tos_accepted() -> bool {
    crate::scene::tos_ok_for_online()
}

/// 需要走条款流程时的统一回包：请求弹窗（游戏线程）+ 告诉页面原因。
fn reply_need_tos(reply: &dyn Fn(serde_json::Value)) {
    use serde_json::json;
    PENDING_TOS.store(true, Ordering::Relaxed);
    reply(json!({ "ok": false, "error": "must-accept-tos" }));
}

/// 游戏线程每帧调一次：有请求就构造 `SongScene` 推出去。
///
/// 照抄 `page/history.rs` 的 `DAILY_START` 模式（`thread_local` 暂存 → `next_scene` 取走），
/// 区别只是这里跨线程，所以用 `Mutex` 而不是 `RefCell`。
pub fn take_next_scene(s: &mut crate::page::SharedState) -> Option<prpr::scene::NextScene> {
    // —— 1) 本地谱面开局（path 来自 listCharts）——
    if let Some(path) = PENDING_CHART.lock().ok().and_then(|mut m| m.take()) {
        let Some(icons) = crate::page::global_icons() else {
            tracing::error!("[webui] 开局失败：UI icons 还没准备好");
            return None;
        };
        // `load_local()` 会建贴图，所以只能在游戏线程调用
        let Some(item) = crate::page::load_local().into_iter().find(|it| it.local_path.as_deref() == Some(path.as_str())) else {
            tracing::error!("[webui] 开局失败：曲库里找不到 {path}");
            return None;
        };
        tracing::info!("[webui] 开局：{}", item.info.name);
        let local_path = item.local_path.clone();
        // 每张谱面存的那份 mods（ChartPanel 改过的话要生效）
        let mods = {
            let data = crate::get_data();
            data.charts.iter().find(|it| it.local_path == path).map(|it| it.mods).unwrap_or_else(prpr::config::Mods::empty)
        };
        // 从 WebView 曲库开局：**跳过 SongScene 信息页**（用户要求删掉这个界面），
        // `auto_launch` 会立刻 launch → LoadingScene → 游戏。
        return Some(prpr::scene::NextScene::Overlay(Box::new(
            crate::scene::SongScene::new(item, local_path, icons, s.icons.clone(), mods).with_auto_launch(),
        )));
    }
    // —— 1b) 回放播放（watchReplay）——
    // 先建谱面信息页；用户在信息页按 play 时才真正开始播放（槽 B 由 SongScene::launch 转交）。
    if let Some(replay) = crate::scene::replay::take_pending_scene() {
        let Some(icons) = crate::page::global_icons() else {
            tracing::error!("[webui] 回放开局失败：UI icons 还没准备好");
            return None;
        };
        // 找谱面：在线谱要本地已下载过；本地谱按路径找
        let (item, local_path) = match replay.chart.as_ref() {
            Some(crate::replay::ChartRef::Id(id)) => {
                let found = crate::page::load_local().into_iter().find(|it| it.info.id == Some(*id));
                match found {
                    Some(it) => {
                        let lp = it.local_path.clone();
                        (it, lp)
                    }
                    None => {
                        tracing::error!("[webui] 回放对应的在线谱面 {id} 不在本地曲库，无法播放");
                        return None;
                    }
                }
            }
            Some(crate::replay::ChartRef::Local(path)) => match crate::page::load_local().into_iter().find(|it| it.local_path.as_deref() == Some(path.as_str())) {
                Some(it) => {
                    let lp = it.local_path.clone();
                    (it, lp)
                }
                None => {
                    tracing::error!("[webui] 回放对应的本地谱面 {path} 不在曲库，无法播放");
                    return None;
                }
            },
            None => {
                tracing::error!("[webui] 回放没有记录谱面引用，无法播放");
                return None;
            }
        };
        tracing::info!("[webui] 播放回放：{}", item.info.name);
        let mut scene = crate::scene::SongScene::new(item, local_path, icons, s.icons.clone(), prpr::config::Mods::empty());
        scene.set_replay_on_launch(replay);
        return Some(prpr::scene::NextScene::Overlay(Box::new(scene)));
    }
    // —— 2) 在线谱面开局（listOnlineCharts 点卡）——
    // `local_path` 传 None：SongScene 会先拉到远程曲绘/信息；`with_auto_launch()` 让它
    // 自动走「下载 → 解压 → 登记进 data.charts → 开局」，不再停在信息页。
    if let Some(chart) = PENDING_ONLINE_CHART.lock().ok().and_then(|mut m| m.take()) {
        let Some(icons) = crate::page::global_icons() else {
            tracing::error!("[webui] 在线开局失败：UI icons 还没准备好");
            return None;
        };
        let item = crate::page::ChartItem::from_remote(chart.as_ref());
        tracing::info!("[webui] 在线开局：{}（id={}）", item.info.name, chart.id);
        return Some(prpr::scene::NextScene::Overlay(Box::new(
            crate::scene::SongScene::new(item, None, icons, s.icons.clone(), prpr::config::Mods::empty()).with_auto_launch(),
        )));
    }
    None
}

// ============================================================
//  收藏夹 → 打开原生页面（设置 / 收藏 / 曲库）
// ============================================================

/// IPC 线程只把要打开的页面放这里；**真正建页面必须回到游戏线程**（要 `global_icons`）。
static PENDING_PAGE: Mutex<Option<crate::hud::PageId>> = Mutex::new(None);

/// 游戏线程每帧调一次：有请求就交给 `MainScene::goto_hud_page` 切页（仿 `PENDING_CHART`）。
pub fn take_pending_page() -> Option<crate::hud::PageId> {
    PENDING_PAGE.lock().ok()?.take()
}

/// WebView 请求打开的「工具覆盖页」。
///
/// 这两项在原生 Home 面板里是 `NextPage::Overlay`（压在 Home 之上），**不属于
/// `PageId`**（`PageId` 是 HUD 布局里那 8 个带 key/序号的整页），所以不能塞进
/// `PENDING_PAGE`，另开一条通道交给 `MainScene::open_web_tool`。
#[derive(Clone, Copy, Debug)]
pub enum WebTool {
    /// 谱面工具箱（`ChartToolboxPage`）
    Toolbox,
    /// 活动（`EventPage`）
    Event,
    /// 编辑按键准度条（`TimingEditor`）
    Timing,
}

/// IPC 线程放请求；游戏线程建页面（要 `global_icons` / `state.icons`）。
static PENDING_TOOL: Mutex<Option<WebTool>> = Mutex::new(None);

/// 游戏线程每帧调一次：有请求就交给 `MainScene::open_web_tool` 打开覆盖页。
pub fn take_pending_tool() -> Option<WebTool> {
    PENDING_TOOL.lock().ok()?.take()
}

/// 「这次原生页面是 WebView 推过来的」标记。
///
/// 收藏夹里点 PLAY / 工具面板时，WebView 会 `hide()` 让位，原生页面栈 push 一页
/// （曲库 / 谱面工具箱 / 活动）。用户按原生返回箭头时，如果这一页是 WebView 推的，
/// 就应该**回到 WebView**（从哪来回哪去），而不是落到原生主界面。
///
/// 只由「WebView → 原生」的三个入口置位；`open_settings` / `open_respack` 是
/// 反过来「显示 WebView」，会把标记清掉。
static WEBVIEW_HANDOFF: AtomicBool = AtomicBool::new(false);

/// WebView 当前可见性。
///
/// ⚠️ 初值必须是 `false`：只有 Windows 的 wry WebView 是「建出来就可见」的
/// （`start()` 里 `with_url` 直接导航到首页，子窗口默认显示）；安卓 / iOS 的
/// WebView 宿主初始是 `GONE` / 隐藏的，要等 Rust 显式发 `WebCmd::Visible(true)`
/// （经 `show_start_page` / `show`）才真正显示。
///
/// 之前这里写 `true`，导致安卓 / iOS 启动自举（main.rs 的「页面栈回到根且 WebView
/// 被藏着 → 重新顶出来」兜底）被 `!webview_visible()` 这个条件直接关掉：启动时
/// `show_start_page()` 没被任何路径调用（HUD 的 `take_goto` 一开始是 `None`，而原生
/// Home 页已删、靠 `host_available()` 早返回不画），WebView 一直 `GONE` 却「被认为可见」
/// → SurfaceView 露白、WebView 永不显示 = 完全白屏。
///
/// Windows 在 `init_host` 的 `start()` 成功后把此标记置 `true`（与「建出来即可见」对齐）；
/// 安卓 / iOS 保持 `false`，靠上面那条兜底在首页就绪后显式 `show_start_page()` 显示。
static WEBVIEW_VISIBLE: AtomicBool = AtomicBool::new(false);

/// 页面是否「活着」—— 收到过页面发来的任何 IPC 就置位。
///
/// `ERROR_JS` 会在 `DOMContentLoaded` 时发一条 `log`，所以**正常加载的页面必然置位**；
/// 反过来，一直没置位就说明页面根本没跑起来（白屏 / 加载失败）。
static PAGE_ALIVE: AtomicBool = AtomicBool::new(false);

/// WebView 被判定为不可用（页面起不来）。
///
/// 一旦置位，`host_available()` 返回 false → 所有「走 WebView」的地方**自动回退原生界面**，
/// 用户至少不会对着一片白屏。
static WEBVIEW_DEAD: AtomicBool = AtomicBool::new(false);

/// 取走并复位「原生页面是 WebView 推过来的」标记。
pub fn take_webview_handoff() -> bool {
    WEBVIEW_HANDOFF.swap(false, Ordering::Relaxed)
}

/// 置位「原生页面是 WebView 推过来的」：原生页自己 pop 回根时，
/// MainScene 会据此把 WebView 的**原来那一页**顶回来（而不是回开始页）。
pub fn mark_handoff() {
    WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
}

/// 外部链接（QQ 频道等）：游戏线程用系统浏览器打开。
static PENDING_URL: Mutex<Option<String>> = Mutex::new(None);

/// 游戏线程每帧调一次：有请求就用 `open_url` 打开。
pub fn take_pending_url() -> Option<String> {
    PENDING_URL.lock().ok()?.take()
}

// ============================================================
//  设置页 → 改游戏配置（WebView 的 Setting.html 通过桥读写真实 Config）
// ============================================================

/// IPC 线程把「配置增量」放这里；真正合并 / 存档 / 触发生效必须在游戏线程做
///（主题、音量、全屏这些副作用要碰游戏线程的全局状态）。
static PENDING_CONFIG: Mutex<Option<serde_json::Value>> = Mutex::new(None);
/// 进入全屏前的窗口矩形（left, top, right, bottom），退出全屏时原样还原。
/// ⚠️ 以前退出全屏是把窗口**硬编码**拉成 973x608，用户自己拉过的窗口大小/位置直接丢。
static PRE_FULLSCREEN_RECT: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);

/// 游戏线程每帧调一次：有增量就合并进真实 `Config`、存档、并触发生效。
pub fn apply_pending_config() {
    let updates = PENDING_CONFIG.lock().ok().and_then(|mut g| g.take());
    if let Some(updates) = updates {
        if let Err(e) = apply_config(updates) {
            tracing::error!("[webui] 应用设置页配置失败：{e:?}");
        }
    }
}

/// 递归合并 JSON：把 `incoming` 合进 `base`（对象逐键合并，标量直接覆盖）。
///
/// 这样 `set_config` 只发「改了的那几个字段」，未涉及的字段保留当前值，
/// 不会把整个 Config 重置成默认值。
fn merge_json(base: &mut serde_json::Value, incoming: &serde_json::Value) {
    use serde_json::Value;
    match (base, incoming) {
        (Value::Object(b), Value::Object(i)) => {
            for (k, v) in i {
                merge_json(b.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (base, incoming) => {
            *base = incoming.clone();
        }
    }
}

/// 把 IPC 送来的配置增量真正落到游戏里：合并 → 序列化回 `Config` → 存档 → 立即生效少量全局项。
fn apply_config(updates: serde_json::Value) -> Result<()> {
    use std::sync::atomic::Ordering;

    // 1) `autoplay` 自成一套逻辑（走 `Mods::AUTOPLAY`，不是普通字段），先抽出来单独处理。
    let autoplay = updates.get("autoplay").and_then(|v| v.as_bool());
    let updates = match autoplay {
        Some(_) => {
            let mut u = updates;
            if let Some(obj) = u.as_object_mut() {
                obj.remove("autoplay");
            }
            u
        }
        None => updates,
    };

    // 2) 合并进当前配置再整体反序列化（保留未涉及的字段）。
    let data = crate::get_data_mut();
    let mut cfg_val = serde_json::to_value(&data.config)?;
    merge_json(&mut cfg_val, &updates);
    let mut new_cfg: prpr::config::Config = serde_json::from_value(cfg_val)?;

    // 3) 应用 autoplay（同时写 mods + 兼容字段）。
    if let Some(on) = autoplay {
        new_cfg.set_autoplay(on);
    }
    data.config = new_cfg;

    // 4) 存档（data.json 里含 config）。
    crate::save_data()?;

    // 5) 立即生效的少数全局副作用；其余配置项在下次使用时才读取，无需当场应用。
    //
    // ⚠️⚠️ 必须**只应用「这次增量里真带了的字段」**：
    // `set_fullscreen()` 在 Windows 上是窗口手术（改 GWL_STYLE + SetWindowPos），
    // 而且退出全屏那支会把窗口尺寸重置掉。以前这里每帧无条件调，
    // 结果用户拖一下音量滑块 → 窗口被强行改尺寸/样式（「窗口直接变形」），
    // 极端情况下还会把 WebView 搞崩（闪退）。
    // 现在只有 `updates` 里真的出现对应键才应用，改音量/判定窗口完全不碰窗口。
    let cfg = &crate::get_data().config;
    if updates.get("uiDark").is_some() {
        crate::newui::set_dark(cfg.ui_dark);
    }
    if updates.get("volumeSfx").is_some() {
        prpr::ui::UI_SFX_VOLUME.store(cfg.volume_sfx.to_bits(), Ordering::Relaxed);
    }
    if updates.get("fullscreenMode").is_some() {
        set_fullscreen(cfg.fullscreen_mode);
    }

    Ok(())
}

// ============================================================
//  资源包页 → 改游戏数据 / 触发导入导出（WebView 的 ResPack.html 通过桥操作真实资源包）
// ============================================================

/// 选中的资源包下标（0=默认包）→ 落到 `data.respack_id`。
static PENDING_RESPCK_SET: Mutex<Option<usize>> = Mutex::new(None);
/// 资源包导入请求（来自 WebView 的「导入」按钮）。
static PENDING_RESPCK_IMPORT: Mutex<bool> = Mutex::new(false);
/// 资源包导出请求（存要导出的下标）。
static PENDING_RESPCK_EXPORT: Mutex<Option<usize>> = Mutex::new(None);
/// 资源包删除请求（存要删除的下标）。
static PENDING_RESPCK_DELETE: Mutex<Option<usize>> = Mutex::new(None);

/// 标记「当前这次资源包导入是由 WebView 发起的」，用于导入完成后只刷新 WebView、不误吞别处导入。
static RESPCK_IMPORT_FROM_WEBVIEW: AtomicBool = AtomicBool::new(false);
/// 导出时待压缩的源目录（WebView 入口没有原生页的 `export_path` 字段，用静态暂存）。
static RESPCK_EXPORT_SRC: Mutex<Option<PathBuf>> = Mutex::new(None);
/// 导出压缩线程的完成信号。
static RESPCK_EXPORT_DONE: Mutex<Option<Result<()>>> = Mutex::new(None);

pub fn take_pending_respack_set() -> Option<usize> {
    PENDING_RESPCK_SET.lock().ok().and_then(|mut g| g.take())
}
pub fn take_pending_respack_import() -> bool {
    // 取走并复位：这是每帧轮询的「一次性触发」标志，读后必须清掉，
    // 否则会反复触发系统文件选择框。
    PENDING_RESPCK_IMPORT
        .lock()
        .ok()
        .map(|mut g| std::mem::replace(&mut *g, false))
        .unwrap_or(false)
}
pub fn take_pending_respack_export() -> Option<usize> {
    PENDING_RESPCK_EXPORT.lock().ok().and_then(|mut g| g.take())
}
pub fn take_pending_respack_delete() -> Option<usize> {
    PENDING_RESPCK_DELETE.lock().ok().and_then(|mut g| g.take())
}

/// 让 WebView 资源包页重新拉一遍列表（导入 / 删除 / 导出完成后调用）。
pub fn notify_respack_refresh() {
    if let Some(h) = HOST.get() {
        let _ = h.cmd_tx.send(WebCmd::Eval(
            "window.__phircRespackRefresh && window.__phircRespackRefresh()".to_owned(),
        ));
    }
}

/// 在 WebView 资源包页上弹一个轻量 toast（导入成功 / 错误提示等）。
pub fn notify_respack_toast(msg: &str) {
    if let Some(h) = HOST.get() {
        let safe = msg.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', " ");
        let _ = h.cmd_tx.send(WebCmd::Eval(format!(
            "window.__phircRespackToast && window.__phircRespackToast('{safe}')"
        )));
    }
}

/// 资源包目录：0=内置默认包（`<exe>/assets/respack`），其余=`data/respack/<uuid>`。
fn respack_dir(index: usize) -> Option<PathBuf> {
    if index == 0 {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?.join("assets").join("respack");
        Some(dir)
    } else {
        let data = crate::get_data();
        let uuid = data.respacks.get(index - 1)?;
        Some(PathBuf::from(format!("{}/{}", crate::dir::respacks().ok()?, uuid)))
    }
}

/// 资源包显示名：0=「默认资源包」，其余读 `info.yml` 的 `name` 字段。
fn respack_name(index: usize) -> String {
    if index == 0 {
        "默认资源包".to_owned()
    } else if let Some(dir) = respack_dir(index) {
        if let Ok(file) = std::fs::File::open(dir.join("info.yml")) {
            if let Ok(v) = serde_yaml::from_reader::<_, serde_yaml::Value>(file) {
                if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                    return name.to_owned();
                }
            }
        }
        "资源包".to_owned()
    } else {
        "资源包".to_owned()
    }
}

/// 资源包信息（信息弹窗用）。
fn respack_info_value(index: usize) -> serde_json::Value {
    use serde_json::json;
    if index == 0 {
        // 内置包也有 info.yml，图集参数照读（预览要用来裁长条三段）
        let mut extra = serde_json::json!({});
        if let Some(dir) = respack_dir(0) {
            if let Ok(file) = std::fs::File::open(dir.join("info.yml")) {
                if let Ok(v) = serde_yaml::from_reader::<_, serde_yaml::Value>(file) {
                    let atlas = |key: &str| -> Vec<f64> {
                        v.get(key)
                            .and_then(|x| x.as_sequence())
                            .map(|a| a.iter().filter_map(|n| n.as_f64().or_else(|| n.as_u64().map(|u| u as f64))).collect())
                            .unwrap_or_default()
                    };
                    extra = json!({
                        "hitFx": atlas("hitFx"),
                        "holdAtlas": atlas("holdAtlas"),
                        "holdAtlasMH": atlas("holdAtlasMH"),
                        "hitFxScale": v.get("hitFxScale").and_then(|x| x.as_f64()).unwrap_or(1.0),
                        "holdCompact": v.get("holdCompact").and_then(|x| x.as_bool()).unwrap_or(false),
                        "holdRepeat": v.get("holdRepeat").and_then(|x| x.as_bool()).unwrap_or(false),
                    });
                }
            }
        }
        return json!({
            "index": 0,
            "name": "默认资源包",
            "author": "官方",
            "description": "游戏内置的默认资源包，不可删除、不可导出。",
            "path": respack_dir(0).map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            "isDefault": true,
        })
        .as_object()
        .cloned()
        .map(|mut m| {
            for (k, v) in extra.as_object().cloned().unwrap_or_default() {
                m.insert(k, v);
            }
            serde_json::Value::Object(m)
        })
        .unwrap_or_else(|| serde_json::json!({ "index": 0, "name": "默认资源包" }));
    }
    if let Some(dir) = respack_dir(index) {
        if let Ok(file) = std::fs::File::open(dir.join("info.yml")) {
            if let Ok(v) = serde_yaml::from_reader::<_, serde_yaml::Value>(file) {
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("资源包").to_owned();
                let author = v.get("author").and_then(|n| n.as_str()).unwrap_or("未知").to_owned();
                let description = v
                    .get("description")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_owned();
                let atlas = |key: &str| -> Vec<f64> {
                    v.get(key)
                        .and_then(|x| x.as_sequence())
                        .map(|a| a.iter().filter_map(|n| n.as_f64().or_else(|| n.as_u64().map(|u| u as f64))).collect())
                        .unwrap_or_default()
                };
                let hit_fx = atlas("hitFx");
                let hold_atlas = atlas("holdAtlas");
                let hold_atlas_mh = atlas("holdAtlasMH");
                return json!({
                    "index": index,
                    "name": name,
                    "author": author,
                    "description": description,
                    "path": dir.to_string_lossy().to_string(),
                    "isDefault": false,
                    // —— 预览要用的图集参数（与 info.yml 同名字段）——
                    "hitFx": hit_fx,
                    "holdAtlas": hold_atlas,
                    "holdAtlasMH": hold_atlas_mh,
                    "hitFxScale": v.get("hitFxScale").and_then(|x| x.as_f64()).unwrap_or(1.0),
                    "holdCompact": v.get("holdCompact").and_then(|x| x.as_bool()).unwrap_or(false),
                    "holdRepeat": v.get("holdRepeat").and_then(|x| x.as_bool()).unwrap_or(false),
                });
            }
        }
    }
    json!({ "index": index, "name": "资源包", "author": "未知", "description": "", "path": "", "isDefault": false })
}

/// 资源包列表（给 ResPack.html 初始化用）：[{ index, name, isDefault }]，附带当前选中的 `current`。
fn respack_list() -> serde_json::Value {
    use serde_json::json;
    let data = crate::get_data();
    let mut items = vec![json!({ "index": 0, "name": "默认资源包", "isDefault": true })];
    let root = crate::dir::respacks().unwrap_or_default();
    for (i, uuid) in data.respacks.iter().enumerate() {
        let p = PathBuf::from(format!("{root}/{uuid}"));
        let name = if let Ok(file) = std::fs::File::open(p.join("info.yml")) {
            serde_yaml::from_reader::<_, serde_yaml::Value>(file)
                .ok()
                .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(|s| s.to_owned()))
                .unwrap_or_else(|| "资源包".to_owned())
        } else {
            "资源包".to_owned()
        };
        items.push(json!({ "index": i + 1, "name": name, "isDefault": false }));
    }
    json!({ "items": items, "current": data.respack_id })
}

/// 游戏线程每帧调一次：落地资源包的写操作（选中 / 导入 / 导出 / 删除），并轮询导入导出完成。
pub fn apply_pending_respack() {
    // 1) 选中：写 `data.respack_id`
    if let Some(idx) = take_pending_respack_set() {
        let data = crate::get_data_mut();
        let max = data.respacks.len(); // 合法下标 0..=max
        if idx <= max {
            data.respack_id = idx;
            let _ = crate::save_data();
        }
    }

    // 2) 导入：只触发原生文件选择，真正的导入在 main.rs 的 `take_file` 里完成
    if take_pending_respack_import() {
        RESPCK_IMPORT_FROM_WEBVIEW.store(true, Ordering::Relaxed);
        request_file("_import_respack");
    }
    // 导入完成（`RESPACK_ITEM` 被设置）→ 刷新列表 + 提示
    if RESPCK_IMPORT_FROM_WEBVIEW.load(Ordering::Relaxed) && MainScene::take_imported_respack().is_some() {
        RESPCK_IMPORT_FROM_WEBVIEW.store(false, Ordering::Relaxed);
        notify_respack_refresh();
        notify_respack_toast("资源包导入成功");
    }

    // 3) 导出：先记下源目录，再弹系统「另存为」对话框（阻塞游戏线程，与原生一致）
    if let Some(idx) = take_pending_respack_export() {
        if let Some(src) = respack_dir(idx) {
            *RESPCK_EXPORT_SRC.lock().unwrap() = Some(src);
            request_export(format!("{}.zip", respack_name(idx)));
        }
    }
    poll_respack_export();

    // 4) 删除：内置包拒绝；其余删目录 + 从 data 移除 + 修正 respack_id
    if let Some(idx) = take_pending_respack_delete() {
        if idx == 0 {
            notify_respack_toast("内置资源包无法删除");
        } else {
            // `get_data_mut()` 返回的就是 `&mut Data`，绑定本身不用再 `mut`。
            let data = crate::get_data_mut();
            if idx <= data.respacks.len() {
                let uuid = data.respacks[idx - 1].clone();
                let root = crate::dir::respacks().unwrap_or_default();
                let _ = std::fs::remove_dir_all(format!("{root}/{uuid}"));
                data.respacks.remove(idx - 1);
                if data.respack_id == idx {
                    data.respack_id = 0;
                } else if data.respack_id > idx {
                    data.respack_id -= 1;
                }
                let _ = crate::save_data();
                // 不用 `drop(data)`：那只是丢一个引用，什么也不做（借用在最后一次使用处就结束了）。
                notify_respack_refresh();
                notify_respack_toast("资源包已删除");
            }
        }
    }
}

/// 轮询导出：取系统另存为的结果 → 起线程压缩源目录 → 完成后刷新 + 提示。
fn poll_respack_export() {
    if let Some(config) = take_export() {
        let src = RESPCK_EXPORT_SRC.lock().unwrap().take();
        match config {
            // `err` 是 `io::Error`，`Into<anyhow::Error>` 不唯一（推断不出目标类型），显式构造。
            Err(err) => show_error(anyhow::Error::new(err)),
            Ok(cfg) => match src {
                Some(src) => {
                    let file = cfg.file;
                    let deleter = cfg.deleter;
                    std::thread::spawn(move || {
                        let result = (|| -> Result<()> {
                            let mut writer = std::io::BufWriter::new(file);
                            compress_folder(&src, &mut writer)?;
                            writer.flush()?;
                            Ok(())
                        })();
                        if result.is_err() {
                            let _ = (deleter)();
                        }
                        *RESPCK_EXPORT_DONE.lock().unwrap() = Some(result);
                    });
                }
                None => {
                    drop(cfg.file);
                    let _ = (cfg.deleter)();
                    show_error(anyhow!("没有选中的资源包可导出"));
                }
            },
        }
    }
    if let Some(res) = RESPCK_EXPORT_DONE.lock().unwrap().take() {
        match res {
            Ok(()) => {
                resolve_export();
                notify_respack_refresh();
                notify_respack_toast("资源包导出成功");
            }
            Err(e) => show_error(e),
        }
    }
}

/// 导入失败时（main.rs 的 `_import_respack` 错误分支调用）通知 WebView 弹错误。
pub fn notify_respack_import_error(msg: &str) {
    notify_respack_toast(&format!("导入失败：{msg}"));
}

// ============================================================
//  多比例适配（等比缩放）
// ============================================================

/// 页面的**设计分辨率**：页面按这个尺寸排版，再整体等比缩放到窗口。
///
/// 为什么需要它：`kaichang/` 里的页面全是死值定位（`right:320px`、`height:100vh`…），
/// 游戏窗口默认只有 973×608，直接塞进去必然重叠。等比缩放后，**页面拿到的视口
/// 在两个方向上都不会小于设计尺寸**（`zoom = min(winW/设计W, winH/设计H)`），
/// 所以无论窗口是什么比例，布局都跟设计稿一致，也不会被裁掉。
///
/// * `PHIRC_WEBUI_DESIGN=1600x900` 改设计尺寸（默认 1600×900）
/// * `PHIRC_WEBUI_FIT=fill` 完全关掉缩放（回到「铺满窗口、zoom=1」的老行为）
#[cfg(target_os = "windows")]
fn design_size() -> Option<(f64, f64)> {
    if std::env::var("PHIRC_WEBUI_FIT").is_ok_and(|v| v.trim().eq_ignore_ascii_case("fill")) {
        return None;
    }
    let raw = std::env::var("PHIRC_WEBUI_DESIGN").unwrap_or_else(|_| "1600x900".to_owned());
    let (w, h) = raw.split_once(['x', 'X', '*'])?;
    let (w, h) = (w.trim().parse::<f64>().ok()?, h.trim().parse::<f64>().ok()?);
    if w < 320.0 || h < 240.0 {
        tracing::warn!("[webui] PHIRC_WEBUI_DESIGN 太小（{raw}），忽略");
        return None;
    }
    Some((w, h))
}

/// 算出让页面视口 ≥ 设计尺寸的缩放系数。
#[cfg(target_os = "windows")]
fn fit_zoom(win_w: f64, win_h: f64, design: Option<(f64, f64)>) -> f64 {
    let Some((dw, dh)) = design else { return 1.0 };
    if win_w <= 0.0 || win_h <= 0.0 {
        return 1.0;
    }
    (win_w / dw).min(win_h / dh).clamp(0.2, 4.0)
}

// ============================================================
//  Web 根：`kaichang/` 目录
// ============================================================

/// 找 Web 根目录。顺序：
/// 1. `PHIRC_WEBUI_ROOT` 环境变量（调试用）；
/// 2. `D:\Phigros\kaichang`（开发目录优先 —— 改完 HTML 重启就能看到，不必重新发布；
///    iOS 上这个路径必然不存在，自然跳过）；
/// 3. `<exe 同目录>/kaichang`（随程序发布的副本）—— **iOS 上这正是 .app bundle 目录**
///    （`current_exe()` 指向 bundle 内的可执行文件），kaichang 由 Xcode 工程打进 bundle。
#[cfg(any(target_os = "windows", target_os = "ios"))]
fn web_root() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PHIRC_WEBUI_ROOT") {
        let p = PathBuf::from(p);
        if p.is_dir() {
            return Some(p);
        }
    }
    let dev = PathBuf::from(r"D:\Phigros\kaichang");
    if dev.is_dir() {
        return Some(dev);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("kaichang");
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    None
}

/// 读一个 Web 资源（防目录穿越）。`raw_path` 是 URI 路径，**可能带百分号编码**。
#[cfg(any(target_os = "windows", target_os = "ios"))]
fn serve_asset(raw_path: &str) -> Option<(Vec<u8>, &'static str)> {
    let root = web_root()?;
    let decoded = percent_decode(raw_path)?;
    let rel = decoded.trim_start_matches('/');

    // 防目录穿越：不允许 `..` / 反斜杠 / 盘符
    if rel.split('/').any(|seg| seg == ".." || seg.contains('\\') || seg.contains(':')) {
        return None;
    }

    let mut file = root.clone();
    for seg in rel.split('/').filter(|s| !s.is_empty() && *s != ".") {
        file.push(seg);
    }
    // 目录 → index.html
    if file.is_dir() || !file.exists() {
        let idx = file.join("index.html");
        if idx.is_file() {
            file = idx;
        } else {
            // 打包去重兜底：`kaichang/assets/<name>` 与根级同名文件在 APK 里只留
            // 根级一份（见 build_apk.ps1），而页面两种路径都引用 —— 这里让带
            // `assets/` 前缀的 miss 去掉前缀再试一次。
            if let Some(rest) = rel.strip_prefix("assets/") {
                let alt = root.join(rest);
                if alt.is_file() {
                    let data = std::fs::read(&alt).ok()?;
                    return Some((data, mime_of(&alt.to_string_lossy())));
                }
            }
            return None;
        }
    }
    if !file.is_file() {
        return None;
    }
    let data = std::fs::read(&file).ok()?;
    Some((data, mime_of(&file.to_string_lossy())))
}

/// 资源包贴图协议：`phirc://respack/<index>/<file>`。
///
/// * `index = 0` → 内置默认包（`<exe>/assets/respack/<file>`）；
/// * 其余 → `data/respack/<uuid>/<file>`（uuid 取 `data.respacks[index-1]`）。
///
/// 预览区据此拿 click.png / drag.png / flick.png / hold.png / hit_fx.png 等真实贴图。
#[cfg(any(target_os = "windows", target_os = "ios"))]
fn serve_respack(path: &str) -> Option<(Vec<u8>, &'static str)> {
    // 入参形如 `0/click.png`（host `respack` 已被 protocol_response 剥掉）
    let (idx_str, file) = path.split_once('/')?;
    let index: usize = idx_str.parse().ok()?;
    let dir = respack_dir(index)?;
    // 防目录穿越：不允许 `..` / 反斜杠
    if file.split('/').any(|seg| seg == ".." || seg.contains('\\')) {
        return None;
    }
    let mut p = dir;
    for seg in file.split('/').filter(|s| !s.is_empty()) {
        p.push(seg);
    }
    if !p.is_file() {
        return None;
    }
    let data = std::fs::read(&p).ok()?;
    Some((data, mime_of(&p.to_string_lossy())))
}

/// 极简百分号解码（中文文件名如 `默认.png` 必须走这一步）。
#[cfg(any(target_os = "windows", target_os = "ios"))]
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() {
                return None;
            }
            let hi = (b[i + 1] as char).to_digit(16)?;
            let lo = (b[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// rpe-next 的本地静态服务端口（失败返回 0，进程内只启动一次）。
///
/// ⚠️ 为什么不能直接用 `phirc://`：WebView2 里自定义协议是 **opaque origin**，
/// ES module 的同源检查会直接拦掉模块加载 —— 而 rpe-next 是纯 ES module 应用
/// （`<script type="module">` + 上百个 `.mjs`）。只有 http 来源能跑，所以这里
/// 起一个只监听 `127.0.0.1` 的极简静态服务，根目录就是 `kaichang/`。
#[cfg(any(target_os = "windows", target_os = "ios"))]
pub fn static_port() -> u16 {
    static PORT: OnceLock<u16> = OnceLock::new();
    *PORT.get_or_init(|| {
        // 端口固定优先（47653 起试几个）：rpe-next 的项目库 / 设置存在 IndexedDB 与
        // localStorage 里，而这两者按「协议+主机+端口」隔离 —— 每次换端口就等于丢库。
        let mut listener = None;
        for port in 47653u16..=47658 {
            if let Ok(l) = std::net::TcpListener::bind(("127.0.0.1", port)) {
                listener = Some(l);
                break;
            }
        }
        let listener = match listener.or_else(|| std::net::TcpListener::bind("127.0.0.1:0").ok()) {
            Some(l) => l,
            None => {
                crate::diag("[webui] rpe-next 静态服务启动失败：没有可用端口");
                return 0;
            }
        };
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let _ = serve_static_conn(stream);
                });
            }
        });
        crate::diag(&format!("[webui] rpe-next 静态服务：http://127.0.0.1:{port}/"));
        port
    })
}

/// 单连接：只支持 GET，按 `kaichang/` 根目录读文件（挡 `..` 越界）。
#[cfg(any(target_os = "windows", target_os = "ios"))]
fn serve_static_conn(mut stream: std::net::TcpStream) -> std::io::Result<()> {
    use std::io::Read;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let mut parts = req.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw = parts.next().unwrap_or("/");
    let mut status = 404u16;
    let mut body = Vec::new();
    let mut mime = "text/plain; charset=utf-8";
    if method == "GET" {
        let path = raw.split('?').next().unwrap_or("/");
        let decoded = percent_decode(path).unwrap_or_else(|| path.to_owned());
        let rel = decoded.trim_start_matches('/');
        if let Some(root) = web_root() {
            let full = root.join(rel);
            let inside = match (full.canonicalize(), root.canonicalize()) {
                (Ok(f), Ok(r)) => f.starts_with(&r),
                _ => false,
            };
            if inside && full.is_file() {
                match std::fs::read(&full) {
                    Ok(bytes) => {
                        status = 200;
                        body = bytes;
                        mime = mime_of(&full.to_string_lossy());
                    }
                    Err(_) => status = 500,
                }
            }
        }
    }
    let reason = match status {
        200 => "OK",
        500 => "Internal Server Error",
        _ => "Not Found",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&body)?;
    Ok(())
}

#[cfg(all(test, any(target_os = "windows", target_os = "ios")))]
mod static_server_tests {
    use super::*;
    use std::io::{Read, Write};

    fn http_get(port: u16, path: &str) -> String {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("连接静态服务");
        write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").expect("写请求");
        let mut out = Vec::new();
        stream.read_to_end(&mut out).expect("读响应");
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn static_server_serves_rpe_next() {
        let root = match web_root() {
            Some(r) => r,
            None => return,   // 没有 web 根（CI）就跳过
        };
        if !root.join("rpe-next/index.html").is_file() {
            return;
        }
        let port = static_port();
        assert!(port > 0, "静态服务端口");
        let ok = http_get(port, "/rpe-next/index.html");
        assert!(ok.starts_with("HTTP/1.1 200"), "首页状态：{}", ok.lines().next().unwrap_or(""));
        assert!(ok.contains("text/html"), "首页 MIME");
        assert!(ok.contains("Re:PhiEdit"), "首页内容");
        let js = http_get(port, "/rpe-next/src/ui/app.mjs");
        assert!(js.starts_with("HTTP/1.1 200"), "模块状态");
        assert!(js.contains("javascript"), "模块 MIME 必须是 javascript（否则 ES module 不加载）");
        let bad = http_get(port, "/../Cargo.toml");
        assert!(bad.starts_with("HTTP/1.1 404"), "越界必须 404：{}", bad.lines().next().unwrap_or(""));
    }
}

/// 按扩展名给 MIME。
#[cfg(any(target_os = "windows", target_os = "ios"))]
fn mime_of(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "txt" | "glsl" | "frag" | "vert" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// 自定义协议回包。
#[cfg(target_os = "windows")]
fn protocol_response(request: &wry::http::Request<Vec<u8>>) -> wry::http::Response<std::borrow::Cow<'static, [u8]>> {
    use std::borrow::Cow;
    use wry::http::{header, StatusCode};

    let uri = request.uri();
    // `phirc://web/...` 的 host 是 `web`、path 是 `/xxx`；`phirc://respack/<idx>/<file>` 的
    // host 是 `respack`、path 是 `/<idx>/<file>`。⚠️ **必须按 host 分流** —— 早先只判断
    // `raw_path.starts_with("respack/")`，而 host 被吃掉后 path 只剩 `/0/click.png`，
    // 于是资源包贴图永远 404（表现就是「预览一片空白」）。
    let raw_path = uri.path().trim_start_matches('/');
    let host = uri.host().unwrap_or("");
    let asset = if raw_path.starts_with("respack/") {
        // `phirc://web/respack/<index>/<file>` → 资源包贴图（预览用）。
        // ⚠️ 必须走 **和页面同源的 host（`web`）**：`phirc://respack/...` 是另一个
        // origin，WebView2 会把跨源的子资源请求直接挡掉 —— 表现就是「图片一张都加载
        // 不出来、协议层连请求记录都没有」（只有 JS 侧 onerror）。
        serve_respack(raw_path.strip_prefix("respack/").unwrap_or(raw_path))
    } else if host == "respack" {
        // 兼容 `phirc://respack/<index>/<file>` 这种 host 形式（若将来被放行）
        serve_respack(raw_path)
    } else if raw_path == "local-cover" {
        local_cover_query(uri.query()).map(|data| (data, "image/jpeg"))
    } else {
        serve_asset(uri.path())
    };

    // 诊断：把每个 phirc:// 请求记进 phira-crash.log —— 页面白屏时，
    // 「有没有请求 index.html / 返回 200 还是 404」是最直接的证据。
    crate::diag(&format!(
        "[webui] 协议请求 {} → {}",
        uri.path(),
        if asset.is_some() { "200" } else { "404" }
    ));

    match asset {
        Some((data, mime)) => wry::http::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            // 开发期不缓存：改完 HTML 重启即可生效
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Cow::Owned(data))
            .unwrap(),
        None => wry::http::Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Cow::Borrowed(b"not found".as_slice()))
            .unwrap(),
    }
}

// ============================================================
//  IPC（页面 → Rust）
// ============================================================

/// 处理页面发来的 IPC。协议：`{ id, cmd, args }`；`id` 缺省表示不需要回包。
fn handle_ipc(body: &str, tx: &mpsc::Sender<WebCmd>) {
    use serde_json::{json, Value};

    // 页面能发 IPC 就说明它的 JS 在跑（`ERROR_JS` 在 DOMContentLoaded 时必发一条）。
    PAGE_ALIVE.store(true, Ordering::Relaxed);

    let Ok(req) = serde_json::from_str::<Value>(body) else {
        tracing::warn!("[webui] IPC 不是合法 JSON：{body}");
        return;
    };
    let cmd = req.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
    let id = req.get("id").cloned();
    let reply = |payload: Value| {
        if let Some(id) = &id {
            let _ = tx.send(WebCmd::Eval(format!("window.__phircResolve({id}, {payload})")));
        }
    };

    match cmd {
        // 页面说「我完事了」（左上角返回 / Esc）。原生 Home 页已「删掉」，
        // 所以不再露出原生 UI，而是回开始页；WebView 不可用才真的隐藏、让位给原生兜底。
        "hide" => {
            if host_available() {
                show_start_page();
            } else {
                WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
                let _ = tx.send(WebCmd::Visible(false));
            }
            reply(json!({ "ok": true }));
        }
        // 曲库列表（含封面 URL）
        // 曲库列表：曲库可能很大 → 丢线程，别占 WebView 消息泵
        "list_charts" => {
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                reply_async(&tx2, &pid, json!({ "ok": true, "data": chart_list() }));
            });
        }
        // 在线曲库一页（上架 / 特殊 / 未上架 / 热门）——查询口径与原生曲库页
        // `page/library.rs` 的 `load_online` **逐字对应**（type / popular / 分页 / 排序），
        // 只是结果换成 JSON。走网络 → 单独线程，别堵 WebView。
        "list_online_charts" => {
            let args = req.get("args").cloned().unwrap_or_else(|| json!({}));
            let ty = args.get("type").and_then(|v| v.as_str()).unwrap_or("ranked").to_owned();
            let page = args.get("page").and_then(|v| v.as_u64()).unwrap_or(0);
            let search = args.get("search").and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let order = args.get("order").and_then(|v| v.as_str()).unwrap_or("updated").to_owned();
            if crate::get_data().config.offline_mode {
                reply(json!({ "ok": false, "error": "offline-mode" }));
            } else if crate::get_data().me.is_none() {
                reply(json!({ "ok": false, "error": "must-login" }));
            } else if !tos_accepted() {
                // ⚠️ 不能在这里直接跑 check_read_tos_and_policy（会弹窗 + 写盘，卡死消息泵）
                reply_need_tos(&reply);
            } else {
                let tx2 = tx.clone();
                let pid = id.clone();
                std::thread::spawn(move || {
                    let payload = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(rt) => rt.block_on(online_charts_page(&ty, page, &search, &order)),
                        Err(e) => json!({ "ok": false, "error": format!("无法创建运行时：{e}") }),
                    };
                    reply_async(&tx2, &pid, payload);
                });
            }
        }
        // 自定义 RKS 汇总（本地谱面 + 本地成绩算出来的那个，曲库页右上角显示）。
        // 口径同 `page/rks.rs` 的 `refresh` + `custom_rks::calculate`。
        // 自定义 RKS 汇总：要读 custom-rks.json + 遍历曲库 → 丢线程，别占 WebView 消息泵
        "get_custom_rks" => {
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                reply_async(&tx2, &pid, json!({ "ok": true, "data": custom_rks_summary() }));
            });
        }
        // 在线谱面开局：按 id 从服务器取谱面信息，暂存后让位给原生。
        // 下载发生在 SongScene 里（用户按 play → `global_start_download`），
        // 与原生「消息页点谱面」同一条路，不重复实现下载。
        "start_online_chart" => {
            let id = req
                .get("args")
                .and_then(|a| a.get("id"))
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .unwrap_or(0);
            if id == 0 {
                reply(json!({ "ok": false, "error": "缺少 id" }));
            } else if crate::get_data().config.offline_mode {
                reply(json!({ "ok": false, "error": "offline-mode" }));
            } else if crate::get_data().me.is_none() {
                reply(json!({ "ok": false, "error": "must-login" }));
            } else if !tos_accepted() {
                reply_need_tos(&reply);
            } else {
                let tx2 = tx.clone();
                let pid = req.get("id").cloned();
                std::thread::spawn(move || {
                    let payload = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(rt) => match rt.block_on(fetch_online_chart(id)) {
                            Ok(chart) => {
                                *PENDING_ONLINE_CHART.lock().unwrap() = Some(chart);
                                // 让位给原生：SongScene 先展示谱面信息，按 play 才下载
                                WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
                                WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
                                let _ = tx2.send(WebCmd::Visible(false));
                                json!({ "ok": true })
                            }
                            Err(e) => json!({ "ok": false, "error": e.to_string() }),
                        },
                        Err(e) => json!({ "ok": false, "error": format!("无法创建运行时：{e}") }),
                    };
                    reply_async(&tx2, &pid, payload);
                });
            }
        }
        // 课题模式：WebView 曲库选好 3 张后直接开局。
        // 只暂存路径 —— `ChartItem` 带曲绘纹理，必须在游戏线程用 `load_local()` 构造。
        // ——— 全局 Mod（Mod.html）———
        "get_mods" => reply(mods_state_json(crate::get_data().config.mods)),
        "set_mod" => {
            use serde_json::json;
            let id = req.get("args").and_then(|a| a.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let on = req.get("args").and_then(|a| a.get("on")).and_then(|v| v.as_bool()).unwrap_or(false);
            match mod_flag(&id) {
                Some(flag) => {
                    let mut mods = crate::get_data().config.mods;
                    if on {
                        if !mods.contains(flag) {
                            mods.toggle_mod(flag); // 内部会清掉互斥项
                        }
                    } else {
                        mods.remove(flag);
                    }
                    // 走设置页同一条通道：PENDING_CONFIG → 游戏线程合并落盘
                    let mut patch = json!({ "mods": serde_json::to_value(mods).unwrap_or(json!("")) });
                    // AUTOPLAY 还有个兼容字段 `autoplay`：不一起写，重开游戏会被 init() 按回去
                    if id == "autoplay" {
                        patch["autoplay"] = json!(on);
                    }
                    let mut p = PENDING_CONFIG.lock().unwrap();
                    match p.as_mut() {
                        Some(existing) => merge_json(existing, &patch),
                        None => *p = Some(patch),
                    }
                    drop(p);
                    reply(mods_state_json(mods))
                }
                None => reply(json!({ "ok": false, "error": format!("未知 Mod：{id}") })),
            }
        }
        // 玩家成长树（Growth.html）
        "get_growth" => reply(crate::growth::growth_data()),
        // 段位考核：3 张本地谱 + 段位名 → DanScene
        "start_dan" => {
            let paths: Vec<String> = req
                .get("args")
                .and_then(|a| a.get("paths"))
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_owned())).collect())
                .unwrap_or_default();
            let name = req.get("args").and_then(|a| a.get("name")).and_then(|v| v.as_str()).unwrap_or("段位").to_owned();
            if paths.len() != 3 {
                reply(json!({ "ok": false, "error": "段位考核需要正好 3 张谱面" }));
            } else {
                *PENDING_DAN.lock().unwrap() = Some((paths, name));
                WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
                WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
                let _ = tx.send(WebCmd::Visible(false));
                reply(json!({ "ok": true }));
            }
        }
        "start_challenge" => {
            let paths: Vec<String> = req
                .get("args")
                .and_then(|a| a.get("paths"))
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_owned())).collect())
                .unwrap_or_default();
            if paths.len() != 3 {
                reply(json!({ "ok": false, "error": "课题模式需要正好 3 张谱面" }));
            } else {
                *PENDING_CHALLENGE.lock().unwrap() = Some(paths);
                // 让位给原生：课题场景是 macroquad 绘制的
                WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
                WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
                let _ = tx.send(WebCmd::Visible(false));
                reply(json!({ "ok": true }));
            }
        }
        // ——— 离线渲染（曲库页「渲染」按钮）———
        // 只暂存请求 + 隐藏 WebView，真正的渲染由游戏线程的主循环接管
        // （`record::take_pending_render`），因为它要独占 GL 上下文。
        "render_chart" => {
            let a = req.get("args");
            let get_u32 = |k: &str, d: u32| a.and_then(|v| v.get(k)).and_then(|v| v.as_u64()).map(|v| v as u32).unwrap_or(d);
            let get_str = |k: &str| a.and_then(|v| v.get(k)).and_then(|v| v.as_str()).map(|s| s.to_owned());
            let Some(path) = get_str("path").filter(|p| !p.is_empty()) else {
                reply(json!({ "ok": false, "error": "缺少谱面路径" }));
                return;
            };
            // 曲库传的是相对 data/charts 的 local_path —— 渲染前同样要归一化
            let path = crate::editor::resolve_dir(&path);
            let opts = crate::record::RenderOpts {
                chart: path.clone(),
                out: get_str("out").unwrap_or_default(),
                width: get_u32("width", 1920),
                height: get_u32("height", 1080),
                fps: get_u32("fps", 60),
                msaa: get_u32("msaa", 4).max(1),
                crf: get_u32("crf", 18),
                bitrate_kbps: get_u32("bitrate", 0),
                // GL 回读自下而上：必须翻转（和 Phi Recorder 的 -vf vflip 等价，
                // 现在翻转发生在 PboRing 里，桌面/移动端统一）
                vflip: true,
                camera: get_str("camera").filter(|c| !c.is_empty() && c != "none"),
            };
            if opts.out.is_empty() {
                reply(json!({ "ok": false, "error": "缺少输出文件名" }));
                return;
            }
            tracing::info!("[render] 曲库页请求渲染：{} → {}", opts.chart, opts.out);
            crate::record::request_render(opts);
            // 让位给原生窗口（渲染时 WebView 必须让开，否则进度条被盖住）
            WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
            WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
            let _ = tx.send(WebCmd::Visible(false));
            reply(json!({ "ok": true }));
        }
        // ——— 创建谱面（选素材文件）———
        // 文件框是同步的，必须 spawn：别堵 WebView 消息泵
        // 创建谱面：按类型单选素材（文件框同步 → spawn）
        "pick_asset_file" => {
            let kind = req.get("args").and_then(|a| a.get("kind")).and_then(|v| v.as_str()).unwrap_or("music").to_owned();
            tracing::info!("[editor] pick_asset_file({kind})：正在打开系统文件框…");
            let tx2 = tx.clone();
            let pid = id.clone();
            std::thread::spawn(move || {
                #[cfg(target_os = "windows")]
                let picked = pick_asset_with_parent(&kind);
                #[cfg(not(target_os = "windows"))]
                let picked = crate::editor::pick_asset_file(&kind);
                tracing::info!("[editor] pick_asset_file({kind}) 结果：{}", picked.as_deref().unwrap_or("取消/无"));
                match picked {
                    Some(p) => reply_async(&tx2, &pid, json!({ "ok": true, "path": p })),
                    None => reply_async(&tx2, &pid, json!({ "ok": false, "error": "cancelled" })),
                }
            });
        }
        "create_chart" => {
            let a = req.get("args");
            let music = a.and_then(|a| a.get("music")).and_then(|v| v.as_str()).map(|s| s.to_owned());
            let illu = a.and_then(|a| a.get("illustration")).and_then(|v| v.as_str()).map(|s| s.to_owned());
            let name = a.and_then(|a| a.get("name")).and_then(|v| v.as_str()).map(|s| s.to_owned());
            match crate::editor::create_chart(music.as_deref(), illu.as_deref(), name.as_deref()) {
                Ok(dir) => reply(json!({ "ok": true, "path": dir })),
                Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
            }
        }
        // ——— 分享码 / 渲染队列 ———
        // 谱面对比 / 合并（制谱器）
        "diff_charts" => {
            let a = req.get("args").and_then(|a| a.get("a")).cloned().unwrap_or(json!({}));
            let b = req.get("args").and_then(|a| a.get("b")).cloned().unwrap_or(json!({}));
            reply(crate::editor::diff_charts(&a, &b));
        }
        "apply_merge" => {
            let base = req.get("args").and_then(|a| a.get("base")).cloned().unwrap_or(json!({}));
            let edits = req.get("args").and_then(|a| a.get("edits")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
            match crate::editor::apply_merge(&base, &edits) {
                Ok(v) => reply(json!({ "ok": true, "chart": v })),
                Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
            }
        }
        "share_code" => {
            let dir = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            match crate::editor::share_code(&dir) {
                Ok(v) => reply(v),
                Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
            }
        }
        "import_share_code" => {
            let code = req.get("args").and_then(|a| a.get("code")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            match crate::editor::import_share_code(&code) {
                Ok(dir) => reply(json!({ "ok": true, "path": dir })),
                Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
            }
        }
        // 渲染队列：入队（label 给 UI 显示）
        "queue_render" => {
            let a = req.get("args");
            let get_u32 = |k: &str, d: u32| a.and_then(|v| v.get(k)).and_then(|v| v.as_u64()).map(|v| v as u32).unwrap_or(d);
            let get_str = |k: &str| a.and_then(|v| v.get(k)).and_then(|v| v.as_str()).map(|s| s.to_owned());
            let Some(path) = get_str("path").filter(|p| !p.is_empty()) else {
                reply(json!({ "ok": false, "error": "缺少谱面路径" }));
                return;
            };
            let opts = crate::record::RenderOpts {
                chart: path.clone(),
                out: get_str("out").unwrap_or_default(),
                width: get_u32("width", 1920),
                height: get_u32("height", 1080),
                fps: get_u32("fps", 60),
                msaa: get_u32("msaa", 4).max(1),
                crf: get_u32("crf", 18),
                bitrate_kbps: get_u32("bitrate", 0),
                // GL 回读自下而上：必须翻转（和 Phi Recorder 的 -vf vflip 等价，
                // 现在翻转发生在 PboRing 里，桌面/移动端统一）
                vflip: true,
                camera: get_str("camera").filter(|c| !c.is_empty() && c != "none"),
            };
            let label = get_str("label").unwrap_or_else(|| opts.out.clone());
            tracing::info!("[render] 入队：{label}");
            let len = crate::record::enqueue_render(opts, label);
            // 队列要跑起来就得让位给原生窗口（和 render_chart 一样）
            WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
            WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
            let _ = tx.send(WebCmd::Visible(false));
            reply(json!({ "ok": true, "queueLen": len }));
        }
        "queue_status" => reply(crate::record::queue_snapshot()),
        // ——— 谱面管理（制谱器）———
        // ⚠️ 命令名不能叫 list_charts——曲库页已有同名命令（返回格式不同）会遮蔽这里
        "list_editor_charts" => {
            match crate::editor::list_charts() {
                Ok(v) => reply(v),
                Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
            }
        }
        "delete_chart" => {
            let path = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            match crate::editor::delete_chart(&path) {
                Ok(()) => reply(json!({ "ok": true })),
                Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
            }
        }
        // ——— 制谱器（ChartEditor.html）———
        // 打开照搬的 rpe-next 编辑器（本地 http 静态服务，见 static_port 的注释）
        "open_rpen" => {
            let dir = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            #[cfg(any(target_os = "windows", target_os = "ios"))]
            {
                let port = static_port();
                if port == 0 {
                    reply(json!({ "ok": false, "error": "静态服务启动失败（见 phira-crash.log）" }));
                    return;
                }
                let q = urlencode(&dir);
                reply(json!({ "ok": true, "port": port }));
                let _ = tx.send(WebCmd::Navigate(format!(
                    "http://127.0.0.1:{port}/rpe-next/index.html?phirc_path={q}"
                )));
            }
            #[cfg(not(any(target_os = "windows", target_os = "ios")))]
            {
                let _ = dir;
                reply(json!({ "ok": false, "error": "当前平台暂不支持 RPEN 页面" }));
            }
        }
        // 从 rpe-next 返回制谱器（跨协议跳转只能由 Rust 侧 Navigate 发起）
        "back_to_editor" => {
            let dir = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            reply(json!({ "ok": true }));
            let _ = tx.send(WebCmd::Navigate(format!("phirc://web/ChartEditor.html?path={}", urlencode(&dir))));
        }
        // rpe-next 导出 → 原地写回谱面文件（name 只允许纯文件名）
        "write_chart_file" => {
            let dir = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let name = req.get("args").and_then(|a| a.get("name")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let text = req.get("args").and_then(|a| a.get("text")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            if dir.is_empty() || name.is_empty() {
                reply(json!({ "ok": false, "error": "缺少目录或文件名" }));
                return;
            }
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match crate::editor::write_chart_file(&dir, &name, &text) {
                    Ok(v) => v,
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 把谱面 + 音乐 + 曲绘打成 base64 给 rpe-next 的 <input type=file> 导入
        "editor_bundle" => {
            let dir = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            if dir.is_empty() {
                reply(json!({ "ok": false, "error": "缺少谱面目录" }));
                return;
            }
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match crate::editor::bundle(&dir) {
                    Ok(v) => v,
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        "load_chart" => {
            let dir = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            if dir.is_empty() {
                reply(json!({ "ok": false, "error": "缺少谱面目录" }));
                return;
            }
            match crate::editor::load_chart(&dir) {
                Ok(v) => reply(v),
                Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
            }
        }
        // 音乐波形（时间轴）：解码较慢，放后台线程，别堵 WebView
        "editor_waveform" => {
            let dir = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let buckets = req.get("args").and_then(|a| a.get("buckets")).and_then(|v| v.as_u64()).unwrap_or(1600) as usize;
            if dir.is_empty() {
                reply(json!({ "ok": false, "error": "缺少谱面目录" }));
                return;
            }
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match crate::editor::waveform(&dir, buckets) {
                    Ok(v) => v,
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        "save_chart" => {
            let dir = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let chart = req.get("args").and_then(|a| a.get("chart")).cloned().unwrap_or(json!({}));
            if dir.is_empty() {
                reply(json!({ "ok": false, "error": "缺少谱面目录" }));
                return;
            }
            // format 决定写回格式：pec = RPE 文本原地写回；其它 = 官方 JSON
            let format = req.get("args").and_then(|a| a.get("format")).and_then(|v| v.as_str()).unwrap_or("pgr").to_owned();
            match crate::editor::save_chart(&dir, &chart, &format) {
                Ok(f) => reply(json!({ "ok": true, "file": f })),
                Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
            }
        }
        "save_info" => {
            let dir = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let patch = req.get("args").and_then(|a| a.get("info")).cloned().unwrap_or(json!({}));
            match crate::editor::save_info(&dir, &patch) {
                Ok(()) => reply(json!({ "ok": true })),
                Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
            }
        }
        "new_chart" => {
            let name = req.get("args").and_then(|a| a.get("name")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            match crate::editor::new_chart(&name) {
                Ok(dir) => reply(json!({ "ok": true, "path": dir })),
                Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
            }
        }
        // ——— .phc 插件 ———（全部 spawn 线程：文件框同步、要读写磁盘，别占消息泵）
        "import_plugin" => {
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                // 桌面端 `request_file` 是同步 rfd 框（与 pick_file 同一套）
                prpr::scene::request_file("phc_plugin");
                let payload = match prpr::scene::take_file() {
                    Some((got, file)) if got == "phc_plugin" => match import_plugin_from_file(std::path::Path::new(&file)) {
                        Ok(v) => json!({ "ok": true, "data": v }),
                        Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                    },
                    Some(_) => json!({ "ok": false, "error": "返回的文件 id 不匹配" }),
                    None => json!({ "ok": false, "error": "canceled" }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 启动旁挂进程插件（runtime = python / rust）
        "start_plugin" => {
            let pid_name = req.get("args").and_then(|a| a.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match start_sidecar(&pid_name) {
                    Ok(v) => json!({ "ok": true, "data": v }),
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 停掉旁挂进程插件
        "stop_plugin" => {
            let pid_name = req.get("args").and_then(|a| a.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                stop_sidecar(&pid_name);
                reply_async(&tx2, &pid, json!({ "ok": true }));
            });
        }
        // 给旁挂插件推一个事件（页面 → 插件 stdin）
        "plugin_event" => {
            let pid_name = req.get("args").and_then(|a| a.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let event = req.get("args").and_then(|a| a.get("event")).cloned().unwrap_or_else(|| json!({}));
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match send_sidecar_event(&pid_name, &event) {
                    Ok(()) => json!({ "ok": true }),
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 插件自己的持久化存储（旁挂插件的 storageSet 命令落到这里）
        "plugin_storage_set" => {
            let pid_name = req.get("args").and_then(|a| a.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let key = req.get("args").and_then(|a| a.get("key")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let value = req.get("args").and_then(|a| a.get("value")).cloned().unwrap_or(serde_json::Value::Null);
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match set_plugin_storage(&pid_name, &key, value) {
                    Ok(()) => json!({ "ok": true }),
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        "list_plugins" => {
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                reply_async(&tx2, &pid, json!({ "ok": true, "data": plugin_list() }));
            });
        }
        "read_plugin_script" => {
            let pid_name = req.get("args").and_then(|a| a.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match plugin_script(&pid_name) {
                    Ok(t) => json!({ "ok": true, "data": t }),
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        "set_plugin_enabled" => {
            let pid_name = req.get("args").and_then(|a| a.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let on = req.get("args").and_then(|a| a.get("enabled")).and_then(|v| v.as_bool()).unwrap_or(true);
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match set_plugin_enabled(&pid_name, on) {
                    Ok(()) => json!({ "ok": true }),
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        "delete_plugin" => {
            let pid_name = req.get("args").and_then(|a| a.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match delete_plugin(&pid_name) {
                    Ok(()) => json!({ "ok": true }),
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // ——— 谱面管理面板 ———（全部 spawn 线程：读 info.yml / 弹文件框 / 写盘）
        // 一次拿齐：info（info.yml 全字段）+ mods 位 + 血条等全局配置 + 只读信息
        "chart_panel" => {
            let path = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match chart_panel_payload(&path) {
                    Ok(v) => json!({ "ok": true, "data": v }),
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 面板改 Mods（位标志，口径与 prpr::config::Mods 一致）
        "chart_panel_mods" => {
            let path = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let bits = req.get("args").and_then(|a| a.get("bits")).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match set_chart_mods(&path, bits) {
                    Ok(()) => json!({ "ok": true }),
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 面板保存「编辑谱面」
        "chart_panel_save" => {
            let path = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let patch = req.get("args").and_then(|a| a.get("patch")).cloned().unwrap_or_else(|| json!({}));
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match chart_panel_save(&path, &patch) {
                    Ok(()) => json!({ "ok": true }),
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 面板替换谱面文件（弹系统文件框）
        "chart_panel_pick" => {
            let path = req.get("args").and_then(|a| a.get("path")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let kind = req.get("args").and_then(|a| a.get("kind")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                prpr::scene::request_file("chart_panel_pick");
                let payload = match prpr::scene::take_file() {
                    Some((got, file)) if got == "chart_panel_pick" => match copy_chart_file(&path, &kind, &file) {
                        Ok(name) => json!({ "ok": true, "data": name }),
                        Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                    },
                    Some(_) => json!({ "ok": false, "error": "返回的文件 id 不匹配" }),
                    None => json!({ "ok": false, "error": "canceled" }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 「在网站中打开」
        "open_chart_web" => {
            let id = req.get("args").and_then(|a| a.get("id")).and_then(|v| v.as_i64()).unwrap_or(0);
            let _ = prpr::ext::open_url(&format!("https://phira.moe/chart/{id}"));
            reply(json!({ "ok": true }));
        }
    // ——— 回放系统 ———（全部 spawn 线程：要读目录 / 弹文件框 / 拷文件，别占消息泵）
        // 列出全部回放 → [{ file, name, level, difficulty, player, imported,
        //                  chart:{id|local}, duration, speed, offset,
        //                  result:{score,accuracy,maxCombo,numOfNotes,counts}|null, recordedAt }]
        "list_replays" => {
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let data = crate::replay::library::all()
                    .map(|items| {
                        json!(items
                            .iter()
                            .map(|it| {
                                json!({
                                    "file": it.file_name(),
                                    "name": it.meta.name,
                                    "level": it.meta.level,
                                    "difficulty": it.meta.difficulty,
                                    "player": it.player(),
                                    "imported": it.imported,
                                    "chart": match &it.chart {
                                        Some(crate::replay::ChartRef::Id(id)) => json!({ "id": id }),
                                        Some(crate::replay::ChartRef::Local(p)) => json!({ "local": p }),
                                        None => serde_json::Value::Null,
                                    },
                                    "duration": it.duration,
                                    "durationText": it.duration_text(),
                                    "playable": it.playable(),
                                    "localPath": it.binding.as_ref().and_then(|b| b.local_path.clone()),
                                    "speed": it.speed,
                                    "offset": it.offset,
                                    "result": it.result.as_ref().map(|r| json!({
                                        "score": r.score,
                                        "accuracy": r.accuracy,
                                        "maxCombo": r.max_combo,
                                        "numOfNotes": r.num_of_notes,
                                        "counts": r.counts,
                                    })),
                                    "recordedAt": it.meta.recorded_at,
                                })
                            })
                            .collect::<Vec<_>>())
                    })
                    .unwrap_or_default();
                reply_async(&tx2, &pid, json!({ "ok": true, "data": data }));
            });
        }
        // 导入本地谱面（弹出系统文件框）—— 曲库页「导入」用：直接导入谱面，
        // 不再打开「谱面工具箱」（用户要求导入就是导入）。
        "import_chart" => {
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                prpr::scene::request_file("_import_chart");
                let payload = match prpr::scene::take_file() {
                    Some((got, file)) if got == "_import_chart" => match std::fs::File::open(&file) {
                        Err(err) => json!({ "ok": false, "error": format!("打不开文件：{err:#}") }),
                        Ok(f) => match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                            Err(err) => json!({ "ok": false, "error": format!("{err:#}") }),
                            Ok(rt) => match rt.block_on(crate::scene::import_chart(f)) {
                                Err(err) => json!({ "ok": false, "error": format!("导入失败：{err:#}") }),
                                Ok((chart, warnings)) => {
                                    let name = chart.info.name.clone();
                                    let warning = crate::scene::parse_warnings_to_string(&warnings);
                                    crate::get_data_mut().charts.push(chart);
                                    if let Err(err) = crate::save_data() {
                                        json!({ "ok": false, "error": format!("已导入但保存失败：{err:#}") })
                                    } else {
                                        // 原生曲库页也要刷新
                                        crate::charts_view::NEED_UPDATE.store(true, Ordering::Relaxed);
                                        json!({ "ok": true, "data": { "name": name, "warning": warning } })
                                    }
                                }
                            }
                        }
                    }
                    Some(_) => json!({ "ok": false, "error": "返回的文件 id 不匹配" }),
                    None => json!({ "ok": false, "error": "canceled" }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 导入回放文件（弹出系统文件框）
        "import_replay" => {
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                prpr::scene::request_file("phc_replay");
                let payload = match prpr::scene::take_file() {
                    Some((got, file)) if got == "phc_replay" => {
                        match crate::replay::library::import(std::path::Path::new(&file), None) {
                            Ok(_) => json!({ "ok": true }),
                            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                        }
                    }
                    Some(_) => json!({ "ok": false, "error": "返回的文件 id 不匹配" }),
                    None => json!({ "ok": false, "error": "canceled" }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 导出回放（弹出系统文件框）
        "export_replay" => {
            let file_name = req.get("args").and_then(|a| a.get("file")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match crate::replay::library::all() {
                    Ok(items) => match items.into_iter().find(|it| it.file_name() == file_name) {
                        Some(item) => {
                            // 复用 pick_file 的保存框：request_file 只支持打开，
                            // 这里直接用 rfd 的保存对话框（桌面端同步，和 pick_file 同套路）。
                            // 安卓没有 rfd（桌面专属依赖），走系统保存框要接
                            // MainActivity 的 EXPORT_FILE（Storage Access Framework），先返回不支持。
                            #[cfg(not(target_os = "android"))]
                            let picked = rfd::FileDialog::new()
                                .add_filter("回放文件", &["ppr"])
                                .set_file_name(&file_name)
                                .save_file();
                            #[cfg(target_os = "android")]
                            let picked: Option<std::path::PathBuf> = {
                                let _ = &file_name;
                                None
                            };
                            match picked {
                                Some(dest) => match crate::replay::library::export(&item, std::path::Path::new(&dest)) {
                                    Ok(()) => json!({ "ok": true }),
                                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                                },
                                None => json!({ "ok": false, "error": "canceled" }),
                            }
                        }
                        None => json!({ "ok": false, "error": "找不到该回放" }),
                    },
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 删除回放
        "delete_replay" => {
            let file_name = req.get("args").and_then(|a| a.get("file")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match crate::replay::library::all() {
                    Ok(items) => match items.into_iter().find(|it| it.file_name() == file_name) {
                        Some(item) => match crate::replay::library::delete(&item) {
                            Ok(()) => json!({ "ok": true }),
                            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                        },
                        None => json!({ "ok": false, "error": "找不到该回放" }),
                    },
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 给回放关联本地谱面（导入的回放缺谱面时用；localPath 来自 listCharts）
        "associate_replay" => {
            let file_name = req.get("args").and_then(|a| a.get("file")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let local_path = req.get("args").and_then(|a| a.get("localPath")).and_then(|v| v.as_str()).map(|s| s.to_owned());
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match crate::replay::library::all() {
                    Ok(items) => match items.into_iter().find(|it| it.file_name() == file_name) {
                        Some(item) => match crate::replay::library::associate(&item, local_path) {
                            Ok(()) => json!({ "ok": true }),
                            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                        },
                        None => json!({ "ok": false, "error": "找不到该回放" }),
                    },
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 播放回放：读磁带 → 暂存 → 让位给游戏线程开局（与在线谱面同一套路）
        "watch_replay" => {
            let file_name = req.get("args").and_then(|a| a.get("file")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let tx2 = tx.clone();
            let pid = req.get("id").cloned();
            std::thread::spawn(move || {
                let payload = match crate::replay::library::load_by_file_name(&file_name) {
                    Ok(replay) => {
                        crate::scene::replay::queue(replay);
                        // 和 start_game 一样：隐藏 WebView 让位给游戏，并记住「从哪来的」，
                        // 回放播完（或中途退出）后回到点播放的那个页面。
                        WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
                        WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
                        let _ = tx2.send(WebCmd::Visible(false));
                        json!({ "ok": true })
                    }
                    Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 打开原生曲库页里的浮层（课题面板 / 自定义 RKS 编辑器 / 最佳谱面榜）。
        // 曲库页本体是 WebView，这三个是原生浮层 → 让位给原生，并把要开的浮层记下来，
        // `LibraryPage::enter` 据此自动展开（用户从哪进来就能原路退回 WebView）。
        "open_overlay" => {
            let name = req.get("args").and_then(|a| a.get("name")).and_then(|v| v.as_str()).unwrap_or("");
            let kind = match name {
                "challenge" => Some(crate::page::library::OverlayKind::Challenge),
                "board" => Some(crate::page::library::OverlayKind::Board),
                _ => None,
            };
            match kind {
                Some(kind) => {
                    *crate::page::library::PENDING_OVERLAY.lock().unwrap() = Some(kind);
                    *PENDING_PAGE.lock().unwrap() = Some(crate::hud::PageId::Library);
                    WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
                    WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
                    let _ = tx.send(WebCmd::Visible(false));
                    reply(json!({ "ok": true }));
                }
                None => reply(json!({ "ok": false, "error": format!("未知浮层：{name}") })),
            }
        }
        // 开一局：**只暂存路径**，场景由游戏线程构造（贴图必须在那边建）
        "start_game" => {
            let path = req
                .get("args")
                .and_then(|a| a.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if path.is_empty() {
                reply(json!({ "ok": false, "error": "缺少 path" }));
            } else {
                *PENDING_CHART.lock().unwrap() = Some(path.to_owned());
                // 也置 handoff：打完这一局回到 MainScene 时，应该回到刚点 PLAY 的那个页面。
                WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
                WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
                let _ = tx.send(WebCmd::Visible(false));
                reply(json!({ "ok": true }));
            }
        }
        // 页面往 Rust 日志写一行（调试用）
        "log" => {
            let msg = req.get("args").and_then(|a| a.get("msg")).and_then(|v| v.as_str()).unwrap_or("");
            tracing::info!("[webui/js] {msg}");
            // 桌面没有 logcat，页面的报错必须落盘才能排查
            crate::diag(&format!("[webui/js] {msg}"));
            reply(json!({ "ok": true }));
        }
        // 打开原生页面（设置 / 收藏 / 曲库）：只暂存页面，由游戏线程切页
        "open_page" => {
            let name = req
                .get("args")
                .and_then(|a| a.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let page = match name {
                "settings" => crate::hud::PageId::Settings,
                "favorites" => crate::hud::PageId::Favorites,
                "library" => crate::hud::PageId::Library,
                "home" => crate::hud::PageId::Home,
                // 收藏夹左上角「工具选择」用得到的三项（HUD 里都是整页）
                "message" => crate::hud::PageId::Message,
                "history" => crate::hud::PageId::History,
                "respack" => crate::hud::PageId::Respack,
                "blacklist" => crate::hud::PageId::Blacklist,
                other => {
                    reply(json!({ "ok": false, "error": format!("未知页面：{other}") }));
                    return;
                }
            };
            *PENDING_PAGE.lock().unwrap() = Some(page);
            // 让位给 macroquad 原生 UI（页面是原生绘制的）；记下「是 WebView 推过来的」，
            // 原生返回时据此回到 WebView。
            WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
            WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
            let _ = tx.send(WebCmd::Visible(false));
            reply(json!({ "ok": true }));
        }
        // 打开「工具覆盖页」（谱面工具箱 / 活动）：同样只暂存，由游戏线程建页面
        "open_tool" => {
            let name = req
                .get("args")
                .and_then(|a| a.get("tool"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let tool = match name {
                "toolbox" => WebTool::Toolbox,
                "event" => WebTool::Event,
                "timing" => WebTool::Timing,
                other => {
                    reply(json!({ "ok": false, "error": format!("未知工具：{other}") }));
                    return;
                }
            };
            *PENDING_TOOL.lock().unwrap() = Some(tool);
            WEBVIEW_HANDOFF.store(true, Ordering::Relaxed);
            WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
            let _ = tx.send(WebCmd::Visible(false));
            reply(json!({ "ok": true }));
        }
        // 打开外部链接（QQ 频道等）：只暂存 URL，由游戏线程用系统浏览器打开
        "open_url" => {
            let url = req
                .get("args")
                .and_then(|a| a.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            if url.is_empty() {
                reply(json!({ "ok": false, "error": "缺少 url" }));
            } else {
                *PENDING_URL.lock().unwrap() = Some(url);
                reply(json!({ "ok": true }));
            }
        }
        // 读取真实游戏配置（Setting.html 初始化控件用）
        "get_config" => {
            let data = crate::get_data();
            let mut cfg = serde_json::to_value(&data.config).unwrap_or(serde_json::Value::Null);
            // 少数设置项存在 `Data` 上而不是 `Config` 里（语言 / Anys / 不安全模式），
            // 一起塞进 `__data` 子对象带回去 —— 页面用 `__data.xxx` 读、用 `set_data` 写。
            if let Some(obj) = cfg.as_object_mut() {
                obj.insert(
                    "__data".to_owned(),
                    json!({
                        "language": data.language.clone().unwrap_or_default(),
                        "enableAnys": data.enable_anys,
                        "anysGateway": data.anys_gateway,
                        "acceptInvalidCert": data.accept_invalid_cert,
                    }),
                );
            }
            reply(json!({ "ok": true, "data": cfg }));
        }
        // 写 `Data` 级设置（**白名单**：只认下面这几个键，其余一律拒绝）
        "set_data" => {
            let key = req.get("args").and_then(|a| a.get("key")).and_then(|v| v.as_str()).unwrap_or("");
            let value = req.get("args").and_then(|a| a.get("value")).cloned().unwrap_or(serde_json::Value::Null);
            {
                let data = crate::get_data_mut();
                match key {
                    "language" => data.language = value.as_str().map(str::to_owned),
                    "enableAnys" => data.enable_anys = value.as_bool().unwrap_or(false),
                    "anysGateway" => data.anys_gateway = value.as_str().unwrap_or_default().to_owned(),
                    "acceptInvalidCert" => data.accept_invalid_cert = value.as_bool().unwrap_or(false),
                    other => {
                        reply(json!({ "ok": false, "error": format!("不允许写这个 Data 字段：{other}") }));
                        return;
                    }
                }
            }
            let _ = crate::save_data();
            crate::sync_data(); // 语言要立刻生效
            reply(json!({ "ok": true }));
        }
        // 写入配置增量：只暂存，由游戏线程合并 / 存档 / 生效
        "set_config" => {
            let upd = req
                .get("args")
                .and_then(|a| a.get("update"))
                .cloned()
                .or_else(|| req.get("args").cloned());
            match upd {
                Some(upd) => {
                    let mut p = PENDING_CONFIG.lock().unwrap();
                    match p.as_mut() {
                        Some(existing) => merge_json(existing, &upd),
                        None => *p = Some(upd),
                    }
                    reply(json!({ "ok": true }));
                }
                None => reply(json!({ "ok": false, "error": "缺少 update" })),
            }
        }
        // 读取崩溃日志（手机端没有 adb，这里把 <data>/phira-crash.log 直接喂给页面）
        // 可选语言列表（网页设置页的「语言」下拉用）
        "list_languages" => {
            use prpr_l10n::{LANG_IDENTS, LANG_NAMES};
            let list: Vec<serde_json::Value> = LANG_NAMES
                .iter()
                .zip(LANG_IDENTS.iter())
                .map(|(name, ident)| json!({ "name": name, "code": ident.to_string() }))
                .collect();
            // 当前语言在 `Data` 上（不在 `Config` 里），所以要一起带回去给页面。
            let current = crate::get_data().language.clone().unwrap_or_default();
            reply(json!({ "ok": true, "data": { "current": current, "list": list } }))
        }
        // 切换语言（写 data.language 并立刻生效 —— 和原生设置页同一条路：`sync_data`）
        "set_language" => {
            let code = req.get("args").and_then(|a| a.get("code")).and_then(|v| v.as_str()).unwrap_or("");
            if code.is_empty() {
                reply(json!({ "ok": false, "error": "缺少 code" }));
                return;
            }
            crate::get_data_mut().language = Some(code.to_owned());
            let _ = crate::save_data();
            crate::sync_data();
            reply(json!({ "ok": true }));
        }
        // 设置页 i18n：把 locales/*/settings.ftl 的翻译按「中文字符串」导成映射表给 WebView 用
        // （设置页是硬编码中文的静态页，靠这份表跟随全局语言）
        "get_l10n_map" => {
            let data: serde_json::Value = serde_json::from_str(include_str!("l10n_web.json")).unwrap_or(serde_json::Value::Null);
            reply(json!({ "ok": true, "data": data }))
        }
        // 玩家全局统计（收藏夹卡片上的 All / Clear / Full Combo / Phi）
        "get_stats" => reply(json!({ "ok": true, "data": global_stats() })),
        // 读取崩溃日志（手机端没有 adb，这里把 <data>/phira-crash.log 直接喂给页面）
        "get_log" => {
            let path = std::path::Path::new(&crate::dir::root().unwrap_or_default()).join("phira-crash.log");
            let text = std::fs::read_to_string(&path).unwrap_or_else(|_| "[暂无日志]".to_owned());
            reply(json!({ "ok": true, "data": text }));
        }
        // 恢复默认设置（不影响账号 / 谱面 / 存档）
        "reset_config" => {
            {
                let data = crate::get_data_mut();
                data.config = prpr::config::Config::default();
            }
            let _ = crate::save_data();
            let cfg = &crate::get_data().config;
            crate::newui::set_dark(cfg.ui_dark);
            prpr::ui::UI_SFX_VOLUME.store(cfg.volume_sfx.to_bits(), std::sync::atomic::Ordering::Relaxed);
            set_fullscreen(cfg.fullscreen_mode);
            reply(json!({ "ok": true }));
        }
        // 资源包列表（含当前选中）
        "list_respacks" => reply(json!({ "ok": true, "data": respack_list() })),
        // 选中某个资源包（写 data.respack_id）
        "set_respack" => {
            let idx = req.get("args").and_then(|a| a.get("index")).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            *PENDING_RESPCK_SET.lock().unwrap() = Some(idx);
            reply(json!({ "ok": true }));
        }
        // 触发资源包导入（系统文件选择）
        "import_respack" => {
            *PENDING_RESPCK_IMPORT.lock().unwrap() = true;
            reply(json!({ "ok": true }));
        }
        // 导出某个资源包（系统「另存为」）
        "export_respack" => {
            let idx = req.get("args").and_then(|a| a.get("index")).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            *PENDING_RESPCK_EXPORT.lock().unwrap() = Some(idx);
            reply(json!({ "ok": true }));
        }
        // 删除某个资源包（内置包拒绝）
        "delete_respack" => {
            let idx = req.get("args").and_then(|a| a.get("index")).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            *PENDING_RESPCK_DELETE.lock().unwrap() = Some(idx);
            reply(json!({ "ok": true }));
        }
        // 资源包信息（弹窗用）
        "respack_info" => {
            let idx = req.get("args").and_then(|a| a.get("index")).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            reply(json!({ "ok": true, "data": respack_info_value(idx) }));
        }
        // —— 账号管理（设置 → 账号页）——
        // 已保存账号 + 当前登录账号。Phira 只有邮箱+密码、无长期 token，所以「切换」
        // 就是用保存的凭据重新登录（见 page::user）。
        "list_accounts" => {
            let data = crate::get_data();
            let accounts: Vec<serde_json::Value> = data
                .accounts
                .iter()
                .map(|a| json!({ "email": a.email, "name": a.name, "avatar": a.avatar, "rks": a.rks }))
                .collect();
            let current = crate::page::user::current_email();
            reply(json!({ "ok": true, "data": { "accounts": accounts, "current": current } }));
        }
        // 登录一个新账号并保存（邮箱 + 密码）。走网络，放到单独线程里 block_on，
        // 别堵住 WebView 线程。成功返回登录后的展示名。
        "add_account" => {
            let email = req.get("args").and_then(|a| a.get("email")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let password = req.get("args").and_then(|a| a.get("password")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            if email.is_empty() || password.is_empty() {
                reply(json!({ "ok": false, "error": "邮箱和密码都不能为空" }));
            } else {
                let tx2 = tx.clone();
                let pid = id.clone();
                std::thread::spawn(move || {
                    let payload = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(rt) => match rt.block_on(crate::page::user::add_account(&email, &password)) {
                            Ok(name) => json!({ "ok": true, "data": name }),
                            Err(e) => json!({ "ok": false, "error": e.to_string() }),
                        },
                        Err(e) => json!({ "ok": false, "error": format!("无法创建运行时：{e}") }),
                    };
                    reply_async(&tx2, &pid, payload);
                });
            }
        }
        // 切换到某个已保存的账号。同样走网络、单独线程。
        "switch_account" => {
            let email = req.get("args").and_then(|a| a.get("email")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            if email.is_empty() {
                reply(json!({ "ok": false, "error": "缺少 email" }));
            } else {
                let tx2 = tx.clone();
                let pid = id.clone();
                std::thread::spawn(move || {
                    let payload = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(rt) => match rt.block_on(crate::page::user::switch_account(&email)) {
                            Ok(name) => json!({ "ok": true, "data": name }),
                            Err(e) => json!({ "ok": false, "error": e.to_string() }),
                        },
                        Err(e) => json!({ "ok": false, "error": format!("无法创建运行时：{e}") }),
                    };
                    reply_async(&tx2, &pid, payload);
                });
            }
        }
        // 从已保存列表里删除一个账号（不影响当前登录）
        "forget_account" => {
            let email = req.get("args").and_then(|a| a.get("email")).and_then(|v| v.as_str()).unwrap_or("");
            if email.is_empty() {
                reply(json!({ "ok": false, "error": "缺少 email" }));
            } else {
                match crate::page::user::forget_account(email) {
                    Ok(()) => reply(json!({ "ok": true })),
                    Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
                }
            }
        }
        // 退出当前账号（清掉 `Data::me` / 凭据，保留已保存列表）
        "account_logout" => match crate::page::user::logout() {
            Ok(()) => reply(json!({ "ok": true })),
            Err(e) => reply(json!({ "ok": false, "error": e.to_string() })),
        },
        // —— 原生文件选择（设置 → 自定义字体 / 主页立绘）——
        // 桌面端 `request_file` 是同步 rfd 对话框：选完直接把路径写进 `CHOSEN_FILE.1`，
        // 所以能放进单独线程里跑（不堵 WebView 线程）。移动端对话框是异步的，这里会
        // 立刻 `take_file()` 失败 → 回一句「未选择文件」；那边的原生设置页仍可用。
        "pick_file" => {
            let file_id = req.get("args").and_then(|a| a.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let tx2 = tx.clone();
            let pid = id.clone();
            std::thread::spawn(move || {
                let payload = if file_id.is_empty() {
                    json!({ "ok": false, "error": "缺少 id" })
                } else {
                    prpr::scene::request_file(&file_id);
                    let outcome: Result<String, String> = match prpr::scene::take_file() {
                        Some((got, file)) if got == file_id => match custom_asset_path(&file_id) {
                            Some(dst) => std::fs::copy(&file, &dst).map(|_| dst).map_err(|e| e.to_string()),
                            None => Err(format!("未知文件类型：{file_id}")),
                        },
                        Some(_) => Err("返回的文件 id 不匹配".to_owned()),
                        None => Err("未选择文件".to_owned()),
                    };
                    match outcome {
                        Ok(_) => json!({ "ok": true }),
                        Err(e) => json!({ "ok": false, "error": e }),
                    }
                };
                reply_async(&tx2, &pid, payload);
            });
        }
        // 删除导入的字体 / 立绘，恢复内置默认
        "reset_file" => {
            let file_id = req.get("args").and_then(|a| a.get("id")).and_then(|v| v.as_str()).unwrap_or("");
            match custom_asset_path(file_id) {
                Some(p) => {
                    let _ = std::fs::remove_file(p);
                    reply(json!({ "ok": true }));
                }
                None => reply(json!({ "ok": false, "error": format!("未知文件类型：{file_id}") })),
            }
        }
        other => {
            tracing::warn!("[webui] 未知 IPC 命令：{other}");
            reply(json!({ "ok": false, "error": format!("未知命令：{other}") }));
        }
    }
}

/// 在 WebView 线程外（异步 / 文件对话框线程）兑现对应的页面 Promise。
///
/// `handle_ipc` 里的 `reply` 闭包借用的是 `tx` / `id`，塞不进 `thread::spawn` 的
/// `move` 闭包；这里给克隆出来的 `tx` / `pid` 一份等价实现（协议见 `BRIDGE_JS`：
/// `window.__phircResolve(<id>, <payload>)`）。
fn reply_async(tx: &mpsc::Sender<WebCmd>, pid: &Option<serde_json::Value>, payload: serde_json::Value) {
    if let Some(pid) = pid {
        let _ = tx.send(WebCmd::Eval(format!("window.__phircResolve({pid}, {payload})")));
    }
}

/// WebUI 自定义字体 / 主页立绘在数据目录里的落点（与原生设置页 `settings.rs` 的
/// `custom_font_path` / `custom_char_path` 一致，启动时优先加载 / 回退内置）。
fn custom_asset_path(id: &str) -> Option<String> {
    let root = crate::dir::root().ok()?;
    match id {
        "ui_font" => Some(format!("{root}/font.ttf")),
        "home_char" => Some(format!("{root}/char.img")),
        _ => None,
    }
}

/// 玩家全局统计（收藏夹卡片上的 `All / Clear / Full Combo / Phi` 四个数字）。
///
/// 口径：以**本地谱面库**（`data.charts`）为范围，每张谱取 `charts[].record` 与
/// `local_records` 里**更优的那一份**成绩（沿用 `page/best_board.rs` 的取法）：
///
/// * `all`   = 谱面库里的谱面数
/// * `clear` = 有成绩（`score > 0`）的谱面数
/// * `fc`    = 最高成绩达成 Full Combo 的谱面数
/// * `phi`   = 最高成绩全 Perfect（φ）的谱面数
///
/// ⚠️ 不掺在线谱：服务端的最好成绩要现拉（`Client::query()`），拿不到就会显示成 0，
/// 不如只用本地库来得稳定。`counts` 在服务端接口缺字段时会是全 0，所以判 φ 必须
/// 先用 `num_of_notes > 0` 兜一下，否则会把「不知道判定分布」误算成收歌。
fn global_stats() -> serde_json::Value {
    use serde_json::json;

    let data = crate::get_data();
    let all = data.charts.len();
    let mut clear = 0usize;
    let mut fc = 0usize;
    let mut phi = 0usize;

    for chart in &data.charts {
        let ordinary = chart.record.as_ref();
        let fallback = data.local_records.get(&chart.local_path).and_then(|it| it.as_ref());
        let best = match (ordinary, fallback) {
            (Some(a), Some(b)) => Some(if b.score > a.score { b } else { a }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let Some(rec) = best else { continue };

        if rec.score > 0 {
            clear += 1;
        }
        if rec.full_combo {
            fc += 1;
        }
        if rec.num_of_notes > 0 && rec.counts[0] >= rec.num_of_notes {
            phi += 1;
        }
    }

    json!({ "all": all, "clear": clear, "fc": fc, "phi": phi })
}

/// 曲库列表（给收藏夹页面用）。
///
/// ⚠️ 这里在 IPC 线程读 `Data`：和旧实现（协议线程里读 `get_data()`）同一做法，
/// 只读不写、且曲库在游玩期间不会被改。
fn chart_list() -> serde_json::Value {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use serde_json::json;

    let data = crate::get_data();
    let items: Vec<serde_json::Value> = data
        .charts
        .iter()
        .map(|c| {
            // ⚠️ 必须用**相对路径**：页面的真实 origin 是 `http://phirc.web`
            // （WebView2 对自定义协议的归一化），写成 `phirc://web/local-cover` 是
            // 另一个 origin → 子资源请求被直接挡掉，表现是「图片一张都加载不出来、
            // 协议层连请求记录都没有」（只有 JS onerror）。相对路径会和页面同源。
            let cover = format!("local-cover?p={}", URL_SAFE_NO_PAD.encode(&c.local_path));
            // 曲库右侧大图（860×484）用高分辨率版本，避免 200px 缩略图被放大约 2.4 倍发虚。
            let cover_big = format!("local-cover?p={}&big=1", URL_SAFE_NO_PAD.encode(&c.local_path));
            // 最高记录：`charts[].record`（云/官方最好成绩）与 `local_records`（本地最好成绩）
            // 取**分数更高**的那份（同 page/best_board.rs / get_custom_rks 的取法）。
            let ordinary = c.record.as_ref();
            let fallback = data.local_records.get(&c.local_path).and_then(|it| it.as_ref());
            let best = match (ordinary, fallback) {
                (Some(a), Some(b)) => Some(if b.score > a.score { b } else { a }),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            let record = best.map(|r| {
                json!({
                    "score": r.score,
                    "accuracy": r.accuracy,
                    "fullCombo": r.full_combo,
                })
            });
            json!({
                "name": c.info.name,
                "composer": c.info.composer,
                "charter": c.info.charter,
                "illustrator": c.info.illustrator,
                "level": c.info.level,
                "difficulty": c.info.difficulty,
                "path": c.local_path,
                // ⚠️ 必须带这个标记：曲库页靠它区分「本地直接开局」和「在线谱面开局」。
                // 漏了的话所有本地卡都会走在线分支（没有 id）→ 点了没反应（「点不动」）。
                "local": true,
                "cover": cover,
                "coverBig": cover_big,
                "record": record,
            })
        })
        .collect();
    json!(items)
}

/// 在线曲库一页（上架 / 特殊 / 未上架 / 热门）。
///
/// 查询口径与原生曲库页 `page/library.rs::load_online` 一致：
/// * `ranked` / `special` / `unstable` → `type=0/1/2`
/// * `popular` → `/popular`（不带搜索 / 排序 / 定数区间）
/// * 分页每页 28 条（同 `PAGE_NUM`）
///
/// ⚠️ 这里在 IPC 线程的网络线程里读 `get_data()`（只读 `enable_anys`），与
/// `add_account` 等同理；曲库在浏览期间不会被改。
async fn online_charts_page(ty: &str, page: u64, search: &str, order: &str) -> serde_json::Value {
    use crate::client::{Chart, Client};
    use serde_json::json;
    const PAGE_NUM: u64 = 28;
    // 与 `tags.rs` 的 `DIVISION_TAGS[0]`（默认 division）保持一致；那个常量没公开。
    const DIVISION: &str = "regular";

    let popular = ty == "popular";
    let typ = match ty {
        "special" => 1,
        "unstable" => 2,
        _ => 0,
    };
    let anys = crate::get_data().enable_anys;
    // 已下载到本机的在线谱面 id 集合：在线列表据此把 `local` 置 true，
    // 前端才能给「未下载」的谱面曲绘加暗 + 锁图标。
    let local_ids: std::collections::HashSet<i32> = crate::get_data().charts.iter().filter_map(|c| c.info.id).collect();

    let mut q = Client::query::<Chart>();
    if popular {
        q = q.suffix("/popular");
    } else {
        // ⚠️ `order` / `search` 形参是 `impl Into<Cow<'static, str>>`，必须给拥有所有权的串
        q = q.search(search.to_owned()).order(order.to_owned()).tags("").query("rating", "0,16.5");
    }
    let res = q.query("type", typ.to_string()).query("division", DIVISION).page(page).page_num(PAGE_NUM).send().await;
    match res {
        Ok((remote, count)) => {
            crate::diag(&format!("[webui] 在线曲库 {ty} page={page} → {} 条 / 共 {count}", remote.len()));
            let charts: Vec<serde_json::Value> = remote
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "name": c.name,
                        "level": c.level,
                        "difficulty": c.difficulty,
                        "charter": c.charter,
                        "composer": c.composer,
                        "illustrator": c.illustrator,
                        "intro": c.description.clone().unwrap_or_default(),
                        "rating": c.rating,
                        "ranked": c.ranked,
                        "reviewed": c.reviewed,
                        "stable": c.stable,
                        "cover": cover_url(&c.illustration.url, anys),
                        // 在线谱面默认没有本地路径 / 本地成绩，前端据此禁掉「开局」；
                        // 但**已下载**的要把 local 置 true（曲绘去暗去锁，也能直接开局）。
                        "local": local_ids.contains(&c.id),
                    })
                })
                .collect();
            let total_page = if count == 0 { 0 } else { (count - 1) / PAGE_NUM + 1 };
            json!({ "ok": true, "data": { "charts": charts, "count": count, "total_page": total_page } })
        }
        Err(e) => {
            // 完整错误链进日志（页面只显示第一层），下次实测能直接看到失败原因
            crate::diag(&format!("[webui] 在线曲库 {ty} page={page} 查询失败：{e:#}"));
            json!({ "ok": false, "error": e.to_string() })
        }
    }
}

// ============================================================
//  .phc 插件（用户自己导入的扩展）
// ============================================================
//
// 格式：`.phc` 有两种形态，都能导入 ——
//   A) **ZIP**（推荐）：根目录含 `manifest.json`，可选 `main.js` 与其它资源；
//   B) **纯 JSON**：单个 JSON 文件，就是 manifest，脚本写在 `"script"` 字段里。
// manifest 字段：`phc`(格式版本) / `id`(唯一，[A-Za-z0-9._-]{1,64}) / `name` / `version`
//   / `author` / `description` / `entry`(默认 `main.js`) / `script`(仅纯 JSON 形态)。
//
// 落盘：`<data>/plugins/<id>/`，启用状态在同目录 `state.json`（`{"<id>": true}`）。

/// 插件根目录（不存在就建）。
fn plugins_dir() -> anyhow::Result<PathBuf> {
    let root = crate::dir::root()?;
    let dir = PathBuf::from(root).join("plugins");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn plugin_state_path() -> anyhow::Result<PathBuf> {
    Ok(plugins_dir()?.join("state.json"))
}

fn load_plugin_state() -> serde_json::Map<String, serde_json::Value> {
    plugin_state_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn save_plugin_state(map: &serde_json::Map<String, serde_json::Value>) -> anyhow::Result<()> {
    std::fs::write(plugin_state_path()?, serde_json::to_string_pretty(map)?)?;
    Ok(())
}

/// 校验插件 id —— 同时兼作**目录穿越**防护（id 会当目录名用）。
fn valid_plugin_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// 读插件目录里的 manifest（ZIP 形态 `manifest.json` / 纯 JSON 形态 `plugin.json`）。
fn read_plugin_manifest(dir: &std::path::Path) -> Option<serde_json::Value> {
    for name in ["manifest.json", "plugin.json"] {
        if let Ok(t) = std::fs::read_to_string(dir.join(name)) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                return Some(v);
            }
        }
    }
    None
}

fn plugin_entry_of(manifest: &serde_json::Value) -> String {
    manifest.get("entry").and_then(|v| v.as_str()).unwrap_or("main.js").to_owned()
}

// ——— 旁挂进程运行时（runtime = python / rust）———
//
// 不走 DLL / 进程内 FFI：**插件就是一个子进程**，用 JSON-lines 走 stdin/stdout 对话。
// 好处：不需要 ABI 稳定、插件崩了只死自己、Python 和 Rust 能共用同一套协议。
//
// 协议（每行一个 JSON）：
//   宿主 → 插件：`{"event":"init","id":…,"dir":…,"api":1,"storage":{…}}` / `ready` / `charts` / `cardRender` / `click` / `shutdown`
//   插件 → 宿主：`{"cmd":"log|toast|addToolbarButton|badge|storageSet|startGame|openPage|openUrl|reload", …}`
// 宿主把插件的每一行原样转发给页面执行：`window.__phcPluginCommand(<id>, <line>)`。

struct Sidecar {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
}

static SIDECARS: Mutex<Vec<(String, Sidecar)>> = Mutex::new(Vec::new());

/// 找 Python 解释器：`PHC_PYTHON` 环境变量 → `python` → `python3` → `py -3`。
fn find_python() -> Option<(String, Vec<String>)> {
    if let Ok(p) = std::env::var("PHC_PYTHON") {
        if std::path::Path::new(&p).is_file() {
            return Some((p, Vec::new()));
        }
    }
    let cands: [(&str, &[&str]); 3] = [("python", &[]), ("python3", &[]), ("py", &["-3"])];
    for (prog, args) in cands {
        if let Ok(out) = std::process::Command::new(prog).args(args).arg("--version").output() {
            if out.status.success() {
                return Some((prog.to_owned(), args.iter().map(|s| (*s).to_owned()).collect()));
            }
        }
    }
    None
}

/// 停掉一个旁挂插件（幂等）。
fn stop_sidecar(id: &str) {
    let mut guard = SIDECARS.lock().unwrap();
    if let Some(pos) = guard.iter().position(|(k, _)| k == id) {
        let (_, mut sc) = guard.remove(pos);
        {
            use std::io::Write;
            let _ = sc.stdin.write_all(b"{\"event\":\"shutdown\"}\n");
            let _ = sc.stdin.flush();
        }
        let _ = sc.child.kill();
        let _ = sc.child.wait();
    }
}

/// 启动旁挂插件进程。返回 `{ id, runtime, pid }`。
fn start_sidecar(id: &str) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    if !valid_plugin_id(id) {
        anyhow::bail!("非法插件 id");
    }
    stop_sidecar(id); // 重复启动先收旧的
    let dir = plugins_dir()?.join(id);
    let m = read_plugin_manifest(&dir).ok_or_else(|| anyhow!("找不到插件 {id} 的 manifest"))?;
    let runtime = m.get("runtime").and_then(|v| v.as_str()).unwrap_or("js").to_owned();
    let entry = plugin_entry_of(&m);
    let entry_path = dir.join(&entry);
    if !entry_path.is_file() {
        anyhow::bail!("找不到入口文件 {entry}");
    }

    let mut cmd = match runtime.as_str() {
        "python" => {
            let (prog, args) = find_python().ok_or_else(|| anyhow!("没有找到 Python 解释器（装一个 Python 3，或用环境变量 PHC_PYTHON 指定 python.exe 的完整路径）"))?;
            let mut c = std::process::Command::new(prog);
            c.args(args).arg(&entry_path);
            c
        }
        "rust" => std::process::Command::new(&entry_path),
        other => anyhow::bail!("runtime `{other}` 不是进程型运行时（进程型只有 python / rust）"),
    };
    cmd.current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("PHC_PLUGIN_ID", id)
        .env("PHC_PLUGIN_DIR", dir.display().to_string());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：别给插件弹一个黑框
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| anyhow!("启动 {runtime} 插件失败：{e}"))?;
    let pid = child.id();
    let stdin = child.stdin.take().ok_or_else(|| anyhow!("拿不到插件 stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("拿不到插件 stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("拿不到插件 stderr"))?;

    // stdout：逐行转发给页面执行
    {
        let id = id.to_owned();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines() {
                let line: String = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(h) = HOST.get() {
                    // `{:?}` 把 id 变成合法的 JS 字符串字面量；line 本身已经是 JSON
                    let js = format!("window.__phcPluginCommand && window.__phcPluginCommand({id:?}, {line});");
                    let _ = h.cmd_tx.send(WebCmd::Eval(js));
                }
            }
            if let Some(h) = HOST.get() {
                let js = format!("window.__phcPluginCommand && window.__phcPluginCommand({id:?}, {{ cmd: 'exited' }});");
                let _ = h.cmd_tx.send(WebCmd::Eval(js));
            }
        });
    }
    // stderr：进诊断日志（插件自己 print 到 stderr 也能看到）
    {
        let id = id.to_owned();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stderr).lines() {
                let line: String = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };
                let line = line.trim();
                if !line.is_empty() {
                    crate::diag(&format!("[plugin:{id}] {line}"));
                }
            }
        });
    }

    SIDECARS.lock().unwrap().push((id.to_owned(), Sidecar { child, stdin }));
    // 立刻发 init（带上插件自己的 storage，让插件能恢复状态）
    let storage = load_plugin_storage(id);
    let _ = send_sidecar_event(id, &json!({
        "event": "init", "api": 1, "id": id, "dir": dir.display().to_string(), "storage": storage,
    }));
    Ok(json!({ "id": id, "runtime": runtime, "pid": pid }))
}

/// 往插件 stdin 写一行事件。写管道可能阻塞 → 单独线程，别堵调用方。
fn send_sidecar_event(id: &str, event: &serde_json::Value) -> anyhow::Result<()> {
    let line = format!("{}\n", serde_json::to_string(event)?);
    let id = id.to_owned();
    std::thread::spawn(move || {
        let mut guard = SIDECARS.lock().unwrap();
        if let Some((_, sc)) = guard.iter_mut().find(|(k, _)| k == &id) {
            use std::io::Write;
            let _ = sc.stdin.write_all(line.as_bytes());
            let _ = sc.stdin.flush();
        }
    });
    Ok(())
}

fn sidecar_running(id: &str) -> bool {
    SIDECARS.lock().unwrap().iter().any(|(k, _)| k == id)
}

/// 插件自己的持久化存储（`plugins/storage.json` 里的 `{ "<id>": {…} }`）。
fn plugin_storage_path() -> anyhow::Result<PathBuf> {
    Ok(plugins_dir()?.join("storage.json"))
}

fn load_plugin_storage(id: &str) -> serde_json::Value {
    plugin_storage_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get(id).cloned())
        .unwrap_or_else(|| serde_json::json!({}))
}

fn set_plugin_storage(id: &str, key: &str, value: serde_json::Value) -> anyhow::Result<()> {
    let path = plugin_storage_path()?;
    let mut all: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !all.is_object() {
        all = serde_json::json!({});
    }
    let obj = all.as_object_mut().unwrap();
    let entry = obj.entry(id.to_owned()).or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    entry.as_object_mut().unwrap().insert(key.to_owned(), value);
    std::fs::write(path, serde_json::to_string_pretty(&all)?)?;
    Ok(())
}

/// 列出已安装插件（含启用状态与是否有脚本）。
fn plugin_list() -> serde_json::Value {
    use serde_json::json;
    let state = load_plugin_state();
    let Ok(root) = plugins_dir() else { return json!([]) };
    let Ok(rd) = std::fs::read_dir(&root) else { return json!([]) };
    let mut out: Vec<serde_json::Value> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Some(dir_name) = p.file_name().and_then(|s| s.to_str()) else { continue };
        if dir_name.starts_with('_') {
            continue; // 解压用的临时目录
        }
        let Some(m) = read_plugin_manifest(&p) else { continue };
        let id = m.get("id").and_then(|v| v.as_str()).unwrap_or(dir_name).to_owned();
        let entry = plugin_entry_of(&m);
        let has_script = p.join(&entry).is_file() || m.get("script").and_then(|v| v.as_str()).is_some();
        let enabled = state.get(&id).and_then(|v| v.as_bool()).unwrap_or(true);
        let runtime = m.get("runtime").and_then(|v| v.as_str()).unwrap_or("js").to_owned();
        let hooks: Vec<String> = m
            .get("hooks")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_owned())).collect())
            .unwrap_or_else(|| vec!["ready".to_owned(), "charts".to_owned()]);
        out.push(json!({
            "id": id,
            "name": m.get("name").and_then(|v| v.as_str()).unwrap_or(&id),
            "version": m.get("version").and_then(|v| v.as_str()).unwrap_or(""),
            "author": m.get("author").and_then(|v| v.as_str()).unwrap_or(""),
            "description": m.get("description").and_then(|v| v.as_str()).unwrap_or(""),
            "entry": entry,
            "hasScript": has_script,
            "enabled": enabled,
            "runtime": runtime,
            "hooks": hooks,
            "running": sidecar_running(&id),
        }));
    }
    out.sort_by(|a, b| a["id"].as_str().unwrap_or("").cmp(b["id"].as_str().unwrap_or("")));
    json!(out)
}

/// 读插件脚本正文（纯 JSON 形态优先用 manifest 里的 `script`）。
fn plugin_script(id: &str) -> anyhow::Result<String> {
    if !valid_plugin_id(id) {
        anyhow::bail!("非法插件 id");
    }
    let dir = plugins_dir()?.join(id);
    let m = read_plugin_manifest(&dir).ok_or_else(|| anyhow!("找不到插件 {id} 的 manifest"))?;
    if let Some(s) = m.get("script").and_then(|v| v.as_str()) {
        return Ok(s.to_owned());
    }
    let entry = plugin_entry_of(&m);
    let p = dir.join(&entry);
    if !p.is_file() {
        anyhow::bail!("插件 {id} 没有脚本文件 {entry}");
    }
    Ok(std::fs::read_to_string(p)?)
}

/// 导入 `.phc`（ZIP 或纯 JSON），落到 `<data>/plugins/<id>/` 并默认启用。
fn import_plugin_from_file(src: &std::path::Path) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    let bytes = std::fs::read(src)?;
    let root = plugins_dir()?;
    let is_zip = bytes.starts_with(b"PK");
    let (manifest, tmp): (serde_json::Value, Option<PathBuf>) = if is_zip {
        let tmp = root.join(format!("_tmp_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp)?;
        let dir = prpr::dir::Dir::new(&tmp)?;
        prpr::ext::unzip_into(std::io::Cursor::new(&bytes), &dir, true)?;
        let mpath = tmp.join("manifest.json");
        if !mpath.is_file() {
            let _ = std::fs::remove_dir_all(&tmp);
            anyhow::bail!("压缩包里没有 manifest.json");
        }
        (serde_json::from_str(&std::fs::read_to_string(&mpath)?)?, Some(tmp))
    } else {
        (
            serde_json::from_slice(&bytes).map_err(|e| anyhow!("不是有效的 .phc（既不是 ZIP 也不是合法 JSON）：{e}"))?,
            None,
        )
    };
    let id = manifest.get("id").and_then(|v| v.as_str()).unwrap_or("").to_owned();
    if !valid_plugin_id(&id) {
        if let Some(t) = &tmp {
            let _ = std::fs::remove_dir_all(t);
        }
        anyhow::bail!("manifest 的 id 非法（只允许字母数字与 . _ -，最长 64）");
    }
    let dst = root.join(&id);
    if dst.exists() {
        std::fs::remove_dir_all(&dst)?;
    }
    if let Some(t) = tmp {
        std::fs::rename(&t, &dst)?;
    } else {
        std::fs::create_dir_all(&dst)?;
        std::fs::write(dst.join("plugin.json"), &bytes)?;
    }
    let mut st = load_plugin_state();
    st.insert(id.clone(), serde_json::Value::Bool(true));
    let _ = save_plugin_state(&st);
    let name = manifest.get("name").and_then(|v| v.as_str()).unwrap_or(&id).to_owned();
    Ok(json!({ "id": id, "name": name }))
}

fn delete_plugin(id: &str) -> anyhow::Result<()> {
    if !valid_plugin_id(id) {
        anyhow::bail!("非法插件 id");
    }
    let dir = plugins_dir()?.join(id);
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    let mut st = load_plugin_state();
    st.remove(id);
    let _ = save_plugin_state(&st);
    Ok(())
}

fn set_plugin_enabled(id: &str, on: bool) -> anyhow::Result<()> {
    if !valid_plugin_id(id) {
        anyhow::bail!("非法插件 id");
    }
    let mut st = load_plugin_state();
    st.insert(id.to_owned(), serde_json::Value::Bool(on));
    save_plugin_state(&st)
}

/// 按 id 取在线谱面信息（`Ptr::<Chart>::new(id).fetch()`，同 `page/message.rs`
/// 里点消息附带的谱面那条路）。只在 IPC 线程的网络线程里调用。
async fn fetch_online_chart(id: i32) -> anyhow::Result<std::sync::Arc<crate::client::Chart>> {
    use crate::client::{Chart, Ptr};
    Ptr::<Chart>::new(id).fetch().await
}

/// 曲绘 URL：开启 Anys 时把 `<API>/files/...` 改写成 `<API>/anys/...`
/// （同 `sensitive/auth/model.rs` 里 `File::request` 的转发规则）。
fn cover_url(url: &str, anys: bool) -> String {
    if anys {
        if let Some(rest) = url.strip_prefix(crate::client::API_URL) {
            if let Some(file) = rest.strip_prefix("/files/") {
                return format!("{}/anys/{}", crate::client::API_URL, file);
            }
        }
    }
    url.to_owned()
}

/// 自定义 RKS 汇总：`{ rks, best_used, ap_used }`。
///
/// 口径完全复用 `page/rks.rs::refresh` + `custom_rks::calculate`：
/// 每张谱取 `charts[].record` 与 `local_records` **更优的那一份**精度，
/// 难度用 `info.difficulty`，不接 AI 定数。
fn custom_rks_summary() -> serde_json::Value {
    use crate::custom_rks::{calculate, highest_record_accuracy, LocalInput, Settings};
    use serde_json::json;
    let path = match crate::dir::root() {
        Ok(root) => PathBuf::from(root).join("custom-rks.json"),
        Err(_) => return json!({ "rks": 0.0, "best_used": 0, "ap_used": 0 }),
    };
    let settings = Settings::load(&path).unwrap_or_default();
    let data = crate::get_data();
    let locals: Vec<LocalInput> = data
        .charts
        .iter()
        .filter_map(|c| {
            let ordinary = c.record.as_ref().map(|r| r.accuracy as f64 * 100.);
            let fallback = data.local_records.get(&c.local_path).and_then(|r| r.as_ref()).map(|r| r.accuracy as f64 * 100.);
            Some(LocalInput {
                path: c.local_path.clone(),
                name: c.info.name.clone(),
                level: c.info.level.clone(),
                difficulty: c.info.difficulty as f64,
                ai_difficulty: None,
                accuracy: highest_record_accuracy(ordinary, fallback),
            })
        })
        .collect();
    let (_, summary) = calculate(&settings, &locals);
    json!({ "rks": summary.rks, "best_used": summary.best_used, "ap_used": summary.ap_used })
}

/// 解析 `?p=<base64url>[&big=1]` 并取曲绘。
///
/// `big=1` → 高分辨率版本（高 720），给曲库右侧大图用；默认仍返回 200px 缩略图
/// （原生卡片 / 收藏夹用，省内存）。两者缓存到不同文件。
#[cfg(any(target_os = "windows", target_os = "ios"))]
fn local_cover_query(query: Option<&str>) -> Option<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

    let q = query?;
    let encoded = q.split('&').find_map(|kv| kv.strip_prefix("p="))?;
    let big = q.split('&').any(|kv| kv == "big=1");
    let path = String::from_utf8(URL_SAFE_NO_PAD.decode(encoded).ok()?).ok()?;
    load_local_cover(&path, big)
}

/// 给曲库 / 收藏夹提供本地谱包的曲绘：命中缩略图缓存就直接读，否则解包 + 生成 + 落缓存。
///
/// `big` = true 时生成高 720 的大图（缓存文件名追加 `_big`）。曲库右侧大图是
/// 860×484，用 200px 缩略图会被放大 ~2.4 倍、明显发虚，所以单独缓存一份大图。
#[cfg(any(target_os = "windows", target_os = "ios"))]
fn load_local_cover(chart_path: &str, big: bool) -> Option<Vec<u8>> {
    // 只允许请求当前曲库里已登记的谱面，防止自定义协议被用来读任意本地文件
    if !crate::get_data().charts.iter().any(|c| c.local_path == chart_path) {
        return None;
    }
    let mut cache_path = crate::page::thumbnail_path(chart_path).ok()?;
    if big {
        let mut p = cache_path.into_os_string();
        p.push("_big");
        cache_path = p.into();
    }
    if let Ok(bytes) = std::fs::read(&cache_path) {
        return Some(bytes);
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    runtime.block_on(async {
        let mut fs = crate::scene::fs_from_path(chart_path).ok()?;
        let info = prpr::fs::load_info(fs.as_mut()).await.ok()?;
        let image_data = fs.load_file(&info.illustration).await.ok()?;
        let image = image::load_from_memory(&image_data).ok()?;
        let thumbnail = if big {
            const BIG_H: u32 = 720;
            let w = ((image.width() as f32 / image.height() as f32) * BIG_H as f32).ceil() as u32;
            image::DynamicImage::ImageRgba8(image::imageops::thumbnail(&image, w.max(1), BIG_H))
        } else {
            crate::images::Images::thumbnail(&image)
        };
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        thumbnail.save_with_format(&cache_path, image::ImageFormat::Jpeg).ok()?;
        std::fs::read(cache_path).ok()
    })
}

// ============================================================
//  JS 侧桥（注入）：window.phirc.*
// ============================================================

/// 给页面用的 IPC 桥。命令走 `window.ipc.postMessage`，回包走
/// `window.__phircResolve(id, payload)`（由 Rust 侧 `evaluate_script` 调回来）。
///
/// 页面里这样用（`path` 就是 [`chart_list`] 里给的 `path`）：
/// ```js
/// const charts = await window.phirc.listCharts();
/// await window.phirc.startGame(charts[0].path);
/// window.phirc.hide();
/// ```
const BRIDGE_JS: &str = r#"
(function () {
  if (window.phirc) { return; }

  var seq = 0;
  var pending = {};

  // Rust → 页面：兑现一个 Promise
  window.__phircResolve = function (id, payload) {
    var p = pending[id];
    if (!p) { return; }
    delete pending[id];
    if (payload && payload.ok === false) {
      p.reject(new Error(payload.error || 'phirc error'));
    } else {
      p.resolve(payload ? payload.data : undefined);
    }
  };

  function call(cmd, args) {
    return new Promise(function (resolve, reject) {
      var id = ++seq;
      pending[id] = { resolve: resolve, reject: reject };
      try {
        window.ipc.postMessage(JSON.stringify({ id: id, cmd: cmd, args: args || {} }));
      } catch (e) {
        delete pending[id];
        reject(e);
      }
    });
  }

  window.phirc = {
    // 曲库列表 → [{ name, composer, charter, illustrator, level, difficulty, path, cover }]
    listCharts: function () { return call('list_charts'); },
    // 在线曲库一页：type ∈ 'ranked'(上架) / 'special'(特殊) / 'unstable'(未上架) / 'popular'(热门)
    // → { charts:[{id,name,level,difficulty,charter,composer,illustrator,cover,rating,ranked,reviewed,stable,local:false}], count, total_page }
    // 未登录 / 离线模式 → { ok:false, error:'must-login' | 'offline-mode' | 'must-accept-tos' }
    listOnlineCharts: function (type, page, search, order) {
      return call('list_online_charts', {
        type: String(type || 'ranked'), page: Number(page || 0),
        search: String(search || ''), order: String(order || 'updated'),
      });
    },
    // ——— 回放系统 ———
    // ——— 谱面管理面板（ChartPanel.html）———
    // 一次拿齐 info.yml 全字段 + mods 位 + 血条配置 + 只读信息
    chartPanel: function (path) { return call('chart_panel', { path: String(path) }); },
    // 改 Mods（bits 是位标志，口径同 prpr::config::Mods）
    chartPanelMods: function (path, bits) { return call('chart_panel_mods', { path: String(path), bits: (bits | 0) }); },
    // 保存「编辑谱面」（patch 为部分字段）
    chartPanelSave: function (path, patch) { return call('chart_panel_save', { path: String(path), patch: patch || {} }); },
    // 替换谱面文件：kind ∈ chart / music / illustration / unlock
    chartPanelPick: function (path, kind) { return call('chart_panel_pick', { path: String(path), kind: String(kind) }); },
    // 在网站中打开谱面
    openChartWeb: function (id) { return call('open_chart_web', { id: (id | 0) }); },

    // 列出全部回放（正式局会自动录制，导入的他人回放也在内）
    listReplays: function () { return call('list_replays'); },
    // 导入 .ppr 回放文件
    importReplay: function () { return call('import_replay'); },
    // 导入本地谱面（弹系统文件框）→ { name, warning }；曲库页「导入」用它，
    // 不再走 openTool('toolbox')（那是谱面工具箱，不是导入）。
    importChart: function () { return call('import_chart'); },
    // 导出指定回放（参数 file = listReplays 返回的 file）
    exportReplay: function (file) { return call('export_replay', { file: String(file) }); },
    // 删除指定回放
    deleteReplay: function (file) { return call('delete_replay', { file: String(file) }); },
    // 给回放关联本地谱面（file = listReplays 返回的 file，localPath = listCharts 返回的 path）
    associateReplay: function (file, localPath) { return call('associate_replay', { file: String(file), localPath: localPath == null ? null : String(localPath) }); },
    // 播放指定回放（走原生游戏场景，观感与录制时一致）
    watchReplay: function (file) { return call('watch_replay', { file: String(file) }); },
    // 自定义 RKS 汇总（本地谱面 + 本地成绩算出来的）：{ rks, best_used, ap_used }
    getCustomRks: function () { return call('get_custom_rks'); },
    // ——— .phc 插件 ———
    // 弹系统文件框导入一个 .phc（ZIP 内含 manifest.json，或单个 JSON）→ { id, name }
    importPlugin: function () { return call('import_plugin'); },
    // 已安装插件列表 → [{ id, name, version, author, description, entry, hasScript, enabled }]
    listPlugins: function () { return call('list_plugins'); },
    // 读插件脚本正文（字符串）
    readPluginScript: function (id) { return call('read_plugin_script', { id: String(id) }); },
    setPluginEnabled: function (id, on) { return call('set_plugin_enabled', { id: String(id), enabled: !!on }); },
    // 启动/停止旁挂进程插件（runtime = python / rust）
    startPlugin: function (id) { return call('start_plugin', { id: String(id) }); },
    stopPlugin: function (id) { return call('stop_plugin', { id: String(id) }); },
    // 给旁挂插件推事件（event 是对象，形如 { event: 'charts', list: [...] }）
    pluginEvent: function (id, event) { return call('plugin_event', { id: String(id), event: event || {} }); },
    // 旁挂插件的持久化存储
    pluginStorageSet: function (id, key, value) { return call('plugin_storage_set', { id: String(id), key: String(key), value: value }); },
    deletePlugin: function (id) { return call('delete_plugin', { id: String(id) }); },
    // 在线谱面开局（id 用 listOnlineCharts() 返回的 id）：成功后隐藏 WebView 进SongScene，
    // 按 play 时才真正下载谱面（可取消）
    startOnlineChart: function (id) { return call('start_online_chart', { id: Number(id) }); },
    // 课题模式：WebView 曲库选好 3 张后直接开局（paths = 3 个 local_path）
    startChallenge: function (paths) { return call('start_challenge', { paths: paths }); },
    // 离线渲染：opts = { out, width, height, fps, msaa, crf, bitrate }
    renderChart: function (path, opts) { return call('render_chart', Object.assign({ path: String(path) }, opts || {})); },
    // 制谱器
    loadChart: function (path) { return call('load_chart', { path: String(path) }); },
    saveChart: function (path, chart, format) { return call('save_chart', { path: String(path), chart: chart, format: String(format || 'pgr') }); },
    editorWaveform: function (path, buckets) { return call('editor_waveform', { path: String(path), buckets: Number(buckets) || 1600 }); },
    openRpen: function (path) { return call('open_rpen', { path: String(path) }); },
    backToEditor: function (path) { return call('back_to_editor', { path: String(path) }); },
    writeChartFile: function (path, name, text) { return call('write_chart_file', { path: String(path), name: String(name), text: String(text) }); },
    editorBundle: function (path) { return call('editor_bundle', { path: String(path) }); },
    saveInfo: function (path, info) { return call('save_info', { path: String(path), info: info || {} }); },
    newChart: function (name) { return call('new_chart', { name: String(name || '') }); },
    // 谱面管理 / 创建
    listEditorCharts: function () { return call('list_editor_charts', {}); },
    startDan: function (paths, name) { return call('start_dan', { paths: paths, name: String(name || '段位') }); },
    getGrowth: function () { return call('get_growth', {}); },
    getMods: function () { return call('get_mods', {}); },
    setMod: function (id, on) { return call('set_mod', { id: String(id), on: !!on }); },
    deleteChart: function (path) { return call('delete_chart', { path: String(path) }); },
    pickAssetFile: function (kind) { return call('pick_asset_file', { kind: String(kind || 'music') }); },
    bridgeVersion: 3,
    // 分享码 / 渲染队列
    shareCode: function (path) { return call('share_code', { path: String(path) }); },
    importShareCode: function (code) { return call('import_share_code', { code: String(code || '') }); },
    queueRender: function (path, opts, label) { return call('queue_render', Object.assign({ path: String(path), label: String(label || '') }, opts || {})); },
    queueStatus: function () { return call('queue_status', {}); },
    diffCharts: function (a, b) { return call('diff_charts', { a: a, b: b }); },
    applyMerge: function (base, edits) { return call('apply_merge', { base: base, edits: edits || [] }); },
    createChart: function (music, illustration, name) { return call('create_chart', { music: music || null, illustration: illustration || null, name: String(name || '') }); },
    // 展开原生曲库页里的浮层：name ∈ 'challenge'(课题) / 'rks'(自定义 RKS) / 'board'(最佳谱面榜)
    openOverlay: function (name) { return call('open_overlay', { name: String(name) }); },
    // 开一局（path 用 listCharts() 返回的 path）；成功后会隐藏 WebView 进游戏
    startGame: function (path) { return call('start_game', { path: path }); },
    // 让位给 macroquad 原生界面
    hide: function () { return call('hide'); },
    // 打开原生页面（name: 'settings' | 'favorites' | 'library' | 'home'
    //                    | 'message' | 'history' | 'respack' | 'blacklist'）；
    // 成功后会隐藏 WebView 进原生界面
    openPage: function (name) { return call('open_page', { name: name }); },
    // 打开工具覆盖页（name: 'toolbox' 谱面工具箱 | 'event' 活动）；同样会隐藏 WebView
    openTool: function (name) { return call('open_tool', { tool: String(name) }); },
    // 用系统浏览器打开外部链接（QQ 频道等）
    openUrl: function (url) { return call('open_url', { url: String(url) }); },
    // 往 Rust 日志写一行（调试用）
    log: function (msg) { return call('log', { msg: String(msg) }); },
    // 玩家全局统计（收藏夹卡片用）：{ all, clear, fc, phi }
    getStats: function () { return call('get_stats'); },
    // 可选语言列表：[{ name, code }]
    listLanguages: function () { return call('list_languages'); },
    getL10nMap: function () { return call('get_l10n_map'); },
    // 切换语言（立刻生效，和原生设置页一样）
    setLanguage: function (code) { return call('set_language', { code: String(code) }); },
    // 写 Data 级设置（白名单：language / enableAnys / anysGateway / acceptInvalidCert）
    setData: function (key, value) { return call('set_data', { key: String(key), value: value }); },
    // 读取真实游戏配置（Setting.html / 剧情编辑器初始化用）
    getConfig: function () { return call('get_config'); },
    // 写入配置增量：只发改了的那几个字段，未涉及的保留当前值
    // （Setting.html 传 `{key: value}`，剧情编辑器传 `{ story: "<json>" }`）
    setConfig: function (obj) { return call('set_config', obj || {}); },
    // 恢复默认设置（不影响账号 / 谱面 / 存档）
    resetConfig: function () { return call('reset_config'); },
    // —— 资源包页 ——
    // 列出资源包（含当前选中）：{ items:[{index,name,isDefault}], current }
    listRespacks: function () { return call('list_respacks'); },
    // 选中某个资源包（index；0=默认包）
    setRespack: function (index) { return call('set_respack', { index: index }); },
    // 触发导入（系统文件选择）
    importRespack: function () { return call('import_respack'); },
    // 导出某个资源包（系统「另存为」）
    exportRespack: function (index) { return call('export_respack', { index: index }); },
    // 删除某个资源包（内置包会被拒绝）
    deleteRespack: function (index) { return call('delete_respack', { index: index }); },
    // 资源包信息（弹窗用）：{ index, name, author, description, path, isDefault }
    respackInfo: function (index) { return call('respack_info', { index: index }); },
    // —— 账号管理（设置 → 账号页）——
    // 已保存账号 + 当前登录：{ accounts:[{email,name,avatar,rks}], current }
    listAccounts: function () { return call('list_accounts'); },
    // 登录新账号（邮箱 + 密码），成功后自动保存以便一键切换 → 返回展示名
    addAccount: function (email, password) { return call('add_account', { email: String(email), password: String(password) }); },
    // 切换到某个已保存账号（用保存的凭据重新登录）→ 返回展示名
    switchAccount: function (email) { return call('switch_account', { email: String(email) }); },
    // 从已保存列表删除账号（不影响当前登录）
    forgetAccount: function (email) { return call('forget_account', { email: String(email) }); },
    // 退出当前账号
    logout: function () { return call('account_logout'); },
    // —— 原生文件选择（自定义字体 / 主页立绘）——
    // 弹出系统文件选择框，选完复制到数据目录；id: 'ui_font' | 'home_char'
    pickFile: function (id) { return call('pick_file', { id: String(id) }); },
    // 删除导入的字体 / 立绘，恢复内置默认
    resetFile: function (id) { return call('reset_file', { id: String(id) }); }
  };
})();
"#;

// ============================================================
//  alert 兜底（注入）：wry 不处理脚本对话框
// ============================================================

/// 把 `window.alert` 换成**页内弹窗**。
///
/// 为什么需要：wry 0.44 里**没有**任何 `ScriptDialogOpening` 处理，
/// 而 WebView2 的默认脚本对话框在嵌入式/子窗口场景下经常不弹 ——
/// 表现就是「按钮点了完全没反应」（`tripsUide.html` 里的收藏品 / QQ频道 / 设置 /
/// PLAY 现在都只是 `alert(...)` 占位）。
///
/// 只接管 `alert`：它没有返回值，可以安全地改成异步的页内弹窗。
/// `confirm` / `prompt` **不动** —— 它们要同步返回值，换成自定义弹窗会改变语义。
/// 注入脚本：把页面里的 JS 错误 / 未处理 Promise 拒绝 / 资源加载失败报回 Rust。
///
/// 桌面（Windows）上页面白屏时，除了「协议请求有没有 200」之外，
/// 「页面自己的 JS 有没有炸」是另一半证据 —— 而这个在桌面没有 devtools 时根本看不到。
const ERROR_JS: &str = r#"
(function () {
  if (window.__phircErrorShim) { return; }
  window.__phircErrorShim = true;
  function report(kind, detail) {
    try {
      window.ipc.postMessage(JSON.stringify({ cmd: 'log', args: { msg: kind + ': ' + detail } }));
    } catch (e) {}
  }
  window.addEventListener('error', function (e) {
    if (e && e.target && e.target !== window && e.target.tagName) {
      report('资源加载失败', e.target.tagName + ' ' + (e.target.src || e.target.href || ''));
      return;
    }
    report('JS 错误', (e && e.message) + ' @ ' + (e && e.filename) + ':' + (e && e.lineno));
  }, true);
  window.addEventListener('unhandledrejection', function (e) {
    report('Promise 拒绝', e && e.reason);
  });
  document.addEventListener('DOMContentLoaded', function () {
    report('页面就绪', location.href + ' body 子元素=' + (document.body ? document.body.children.length : -1));
  });
})();
"#;

const ALERT_JS: &str = r#"
(function () {
  if (window.__phircAlertShim) { return; }
  window.__phircAlertShim = true;

  var nativeAlert = window.alert;

  function box() {
    var b = document.getElementById('__phirc_alert');
    if (b) { return b; }
    b = document.createElement('div');
    b.id = '__phirc_alert';
    b.style.cssText = [
      'position:fixed', 'left:50%', 'top:50%', 'transform:translate(-50%,-50%)',
      'z-index:2147483646', 'display:none', 'min-width:260px', 'max-width:72vw',
      'background:#1b1b20', 'color:#fff',
      'border:1px solid rgba(120,240,255,.45)', 'border-radius:12px',
      'padding:20px 24px', 'box-shadow:0 18px 50px rgba(0,0,0,.75)',
      'font-size:15px', 'line-height:1.7', 'white-space:pre-wrap',
      'word-break:break-word', 'text-align:center'
    ].join(';') + ';';

    var text = document.createElement('div');
    text.id = '__phirc_alert_text';
    b.appendChild(text);

    var btn = document.createElement('button');
    btn.textContent = '知道了';
    btn.style.cssText = 'margin-top:16px;font-family:inherit;font-size:14px;cursor:pointer;' +
      'color:#a8f4ff;background:rgba(56,226,255,.14);border:1px solid rgba(56,226,255,.4);' +
      'border-radius:8px;padding:7px 22px;';
    btn.addEventListener('click', function () { b.style.display = 'none'; });
    b.appendChild(btn);

    document.body.appendChild(b);
    return b;
  }

  window.alert = function (msg) {
    try {
      var b = box();
      b.querySelector('#__phirc_alert_text').textContent =
        (msg === undefined || msg === null) ? '' : String(msg);
      b.style.display = 'block';
      try { if (window.phirc) { window.phirc.log('[alert] ' + msg); } } catch (e) {}
    } catch (e) {
      // 兜底失败就退回原生（总比什么都不弹强）
      try { nativeAlert.call(window, msg); } catch (e2) {}
    }
  };
})();
"#;

/// 调试用：把每次点击命中的元素显示在左下角（`PHIRC_WEBUI_DEBUG=1` 才注入）。
const DEBUG_JS: &str = r#"
(function () {
  var tip = null;
  function show(text) {
    if (!tip) {
      tip = document.createElement('div');
      tip.style.cssText = 'position:fixed;left:8px;bottom:8px;z-index:2147483647;' +
        'background:rgba(0,0,0,.82);color:#8ff;font:12px/1.5 monospace;' +
        'padding:4px 8px;border-radius:6px;pointer-events:none;max-width:90vw;';
      document.body.appendChild(tip);
    }
    tip.textContent = text;
  }
  document.addEventListener('click', function (e) {
    var t = e.target;
    if (!t) { return; }
    var cls = (typeof t.className === 'string' && t.className) ? '.' + t.className.trim().split(/\s+/).join('.') : '';
    show('click → ' + t.tagName + (t.id ? '#' + t.id : '') + cls);
    try { if (window.phirc) { window.phirc.log('click ' + t.tagName + (t.id ? '#' + t.id : '') + cls); } } catch (err) {}
  }, true);
})();
"#;

// ============================================================
//  全屏触点（注入，不改 HTML）
// ============================================================

/// 注入脚本：在开始页铺一层**全屏透明触点**，点一下进收藏夹（`tripsUide.html`）。
///
/// 为什么用注入而不是改 HTML：`index.html` 要逐字节照搬。
///
/// 优化点（这就是「触点优化」）：
/// * **只在开始页装**：本脚本每次页面加载都会跑，`tripsUide.html` 上不能再铺一层，
///   否则收藏夹自己的卡片/按钮会被盖住点不动；
/// * **单击 / 双击消歧**：单击延时 260ms 再跳，期间来了第二次点击就取消 ——
///   这样 `index.html` 原有的「双击左侧 40% 换曲绘」依然可用（不牺牲原功能）；
/// * **触摸兜底**：`touch-action: manipulation` 去掉移动端 300ms 双击缩放延迟；
///   `touchend` 主动触发，并用时间窗忽略随后补发的 `click`，避免一次点击跳两次；
/// * **捕获阶段监听**：页面里任何 `stopPropagation` 都吞不掉触点；
/// * **`Escape` 让位**：把界面还给 macroquad 原生 UI，免得 WebView 把游戏卡死。
const HOTSPOT_JS: &str = r#"
(function () {
  if (window.__phircHotspotInstalled) { return; }
  window.__phircHotspotInstalled = true;

  var TARGET = 'tripsUide.html';   // 收藏夹
  var CLICK_DELAY = 260;           // 单击→跳转 延时，用于和双击消歧

  // 只在开始页铺触点：phirc://web/index.html、phirc://web/、phirc://web
  //
  // ⚠️ 千万别写成 `/(\/index(\.html)?)?$/` —— 整个分组可选 + `$` 锚点会让它对
  // **任何路径**都返回 true，于是收藏夹页也会被铺一层触点、卡片按钮全点不动。
  function isStartPage() {
    var p = location.pathname || '';
    if (p === '') { return false; }
    if (p.charAt(p.length - 1) === '/') { return true; }   // 目录 → 会取 index.html
    return /\/index(\.html)?$/.test(p);                    // …/index 或 …/index.html
  }

  function install() {
    if (!isStartPage()) { return; }
    if (!document.body) { return; }
    if (document.getElementById('__phirc_hotspot')) { return; }

    var el = document.createElement('div');
    el.id = '__phirc_hotspot';
    el.setAttribute('aria-hidden', 'true');
    el.style.cssText = [
      'position:fixed',
      'left:0', 'top:0', 'right:0', 'bottom:0',
      'width:100%', 'height:100%',
      'z-index:2147483647',
      'background:transparent',
      'cursor:pointer',
      'touch-action:manipulation',
      '-webkit-tap-highlight-color:transparent',
      '-webkit-user-select:none', 'user-select:none',
      'display:block'
    ].join(';') + ';';

    var timer = null;
    var lastTouch = 0;

    function go() {
      timer = null;
      try { location.href = TARGET; } catch (e) {}
    }

    function activate() {
      if (timer !== null) {
        // 第二次点击 = 双击 → 取消导航，交回页面自己的 dblclick 逻辑
        clearTimeout(timer);
        timer = null;
        return;
      }
      timer = setTimeout(go, CLICK_DELAY);
    }

    // 触摸：主动触发；随后 WebView 补发的 click 用时间窗忽略掉
    el.addEventListener('touchend', function () {
      lastTouch = Date.now();
      activate();
    }, { capture: true, passive: true });

    // 鼠标 / 触摸补发：捕获阶段，保证不被页面 stopPropagation 吞掉
    el.addEventListener('click', function () {
      if (Date.now() - lastTouch < 700) { return; }
      activate();
    }, true);

    document.body.appendChild(el);

    // Escape：让出界面，回到 macroquad 原生 UI
    window.addEventListener('keydown', function (e) {
      if (e.key === 'Escape' || e.keyCode === 27) {
        try { window.ipc.postMessage(JSON.stringify({ cmd: 'hide' })); } catch (err) {}
      }
    }, true);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', install);
  } else {
    install();
  }
})();
"#;

// ============================================================
//  设置页导航（注入）：返回按钮 + Esc
// ============================================================

/// 注入脚本：在 Setting.html 上，把左上角返回按钮和 Esc 键接到「返回上一页」。
///
/// 逻辑：优先 WebView 历史回退（从哪进来回哪去）；无历史则回开始页。
///
/// 只在 Setting.html 上生效；`.back-btn-wrapper` 本就是 Setting.html 的元素，
/// 全局 Esc 监听也用 `isSettingsPage()` 守着，不会误伤别的页面。
const SETTINGS_NAV_JS: &str = r#"
(function () {
  function isSettingsPage() {
    var p = location.pathname || '';
    return p === '/Setting.html' || p.endsWith('/Setting.html') ||
           p === '/Setting' || p.endsWith('/Setting');
  }
  function back() {
    // 返回**上一页**：优先 WebView 历史回退（从哪进来回哪去，比如从收藏夹进设置就回收藏夹）；
    // 没有历史（例如从原生菜单直接打开、或单独启动到本页）就回开始页。
    // 不再走 Rust `hide`（那条会导航到开始页，不符合「返回上一页」）。
    try {
      if (window.history && history.length > 1) { history.back(); }
      else { location.href = 'index.html'; }
    } catch (e) { try { location.href = 'index.html'; } catch (e2) {} }
  }
  window.addEventListener('keydown', function (e) {
    if ((e.key === 'Escape' || e.keyCode === 27) && isSettingsPage()) { back(); }
  }, true);
  function bind() {
    if (!isSettingsPage()) { return; }
    var el = document.querySelector('.back-btn-wrapper');
    if (el && !el.__phircBack) {
      el.__phircBack = true;
      el.style.cursor = 'pointer';
      el.addEventListener('click', back);
    }
  }
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', bind);
    } else { bind(); }
})();
"#;

/// 注入脚本：在 ResPack.html 上，把左上角返回按钮和 Esc 键接到「返回上一页」。
///
/// 逻辑同 Setting.html：优先 WebView 历史回退；无历史回开始页。
const RESPACK_NAV_JS: &str = r#"
(function () {
  function isResPackPage() {
    var p = location.pathname || '';
    return p === '/ResPack.html' || p.endsWith('/ResPack.html') ||
           p === '/ResPack' || p.endsWith('/ResPack');
  }
  function back() {
    // 返回**上一页**：优先 WebView 历史回退（从哪进来回哪去，比如从收藏夹进设置就回收藏夹）；
    // 没有历史（例如从原生菜单直接打开、或单独启动到本页）就回开始页。
    // 不再走 Rust `hide`（那条会导航到开始页，不符合「返回上一页」）。
    try {
      if (window.history && history.length > 1) { history.back(); }
      else { location.href = 'index.html'; }
    } catch (e) { try { location.href = 'index.html'; } catch (e2) {} }
  }
  window.addEventListener('keydown', function (e) {
    if ((e.key === 'Escape' || e.keyCode === 27) && isResPackPage()) { back(); }
  }, true);
  function bind() {
    if (!isResPackPage()) { return; }
    var el = document.querySelector('.back-btn-wrapper');
    if (el && !el.__phircBack) {
      el.__phircBack = true;
      el.style.cursor = 'pointer';
      el.addEventListener('click', back);
    }
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bind);
  } else { bind(); }
})();
"#;

// ============================================================
//  BP 赛事主持入口（浮动按钮 + bpHost.html 跳板页）
// ============================================================

/// 注入脚本：给 phirc:// 页面挂左下角「BP」浮动入口钮（连 bp-host 单文件服务器开的对局）。
///
/// click → `bpHost.html`（地址簿跳板页：记录主机地址 + iframe 全屏嵌入托管前端；
/// iframe 内 origin = 主机地址 → `/api/status` 与 `/ws` 同源原生可用，**bp-host 前端零改动**）。
///
/// 守卫：
/// * 仅 `phirc://` 协议页面挂载——WebView2 的 initialization script 对 iframe 同样执行，
///   http:// 主机页里会重复跑一遍，协议守卫天然挡掉（那里有 bp 自己的导航）；
/// * `bpHost.html` 自身不挂（页面已有左上常驻返回钮，避免双钮重叠）。
///
/// ⚠️ z-index 必须**等于** `HOTSPOT_JS` 全屏触点的 `2147483647`（不能低于）：
/// 开始页的触点是全屏透明层，BP 钮若在这个层级之下（首版 2147483646），点击会先落进
/// 触点 → 被 260ms 单击消歧送去 `tripsUide.html`，**BP 入口在开始页整页失效**。
/// 同层级时按 DOM 顺序取顶层：注入链里 BP_NAV_JS 在 HOTSPOT_JS 之后执行，后 append 居上。
const BP_NAV_JS: &str = r#"
(function () {
  if (window.__phircBpNavInstalled) { return; }
  window.__phircBpNavInstalled = true;

  function isPhircTopPage() {
    if (location.protocol !== 'phirc:') { return false; }
    var p = location.pathname || '';
    return !/(^|\/)bpHost(\.html)?$/.test(p);
  }

  function install() {
    if (!isPhircTopPage()) { return; }
    if (!document.body) { return; }
    if (document.getElementById('__phirc_bp_nav')) { return; }
    var b = document.createElement('button');
    b.id = '__phirc_bp_nav';
    b.title = 'BP 赛事主持';
    b.textContent = 'BP';
    b.style.cssText = [
      'position:fixed', 'left:18px', 'bottom:20px', 'z-index:2147483647',
      'width:52px', 'height:52px', 'border-radius:999px', 'padding:0',
      'border:1px solid rgba(255,255,255,0.22)',
      'background:rgba(77,121,255,0.75)', 'color:#ffffff',
      'font:bold 13px/1 "Saira","Microsoft YaHei",sans-serif',
      'letter-spacing:1px', 'cursor:pointer',
      'backdrop-filter:blur(24px)', '-webkit-backdrop-filter:blur(24px)',
      'touch-action:manipulation', '-webkit-tap-highlight-color:transparent'
    ].join(';') + ';';
    b.addEventListener('click', function (e) {
      // 不给宿主页（如开始页触点的 260ms 消歧）任何双跳转机会
      if (e && e.stopPropagation) { e.stopPropagation(); }
      location.href = 'bpHost.html';
    });
    document.body.appendChild(b);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', install);
  } else { install(); }
})();
"#;

// ============================================================
//  Windows 宿主
// ============================================================

/// 把 macroquad 窗口的 HWND 包装成 raw-window-handle 0.6 的 `HasWindowHandle`，
/// 供 wry 0.44 的 `WebViewBuilder::new_as_child` 作为父窗口挂子 WebView。
#[cfg(target_os = "windows")]
struct HwndHandle(isize);

#[cfg(target_os = "windows")]
impl raw_window_handle::HasWindowHandle for HwndHandle {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        use raw_window_handle::{RawWindowHandle, Win32WindowHandle, WindowHandle};
        // raw-window-handle 0.6：Win32 句柄用 NonZeroIsize 承载；借用立即结束，可安全返回
        let hwnd = std::num::NonZeroIsize::new(self.0).expect("invalid game HWND");
        let handle = RawWindowHandle::Win32(Win32WindowHandle::new(hwnd));
        Ok(unsafe { WindowHandle::borrow_raw(handle) })
    }
}

#[cfg(target_os = "windows")]
impl raw_window_handle::HasDisplayHandle for HwndHandle {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        // Windows 没有「display」概念；rfd 的 set_parent 只要 trait 满足，实际只用 window handle
        Ok(raw_window_handle::DisplayHandle::windows())
    }
}

/// 带 owner 的文件选择框（Windows）：对话框归游戏窗口所有，不会被全屏窗口挡住。
#[cfg(target_os = "windows")]
pub fn pick_asset_with_parent(kind: &str) -> Option<String> {
    let mut dlg = rfd::FileDialog::new();
    dlg = match kind {
        "music" => dlg.set_title("选择音乐文件").add_filter("音乐", &["mp3", "ogg", "wav", "flac", "m4a", "aac"]),
        _ => dlg.set_title("选择曲绘图片").add_filter("图片", &["png", "jpg", "jpeg", "webp", "bmp"]),
    };
    if let Some(&hwnd) = GAME_HWND.get() {
        if hwnd != 0 {
            dlg = dlg.set_parent(&HwndHandle(hwnd));
        }
    }
    dlg.pick_file().map(|it| it.display().to_string())
}

// `HWND SetFocus(HWND)` —— 参数/返回值都是 `HWND`，Rust 侧用 `isize`（ABI 一致）。
#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn SetFocus(hwnd: isize) -> isize;
}

/// 非 wry 平台（安卓 / iOS）的 `window.ipc` 垫片。
///
/// `BRIDGE_JS` 的 `call()` 依赖 `window.ipc.postMessage`（wry 在 Windows 上原生提供），
/// 移动端没有 wry —— 必须在桥脚本**之前**把 `window.ipc` 接到平台原生桥上：
/// * 安卓：`WebViewHost.java` 的 `addJavascriptInterface(..., "phiraAndroid")`；
/// * iOS：`ios/webview.m` 的消息处理器（`messageHandlers.phira`）。
#[cfg(any(target_os = "android", target_os = "ios"))]
const IPC_SHIM_JS_TPL: &str = r#"(function () {
  if (window.ipc) { return; }
  window.ipc = {
    postMessage: function (msg) {
      try {
        __PHIRC_BRIDGE__.postMessage(String(msg));
      } catch (e) { /* WebView 尚未就绪 */ }
    }
  };
})();
"#;

/// 安卓版注入脚本：先装 `window.ipc` → `phiraAndroid` 垫片。
#[cfg(target_os = "android")]
fn init_scripts() -> String {
    format!(
        "{}\n{}",
        IPC_SHIM_JS_TPL.replace("__PHIRC_BRIDGE__", "window.phiraAndroid"),
        init_scripts_base()
    )
}

/// iOS 版注入脚本：先装 `window.ipc` → `messageHandlers.phira` 垫片。
#[cfg(target_os = "ios")]
fn init_scripts() -> String {
    format!(
        "{}\n{}",
        IPC_SHIM_JS_TPL.replace("__PHIRC_BRIDGE__", "window.webkit.messageHandlers.phira"),
        init_scripts_base()
    )
}

/// 组合注入脚本（桥 → alert 兜底 →（可选）点击调试 → 开始页触点 → 设置/资源包返回 → BP 浮动入口）。
///
/// Windows 的 `with_initialization_script` 与安卓的 `onPageFinished` 注入共用这一份；
/// DEBUG_JS 只在 `PHIRC_WEBUI_DEBUG=1` 时注入（默认不带左下常驻点击提示）。
fn init_scripts_base() -> String {
    let debug_js = if std::env::var("PHIRC_WEBUI_DEBUG").is_ok() { DEBUG_JS } else { "" };
    format!("{BRIDGE_JS}\n{ERROR_JS}\n{ALERT_JS}\n{debug_js}\n{HOTSPOT_JS}\n{SETTINGS_NAV_JS}\n{RESPACK_NAV_JS}\n{BP_NAV_JS}")
}

/// Windows：垫片不需要（`window.ipc` 由 wry 原生提供）。
#[cfg(target_os = "windows")]
fn init_scripts() -> String {
    init_scripts_base()
}

/// 启动 WebView 线程。
#[cfg(target_os = "windows")]
fn start() -> Result<Host> {
    use wry::{http::Request, WebViewBuilder};

    // 1) 游戏窗口 HWND（窗口标题在 lib.rs 设为 "Phira"）
    let hwnd = find_game_hwnd().ok_or_else(|| anyhow!("找不到游戏窗口 HWND（标题应为 \"Phira\"）"))?;
    let _ = GAME_HWND.set(hwnd);
    let parent = HwndHandle(hwnd);

    // 窗口若一开始就是全屏，miniquad 创建时就没给 WS_CLIPCHILDREN → 先补上，
    // 免得 WebView 一建出来就被游戏画面盖住（这里只动 style，不碰尺寸）。
    ensure_clip_children(hwnd as windows_sys::Win32::Foundation::HWND);

    // 2) 命令通道：主线程 → WebView 线程；IPC 回包也用同一条
    let (cmd_tx, cmd_rx) = mpsc::channel::<WebCmd>();
    let ipc_tx = cmd_tx.clone();

    // 3) 显式铺满客户区（默认 bounds 只有 200×200）
    let (w, h) = parent_client_logical_size(hwnd).unwrap_or((973.0, 608.0));
    let bounds = wry::Rect {
        position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
        size: wry::dpi::LogicalSize::new(w, h).into(),
    };

    // 3.5) 多比例适配：页面按设计分辨率排版，再整体等比缩放到窗口
    let design = design_size();
    let zoom = fit_zoom(w, h, design);
    match design {
        Some((dw, dh)) => tracing::info!("[webui] 设计分辨率 {dw}×{dh}，窗口 {w}×{h} → 缩放 {zoom:.3}"),
        None => tracing::info!("[webui] 已关闭等比缩放（PHIRC_WEBUI_FIT=fill）"),
    }

    // 4) 独立线程建 WebView，并跑「泵消息 + 收命令」循环
    std::thread::Builder::new()
        .name("phirc-webview".into())
        .spawn(move || {
            let entry = entry_url();
            // 注入脚本合成一段（`with_initialization_script` 只存一份，合成最省事）。
            let init_js = init_scripts();
            let builder = WebViewBuilder::new_as_child(&parent)
                .with_bounds(bounds)
                // 【关键】必须显式抢键盘焦点。WebView2 的子窗口默认**不**自动获得焦点，
                // 而键盘事件只投递给有焦点的窗口 —— 拿不到焦点的话：
                //   * 注入脚本里监听 `Escape` 的「让位」永远不触发；
                //   * 页面里的 `<input>`（比如收藏夹的新建卡片弹窗）一个字也打不进。
                .with_focused(true)
                // F12 开 DevTools（调试手写 HTML / WebGL 用；正式发布想关掉就把这行删了）
                .with_devtools(true)
                .with_asynchronous_custom_protocol("phirc".to_string(), |request: Request<Vec<u8>>, responder| {
                    responder.respond(protocol_response(&request));
                })
                .with_ipc_handler(move |request: Request<String>| {
                    handle_ipc(request.body(), &ipc_tx);
                })
                .with_initialization_script(&init_js)
                .with_url(entry.clone());

            let webview: wry::WebView = match builder.build() {
                Ok(w) => w,
                Err(e) => {
                    // WebView2 运行时缺失 / 创建失败：保持原生 UI，不要黑屏
                    tracing::error!("[webui] WebView 构建失败（WebView2 运行时是否已安装？）：{e:?}");
                    return;
                }
            };
            if let Err(e) = webview.zoom(zoom) {
                tracing::error!("[webui] zoom({zoom:.3}) 失败：{e:?}");
            }
            tracing::info!("[webui] WebView 创建成功：{entry}（{w}×{h} 逻辑像素，zoom {zoom:.3}）");
            crate::diag(&format!(
                "[webui] 启动：hwnd=0x{hwnd:x} 客户区={w}x{h} zoom={zoom:.3} entry={entry}"
            ));
            crate::diag(&format!("[webui] 启动子窗口：{}", dump_child_windows(hwnd)));

            // 【关键】绝不能用阻塞的 `recv()` 等在这里：
            //   - wry 源码里没有任何 PeekMessage/GetMessage/DispatchMessage，泵消息是宿主的责任；
            //   - WebView2 的容器窗口是 `WS_CHILD` 且建在本线程上，不泵消息就永远不绘制 → 黑屏；
            //   - 主线程一旦 SendMessage 到这个跨线程子窗口，而本线程不泵消息，
            //     主线程就一直等 → 游戏窗口卡死。
            //
            // 另外每 ~200ms 对一次客户区尺寸：bounds 只在创建时设过一次，
            // 不主动同步的话，拖动 / 最大化窗口后 WebView 会停在初始大小。
            let mut last_size = (w, h);
            let mut last_zoom = zoom;
            let mut keepalive: u32 = 0;
            loop {
                unsafe { pump_win32_messages() };

                // ⚠️ **每一轮都查**（原来是 `tick % 25`，约 200ms 一次）。
                // 换全屏 / 拖窗口时那 200ms 里 WebView 还停在旧尺寸，露出来的就是
                // 底下 macroquad 的原生界面（用户实测「还是有老界面」）。
                // `GetClientRect` 极便宜，直接每轮比对即可。
                {
                    if let Some((cw, ch)) = parent_client_logical_size(hwnd) {
                        if (cw - last_size.0).abs() > 0.5 || (ch - last_size.1).abs() > 0.5 {
                            last_size = (cw, ch);
                            let rect = wry::Rect {
                                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                                size: wry::dpi::LogicalSize::new(cw, ch).into(),
                            };
                            match webview.set_bounds(rect) {
                                // 拖动窗口时会连续触发，用 debug 免得刷屏
                                Ok(()) => {
                                    tracing::debug!("[webui] WebView 跟随窗口：{cw}×{ch}");
                                    crate::diag(&format!("[webui] WebView 跟随窗口：{cw}×{ch}"));
                                }
                                Err(e) => tracing::error!("[webui] set_bounds({cw}×{ch}) 失败：{e:?}"),
                            }
                            // 窗口比例变了，等比缩放的系数也要跟着变
                            let z = fit_zoom(cw, ch, design);
                            if (z - last_zoom).abs() > 1e-4 {
                                last_zoom = z;
                                if let Err(e) = webview.zoom(z) {
                                    tracing::error!("[webui] zoom({z:.3}) 失败：{e:?}");
                                }
                            }
                        }
                    }
                }

                // 每 ~1s 幂等重断言一次：尺寸 + 可见性 + 窗口 style。
                // WebView2 的容器在父窗口 style / 尺寸变化后**有可能自己缩回去或隐藏**，
                // 一旦那样，露出来的就是底下 macroquad 的原生界面（或一片空白）。
                // 重新 set_bounds + set_visible(true) 是幂等的，代价可忽略。
                // 顺带每轮再确认一次窗口 style（剥掉 WS_POPUP、保住 WS_CLIPCHILDREN）——
                // 万一有谁把窗口改回 WS_POPUP，立刻纠正，避免全屏白屏复发。
                {
                    #[cfg(target_os = "windows")]
                    ensure_clip_children(hwnd as windows_sys::Win32::Foundation::HWND);
                    keepalive = keepalive.wrapping_add(1);
                    if keepalive.is_multiple_of(120) {
                        if let Some((cw, ch)) = parent_client_logical_size(hwnd) {
                            let rect = wry::Rect {
                                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                                size: wry::dpi::LogicalSize::new(cw, ch).into(),
                            };
                            let _ = webview.set_bounds(rect);
                            if webview_visible() {
                                let _ = webview.set_visible(true);
                            }
                            crate::diag(&format!(
                                "[webui] keepalive 客户区={cw}x{ch} 期望可见={} 子窗口：{}",
                                webview_visible(),
                                dump_child_windows(hwnd)
                            ));
                        }
                    }
                    // 看门狗：跑满 ~6s 还没收到页面任何 IPC → 页面根本没起来（白屏）。
                    // 这时把 WebView 藏掉并标记不可用，让原生界面兜底 ——
                    // 否则用户面对的就是一片白，连设置都进不去。
                    if keepalive == 750 && !PAGE_ALIVE.load(Ordering::Relaxed) {
                        WEBVIEW_DEAD.store(true, Ordering::Relaxed);
                        WEBVIEW_VISIBLE.store(false, Ordering::Relaxed);
                        let _ = webview.set_visible(false);
                        crate::diag(
                            "[webui] 页面 6s 内没有任何 IPC → 判定 WebView 不可用，已隐藏并回退原生界面",
                        );
                    }
                }

                match cmd_rx.recv_timeout(std::time::Duration::from_millis(8)) {
                    Ok(WebCmd::Eval(js)) => {
                        let _ = webview.evaluate_script(&js);
                    }
                    Ok(WebCmd::Visible(v)) => {
                        if let Err(e) = webview.set_visible(v) {
                            tracing::error!("[webui] set_visible({v}) 失败：{e:?}");
                        }
                        if v {
                            // 重新显示时把键盘焦点收回页面（ShowWindow 不保证给焦点）
                            if let Err(e) = webview.focus() {
                                tracing::error!("[webui] focus() 失败：{e:?}");
                            }
                        } else {
                            // 隐藏后把键盘焦点还给游戏窗口，否则游戏内自绘输入框打不进字
                            unsafe { SetFocus(hwnd) };
                        }
                    }
                    Ok(WebCmd::Navigate(url)) => {
                        // ⚠️ 自定义协议 `phirc://` 用 `webview.load_url` 在 `new_as_child`
                        // 子 WebView 下**不生效**（不触发 serve_asset、也不报错，表现为
                        // 「点了没反应」）——返回(show_start_page)、原生菜单进设置/资源包
                        // (open_settings/open_respack) 全中招。App 里所有能用的导航都是
                        // 页面内 `location.href`，所以这里改走客户端导航（用**相对路径**，
                        // 与开始页触点 / tripsUide 那条路的写法一致）。
                        let target = url.strip_prefix("phirc://web/").map(|p| p.to_owned()).unwrap_or(url);
                        let script = format!("window.location.href = {};", serde_json::to_string(&target).unwrap_or_default());
                        if let Err(e) = webview.evaluate_script(&script) {
                            tracing::error!("[webui] Navigate evaluate_script({target}) 失败：{e:?}");
                        }
                        // 跳转后把键盘焦点给页面（Esc / 输入框要靠它）
                        if let Err(e) = webview.focus() {
                            tracing::error!("[webui] focus() 失败：{e:?}");
                        }
                    }
                    Ok(WebCmd::Refresh) => {
                        // 父窗口 style / 尺寸刚变过，WebView2 的 DComp 表面可能失活。
                        // 重设 bounds（按当前真实客户区）+ 可见 + 焦点，逼它重新上屏。
                        if let Some((cw, ch)) = parent_client_logical_size(hwnd) {
                            let rect = wry::Rect {
                                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                                size: wry::dpi::LogicalSize::new(cw, ch).into(),
                            };
                            if let Err(e) = webview.set_bounds(rect) {
                                tracing::error!("[webui] Refresh set_bounds({cw}x{ch}) 失败：{e:?}");
                            }
                        }
                        if webview_visible() {
                            let _ = webview.set_visible(true);
                            if let Err(e) = webview.focus() {
                                tracing::error!("[webui] Refresh focus() 失败：{e:?}");
                            }
                        }
                        crate::diag("[webui] 强制刷新 WebView 表面（重设 bounds + 可见 + 焦点）");
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .map_err(|e| anyhow!("启动 WebView 线程失败：{e}"))?;

    Ok(Host { cmd_tx })
}

/// 泵一次本线程的 Win32 消息（把队列排空）。
///
/// WebView2 的绘制、以及「主线程 SendMessage 到跨线程子窗口」的应答都依赖这一步。
/// 没有它就会出现「黑屏 + 窗口卡死拖不动」。
///
/// # Safety
/// 只能在本线程（拥有 WebView 容器窗口的线程）调用。
#[cfg(target_os = "windows")]
unsafe fn pump_win32_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };

    // hwnd 传 NULL：泵本线程所有窗口的消息
    let mut msg: MSG = std::mem::zeroed();
    while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// 取游戏窗口客户区的**逻辑**尺寸（CSS 像素），供 `with_bounds` 使用。
///
/// `set_bounds` 收逻辑坐标，内部按窗口 DPI 换算成物理像素；而 `GetClientRect`
/// 返回物理像素，所以要反向除回 DPI 缩放。
#[cfg(target_os = "windows")]
fn parent_client_logical_size(hwnd: isize) -> Option<(f64, f64)> {
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;

    let hwnd = hwnd as HWND;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
        return None;
    }
    let phys_w = (rect.right - rect.left) as f64;
    let phys_h = (rect.bottom - rect.top) as f64;
    if phys_w <= 0.0 || phys_h <= 0.0 {
        return None;
    }

    // DPI → 缩放系数（96 = 100%）
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = if dpi > 0 { dpi as f64 / 96.0 } else { 1.0 };
    let scale = if scale > 0.0 { scale } else { 1.0 };

    Some((phys_w / scale, phys_h / scale))
}

/// 用窗口标题找 macroquad 窗口 HWND（标题在 lib.rs 设为 `"Phira"`）。
///
/// 带重试：万一窗口标题还没落地（时序问题），最多等 1 秒再放弃 ——
/// 正常情况下第一次就命中，不会真的等。
#[cfg(target_os = "windows")]
fn find_game_hwnd() -> Option<isize> {
    for attempt in 0..10 {
        if let Some(hwnd) = find_hwnd_by_title("Phira") {
            return Some(hwnd);
        }
        if attempt < 9 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    tracing::error!("[webui] 重试 10 次仍未找到标题为 \"Phira\" 的窗口");
    None
}

// ============================================================
//  安卓：JNI 宿主
//
//  WebView 由 Java 侧 `WebViewHost`（android/app/.../WebViewHost.java）创建并持有：
//   * 页面来自 APK `assets/kaichang/`，走 `file:///android_asset/` 加载；
//   * JS 桥：页面 `window.ipc.postMessage(json)` → Java `Bridge.post` →
//     [`webview_ipc`] → [`handle_ipc`]（与 Windows 完全同一条业务层）；
//   * 回包：[`handle_ipc`] 发 `WebCmd::Eval("window.__phircResolve(...)")` →
//     转发线程 → Java `WebViewHost.eval` → `webView.evaluateJavascript`。
//
//  命令通道与 Windows 共用同一个 `HOST`（业务层零改动），只是 `WebCmd`
//  的解释端从「wry 线程」换成这里的「JNI 转发线程」。
// ============================================================

/// Java 侧 `WebViewHost` 实例的全局引用（`nativeRegisterHost` 时存入）。
#[cfg(target_os = "android")]
static WEBVIEW_HOST: OnceLock<Global<JObject<'static>>> = OnceLock::new();

/// 安卓版 [`init_host`]：命令通道照旧进 `HOST`，转发线程把 `WebCmd` 翻成 JNI 调用。
#[cfg(target_os = "android")]
pub fn init_host() {
    if HOST.get().is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel::<WebCmd>();
    let _ = HOST.set(Host { cmd_tx: tx });
    let _ = std::thread::Builder::new()
        .name("phirc-webview-android".into())
        .spawn(move || forward_loop(rx));
    tracing::info!("[webui] 安卓 WebView 命令通道就绪（等待 Java 侧注册 WebViewHost）");
}

/// `WebCmd` → Java `WebViewHost` 实例方法（转发线程主体）。
#[cfg(target_os = "android")]
fn forward_loop(rx: mpsc::Receiver<WebCmd>) {
    use jni::{jni_sig, jni_str, objects::JValue, vm::JavaVM};

    while let Ok(cmd) = rx.recv() {
        let Some(vm) = JavaVM::singleton().ok() else {
            tracing::error!("[webui] JavaVM 未初始化，丢弃 WebView 命令");
            continue;
        };
        let Some(host) = WEBVIEW_HOST.get() else {
            tracing::warn!("[webui] WebViewHost 尚未注册（Java 侧未 ready），丢弃一条 WebView 命令");
            continue;
        };
        let host = host.as_ref();
        let _ = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            match cmd {
                WebCmd::Eval(js) => {
                    let s = env.new_string(&js)?;
                    env.call_method(host, jni_str!("eval"), jni_sig!("(Ljava/lang/String;)V"), &[(&s).into()])?;
                }
                WebCmd::Visible(v) => {
                    env.call_method(host, jni_str!("setVisible"), jni_sig!("(Z)V"), &[JValue::from(v)])?;
                }
                WebCmd::Navigate(url) => {
                    // 业务层发的是 `phirc://web/xxx`；安卓换成 assets 里的 file:// 路径
                    //（query 原样保留，页面里 `URLSearchParams` 照常能读）。
                    let page = url.strip_prefix("phirc://web/").unwrap_or(&url);
                    let target = format!("file:///android_asset/kaichang/{}", page.trim_start_matches('/'));
                    let s = env.new_string(&target)?;
                    env.call_method(host, jni_str!("loadPage"), jni_sig!("(Ljava/lang/String;)V"), &[(&s).into()])?;
                }
                WebCmd::Refresh => {
                    // 全屏切换等场景：重新断言可见性 + 焦点（Java 侧幂等）。
                    env.call_method(host, jni_str!("refresh"), jni_sig!("()V"), &[])?;
                }
            }
            Ok(())
        });
    }
}

/// Java `WebViewHost.init` 末尾调用：把实例交给 Rust（存全局引用）。
#[cfg(target_os = "android")]
#[export_name = "Java_org_flos_phira_modded_WebViewHost_nativeRegisterHost"]
extern "system" fn webview_register_host(mut env: EnvUnowned, _: JClass, host: JObject) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let g: Global<JObject<'static>> = env.new_global_ref(host)?;
        if WEBVIEW_HOST.set(g).is_err() {
            tracing::warn!("[webui] WebViewHost 重复注册，忽略");
        } else {
            tracing::info!("[webui] WebViewHost 已注册（安卓 WebView 链路就绪）");
        }
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// 页面 `window.ipc.postMessage(json)` → Java `Bridge.post` → 这里。
///
/// [`handle_ipc`] 可能做网络（账号登录等），绝不能堵 JNI 调用线程 → 丢线程跑。
#[cfg(target_os = "android")]
#[export_name = "Java_org_flos_phira_modded_WebViewHost_nativeIpc"]
extern "system" fn webview_ipc(mut env: EnvUnowned, _: JClass, json: JString) {
    let Some(body) = crate::read_java_string(&mut env, &json, "webview ipc") else { return };
    let Some(h) = HOST.get() else { return };
    let tx = h.cmd_tx.clone();
    std::thread::spawn(move || handle_ipc(&body, &tx));
}

/// 页面加载完成（Java `onPageStarted` / `onPageFinished` 都会调，幂等）：
/// 注入桥脚本 —— 与 Windows 的 `with_initialization_script` 同一份 [`init_scripts`]，
/// `BRIDGE_JS` 是直接赋值，重复注入无害。
#[cfg(target_os = "android")]
#[export_name = "Java_org_flos_phira_modded_WebViewHost_nativeReady"]
extern "system" fn webview_ready(_env: EnvUnowned, _: JClass) {
    let Some(h) = HOST.get() else { return };
    let _ = h.cmd_tx.send(WebCmd::Eval(init_scripts()));
}


/// 按精确标题找顶层窗口。
#[cfg(target_os = "windows")]
fn find_hwnd_by_title(title: &str) -> Option<isize> {
    use std::ffi::CString;
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowA;

    let title = CString::new(title).ok()?;
    // windows-sys 0.59 的 FindWindowA 参数是 PCSTR（*const u8）；CString::as_ptr() 是 *const i8。
    let hwnd = unsafe { FindWindowA(std::ptr::null::<u8>(), title.as_ptr() as *const u8) };
    if hwnd.is_null() {
        None
    } else {
        Some(hwnd as isize)
    }
}

// ============================================================
//  iOS：WKWebView 宿主
//
//  ObjC 胶水在 `phira/ios/webview.m`（build.rs 的 `build_ios_webview` 编译），
//  C 函数边界（与 motion.m / ime.m 同套路）。与安卓段结构完全平行：
//  命令通道进同一个 `HOST`，业务层（handle_ipc）零改动。
//
//  与安卓的关键差异：`phirc://` 由 WKURLSchemeHandler **原生支持**，
//  `WebCmd::Navigate` 的 URL 原样直传，不用翻成 file://。
// ============================================================

/// MIME 字符串 → `ios/webview.m` `mime_string()` 的枚举（两处必须同步改）。
#[cfg(target_os = "ios")]
fn mime_id_of(mime: &str) -> i32 {
    match mime {
        "text/html" => 0,
        "application/javascript" => 1,
        "text/css" => 2,
        "image/png" => 3,
        "image/jpeg" => 4,
        "image/webp" => 5,
        "font/ttf" => 6,
        "font/otf" => 7,
        "application/json" => 8,
        _ => 9,
    }
}

/// ObjC 侧导出的 C 函数（`ios/webview.m` 实现）。
#[cfg(target_os = "ios")]
extern "C" {
    fn phira_webui_create(init_js: *const std::os::raw::c_char, ipc_fn: extern "C" fn(*const std::os::raw::c_char));
    fn phira_webui_navigate(url: *const std::os::raw::c_char);
    fn phira_webui_set_visible(visible: std::os::raw::c_int);
    fn phira_webui_eval(js: *const std::os::raw::c_char);
    fn phira_webui_destroy();
}

/// iOS 版 [`init_host`]：命令通道照旧进 `HOST`，转发线程把 `WebCmd` 翻成 C 调用。
#[cfg(target_os = "ios")]
pub fn init_host() {
    use std::ffi::CString;
    use std::os::raw::c_char;

    if HOST.get().is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel::<WebCmd>();
    let _ = HOST.set(Host { cmd_tx: tx });

    // 建 WKWebView（ObjC 侧 dispatch_async 主队列，幂等；此刻 root view 已就绪）
    match CString::new(init_scripts()) {
        Ok(js) => unsafe { phira_webui_create(js.as_ptr(), phira_webui_ipc) },
        Err(_) => tracing::error!("[webui-iOS] init 脚本含 NUL，无法注入"),
    }

    let _ = std::thread::Builder::new()
        .name("phirc-webview-ios".into())
        .spawn(move || forward_loop_ios(rx));
    tracing::info!("[webui] iOS WKWebView 宿主已请求创建（等待导航）");
}

/// `WebCmd` → ObjC C 函数（iOS 转发线程主体）。
#[cfg(target_os = "ios")]
fn forward_loop_ios(rx: mpsc::Receiver<WebCmd>) {
    use std::ffi::CString;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            WebCmd::Eval(js) => match CString::new(js) {
                Ok(s) => unsafe { phira_webui_eval(s.as_ptr()) },
                Err(_) => tracing::warn!("[webui-iOS] Eval 脚本含 NUL，已丢弃"),
            },
            WebCmd::Visible(v) => unsafe { phira_webui_set_visible(v as std::os::raw::c_int) },
            WebCmd::Navigate(url) => {
                // `phirc://web/...` 原样直传（scheme handler 原生支持，无需翻译）
                match CString::new(url) {
                    Ok(s) => unsafe { phira_webui_navigate(s.as_ptr()) },
                    Err(_) => tracing::warn!("[webui-iOS] Navigate URL 含 NUL，已丢弃"),
                }
            }
            WebCmd::Refresh => unsafe { phira_webui_set_visible(1) },
        }
    }
}

/// 页面 → Rust：一条 IPC 消息（`ios/webview.m` 的消息处理器调用，WebKit 线程）。
///
/// [`handle_ipc`] 可能做网络（账号登录等），绝不能堵调用线程 → 丢线程跑。
#[cfg(target_os = "ios")]
#[no_mangle]
pub extern "C" fn phira_webui_ipc(json: *const std::os::raw::c_char) {
    use std::ffi::CStr;

    if json.is_null() {
        return;
    }
    let Ok(text) = unsafe { CStr::from_ptr(json) }.to_str() else { return };
    let Some(h) = HOST.get() else { return };
    let tx = h.cmd_tx.clone();
    std::thread::spawn(move || handle_ipc(text, &tx));
}

/// `phirc://` 资源请求（`ios/webview.m` 的 scheme handler 调用）。
///
/// `path_and_query` 形如 `/index.html` 或 `/local-cover?p=<base64url>`。
/// 成功返回 1 并填 `mime_id` / `data` / `len`；`data` 用 `malloc` 分配、
/// ObjC 侧 `free`（同一 libSystem 分配器，iOS 上安全）。
#[cfg(target_os = "ios")]
#[no_mangle]
pub extern "C" fn phira_webui_asset(
    path_and_query: *const std::os::raw::c_char,
    mime_id: *mut std::os::raw::c_int,
    data: *mut *const std::os::raw::c_void,
    len: *mut u64,
) -> std::os::raw::c_int {
    use std::ffi::CStr;
    use std::os::raw::c_void;

    unsafe extern "C" {
        fn malloc(size: usize) -> *mut c_void;
        fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
    }

    if path_and_query.is_null() {
        return 0;
    }
    let Ok(raw) = unsafe { CStr::from_ptr(path_and_query) }.to_str() else { return 0 };

    // path?query 分流（与 Windows 的 protocol_response 完全一致：
    // `respack/` 前缀 → 资源包贴图；`local-cover` → 曲绘 query；其余 → 静态资源）
    let (path, query) = match raw.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (raw, None),
    };
    let raw_path = path.trim_start_matches('/');
    let served = if raw_path.starts_with("respack/") {
        serve_respack(raw_path.strip_prefix("respack/").unwrap_or(raw_path))
    } else if raw_path == "local-cover" {
        // 缩略图由 `load_local_cover` 用 image::ImageFormat::Jpeg 生成，与 Windows
        // 的 protocol_response 一致用 image/jpeg。
        query.and_then(local_cover_query).map(|b| (b, "image/jpeg"))
    } else {
        serve_asset(path)
    };
    let Some((bytes, mime)) = served else { return 0 };

    unsafe {
        if !mime_id.is_null() {
            *mime_id = mime_id_of(mime);
        }
        if !data.is_null() && !len.is_null() {
            let buf = malloc(bytes.len().max(1));
            if buf.is_null() {
                return 0;
            }
            memcpy(buf, bytes.as_ptr() as *const c_void, bytes.len());
            *data = buf;
            *len = bytes.len() as u64;
        }
    }
    1
}
