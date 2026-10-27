//! 谱面 → mp4 离线渲染（做法对齐 Phi-Recorder：离屏 MSAA 渲染 + 逐帧编码）。
//!
//! 流程：
//!   LoadingScene 载谱 → `Main`（`TimeManager::manual` 手动时间轴，非实时）
//!   → 每帧 `main.update()` / `main.render()` 画进 `MSRenderTarget`
//!   → `blit()` 解析多重采样 → **PBO 异步回读**（不阻塞 GPU）
//!   → 交给平台编码器（[`FrameSink`]）。
//!
//! 编码器按平台选：
//!   * Windows / Linux / macOS：外部 `ffmpeg`（libx264，CRF 可控，质量最好）
//!   * Android：`MediaCodec` 硬件 H.264 + `MediaMuxer`（`VideoEncoder.java`）
//!   * iOS：`AVAssetWriter` + VideoToolbox（`ios/video_encoder.m`）
//!
//! 用法：
//!   phira-main.exe --render <谱面目录> <输出.mp4>
//!                  [--width 1920] [--height 1080] [--fps 60] [--msaa 4] [--crf 18]
//!                  [--bitrate 16000] [--no-vflip]
//!
//! 移动端没有 ffmpeg.exe，`<输出.mp4>` 只取**文件名**，落在应用专属目录
//! （Android：`getExternalFilesDir`；iOS：Documents），日志里会打出完整路径。

use anyhow::{anyhow, bail, Context, Result};
use macroquad::prelude::*;
use macroquad::window::get_internal_gl;
use prpr::{
    config::Mods,
    core::MSRenderTarget,
    fs::{self},
    scene::{GameMode, GameScene, LoadingScene, Main},
    time::TimeManager,
    ui::{FontArc, TextPainter},
};

/// 离线渲染的片头时长（= `GameScene::BEFORE_TIME`）。
/// 游戏先播 0.7s 的开场（背景淡入 + 判定线出现），音乐在它之后才开始，
/// 所以音轨要整体延迟这么多，视频长度也要把它算进去。
const RENDER_INTRO: f64 = 1.2;
/// 尾判等待（`GameScene` 内部私有常量 WAIT_TIME）。
const RENDER_WAIT: f64 = 0.5;
/// 结束淡出（AFTER_TIME 0.7 + 0.3 的收尾余量）。
const RENDER_OUTRO: f64 = 1.0;
use sasa::AudioClip;
use std::{
    cell::RefCell,
    collections::VecDeque,
    path::{Path, PathBuf},
    rc::Rc,
};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use std::{io::Write, process::{Command, Stdio}};

#[derive(Clone, Debug, PartialEq)]
pub struct RenderOpts {
    /// 谱面目录（本地谱面路径）
    pub chart: String,
    /// 输出 mp4（桌面端是路径；移动端只取文件名）
    pub out: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// MSAA 采样数（1 = 关；4/8 = 抗锯齿）
    pub msaa: u32,
    /// 桌面端 x264 CRF（越小越清晰，18 接近视觉无损）
    pub crf: u32,
    /// 移动端硬件编码码率（kbps）；0 = 按分辨率自动
    pub bitrate_kbps: u32,
    /// 回读时上下翻转。**默认开**：GL 的 `glReadPixels` 自下而上，
    /// Phi Recorder 也恒定加 `-vf vflip`；个别实现方向相反时用 `--no-vflip` 关掉。
    pub vflip: bool,
    /// 导演模式运镜模板：`push` / `pull` / `orbit` / `sway` / `drift`；None = 固定机位
    pub camera: Option<String>,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            chart: String::new(),
            out: String::new(),
            width: 1920,
            height: 1080,
            fps: 60,
            msaa: 4,
            crf: 18,
            bitrate_kbps: 0,
            // GL 的 glReadPixels 是自下而上的；Phi Recorder 也恒定加 `-vf vflip`。
            // 想关掉用 `--no-vflip`。
            vflip: true,
            camera: None,
        }
    }
}

impl RenderOpts {
    /// 硬件编码器用的码率（kbps）：0 → 按 像素率 × 0.13 估算。
    pub fn bitrate_kbps(&self) -> u32 {
        if self.bitrate_kbps > 0 {
            return self.bitrate_kbps;
        }
        let px_rate = self.width as f64 * self.height as f64 * self.fps as f64;
        ((px_rate * 0.13) / 1000.).round().clamp(2_000., 120_000.) as u32
    }
}

// ============================================================
// 帧接收端（编码器抽象）
// ============================================================

pub trait FrameSink {
    /// 写一帧 RGBA（长度 = w*h*4）。`pts_us` 是显示时间戳（微秒）。
    fn write(&mut self, rgba: &[u8], pts_us: i64) -> Result<()>;
    /// 收尾（封装文件）。返回最终路径（拿不到时返回 None）。
    fn finish(self: Box<Self>) -> Result<Option<PathBuf>>;
}

// ============================================================
// 桌面：外部 ffmpeg
// ============================================================

#[cfg(not(any(target_os = "android", target_os = "ios")))]
struct FfmpegSink {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    out: PathBuf,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl FrameSink for FfmpegSink {
    fn write(&mut self, rgba: &[u8], _pts_us: i64) -> Result<()> {
        // rawvideo 输入按帧顺序排列，时间戳由 -r 决定，这里忽略 pts
        let stdin = self.stdin.as_mut().ok_or_else(|| anyhow!("ffmpeg stdin 已关闭"))?;
        stdin.write_all(rgba).context("写入 ffmpeg 失败（进程提前退出？）")
    }

    fn finish(mut self: Box<Self>) -> Result<Option<PathBuf>> {
        drop(self.stdin.take());
        let status = self.child.wait().context("等待 ffmpeg 结束失败")?;
        if !status.success() {
            bail!("ffmpeg 退出码 {status}（编码失败，输出文件可能不完整）");
        }
        Ok(Some(self.out.clone()))
    }
}

/// 找 ffmpeg：环境变量 → PATH → exe 同目录。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn find_ffmpeg() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PHIRA_FFMPEG") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join(exe_name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join(exe_name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// 组装 ffmpeg 参数（纯函数，便于测试）。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn build_args(opts: &RenderOpts, music: Option<&Path>) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-y".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "-s".into(),
        format!("{}x{}", opts.width, opts.height),
        "-r".into(),
        opts.fps.to_string(),
        "-i".into(),
        "pipe:0".into(),
    ];
    if let Some(m) = music {
        a.push("-i".into());
        a.push(m.to_string_lossy().into_owned());
    }
    a.push("-map".into());
    a.push("0:v:0".into());
    if music.is_some() {
        a.push("-map".into());
        a.push("1:a:0".into());
    }
    a.push("-c:v".into());
    a.push("libx264".into());
    a.push("-preset".into());
    a.push("medium".into()); // 离线渲染不吃实时性，用 medium 换质量
    a.push("-crf".into());
    a.push(opts.crf.to_string());
    // 颜色标记：不写的话播放器可能按 BT.601 解释，整体偏色（Phi Recorder 同样标 BT.709）
    for (k, v) in [("-colorspace", "bt709"), ("-color_primaries", "bt709"), ("-color_trc", "bt709")] {
        a.push(k.into());
        a.push(v.into());
    }
    // 方向已经在 PboRing 回读时翻过了（桌面/移动端同一套），这里不再 `-vf vflip`，
    // 否则会翻两次。
    a.push("-pix_fmt".into());
    a.push("yuv420p".into());
    if music.is_some() {
        a.push("-c:a".into());
        a.push("aac".into());
        a.push("-b:a".into());
        // Phi Recorder 用 320k；192k 在鼓点密集处能听出毛刺
        a.push("320k".into());
        // 音轨整体延迟到开场结束再响（对齐画面），尾部补静音到视频结束
        a.push("-af".into());
        a.push(format!("adelay={}:all=1,apad", (RENDER_INTRO * 1000.).round() as i64));
        a.push("-shortest".into());
    }
    // 放到文件头：手机/网页播放器不用先下完整个文件就能起播
    a.push("-movflags".into());
    a.push("+faststart".into());
    a.push(opts.out.clone());
    a
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn create_ffmpeg_sink(opts: &RenderOpts, music: Option<&Path>) -> Result<Box<dyn FrameSink>> {
    let ffmpeg = find_ffmpeg().ok_or_else(|| {
        anyhow!("找不到 ffmpeg —— 请装到 PATH，或把 ffmpeg.exe 放到游戏目录，或设环境变量 PHIRA_FFMPEG 指向它")
    })?;
    let args = build_args(opts, music);
    info!("render: ffmpeg {:?} {:?}", ffmpeg, args);
    let mut child = Command::new(&ffmpeg)
        .args(&args)
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("启动 ffmpeg 失败：{ffmpeg:?}"))?;
    let stdin = child.stdin.take().ok_or_else(|| anyhow!("拿不到 ffmpeg 的 stdin"))?;
    Ok(Box::new(FfmpegSink {
        child,
        stdin: Some(stdin),
        out: PathBuf::from(&opts.out),
    }))
}

// ============================================================
// Android：MediaCodec + MediaMuxer（见 VideoEncoder.java）
// ============================================================

#[cfg(target_os = "android")]
mod android_sink {
    use super::*;
    use jni::{jni_sig, jni_str, objects::JObject, vm::JavaVM, JValue};

    const CLASS: &str = "org/flos/phira/modded/VideoEncoder";

    pub struct AndroidSink {
        w: usize,
        h: usize,
        nv12: Vec<u8>,
        frames: u64,
        name: String,
    }

    fn with_env<T>(f: impl FnOnce(&mut jni::Env) -> jni::errors::Result<T>) -> Result<T> {
        JavaVM::singleton()
            .map_err(|e| anyhow!("Android Java VM 不可用：{e}"))?
            .attach_current_thread(f)
            .map_err(|e| anyhow!("JNI 调用失败：{e}"))
    }

    impl AndroidSink {
        pub fn new(
            name: &str,
            w: u32,
            h: u32,
            fps: u32,
            bitrate_kbps: u32,
            music: Option<&Path>,
            delay_us: i64,
        ) -> Result<Self> {
            let music = music.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let ok = with_env(|env| -> jni::errors::Result<bool> {
                let activity = unsafe { JObject::from_raw(env, ndk_context::android_context().context() as _) };
                let cls = env.find_class(jni_str!("org/flos/phira/modded/VideoEncoder"))?;
                let jname = env.new_string(name)?;
                let jmusic = env.new_string(&music)?;
                Ok(env
                    .call_static_method(
                        cls,
                        jni_str!("start"),
                        jni_sig!("(Landroid/content/Context;Ljava/lang/String;IIIILjava/lang/String;J)Z"),
                        &[
                            JValue::Object(&activity),
                            JValue::Object(jname.as_ref()),
                            JValue::Int(w as i32),
                            JValue::Int(h as i32),
                            JValue::Int(fps as i32),
                            JValue::Int(bitrate_kbps as i32),
                            JValue::Object(jmusic.as_ref()),
                            JValue::Long(delay_us),
                        ],
                    )?
                    .z()?)
            })?;
            if !ok {
                bail!("VideoEncoder.start 返回 false（看 logcat 里的 PhiraVideo）");
            }
            Ok(Self {
                w: w as usize,
                h: h as usize,
                nv12: vec![0u8; (w as usize) * (h as usize) * 3 / 2],
                frames: 0,
                name: name.to_owned(),
            })
        }
    }

    impl FrameSink for AndroidSink {
        fn write(&mut self, rgba: &[u8], pts_us: i64) -> Result<()> {
            rgba_to_nv12(rgba, self.w, self.h, &mut self.nv12);
            let ok = with_env(|env| -> jni::errors::Result<bool> {
                let cls = env.find_class(jni_str!("org/flos/phira/modded/VideoEncoder"))?;
                let arr = env.byte_array_from_slice(&self.nv12)?;
                let obj: &JObject = arr.as_ref();
                Ok(env
                    .call_static_method(cls, jni_str!("writeFrame"), jni_sig!("([BJ)Z"), &[JValue::Object(obj), JValue::Long(pts_us)])?
                    .z()?)
            })?;
            if !ok {
                bail!("安卓编码器 writeFrame 返回 false（看 logcat 的 PhiraVideo）");
            }
            self.frames += 1;
            Ok(())
        }

        fn finish(self: Box<Self>) -> Result<Option<PathBuf>> {
            let ok = with_env(|env| -> jni::errors::Result<bool> {
                let cls = env.find_class(jni_str!("org/flos/phira/modded/VideoEncoder"))?;
                Ok(env.call_static_method(cls, jni_str!("finish"), jni_sig!("()Z"), &[])?.z()?)
            })?;
            if !ok {
                bail!("安卓编码器 finish 失败（看 logcat 的 PhiraVideo）");
            }
            Ok(Some(PathBuf::from(format!("(应用外部目录)/{}", self.name))))
        }
    }
}

/// RGBA → NV12（BT.601 limited range，硬件编码器通用）。
#[cfg(any(test, target_os = "android"))]
fn rgba_to_nv12(rgba: &[u8], w: usize, h: usize, out: &mut [u8]) {
    let y_size = w * h;
    debug_assert!(rgba.len() >= y_size * 4);
    debug_assert!(out.len() >= y_size * 3 / 2);
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let i = (row + x) * 4;
            let r = rgba[i] as i32;
            let g = rgba[i + 1] as i32;
            let b = rgba[i + 2] as i32;
            let yv = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            out[row + x] = yv.clamp(0, 255) as u8;
        }
    }
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let mut r = 0i32;
            let mut g = 0i32;
            let mut b = 0i32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = ((y + dy) * w + (x + dx)) * 4;
                    r += rgba[i] as i32;
                    g += rgba[i + 1] as i32;
                    b += rgba[i + 2] as i32;
                }
            }
            let (r, g, b) = (r / 4, g / 4, b / 4);
            let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
            let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
            let off = y_size + (y / 2) * w + x;
            out[off] = u.clamp(0, 255) as u8;
            out[off + 1] = v.clamp(0, 255) as u8;
        }
    }
}

// ============================================================
// iOS：AVAssetWriter（见 ios/video_encoder.m）
// ============================================================

#[cfg(target_os = "ios")]
mod ios_sink {
    use super::*;
    use std::ffi::{c_char, c_void, CString};

    extern "C" {
        fn phira_video_start(name: *const c_char, w: i32, h: i32, fps: i32, bitrate_kbps: i32) -> *mut c_void;
        fn phira_video_append(ctx: *mut c_void, rgba: *const u8, pts_us: i64) -> bool;
        fn phira_video_finish(ctx: *mut c_void) -> bool;
        fn phira_video_cancel(ctx: *mut c_void);
        fn phira_video_last_path() -> *const c_char;
        /// 把音乐（原文件直通）混进已写好的 mp4；失败返回 false（视频保留）。
        fn phira_video_mux_audio(video_path: *const c_char, music_path: *const c_char, delay_us: i64) -> bool;
    }

    pub struct IosSink {
        ctx: *mut c_void,
        name: String,
        music: Option<String>,
        delay_us: i64,
    }

    // 编码器只在渲染线程用，指针不会跨线程
    unsafe impl Send for IosSink {}

    impl IosSink {
        pub fn new(
            name: &str,
            w: u32,
            h: u32,
            fps: u32,
            bitrate_kbps: u32,
            music: Option<&Path>,
            delay_us: i64,
        ) -> Result<Self> {
            let cname = CString::new(name).context("文件名里有 NUL")?;
            let ctx = unsafe { phira_video_start(cname.as_ptr(), w as i32, h as i32, fps as i32, bitrate_kbps as i32) };
            if ctx.is_null() {
                bail!("iOS 视频编码器初始化失败（看 Xcode 日志里的 [phira-video]）");
            }
            Ok(Self {
                ctx,
                name: name.to_owned(),
                music: music.map(|p| p.to_string_lossy().into_owned()),
                delay_us,
            })
        }
    }

    impl FrameSink for IosSink {
        fn write(&mut self, rgba: &[u8], pts_us: i64) -> Result<()> {
            if !unsafe { phira_video_append(self.ctx, rgba.as_ptr(), pts_us) } {
                bail!("iOS 编码器 append 失败");
            }
            Ok(())
        }

        fn finish(self: Box<Self>) -> Result<Option<PathBuf>> {
            if !unsafe { phira_video_finish(self.ctx) } {
                bail!("iOS 编码器 finish 失败");
            }
            let p = unsafe {
                let ptr = phira_video_last_path();
                if ptr.is_null() {
                    None
                } else {
                    Some(PathBuf::from(std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()))
                }
            };
            // 有音乐就再混一次（原文件直通，不重编码视频）；失败保持无声视频
            if let (Some(video), Some(music)) = (p.as_ref(), self.music.as_ref()) {
                if let (Ok(cv), Ok(cm)) = (CString::new(video.to_string_lossy().as_bytes()), CString::new(music.as_str())) {
                    let ok = unsafe { phira_video_mux_audio(cv.as_ptr(), cm.as_ptr(), self.delay_us) };
                    if !ok {
                        warn!("render: iOS 混音失败，保留无声视频");
                    }
                }
            }
            Ok(p.or_else(|| Some(PathBuf::from(format!("(Documents)/{}", self.name)))))
        }
    }

    impl Drop for IosSink {
        fn drop(&mut self) {
            // 正常路径 finish 已释放 ctx；异常路径（write 报错）在这里取消
            if !self.ctx.is_null() {
                unsafe { phira_video_cancel(self.ctx) };
                self.ctx = std::ptr::null_mut();
            }
        }
    }
}

// ============================================================
// 平台分派
// ============================================================

#[cfg(any(test, target_os = "android", target_os = "ios"))]
fn mobile_file_name(out: &str) -> String {
    Path::new(out)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "render.mp4".to_owned())
}

fn create_sink(opts: &RenderOpts, music: Option<&Path>) -> Result<Box<dyn FrameSink>> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        create_ffmpeg_sink(opts, music)
    }
    #[cfg(target_os = "android")]
    {
        // 音乐原文件直通封装（MediaExtractor → MediaMuxer，不解码不重编码）；
        // 打不开就当无声视频，不影响渲染。
        android_sink::AndroidSink::new(
            &mobile_file_name(&opts.out),
            opts.width,
            opts.height,
            opts.fps,
            opts.bitrate_kbps(),
            music,
            (RENDER_INTRO * 1_000_000.) as i64,
        )
        .map(|s| Box::new(s) as Box<dyn FrameSink>)
    }
    #[cfg(target_os = "ios")]
    {
        ios_sink::IosSink::new(
            &mobile_file_name(&opts.out),
            opts.width,
            opts.height,
            opts.fps,
            opts.bitrate_kbps(),
            music,
            (RENDER_INTRO * 1_000_000.) as i64,
        )
        .map(|s| Box::new(s) as Box<dyn FrameSink>)
    }
}

// ============================================================
// PBO 异步回读
// ============================================================

/// 环形 PBO：`glReadPixels` 写进 PBO 立刻返回（不 stall），
/// 落后 2 帧再把最早那个映射出来交给编码器。
/// 导演模式的后处理相机：把渲染结果按运镜模板（zoom / pan / rotate）重画到第二个目标。
struct CamPass {
    target: MSRenderTarget,
}
impl CamPass {
    fn new(w: u32, h: u32) -> Self {
        Self { target: MSRenderTarget::new((w, h), 1) }
    }
    /// 依据模板名和归一化进度 t(0..1) 算出 (zoom, pan_x, pan_y, rotation)。
    /// pan 单位是屏幕像素（中心偏移）。
    fn eval(template: &str, t: f64, w: f32, h: f32) -> (f32, f32, f32, f32) {
        use std::f64::consts::TAU;
        let t = t.clamp(0., 1.);
        match template {
            // 缓慢推近
            "push" => (1. + (0.15 * t) as f32, 0., 0., 0.),
            // 缓慢拉远
            "pull" => (1.15 - (0.15 * t) as f32, 0., 0., 0.),
            // 双向环绕（两次完整摇摆）
            "orbit" => (1.06, 0., 0., ((t * TAU * 2.).sin() as f32).to_radians() * 4.),
            // 水平缓摆
            "sway" => (1.08, ((t * TAU * 3.).sin() as f32) * w * 0.03, 0., 0.),
            // 漂移：推近 + 斜向缓移 + 微旋
            "drift" => (
                1.1 + (0.05 * t) as f32,
                ((t * TAU).sin() as f32) * w * 0.025,
                ((t * TAU * 0.7).cos() as f32) * h * 0.02,
                ((t * TAU).sin() as f32).to_radians() * 2.,
            ),
            _ => (1., 0., 0., 0.),
        }
    }
    /// 把谱面纹理按运镜画进自己的 target。
    fn draw(&mut self, src_texture: macroquad::texture::Texture2D, w: u32, h: u32, template: &str, t: f64) {
        use macroquad::prelude::*;
        let (zoom_z, pan_x, pan_y, rot) = Self::eval(template, t, w as f32, h as f32);
        unsafe { get_internal_gl() }.quad_gl.render_pass(Some(self.target.output().render_pass));
        clear_background(BLACK);
        let w = w as f32;
        let h = h as f32;
        // Camera2D：视野宽 = w / zoom_z，target 在画面中心 + pan 偏移
        let camera = Camera2D {
            target: vec2(w * 0.5 + pan_x, h * 0.5 + pan_y),
            zoom: vec2(2. / w * zoom_z, 2. / h * zoom_z),
            rotation: rot,
            ..Default::default()
        };
        set_camera(&camera);
        // 画满整个世界矩形（相机负责缩放/旋转/平移）
        draw_texture_ex(
            src_texture,
            0.,
            0.,
            WHITE,
            DrawTextureParams { dest_size: Some(vec2(w, h)), ..Default::default() },
        );
        set_default_camera();
    }
}

struct PboRing {
    ids: Vec<u32>,
    size: usize,
    next: usize,
    pending: VecDeque<(u32, i64)>,
    /// GL 的 `glReadPixels` 是自下而上的，写进编码器前要翻一次（桌面/移动端共用）。
    flip: bool,
    /// 翻转用的临时缓冲（只分配一次）。
    scratch: Vec<u8>,
    height: usize,
}

impl PboRing {
    fn new(count: usize, size: usize, flip: bool, height: usize) -> Self {
        let mut ids = vec![0u32; count];
        unsafe {
            use miniquad::gl::*;
            glGenBuffers(count as i32, ids.as_mut_ptr());
            for &id in &ids {
                glBindBuffer(GL_PIXEL_PACK_BUFFER, id);
                glBufferData(GL_PIXEL_PACK_BUFFER, size as _, std::ptr::null(), GL_STREAM_READ);
            }
            glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
        }
        Self {
            ids,
            size,
            next: 0,
            pending: VecDeque::with_capacity(count),
            flip,
            scratch: if flip { vec![0u8; size] } else { Vec::new() },
            height: height.max(1),
        }
    }

    /// 把当前 READ_FRAMEBUFFER 的像素异步拷进下一个 PBO。
    fn capture(&mut self, fbo: u32, w: u32, h: u32, pts_us: i64) {
        let id = self.ids[self.next];
        self.next = (self.next + 1) % self.ids.len();
        unsafe {
            use miniquad::gl::*;
            glBindFramebuffer(GL_READ_FRAMEBUFFER, fbo);
            glBindBuffer(GL_PIXEL_PACK_BUFFER, id);
            glReadPixels(0, 0, w as i32, h as i32, GL_RGBA, GL_UNSIGNED_BYTE, std::ptr::null_mut());
            glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
        }
        self.pending.push_back((id, pts_us));
        // 理论上 pump 会及时取走；万一没取，别让 pending 无限长（丢最老的）
        while self.pending.len() > self.ids.len() {
            self.pending.pop_front();
        }
    }

    /// 把已经"凉了"的帧交给 sink；`flush = true` 时排空全部。
    fn pump(&mut self, sink: &mut dyn FrameSink, flush: bool) -> Result<()> {
        // 保持 2 帧在飞行中（GPU 还在写最新的两个）
        let keep = if flush { 0 } else { 2 };
        while self.pending.len() > keep {
            let (id, pts) = self.pending.pop_front().unwrap();
            unsafe {
                use miniquad::gl::*;
                glBindBuffer(GL_PIXEL_PACK_BUFFER, id);
                let ptr = glMapBuffer(GL_PIXEL_PACK_BUFFER, 0x88B8 /* GL_READ_ONLY */);
                if ptr.is_null() {
                    glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
                    bail!("glMapBuffer 返回空（PBO 丢失）");
                }
                let slice = std::slice::from_raw_parts(ptr, self.size);
                let data: &[u8] = if self.flip {
                    // 逐行倒序拷贝（bottom-up → top-down）
                    let row = self.size / self.height;
                    for y in 0..self.height {
                        let src = (self.height - 1 - y) * row;
                        let dst = y * row;
                        self.scratch[dst..dst + row].copy_from_slice(&slice[src..src + row]);
                    }
                    &self.scratch
                } else {
                    slice
                };
                let res = sink.write(data, pts);
                glUnmapBuffer(GL_PIXEL_PACK_BUFFER);
                glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
                res?;
            }
        }
        Ok(())
    }
}

impl Drop for PboRing {
    fn drop(&mut self) {
        unsafe {
            use miniquad::gl::*;
            glDeleteBuffers(self.ids.len() as i32, self.ids.as_ptr());
        }
    }
}

// ============================================================
// 命令行解析
// ============================================================

// ============================================================
// UI 请求通道（曲库页「渲染」按钮 → webui 桥 → 主循环）
// ============================================================

static PENDING_RENDER: std::sync::Mutex<Option<RenderOpts>> = std::sync::Mutex::new(None);

// —— 渲染队列（批量导出：一条接一条跑，UI 可以随时追加） ——
pub struct QueueItem {
    pub opts: RenderOpts,
    pub label: String,
}
static RENDER_QUEUE: std::sync::Mutex<std::collections::VecDeque<QueueItem>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
static QUEUE_DONE: std::sync::Mutex<Vec<(String, bool)>> = std::sync::Mutex::new(Vec::new());

/// 入队一条渲染（label 给 UI 显示，一般是曲名）。
pub fn enqueue_render(opts: RenderOpts, label: String) -> usize {
    if let Ok(mut q) = RENDER_QUEUE.lock() {
        q.push_back(QueueItem { opts, label: label.clone() });
        let len = q.len();
        info!("render: 入队 {label}，队列 {} 条", len);
        return len;
    }
    0
}

/// 主循环取下一条。
pub fn pop_queue() -> Option<QueueItem> {
    RENDER_QUEUE.lock().ok().and_then(|mut q| q.pop_front())
}

pub fn queue_len() -> usize {
    RENDER_QUEUE.lock().map(|q| q.len()).unwrap_or(0)
}

pub fn mark_queue_done(label: &str, ok: bool) {
    if let Ok(mut d) = QUEUE_DONE.lock() {
        d.push((label.to_owned(), ok));
        let n = d.len();
        if n > 20 {
            d.drain(..n - 20);
        }
    }
}

/// 队列状态（给 UI）。
pub fn queue_snapshot() -> serde_json::Value {
    use serde_json::json;
    let pending: Vec<String> = RENDER_QUEUE
        .lock()
        .map(|q| q.iter().map(|it| it.label.clone()).collect())
        .unwrap_or_default();
    let done = QUEUE_DONE.lock().map(|d| {
        d.iter()
            .map(|(l, ok)| json!({ "label": l, "ok": ok }))
            .collect::<Vec<_>>()
    }).unwrap_or_default();
    json!({ "ok": true, "pending": pending, "done": done })
}

/// 桥命令 `renderChart` 收到请求后调用（跑在 IPC 线程）。
pub fn request_render(opts: RenderOpts) {
    if let Ok(mut g) = PENDING_RENDER.lock() {
        *g = Some(opts);
    }
}

/// 游戏线程每帧取一次；拿到就接管渲染循环。
pub fn take_pending_render() -> Option<RenderOpts> {
    PENDING_RENDER.lock().ok().and_then(|mut g| g.take())
}

fn next_val<I: Iterator<Item = String>>(it: &mut I, flag: &str) -> Result<String> {
    it.next().ok_or_else(|| anyhow!("{flag} 缺少参数值"))
}

fn next_num<T: std::str::FromStr, I: Iterator<Item = String>>(it: &mut I, flag: &str) -> Result<T> {
    let v = next_val(it, flag)?;
    v.parse().map_err(|_| anyhow!("{flag} 的值不是合法数字：{v}"))
}

/// Android：`am start ... -e render "<谱面目录>[|输出.mp4]"` 传进来的请求。
/// Java 侧 `MainActivity.onCreate` 会调 `nativeRenderArg` 塞进来。
#[cfg(target_os = "android")]
pub(crate) static ANDROID_RENDER_ARG: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 把 `"<谱面目录>|<输出.mp4>"` 解析成 RenderOpts（输出省略时用目录名）。
#[cfg(any(test, target_os = "android", target_os = "ios"))]
fn opts_from_mobile_arg(arg: &str) -> Result<RenderOpts> {
    let (chart, out) = match arg.split_once('|') {
        Some((c, o)) if !o.trim().is_empty() => (c.trim().to_owned(), o.trim().to_owned()),
        _ => {
            let chart = arg.trim().to_owned();
            let name = Path::new(&chart)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "render".to_owned());
            (chart, format!("{name}.mp4"))
        }
    };
    if chart.is_empty() {
        bail!("render 参数里没有谱面目录");
    }
    Ok(RenderOpts {
        chart,
        out,
        ..RenderOpts::default()
    })
}

/// 解析命令行；没有 `--render` 时返回 `Ok(None)`（走正常游戏流程）。
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Option<RenderOpts>> {
    let mut it = args.into_iter();
    let _exe = it.next();
    let mut opts = RenderOpts::default();
    let mut saw_render = false;
    let mut positional: Vec<String> = Vec::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--render" => saw_render = true,
            "--width" => opts.width = next_num(&mut it, "--width")?,
            "--height" => opts.height = next_num(&mut it, "--height")?,
            "--fps" => opts.fps = next_num(&mut it, "--fps")?,
            "--msaa" => opts.msaa = next_num(&mut it, "--msaa")?,
            "--crf" => opts.crf = next_num(&mut it, "--crf")?,
            "--bitrate" => opts.bitrate_kbps = next_num(&mut it, "--bitrate")?,
            "--vflip" => opts.vflip = true,
            "--no-vflip" => opts.vflip = false,
            other if other.starts_with("--") => bail!("未知参数：{other}"),
            other => positional.push(other.to_owned()),
        }
    }
    if !saw_render {
        // Android：允许用 intent 传渲染请求（手机上没有命令行）
        #[cfg(target_os = "android")]
        if let Some(arg) = ANDROID_RENDER_ARG.lock().ok().and_then(|mut g| g.take()) {
            info!("render: 来自 intent 的请求 {arg}");
            return Ok(Some(opts_from_mobile_arg(&arg)?));
        }
        return Ok(None);
    }
    if positional.len() < 2 {
        bail!("--render 需要两个参数：<谱面目录> <输出.mp4>");
    }
    opts.chart = positional[0].clone();
    opts.out = positional[1].clone();
    if opts.width == 0 || opts.height == 0 || opts.fps == 0 {
        bail!("width/height/fps 必须大于 0");
    }
    // 硬件编码器要求偶数宽高
    opts.width &= !1;
    opts.height &= !1;
    opts.msaa = opts.msaa.max(1);
    Ok(Some(opts))
}

// ============================================================
// 主流程
// ============================================================

/// 渲染整首谱面到 mp4。`font` 用主流程已加载好的界面字体（文字绘制要用）。
pub async fn run(opts: RenderOpts, font: FontArc) -> Result<()> {
    let (w, h, fps) = (opts.width, opts.height, opts.fps as f64);
    info!(
        "render: chart={} out={} {}x{}@{} msaa={} crf={} bitrate={}kbps",
        opts.chart,
        opts.out,
        w,
        h,
        fps,
        opts.msaa,
        opts.crf,
        opts.bitrate_kbps()
    );

    // —— 打开谱面 ——
    let chart_dir = opts.chart.clone();
    let mut fs = crate::scene::fs_from_path(&chart_dir).with_context(|| format!("打开谱面目录失败：{chart_dir}"))?;
    let info_chart = fs::load_info(fs.as_mut()).await.with_context(|| format!("读取 {chart_dir}/info.yml 失败"))?;
    let mut config = crate::get_data().config.clone();
    // ============================================================
    // 渲染专用配置：对齐 Phi Recorder 1.6.1 的默认渲染参数
    // （`prpr_config.mods = Mods::AUTOPLAY` + render_ui_* 全开 + 专用字体/字号）
    // ============================================================
    // 自动游玩：不然画面里没有判定、连击/分数/ACC 全是 0，HUD 一片死气。
    config.mods = Mods::AUTOPLAY;
    // 视频里不要出现「触摸/暂停」等交互残留
    config.interactive = false;
    // 速度固定 1.0：音轨原样喂给 ffmpeg，改速度会立刻音画不同步
    config.speed = 1.0;
    // 个人判定校准不进视频（Phi Recorder 用自己的 offset，默认 0）
    config.offset = 0.0;
    // 连击文字：Phi Recorder 的 `combo = "AUTOPLAY"`
    config.combo_text = "AUTOPLAY".to_owned();
    // Phi Recorder 的整块打歌文字只用一套字体（assets/font.ttf），字号按 scale_ratio
    config.phi_recorder_font = true;
    config.in_game_text_scale = 1.777_777_7;
    // HUD：只保留 Phi Recorder 的那几件（分数 / 连击 / 进度条 / 曲名 / 难度 / 暂停图标）
    config.show_acc = false;
    config.hud_acc = false;
    config.hud_counts = false;
    config.hud_max_combo = false;
    config.hud_time = false;
    config.hud_delta = false;
    config.hud_rks = false;
    config.hud_fps = false;
    config.early_late_hint = false;
    config.touch_debug = false;
    config.motion_pause = false;
    config.chart_debug = false;
    config.chart_debug_line = false;
    config.chart_debug_note = false;
    config.music_spectrum = false;
    config.show_avg_fps = false;
    config.judgement_range_debug.enabled = false;
    // 画面：粒子开、背景按 Phi Recorder 再压暗一层、片头用 phire 的 1.2s 线性淡入
    config.particle = true;
    config.render_bg_dim = true;
    config.render_intro_secs = RENDER_INTRO;
    // 手感参数固定为参考默认值（视频是作品，不是个人练习）
    config.note_flow_speed = 1.0;
    config.note_scale = 1.0;
    // 参考视频里没有的效果一律关掉，避免「同一个谱面渲染出两种画风」
    // 残影：用 Phi Recorder 的帧缓冲累积（preserve_framebuffer），
    // 不用离散拖影那套（note_trail / line_afterimage 保持关闭）
    config.preserve_framebuffer = true;
    config.render_fade = 0.45;
    config.note_trail = false;
    config.line_afterimage = false;
    config.gyro_parallax = false;
    config.block_mask_divisor = 1;
    // prpr 独有的叠加层（Phi Recorder 没有）：判定文字 / 漏键标记 / 准度条
    config.judge_text = false;
    config.miss_marker = false;
    config.timing_bar.enabled = false;
    // 全局着色器 / FXAA 也是本 fork 的附加项：参考视频里没有，且 MSAA 已经做了抗锯齿
    config.global_shader.clear();
    config.fxaa = false;

    // —— 时长：优先解音乐拿总长；读不到/解不动就按谱面音符估算
    //    （RPE 谱面经常不带 music 文件，不能因此让渲染失败）
    let mut music_path = Path::new(&chart_dir).join(&info_chart.music);
    let mut duration: Option<f64> = None;
    match fs.load_file(&info_chart.music).await {
        Ok(bytes) => match AudioClip::decode(bytes) {
            Ok((frames, sr)) => {
                let d = frames.len() as f64 / sr as f64;
                if d > 0.05 { duration = Some(d); } else { warn!("render: 音乐时长异常（{d:.2}s），改用谱面估算"); }
            }
            Err(e) => warn!("render: 音乐解码失败（{e}），改用谱面估算时长"),
        },
        Err(e) => warn!("render: 读不到音乐 {}（{e}），改用谱面估算时长", info_chart.music),
    }
    if duration.is_none() || !music_path.is_file() {
        // 读一遍谱面源文本估算
        let chart_file = info_chart.chart.clone();
        if let Ok(src) = fs.load_file(&chart_file).await {
            duration = crate::editor::estimate_duration(&String::from_utf8_lossy(&src)).or(duration);
        }
        music_path = Path::new(&chart_dir).join("__no_music__");   // 标记为无音乐
    }
    let duration = duration.unwrap_or(30.);
    // 引擎里 `Resource::track_length = music.length()`（core/resource.rs:534），
    // 所以整局时长 = 开场 + 音乐 + 尾判等待 + 结束淡出。
    // 以前只算了「音乐 + 0.5」，结果：片头 0.7s 的音画错位 + 结尾淡出被切掉。
    let total_secs = RENDER_INTRO + duration + RENDER_WAIT + RENDER_OUTRO;
    let total_frames = ((total_secs * fps).ceil() as u64).max(1);
    info!(
        "render: 音乐 {duration:.2}s + 开场 {RENDER_INTRO}s + 尾 {:.1}s = {total_secs:.2}s → {total_frames} 帧",
        RENDER_WAIT + RENDER_OUTRO
    );

    let music = music_path.is_file().then_some(music_path.as_path());

    // —— 编码器 ——
    let mut sink = create_sink(&opts, music)?;

    // —— 建场景：直接 GameScene，**不走 LoadingScene** ——
    // LoadingScene 自带 1.0s 开场 + 0.4s 等待 + 1.4s 过渡，且时长取决于加载耗时：
    // 放进视频里就是「片头不定长 + 音画整体错位」。Phi Recorder 的非 render_loading
    // 路径同样直接构造 GameScene。
    let (illustration, background, _theme) = match LoadingScene::load(fs.as_mut(), &info_chart.illustration).await {
        Ok(v) => v,
        Err(err) => {
            warn!("render: 曲绘/背景加载失败（{err:?}），改用黑图");
            (prpr::ext::BLACK_TEXTURE.clone(), prpr::ext::BLACK_TEXTURE.clone(), macroquad::color::WHITE)
        }
    };
    let scene = GameScene::new(
        GameMode::Normal,
        info_chart,
        config,
        fs,
        None,
        background,
        illustration,
        None,
        None,
        None,
    )
    .await
    .context("载入谱面失败")?;

    // —— 导演模式：后处理相机 pass（不碰引擎内部，把谱面纹理按模板变换重画一遍） ——
    let mut cam_pass = opts.camera.as_deref().filter(|c| *c != "none" && !c.is_empty()).map(|_| {
        CamPass::new(w, h)
    });

    // —— 手动时间轴 + 离屏目标 ——
    let target = Rc::new(MSRenderTarget::new((w, h), opts.msaa));
    let time = Rc::new(RefCell::new(0f64));
    let tm = TimeManager::manual(Box::new({
        let time = time.clone();
        move || *time.borrow()
    }));
    let chooser = {
        let target = target.clone();
        let msaa = opts.msaa;
        move || Some(if msaa > 1 { target.input() } else { target.output() })
    };
    let mut main = Main::new(Box::new(scene), tm, chooser).await.context("初始化渲染主循环失败")?;
    main.top_level = false;
    main.viewport = Some((0, 0, w as i32, h as i32));

    let mut painter = TextPainter::new(font, None);
    let mut pbo = PboRing::new(3, (w as usize) * (h as usize) * 4, opts.vflip, h as usize);
    let started = std::time::Instant::now();
    let mut last_report = 0u64;

    for frame in 0..total_frames {
        *time.borrow_mut() = frame as f64 / fps;
        // ⚠️ MSAA 时场景必须画进 `input()`（多重采样 FBO），再 blit 到 `output()`。
        // 以前一直画 `output()`、却把从没画过的 `input()` blit 过去 —— 开 MSAA 就是黑帧/花屏。
        let scene_pass = if opts.msaa > 1 { target.input() } else { target.output() };
        unsafe { get_internal_gl() }.quad_gl.render_pass(Some(scene_pass.render_pass));
        main.update().context("更新场景失败")?;
        main.render(&mut painter).context("渲染场景失败")?;
        unsafe { get_internal_gl() }.flush();
        if opts.msaa > 1 {
            target.blit(); // 多重采样 resolve 到 output
        }
        // 导演模式：按运镜模板把谱面纹理重画一遍（不进额外的进度信息）
        let out_fbo = if let Some(cp) = cam_pass.as_mut() {
            cp.draw(target.output().texture, w, h, opts.camera.as_deref().unwrap_or(""), frame as f64 / total_frames as f64);
            prpr::core::internal_id(cp.target.output())
        } else {
            prpr::core::internal_id(target.output())
        };
        pbo.capture(out_fbo, w, h, (frame as f64 / fps * 1_000_000.) as i64);
        pbo.pump(sink.as_mut(), false)?;

        let pct = (frame + 1) * 100 / total_frames;
        if pct >= last_report + 10 || frame + 1 == total_frames {
            last_report = pct / 10 * 10;
            let elapsed = started.elapsed().as_secs_f64();
            info!("render: {pct}% 帧 {}/{total_frames} 已用 {elapsed:.1}s", frame + 1);
        }

        // 窗口上画进度条（**不进视频**：画在默认帧缓冲上，FBO 里只有谱面）。
        // 每 15 帧才 present 一次 —— next_frame() 会等 vsync，每帧都等会把离线渲染拖成实时。
        if frame % 15 == 0 || frame + 1 == total_frames {
            unsafe { get_internal_gl() }.quad_gl.render_pass(None);
            set_default_camera();
            let (sw, sh) = (screen_width(), screen_height());
            draw_rectangle(0., 0., sw, sh, Color::new(0.04, 0.04, 0.06, 1.));
            let bar_w = sw * 0.6;
            let bar_x = (sw - bar_w) * 0.5;
            let bar_y = sh * 0.5;
            draw_rectangle(bar_x, bar_y, bar_w, 6., Color::new(1., 1., 1., 0.15));
            draw_rectangle(bar_x, bar_y, bar_w * (frame as f32 + 1.) / total_frames as f32, 6., WHITE);
            // 进度条上下各加一小段刻度，纯矩形，不依赖字体
            draw_rectangle(bar_x, bar_y - 14., 2., 34., Color::new(1., 1., 1., 0.5));
            next_frame().await;
        }
    }
    // 排空剩余 PBO，再收尾
    pbo.pump(sink.as_mut(), true)?;
    drop(pbo);
    let path = sink.finish()?;

    let size = path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()).unwrap_or(0);
    info!(
        "render: 完成 → {}（{:.1} MB，用时 {:.1}s）",
        path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| opts.out.clone()),
        size as f64 / 1024. / 1024.,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        std::iter::once("phira-main.exe".to_owned())
            .chain(v.iter().map(|s| s.to_string()))
            .collect()
    }

    #[test]
    fn no_render_flag_returns_none() {
        assert!(parse_args(args(&[])).unwrap().is_none());
        assert!(parse_args(args(&["--fps", "30"])).unwrap().is_none());
    }

    #[test]
    fn parses_positional_and_flags() {
        let o = parse_args(args(&[
            "--render", "D:/charts/x", "D:/out.mp4", "--width", "3840", "--height", "2160", "--fps", "120", "--msaa", "8", "--crf", "12",
            "--bitrate", "60000", "--vflip",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(o.chart, "D:/charts/x");
        assert_eq!(o.out, "D:/out.mp4");
        assert_eq!(
            (o.width, o.height, o.fps, o.msaa, o.crf, o.bitrate_kbps, o.vflip),
            (3840, 2160, 120, 8, 12, 60000, true)
        );
    }

    #[test]
    fn defaults_are_1080p60_msaa4() {
        let o = parse_args(args(&["--render", "a", "b"])).unwrap().unwrap();
        assert_eq!((o.width, o.height, o.fps, o.msaa, o.crf), (1920, 1080, 60, 4, 18));
        assert_eq!(o.bitrate_kbps, 0);
        assert!(o.vflip, "默认翻转（GL 读回自下而上，和 Phi Recorder 一致）");
        assert!(!parse_args(args(&["--render", "a", "b", "--no-vflip"])).unwrap().unwrap().vflip);
    }

    #[test]
    fn rejects_missing_positionals_and_bad_numbers() {
        assert!(parse_args(args(&["--render", "only-one"])).is_err());
        assert!(parse_args(args(&["--render", "a", "b", "--fps", "abc"])).is_err());
        assert!(parse_args(args(&["--render", "a", "b", "--fps"])).is_err());
        assert!(parse_args(args(&["--render", "a", "b", "--nope"])).is_err());
        assert_eq!(parse_args(args(&["--render", "a", "b", "--msaa", "0"])).unwrap().unwrap().msaa, 1);
    }

    #[test]
    fn odd_size_is_rounded_down_for_hardware_encoders() {
        let o = parse_args(args(&["--render", "a", "b", "--width", "1921", "--height", "1081"])).unwrap().unwrap();
        assert_eq!((o.width, o.height), (1920, 1080));
    }

    #[test]
    fn auto_bitrate_scales_with_pixels() {
        let mut o = RenderOpts::default();
        // 1920*1080*60 = 124,416,000 像素/秒 ×0.13 → 16,174 kbps
        assert_eq!(o.bitrate_kbps(), 16_174);
        o.width = 3840;
        o.height = 2160;
        // 3840*2160*60 ×0.13 → 64,696 kbps
        assert_eq!(o.bitrate_kbps(), 64_696);
        o.bitrate_kbps = 1234;
        assert_eq!(o.bitrate_kbps(), 1234);
    }

    #[test]
    fn build_args_without_music() {
        let o = RenderOpts::default();
        let a = build_args(&o, None);
        let s = a.join(" ");
        assert!(s.contains("-f rawvideo"), "{s}");
        assert!(s.contains("-s 1920x1080"), "{s}");
        assert!(s.contains("-r 60"), "{s}");
        assert!(s.contains("-i pipe:0"), "{s}");
        assert!(s.contains("-c:v libx264"), "{s}");
        assert!(s.contains("-crf 18"), "{s}");
        assert!(s.contains("-pix_fmt yuv420p"), "{s}");
        assert!(!s.contains("1:a:0"), "无音乐时不该映射音频：{s}");
    }

    #[test]
    fn build_args_with_music_and_vflip() {
        let mut o = RenderOpts::default();
        o.out = "D:/o.mp4".into();
        o.vflip = true;
        let a = build_args(&o, Some(Path::new("D:/m.mp3")));
        let s = a.join(" ");
        assert!(s.contains("-i D:/m.mp3"), "{s}");
        assert!(s.contains("-map 1:a:0"), "{s}");
        assert!(!s.contains("-vf vflip"), "方向在回读层翻转，ffmpeg 不该再翻一次：{s}");
        assert!(s.contains("-c:a aac"), "{s}");
        assert!(s.contains("-b:a 320k"), "音频 320k：{s}");
        assert!(s.contains("-shortest"), "{s}");
        assert!(s.contains("-movflags +faststart"), "{s}");
        assert!(s.contains("adelay=1200:all=1,apad"), "音轨延迟开场时长：{s}");
        assert!(s.contains("-colorspace bt709"), "{s}");
        assert_eq!(a.last().unwrap(), "D:/o.mp4");
    }

    #[test]
    fn rgba_to_nv12_layout_and_range() {
        // 4x2 纯红 → Y≈82（BT.601 limited），U≈90，V≈240
        let mut rgba = vec![0u8; 4 * 2 * 4];
        for px in rgba.chunks_exact_mut(4) {
            px[0] = 255;
            px[3] = 255;
        }
        let mut nv12 = vec![0u8; 4 * 2 * 3 / 2];
        rgba_to_nv12(&rgba, 4, 2, &mut nv12);
        let y = nv12[0];
        assert!((80..=86).contains(&y), "Y 应该在 limited range 的红色附近，实际 {y}");
        let (u, v) = (nv12[8], nv12[9]);
        assert!((85..=95).contains(&u), "U 偏了：{u}");
        assert!((235..=245).contains(&v), "V 偏了：{v}");
    }

    #[test]
    fn rgba_to_nv12_gray_is_neutral() {
        let mut rgba = vec![0u8; 2 * 2 * 4];
        for px in rgba.chunks_exact_mut(4) {
            px[0] = 128;
            px[1] = 128;
            px[2] = 128;
            px[3] = 255;
        }
        let mut nv12 = vec![0u8; 2 * 2 * 3 / 2];
        rgba_to_nv12(&rgba, 2, 2, &mut nv12);
        assert!((125..=130).contains(&nv12[4]), "灰阶 U 应接近 128，实际 {}", nv12[4]);
        assert!((125..=130).contains(&nv12[5]), "灰阶 V 应接近 128，实际 {}", nv12[5]);
    }

    #[test]
    fn mobile_arg_parses_chart_and_optional_out() {
        let o = opts_from_mobile_arg("/sdcard/charts/Igallta").unwrap();
        assert_eq!(o.chart, "/sdcard/charts/Igallta");
        assert_eq!(o.out, "Igallta.mp4");
        assert_eq!((o.width, o.height, o.fps, o.msaa), (1920, 1080, 60, 4));

        let o = opts_from_mobile_arg("/sdcard/charts/X|custom.mp4").unwrap();
        assert_eq!(o.chart, "/sdcard/charts/X");
        assert_eq!(o.out, "custom.mp4");

        assert!(opts_from_mobile_arg("").is_err());
        assert!(opts_from_mobile_arg("  |out.mp4").is_err());
    }

    #[test]
    fn mobile_file_name_strips_dirs() {
        assert_eq!(mobile_file_name("D:/a/b/render.mp4"), "render.mp4");
        assert_eq!(mobile_file_name("out.mp4"), "out.mp4");
        assert_eq!(mobile_file_name(""), "render.mp4");
        assert_eq!(mobile_file_name("/"), "render.mp4");
    }
}
