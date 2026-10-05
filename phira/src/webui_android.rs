//! Android 上的原生 WebView 宿主（系统 `WebView` + JNI）。
//!
//! 与 Windows 侧的 [`crate::webui_windows`]（wry 挂 HWND）**职责一一对应**，
//! 但实现方式完全不同：
//!
//! | | Windows | Android |
//! |---|---|---|
//! | 宿主 | wry 子窗口挂在 macroquad HWND 上 | `WebView` 作为 View 叠在 `QuadSurface` 之上 |
//! | 资源 | `webui://` 自定义协议（wry 提供） | `https://webui.local/` + Java 侧内存资源表 |
//! | IPC | `window.ipc.postMessage` | `window.AndroidBridge.postMessage` |
//! | 执行 JS | `WebView::evaluate_script` | `WebView.evaluateJavascript` |
//! | 可见性 | `WebView::set_visible` | `View.setVisibility` |
//!
//! Java 侧实现：`android/app/src/main/java/org/flos/phira/modded/WebUIView.java`，
//! 由 `MainActivity` 的静态方法 `webuiCreate / webuiPutAsset / webuiLoadHome /
//! webuiSetVisible / webuiEval / webuiReady / webuiDestroy` 暴露给 JNI。
//!
//! # 为什么 Java 只管渲染、业务全在 Rust
//!
//! `webui.rs` 的 `handle_ipc` / `WebUiHost` 是两端共用的；本文件只做「JS 消息 → Rust 调用」
//! 和「Rust 指令 → Java 调用」的搬运，不做任何业务判断 —— 这样 Android 与 Windows
//! 的行为不会各自漂移。

use crate::webui::WebUiHost;
use jni::{
    jni_sig, jni_str,
    objects::{JClass, JObject},
    vm::JavaVM,
};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};

/// Java 侧承载静态入口的类描述符。
///
/// `jni_str!` 只接受**字面量**（`concat!` 拼不出来），所以整串写上；
/// 这个常量在 `find_class` 与所有 `call_static_method` 里共用，只在定义处出现一次。
const ACTIVITY_CLASS: &jni::strings::JNIStr = jni_str!("org/flos/phira/modded/MainActivity");

/// 只打印一次「JNI 调用失败」，避免把日志刷爆。
static WARNED_CREATE: AtomicBool = AtomicBool::new(false);
static WARNED_PUSH: AtomicBool = AtomicBool::new(false);
static WARNED_EVAL: AtomicBool = AtomicBool::new(false);

/// 在 JVM 上执行一段操作；失败只记一次日志（同一类错误不重复刷）。
fn with_env<F>(warned: &AtomicBool, what: &str, f: F)
where
    F: FnOnce(&mut jni::Env) -> jni::errors::Result<()>,
{
    // 注意：`attach_current_thread` 直接返回内层 `jni::errors::Result`（不是嵌套 Result）
    let err: Option<String> = match JavaVM::singleton() {
        Ok(vm) => match vm.attach_current_thread(|env| -> jni::errors::Result<()> { f(env) }) {
            Ok(()) => None,
            Err(e) => Some(format!("{e:?}")),
        },
        Err(e) => Some(format!("{e:?}")),
    };
    if let Some(err) = err {
        if !warned.swap(true, Ordering::Relaxed) {
            tracing::warn!("[webui-android] {what} 失败（同类错误后续不再打印）：{err}");
        }
    }
}

/// 调用 `MainActivity` 的一个静态方法。`f` 负责实际的方法调用。
fn call_static<F>(warned: &AtomicBool, what: &str, f: F)
where
    F: FnOnce(&mut jni::Env, JClass) -> jni::errors::Result<()>,
{
    with_env(warned, what, |env| {
        let class = env.find_class(ACTIVITY_CLASS)?;
        f(env, class)
    })
}

/// 拿到当前 Activity 的 `jobject`（`ndk_context` 里存的就是 MainActivity）。
fn activity<'a>(env: &mut jni::Env<'a>) -> JObject<'a> {
    let ctx = ndk_context::android_context().context();
    // SAFETY: `ndk_context` 由 miniquad 在 Activity 创建时写入，生命周期覆盖整个进程；
    // 这里只是包成一个受 `env` 约束的 JObject，不会由我们释放。
    unsafe { JObject::from_raw(env, ctx as _) }
}

/// Android WebView 宿主。所有方法都是「即发即忘」：真正的 Java 调用丢到 UI 线程执行，
/// 游戏线程不阻塞。
pub struct AndroidHost {
    /// 首页资源是否已经推给 Java（只推一次）。
    assets_pushed: AtomicBool,
    /// WebView 是否已经请求创建（只请求一次）。
    created: AtomicBool,
}

impl AndroidHost {
    /// 创建宿主。**不**立即建 WebView —— 那必须在 UI 线程做，而且此时全屏状态
    /// 可能还没稳定。真正的创建推迟到第一次 [`WebUiHost::show`]。
    pub fn new() -> anyhow::Result<Self> {
        if ndk_context::android_context().context().is_null() {
            anyhow::bail!("Android Activity 尚未就绪（ndk_context 为空）");
        }
        Ok(Self {
            assets_pushed: AtomicBool::new(false),
            created: AtomicBool::new(false),
        })
    }

    /// 请求 Java 在 UI 线程建 WebView（幂等）。
    fn ensure_created(&self) {
        if self.created.swap(true, Ordering::Relaxed) {
            return;
        }
        call_static(&WARNED_CREATE, "webuiCreate", |env, _class| {
            let act = activity(env);
            env.call_static_method(
                ACTIVITY_CLASS,
                jni_str!("webuiCreate"),
                jni_sig!("(Landroid/app/Activity;)V"),
                &[(&act).into()],
            )?;
            Ok(())
        });
        // Android 的 WebView 是在 UI 线程异步建的。**这里不能**置
        // `set_webui_embedded(true)` —— 那会让主场景立刻停止 Rust 渲染，而 WebView
        // 还没建好（甚至可能建失败），结果就是**纯黑屏**。
        //
        // 正确做法：等 Java 侧 `onPageFinished` 回调 `QuadNative.webuiOnReady()`
        // （见本文件的 `on_ready`）。在那之前 WebView 是叠在 SurfaceView 之上的
        // 原生 View，没它就显示下面的原生 UI，不会黑。
        tracing::info!("[webui-android] 已请求在 UI 线程创建 WebView");
    }

    /// 在页面里执行 JS（等价于 Windows 侧 wry 的 `evaluate_script`）。
    fn eval(js: &str) {
        with_env(&WARNED_EVAL, "webuiEval", |env| {
            let jstr = env.new_string(js)?;
            env.call_static_method(
                ACTIVITY_CLASS,
                jni_str!("webuiEval"),
                jni_sig!("(Ljava/lang/String;)V"),
                &[(&jstr).into()],
            )?;
            Ok(())
        });
    }

    /// 把内嵌的开始页资源全部推给 Java 的内存资源表（只推一次），然后让页面开始加载。
    ///
    /// Java 侧在 WebView 建好之前会先缓存在静态表里（`WebUIView.PENDING`），
    /// 所以这里**不需要**关心 WebView 有没有建好。
    fn push_assets_once(&self) {
        if self.assets_pushed.swap(true, Ordering::Relaxed) {
            return;
        }
        for path in crate::webui_assets::TEXT_ASSETS
            .iter()
            .copied()
            .chain(crate::webui_assets::BINARY_ASSETS.iter().copied())
        {
            let Some((data, _mime)) = crate::webui_assets::load_asset(path) else {
                tracing::warn!("[webui-android] 内嵌资源缺失：{path}");
                continue;
            };
            with_env(&WARNED_PUSH, "webuiPutAsset", |env| {
                let jpath = env.new_string(path)?;
                let jbytes = env.byte_array_from_slice(&data)?;
                env.call_static_method(
                    ACTIVITY_CLASS,
                    jni_str!("webuiPutAsset"),
                    jni_sig!("(Ljava/lang/String;[B)V"),
                    &[(&jpath).into(), (&jbytes).into()],
                )?;
                Ok(())
            });
        }
        // 资源灌完再让页面加载 —— 否则首屏会 404。
        call_static(&WARNED_PUSH, "webuiLoadHome", |env, _class| {
            env.call_static_method(
                ACTIVITY_CLASS,
                jni_str!("webuiLoadHome"),
                jni_sig!("()V"),
                &[],
            )?;
            Ok(())
        });
        tracing::info!("[webui-android] 开始页资源已推给 WebView，首页开始加载");
    }
}

impl WebUiHost for AndroidHost {
    fn show(&self) {
        // 顺序很重要：**先把资源推过去，再请求建 WebView**。
        // Java 那边现在只有一张静态资源表（不再有「先缓存后搬」的竞态），
        // 但这个顺序能保证页面一 loadUrl 就一定能取到 index.html，
        // 不依赖 UI 线程和游戏线程谁先跑。
        self.push_assets_once();
        self.ensure_created();
        call_static(&WARNED_CREATE, "webuiSetVisible(true)", |env, _class| {
            env.call_static_method(
                ACTIVITY_CLASS,
                jni_str!("webuiSetVisible"),
                jni_sig!("(Z)V"),
                &[jni::objects::JValue::Bool(true)],
            )?;
            Ok(())
        });
    }

    fn hide(&self) {
        call_static(&WARNED_CREATE, "webuiSetVisible(false)", |env, _class| {
            env.call_static_method(
                ACTIVITY_CLASS,
                jni_str!("webuiSetVisible"),
                jni_sig!("(Z)V"),
                &[jni::objects::JValue::Bool(false)],
            )?;
            Ok(())
        });
    }

    fn emit(&self, event: &str, payload: Value) {
        // 与 Windows 侧同形：调用页面里的 window.__phira_recv({event, payload})
        Self::eval(&format!(
            "window.__phira_recv({})",
            serde_json::json!({ "event": event, "payload": payload })
        ));
    }

    fn shutdown(&self) {
        call_static(&WARNED_CREATE, "webuiDestroy", |env, _class| {
            env.call_static_method(
                ACTIVITY_CLASS,
                jni_str!("webuiDestroy"),
                jni_sig!("()V"),
                &[],
            )?;
            Ok(())
        });
    }
}

/// JS → Rust 的 IPC 入口，由 Java 的 `QuadNative.webuiIpc(json)` 调用。
///
/// 处理完把回包 eval 回页面（与 Windows 侧 `with_ipc_handler` 的行为一致）。
pub fn on_ipc(json: String) {
    if let Some(resp) = crate::webui::handle_ipc(&json) {
        AndroidHost::eval(&format!("window.__phira_recv({resp})"));
    }
}

/// Java 侧确认「WebView 已建好且首页加载完成」，由 `QuadNative.webuiOnReady()` 调用。
///
/// **只有到这里才能置 `WEBUI_EMBEDDED`** —— 那一步会让主场景停止 Rust 渲染、
/// 把画面完全交给 WebView。提前置位 = 黑屏（WebView 还没内容而 Rust 又不画了）。
pub fn on_ready() {
    crate::webui::set_webui_embedded(true);
    tracing::info!("[webui-android] WebView 已就绪，界面交给 WebView");
}

/// Java 侧查询 WebView 是否已就绪（WebView 实例已建好）。
pub fn is_ready() -> bool {
    let mut ready = false;
    call_static(&WARNED_CREATE, "webuiReady", |env, _class| {
        let v = env.call_static_method(
            ACTIVITY_CLASS,
            jni_str!("webuiReady"),
            jni_sig!("()Z"),
            &[],
        )?;
        ready = v.z().unwrap_or(false);
        Ok(())
    });
    ready
}
