fn git_stdout(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = std::str::from_utf8(&output.stdout).ok()?.trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

fn main() {
    dotenv_build::output(dotenv_build::Config::default()).unwrap();

    let git_dir = git_stdout(&["rev-parse", "--git-dir"]).unwrap_or_else(|| ".git".to_string());
    println!("cargo:rerun-if-changed={}/HEAD", git_dir);
    println!("cargo:rerun-if-changed={}/packed-refs", git_dir);

    if let Some(ref_path) = git_stdout(&["symbolic-ref", "-q", "HEAD"]) {
        println!("cargo:rerun-if-changed={}/{}", git_dir, ref_path);
    }

    let git_hash = git_stdout(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_HASH={}", git_hash);

    build_ios_motion();
    build_ios_ime();
    build_ios_webview();
    build_ios_video();
}

/// iOS：离线渲染的视频编码器（AVAssetWriter / VideoToolbox）。
///
/// 与 `build_ios_ime` 同套路：只在 iOS 目标下编，其它平台跳过。
/// 接口声明在 `phira/src/record.rs` 的 `ios_sink` 模块（C 边界：
/// `phira_video_start/append/finish/cancel/last_path`）。MRC，不开 ARC。
fn build_ios_video() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "ios" {
        return;
    }
    println!("cargo:rerun-if-changed=ios/video_encoder.m");
    cc::Build::new().file("ios/video_encoder.m").compile("phira_video_ios");
    println!("cargo:rustc-link-lib=framework=AVFoundation");
    println!("cargo:rustc-link-lib=framework=CoreVideo");
    println!("cargo:rustc-link-lib=framework=CoreMedia");
    println!("cargo:rustc-link-lib=framework=Foundation");
}

/// iOS：软键盘桥（隐藏 `UITextField`），用 Objective-C 包一层。
///
/// 与 [`build_ios_motion`] 同一套路：只在 iOS 目标下编，其它平台完全跳过，
/// 所以 Windows / Android 构建不受影响。UIKit / Foundation 都是系统框架，
/// 让链接器带上即可。（历史：这些代码原在 `ios/webui.m`，随 WKWebView 宿主一起；
/// WebView 界面移除后拆成 `ios/ime.m`，但软键盘通道必须保留。）
fn build_ios_ime() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "ios" {
        return;
    }
    println!("cargo:rerun-if-changed=ios/ime.m");
    cc::Build::new().file("ios/ime.m").compile("phira_ime_ios");
    println!("cargo:rustc-link-lib=framework=UIKit");
    println!("cargo:rustc-link-lib=framework=Foundation");
}

/// iOS：WebView 界面宿主（WKWebView + `phirc://` 自定义协议 scheme handler）。
///
/// 与 [`build_ios_ime`] 同套路：只在 iOS 目标下编，其它平台完全跳过。
/// C 边界函数（`phira_webui_create/navigate/set_visible/eval/destroy`）由
/// `phira/src/webui.rs` 的 iOS 段 extern 声明；Rust → ObjC 的两个回调
/// （`phira_webui_ipc` / `phira_webui_asset`）也在那段里 `#[no_mangle]` 导出。
/// MRC（不启用 ARC，与 motion.m / ime.m 一致，静态对象手动 release）。
fn build_ios_webview() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "ios" {
        return;
    }
    println!("cargo:rerun-if-changed=ios/webview.m");
    cc::Build::new().file("ios/webview.m").compile("phira_webview_ios");
    println!("cargo:rustc-link-lib=framework=WebKit");
    println!("cargo:rustc-link-lib=framework=UIKit");
    println!("cargo:rustc-link-lib=framework=Foundation");
}

/// iOS：把 CoreMotion 包一层极小的 Objective-C（读加速度计给「摇一摇再玩」用）。
///
/// 只在 iOS 目标下编，其它平台直接跳过，所以本地 Windows / Android 构建不受影响。
/// CoreMotion 是系统框架，用 `cargo:rustc-link-lib=framework=CoreMotion` 让链接器带上即可，
/// 不需要改 Xcode 工程（最终链接是 cargo 做的）。
fn build_ios_motion() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "ios" {
        return;
    }
    println!("cargo:rerun-if-changed=ios/motion.m");
    // 不启用 ARC：ARC 不允许「无可属限定符的全局可保持对象指针」，
    // motion.m 里的 static 管理器在 MRR 下自己释放，更省事。
    cc::Build::new().file("ios/motion.m").compile("phira_motion");
    println!("cargo:rustc-link-lib=framework=CoreMotion");
    println!("cargo:rustc-link-lib=framework=Foundation");
}
