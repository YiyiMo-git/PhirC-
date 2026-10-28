prpr_l10n::tl_file!("common" ttl crate::);

#[rustfmt::skip]
#[cfg(closed)]
mod inner;

mod anim;
mod blacklist;
mod censor;
/// 课题模式（T7.3）：本地课题记录、徽章规则。
mod challenge;
/// 课题模式（T7.3）：徽章绘制与槽位弹窗。
mod challenge_ui;
mod chart_core;
mod charts_view;
#[path = "../../sensitive/auth/client.rs"]
mod client;
/// 自定义 RKS（T7.4）：本地计算与配置读写。
mod custom_rks;
mod daily;
mod data;
mod history;
pub mod growth;
mod hud;
mod icons;
mod ime;
mod images;
#[path = "../../sensitive/auth/login.rs"]
mod login;
mod mp;
mod newui;
mod page;
mod phic;
mod popup;
mod editor;
mod rate;
mod record;
mod replay;
mod resource;
mod scene;
mod tabs;
mod tags;
mod threed;
/// 应用自绘的功能件（目前是取代原生对话框的文本输入框）。
mod ui;
mod uml;
/// WebView 开始界面宿主（把 `kaichang/index.html` 当开始页显示）。
/// Windows：wry + WebView2；安卓：JNI → Java `WebViewHost`；
/// iOS：C 函数边界 → `ios/webview.m`（WKWebView + `phirc://` scheme handler）。
#[cfg(any(
    all(target_os = "windows", feature = "webui"),
    target_os = "android",
    target_os = "ios"
))]
mod webui;
/// 其余平台 / 未启用 `webui`：空实现，界面照旧走 macroquad 原生 UI。
#[cfg(not(any(
    all(target_os = "windows", feature = "webui"),
    target_os = "android",
    target_os = "ios"
)))]
mod webui {
    pub fn init_host() {}

    /// 没有 WebView 时，永远没有「待开局谱面」。
    pub fn take_next_scene(_s: &mut crate::page::SharedState) -> Option<prpr::scene::NextScene> {
        None
    }
}

use anyhow::Result;
use data::Data;
use macroquad::prelude::*;
use prpr::{
    build_conf,
    core::{init_assets, GAME_FONT, PGR_FONT},
    ext::SafeTexture,
    log,
    scene::show_error,
    time::TimeManager,
    ui::{cleanup_audio, FontArc, TextPainter},
    Main,
};
use prpr_l10n::set_prefered_locale;
#[cfg(not(feature = "hykb"))]
use prpr_l10n::{GLOBAL, LANGS};
use scene::MainScene;
use std::{
    collections::VecDeque,
    sync::{mpsc, Mutex},
};
use tracing::{error, info, warn};

#[cfg(target_os = "android")]
use jni::{
    objects::{JClass, JString},
    sys::jint,
    EnvUnowned,
};

/// 把 Java 的 `String` 读成 Rust 的 `String`，读不出来返回 `None`。
///
/// 必须走 `EnvUnowned::with_env`，绝不能直接 `path.to_string()`：
/// jni 0.22 里 `JString: Display` 在 `JavaVM::singleton()` 没初始化时**不报错**，
/// 而是写出字面量 `"<JNI Not Initialized>"`（见 jni-0.22 x jstring.rs 的 Display 实现）。
/// 原生方法的第一个参数正好是 `EnvUnowned`，只有 `with_env` 会把当前线程的 JVM
/// 注册进这个 crate 的单例；少了它，`setDataPath` 拿到的就是那个占位字符串。
///
/// 之前就是栽在这里：DATA_PATH 变成 `"<JNI Not Initialized>"` 之后
/// `dir::root()` 去建 `<JNI Not Initialized>/data`（相对 cwd，安卓上 cwd 是 `/`）
/// 直接失败，`the_main` 在读到 data.json 之前就返回 Err，游戏一帧都没渲染就退出；
/// 更糟的是日志目录也来自同一个字符串，于是连一行 `[rust stage]` 都写不进去，
/// 表现就是「黑屏闪退 + 日志里只有 Java 侧的行」。
#[cfg(target_os = "android")]
pub(crate) fn read_java_string(env: &mut EnvUnowned, s: &JString, what: &str) -> Option<String> {
    if s.is_null() {
        return None;
    }
    match env.with_env(|env| s.try_to_string(env)).into_outcome() {
        jni::Outcome::Ok(s) => Some(s),
        jni::Outcome::Err(err) => {
            crash_log_stage(&format!("读取 Java 字符串失败（{what}）: {err:?}"));
            None
        }
        jni::Outcome::Panic(_) => {
            crash_log_stage(&format!("读取 Java 字符串时 panic（{what}）"));
            None
        }
    }
}

/// iOS 的摇一摇数据源（实现见 phira/ios/motion.m，由 build.rs 在 iOS 目标下编译）。
#[cfg(target_os = "ios")]
mod motion {
    extern "C" {
        fn phira_motion_start() -> std::os::raw::c_int;
        fn phira_motion_level() -> f32;
        fn phira_motion_tilt(x: *mut f32, y: *mut f32) -> std::os::raw::c_int;
    }

    /// 启动采集；返回是否可用。
    pub fn start() -> bool {
        unsafe { phira_motion_start() != 0 }
    }

    /// 当前摇动强度（g）；负数表示不可用。
    pub fn level() -> f32 {
        unsafe { phira_motion_level() }
    }

    /// 当前姿态倾斜；`None` 表示传感器不可用。
    pub fn tilt() -> Option<(f32, f32)> {
        let (mut x, mut y) = (0f32, 0f32);
        let ok = unsafe { phira_motion_tilt(&mut x, &mut y) };
        (ok != 0).then_some((x, y))
    }
}

static MESSAGES_TX: Mutex<Option<mpsc::Sender<bool>>> = Mutex::new(None);
static DATA_PATH: Mutex<Option<String>> = Mutex::new(None);
static CACHE_DIR: Mutex<Option<String>> = Mutex::new(None);
pub static mut DATA: Option<Data> = None;

#[cfg(target_env = "ohos")]
use napi_derive_ohos::napi;

#[cfg(closed)]
pub async fn load_res(name: &str) -> Vec<u8> {
    let bytes = load_file(name).await.unwrap();
    inner::resolve_data(bytes)
}

#[allow(unused)]
pub async fn load_res_tex(name: &str) -> SafeTexture {
    #[cfg(closed)]
    {
        let bytes = load_res(name).await;
        let image = image::load_from_memory(&bytes).unwrap();
        image.into()
    }
    #[cfg(not(closed))]
    prpr::ext::BLACK_TEXTURE.clone()
}

pub fn sync_data() {
    if get_data().language.is_none() {
        #[cfg(feature = "hykb")]
        let default_lang = "zh-CN".to_owned();
        #[cfg(not(feature = "hykb"))]
        let default_lang = LANGS[GLOBAL.order.lock().unwrap()[0]].to_owned();
        get_data_mut().language = Some(default_lang);
    }
    set_prefered_locale(get_data().language.as_ref().and_then(|it| it.parse().ok()));
    let _ = client::set_access_token_sync(get_data().tokens.as_ref().map(|it| &*it.0));
}

pub fn set_data(data: Data) {
    unsafe {
        DATA = Some(data);
    }
}

#[allow(static_mut_refs)]
pub fn get_data() -> &'static Data {
    unsafe { DATA.as_ref().unwrap() }
}

#[allow(static_mut_refs)]
pub fn get_data_mut() -> &'static mut Data {
    unsafe { DATA.as_mut().unwrap() }
}

pub fn save_data() -> Result<()> {
    std::fs::write(format!("{}/data.json", dir::root()?), serde_json::to_string(get_data())?)?;
    Ok(())
}

mod dir {
    use anyhow::Result;

    use crate::{CACHE_DIR, DATA_PATH};

    fn ensure(s: &str) -> Result<String> {
        let s = format!("{}/{}", DATA_PATH.lock().unwrap().as_ref().map(|it| it.as_str()).unwrap_or("."), s);
        let path = std::path::Path::new(&s);
        if !path.exists() {
            std::fs::create_dir_all(path)?;
        }
        Ok(s)
    }

    pub fn cache() -> Result<String> {
        if let Some(cache) = &*CACHE_DIR.lock().unwrap() {
            ensure(cache)
        } else {
            ensure("cache")
        }
    }

    pub fn bold_font_path() -> Result<String> {
        Ok(format!("{}/bold.ttf", root()?))
    }

    pub fn cache_image_local() -> Result<String> {
        ensure(&format!("{}/image", cache()?))
    }

    pub fn root() -> Result<String> {
        ensure("data")
    }

    pub fn charts() -> Result<String> {
        ensure("data/charts")
    }

    pub fn collections() -> Result<String> {
        ensure("data/collections")
    }

    /// PhiC 剧情章节目录（`.pic` 导入到这里）。
    pub fn chapters() -> Result<String> {
        ensure("data/chapters")
    }

    pub fn custom_charts() -> Result<String> {
        ensure("data/charts/custom")
    }

    pub fn downloaded_charts() -> Result<String> {
        ensure("data/charts/download")
    }

    pub fn respacks() -> Result<String> {
        ensure("data/respack")
    }
}

/// 首次启动自动导入内置的默认资源包 `Phigros.zip`。
///
/// 只有当前**一个资源包都没有**时才导入（不会覆盖用户已有的包），导入失败只
/// warn 不阻塞启动 —— 官方发布不内置这个 zip 时静默跳过。
async fn import_bundled_respack() {
    if !get_data().respacks.is_empty() {
        return;
    }
    let bytes = match load_file("Phigros.zip").await {
        Ok(b) => b,
        Err(_) => return,
    };
    let res: Result<()> = (|| {
        let root = dir::respacks()?;
        let dir = prpr::dir::Dir::new(&root)?;
        let mut uuid = uuid::Uuid::new_v4();
        while dir.exists(uuid.to_string())? {
            uuid = uuid::Uuid::new_v4();
        }
        let id = uuid.to_string();
        dir.create_dir_all(&id)?;
        let sub = dir.open_dir(&id)?;

        // 先把字节落成临时文件，复用与「主动导入」完全相同的校验 + 解压路径
        let tmp = std::env::temp_dir().join(format!("phira-bundled-{id}.zip"));
        std::fs::write(&tmp, &bytes)?;
        let file = std::fs::File::open(&tmp).map(std::io::BufReader::new)?;
        let mut zip = zip::ZipArchive::new(file)?;
        let config: prpr::core::ResPackInfo =
            serde_yaml::from_reader(zip.by_name("info.yml").map_err(|_| anyhow::anyhow!("missing info.yml"))?)?;
        config.verify()?;
        drop(zip);
        let file = std::fs::File::open(&tmp).map(std::io::BufReader::new)?;
        prpr::ext::unzip_into(file, &sub, false)?;
        let _ = std::fs::remove_file(&tmp);

        get_data_mut().respacks.insert(0, id);
        info!("默认资源包已自动导入（Phigros.zip → {}）", config.name);
        save_data()?;
        Ok(())
    })();
    if let Err(err) = res {
        warn!("默认资源包导入失败（不影响启动）: {err:?}");
    }
}

async fn the_main() -> Result<()> {
    log::register();
    crash_log_stage("the_main 进入");
    #[cfg(target_env = "ohos")]
    {
        *DATA_PATH.lock().unwrap() = Some("/data/storage/el2/base".to_owned());
        *CACHE_DIR.lock().unwrap() = Some("/data/storage/el2/base/cache".to_owned());
        prpr::core::DPI_VALUE.store(250, std::sync::atomic::Ordering::Relaxed);
    };

    init_assets();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    #[cfg(target_os = "ios")]
    {
        use objc2_foundation::{NSSearchPathDirectory, NSSearchPathDomainMask, NSSearchPathForDirectoriesInDomains};

        let directories = NSSearchPathForDirectoriesInDomains(NSSearchPathDirectory::LibraryDirectory, NSSearchPathDomainMask::UserDomainMask, true);
        let path = directories.firstObject().unwrap().to_string();
        *DATA_PATH.lock().unwrap() = Some(path);
        *CACHE_DIR.lock().unwrap() = Some("Caches".to_owned());
    }

    // 数据目录一旦取不到（DATA_PATH 空 / 不可写），后面全部会连锁失败，
    // 而且表现是「一帧都没画就退出」，所以这里失败要留下明确的记录。
    let dir = match dir::root() {
        Ok(dir) => dir,
        Err(err) => {
            crash_log_stage(&format!("dir::root() 失败（DATA_PATH={:?}）: {err:?}", DATA_PATH.lock().unwrap().clone()));
            return Err(err);
        }
    };
    crash_log_stage(&format!("数据目录就绪: {dir}"));
    let mut data: Data = std::fs::read_to_string(format!("{dir}/data.json"))
        .map_err(anyhow::Error::new)
        .and_then(|s| Ok(serde_json::from_str(&s)?))
        .unwrap_or_default();
    data.init().await?;
    set_data(data);
    sync_data();
    save_data()?;

    // 首次启动自动导入内置的默认资源包（Phigros.zip，放在可执行文件旁 / APK assets 内）。
    // 用户当前一个资源包都没有时才导入；之后可在「资源包」页自行删除重导。
    import_bundled_respack().await;

    // Warm up the offline banned-word automaton so local edits can check
    // synchronously. No-op without the `aa` feature.
    tokio::spawn(censor::preload());

    let rx = {
        let (tx, rx) = mpsc::channel();
        *MESSAGES_TX.lock().unwrap() = Some(tx);
        rx
    };

    unsafe { get_internal_gl() }
        .quad_context
        .display_mut()
        .set_pause_resume_listener(on_pause_resume);

    // 游戏内场景（开场动画 / 结算画面）统一用 assets/font.ttf
    let game_font = FontArc::try_from_vec(load_file("font.ttf").await?)?;
    GAME_FONT.with({
        let game_font = game_font.clone();
        move |it| *it.borrow_mut() = Some(TextPainter::new(game_font, None))
    });
    crash_log_stage("已加载 font.ttf");

    // 官方视频回落目录：精简包不带 assets/video 时，把 mp4 放 <数据目录>/video/ 即可
    prpr::video_res::set_fallback_dir(format!("{dir}/video"));

    // 打歌界面的 phigros.ttf 只含 ASCII + Latin-1（193 个字形），
    // 自定义连击文字之类的非拉丁字符会直接画成空白。把它挂上 font.ttf 作兜底：
    // prpr 的文字绘制会逐字回退到第二套字体，ASCII 的渲染路径完全不变。
    let pgr_font = FontArc::try_from_vec(load_file("phigros.ttf").await?)?;
    let game_font_for_ui = game_font.clone();
    PGR_FONT.with(move |it| *it.borrow_mut() = Some(TextPainter::new(pgr_font, Some(game_font))));
    crash_log_stage("已加载 phigros.ttf");

    // 界面（菜单 / 设置页 / B0 / RKS 等原生页）的内置字体。
    //
    // 2026-10-23：用户要求**原生 UI 与 WebView 页统一字体** —— WebView 各页
    // （设置 / 曲库 / 开始页…）用的是 `kaichang/cmdysj.ttf`，所以这里改成优先加载
    // 根 `assets/cmdysj.ttf`（从 kaichang 复制来的同一份），原生界面终于和设置页同字体。
    //
    // 历史：`assets/pingfang.otf`（苹果 PingFang SC，不允许再分发）→
    // `assets/harmonyos.ttf`（HarmonyOS Sans SC，免费商用）→
    // `assets/ui-font.otf`（PingFang-Mod SC Medium）→ 现在 `cmdysj.ttf`。
    // 保留 ui-font.otf / harmonyos.ttf 作兜底：万一发布目录里没有 cmdysj.ttf，
    // 也只是字体回退，不会因为读不到文件而启动即退出。
    // 这跟游戏内文字用的 font.ttf 是两套字体，风格不同是有意的。
    // 设置里导入的自定义界面字体依然优先（写到 <data>/font.ttf）。
    // 兜底链 best-effort：cmdysj.ttf 必在；ui-font.otf / harmonyos.ttf 是历史兜底，
    // 精简包可以不带 —— 都没有就复用游戏字体，不能因为缺字体启动即退。
    let builtin_ui_font = match load_file("cmdysj.ttf").await {
        Ok(bytes) => match FontArc::try_from_vec(bytes) {
            Ok(font) => font,
            Err(err) => {
                warn!("cmdysj.ttf 解析失败（{err:?}），走兜底字体");
                fallback_ui_font(&game_font_for_ui).await?
            }
        },
        Err(err) => {
            warn!("cmdysj.ttf 读取失败（{err}），走兜底字体");
            fallback_ui_font(&game_font_for_ui).await?
        }
    };
    crash_log_stage("已加载 cmdysj.ttf（界面字体，与 WebView 页统一）");
    let custom_font = std::fs::read(format!("{dir}/font.ttf"))
        .ok()
        .and_then(|it| FontArc::try_from_vec(it).ok());
    async fn fallback_ui_font(game: &FontArc) -> anyhow::Result<FontArc> {
        for name in ["ui-font.otf", "harmonyos.ttf"] {
            if let Ok(bytes) = load_file(name).await {
                match FontArc::try_from_vec(bytes) {
                    Ok(f) => return Ok(f),
                    Err(err) => warn!("{name} 解析失败（{err:?}）"),
                }
            }
        }
        warn!("界面字体（cmdysj/ui-font/harmonyos）均不可用，复用游戏字体兜底");
        Ok(game.clone())
    }
    let has_custom_font = custom_font.is_some();
    let font = custom_font.unwrap_or_else(|| builtin_ui_font.clone());
    let mut painter = TextPainter::new(font.clone(), has_custom_font.then(|| builtin_ui_font.clone()));
    crash_log_stage("字体初始化完成，准备建 MainScene");

    // —— 离线渲染模式（`--render <谱面目录> <输出.mp4>`）——
    // 不进正常游戏流程：手动时间轴逐帧渲染谱面，管道喂 ffmpeg 出 mp4。
    // 这里放在字体初始化之后，因为文字绘制要用上面那套字体。
    match record::parse_args(std::env::args()) {
        Ok(Some(opts)) => {
            crash_log_stage("进入离线渲染模式");
            let result = record::run(opts, font.clone()).await;
            match &result {
                Ok(()) => crash_log_stage("离线渲染完成"),
                Err(err) => {
                    crash_log_stage(&format!("离线渲染失败: {err:?}"));
                    error!(?err, "render failed");
                }
            }
            return result;
        }
        Ok(None) => {}
        Err(err) => {
            // 参数写错就直接报错退出，别默默进游戏
            error!(?err, "bad render args");
            crash_log_stage(&format!("渲染参数错误: {err}"));
            return Err(err);
        }
    }

    // BOLD_FONT 的兜底也始终用内置界面字体，保证中文标题不受自定义字体影响
    let mut main = Main::new(Box::new(MainScene::new(builtin_ui_font).await?), TimeManager::default(), None).await?;
    crash_log_stage("MainScene 就绪，进入主循环");

    // 创建 WebView 宿主（Windows + `webui` feature 时，开始界面就是
    // `kaichang/index.html`；失败会自动回退到 macroquad 原生 UI）。
    webui::init_host();

    let tm = TimeManager::default();
    let mut fps_time = -1;

    const FPS_BUF_SIZE: usize = 60;
    let mut fps_times = VecDeque::<f32>::with_capacity(FPS_BUF_SIZE);
    let mut last_frame_start = f32::NAN;
    let mut fps_time_sum = 0.;

    let mut paused = false;
    let mut first_frame_logged = false;

    // iOS 的 CoreMotion 采集在下面的主循环里按开关惰性启动（玩家可能进游戏后才开开关）

    'app: loop {
        let frame_start = tm.real_time();
        if !last_frame_start.is_nan() {
            if fps_times.len() == FPS_BUF_SIZE {
                fps_time_sum -= fps_times.pop_front().unwrap();
            }
            let frame_time = frame_start as f32 - last_frame_start;
            fps_times.push_back(frame_time);
            fps_time_sum += frame_time;
        }
        last_frame_start = frame_start as f32;
        let res = || -> Result<()> {
            let signal = if paused {
                rx.recv_timeout(std::time::Duration::from_secs(1)).ok()
            } else {
                rx.try_recv().ok()
            };
            if let Some(msg) = signal {
                paused = msg;
                if msg {
                    main.pause()?;
                } else {
                    main.resume()?;
                }
            }
            if !paused {
                main.update()?;
                main.render(&mut painter)?;
            }
            prpr::ext::flush_pending_texture_deletions();
            Ok(())
        }();
        if let Err(err) = res {
            error!("uncaught error: {err:?}");
            show_error(err);
        }
        // 曲库页「渲染」按钮：接管渲染循环（阻塞几分钟，窗口上画进度条，不进视频）
        // 离线渲染：单条请求优先，其次批量队列；队列没跑完就一直占着渲染
        let queue_item = record::pop_queue();
        if let Some(opts) = record::take_pending_render().or_else(|| queue_item.as_ref().map(|it| it.opts.clone())) {
            let label = queue_item.as_ref().map(|it| it.label.clone()).unwrap_or_else(|| opts.out.clone());
            info!("render: 收到渲染请求（{label}），让位给离线渲染");
            webui::handoff_to_native();
            let ok = record::run(opts, font.clone()).await.is_ok();
            if !ok {
                error!("render failed");
            }
            if let Some(item) = queue_item {
                record::mark_queue_done(&item.label, ok);
            }
            // 队列还有 → 继续渲染下一条（不回 WebView）
            if record::queue_len() == 0 {
                // 渲染完回曲库（WebView 重新显示）
                webui::open_library();
            } else {
                info!("render: 队列剩 {} 条，继续", record::queue_len());
            }
            continue 'app;
        }
        if main.should_exit() {
            break 'app;
        }

        let t = tm.real_time();

        let fps_now = t as i32;
        if fps_now != fps_time {
            fps_time = fps_now;
            if fps_times.len() == FPS_BUF_SIZE {
                let actual_fps = 1. / (fps_time_sum / FPS_BUF_SIZE as f32);
                let current_fps = 1. / (t - frame_start);
                info!("FPS {} (capped at {})", current_fps as u32, actual_fps as u32);
            }
        }

        // iOS：每帧从 CoreMotion 取一次摇动强度喂给引擎（Android 是传感器回调里推的）。
        // 注意这里每帧都惰性 start 一次（内部有 nil 判断，重复调用是空操作）：
        // 因为玩家可能是启动之后才在设置里打开开关的，只在启动时 start 会漏掉这种情况。
        // 开关关闭时完全不碰传感器。
        #[cfg(target_os = "ios")]
        if get_data().config.motion_pause {
            let _ = motion::start();
            prpr::scene::set_motion_level(motion::level());
        }

        // iOS：姿态倾斜每帧取一次（和摇一摇共用一个 CMMotionManager，但独立于开关 ——
        // 视差是 UI 效果，不该被「摇一摇再玩」的开关连坐）。
        #[cfg(target_os = "ios")]
        {
            let _ = motion::start();
            match motion::tilt() {
                Some((x, y)) => prpr::scene::set_motion_tilt(x, y),
                None => prpr::scene::set_motion_tilt(f32::NAN, f32::NAN),
            }
        }

        // 第一帧渲染成功 = 游戏真的跑起来了，记一笔（只记一次）
        if !first_frame_logged {
            first_frame_logged = true;
            crash_log_stage("已渲染第一帧，游戏正常运行");
        }

        // While backgrounded the scene is paused; the blocking `recv_timeout`
        // above already parks this thread, so nothing extra is needed here.
        next_frame().await;
    }
    Ok(())
}

fn build_global_window_conf() -> Conf {
    let mut conf = build_conf();
    conf.window_title = "Phira".to_owned();
    conf.icon = Some(miniquad::conf::Icon {
        small: *include_bytes!("../icon/small"),
        medium: *include_bytes!("../icon/medium"),
        big: *include_bytes!("../icon/big"),
    });

    #[cfg(target_os = "windows")]
    {
        conf.fullscreen = dir::root()
            .ok()
            .and_then(|r| std::fs::read_to_string(std::path::Path::new(&r).join("data.json")).ok())
            .and_then(|s| serde_json::from_str::<Data>(&s).ok())
            .is_some_and(|d| d.config.fullscreen_mode);
    }

    conf
}

/// 保证游戏只会被启动一次：Android 外壳可能既由 miniquad 的 activityOnCreate 触发，
/// 又由 Java 侧显式调 startApp 兜底。
static APP_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Android：把 panic 信息也写进应用私有目录的 phira-crash.log 里。
///
/// 手机上没有 adb 时，abort 只会在 logcat 里留一段看不到的信息；Java 壳下次启动会
/// 把同一份文件弹出来显示，所以这里只要写进去就能看到原因。只装一次。
#[cfg(target_os = "android")]
fn install_panic_hook() {
    install_panic_hook_impl(false)
}

/// 再装一次 panic hook。
///
/// miniquad 在 `start()` 里会无条件 `set_hook` 一个只写 logcat 的 hook，把我们这个
/// 顶掉（在 android.rs 的 run() 里）。所以进主循环前必须重装：这时 `take_hook()`
/// 拿到的是 miniquad 的那个，我们的 hook 记完文件后继续调它，两边的信息都不丢。
#[cfg(target_os = "android")]
fn reinstall_panic_hook() {
    install_panic_hook_impl(true)
}

#[cfg(target_os = "android")]
fn install_panic_hook_impl(force: bool) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if !force && INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    INSTALLED.store(true, Ordering::SeqCst);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 直接把日志写到 setDataPath 给过来的目录下（和 Java 侧 CrashLog 同一个文件）。
        // 不能走 dir::root()：它在 DATA_PATH 还是 None 时会退化成 cwd 下的 "data"，
        // Android 上等于 /data/...（写不进去，日志就丢了）。
        crash_log_stage_panic(&format!("{info}"));
        default_hook(info);
    }));
}

/// panic hook 专用的写日志（单独一份是因为 panic hook 里不能分配锁以外的资源，
/// 而且要在写不进去时至少落一行到 logcat）。
#[cfg(target_os = "android")]
fn crash_log_stage_panic(msg: &str) {
    let dirs = crash_log_dirs();
    if dirs.is_empty() {
        eprintln!("[phira panic] {msg}");
        return;
    }
    for dir in dirs {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("phira-crash.log")) {
            use std::io::Write;
            let _ = writeln!(file, "[rust panic] {msg}");
        }
    }
}

/// 通用诊断输出：同时进 tracing 日志和 phira-crash.log。
///
/// 原来是 `webui::diag`，WebView 整套移除后挪到 crate 根，供 IME / 输入框等
/// 需要「手机上看不到 adb 时也能留痕」的地方使用。
pub(crate) fn diag(msg: &str) {
    tracing::info!("[diag] {msg}");
    crash_log_stage(&format!("[diag] {msg}"));
}

/// 把启动阶段写进应用私有目录的 phira-crash.log（和 Java 侧 CrashLog、panic hook 同一个文件）。
///
/// 手机上没有 adb 时，这是唯一能看到「原生侧走到哪一步」的办法：
/// Java 侧记的是 onCreate / 加载 so / 交目录，这里接上四之后的部分
/// （建窗口、加载资源、第一帧）。
fn crash_log_stage(msg: &str) {
    let dirs = crash_log_dirs();
    if dirs.is_empty() {
        // 一个能写的地方都没有时至少让 logcat 里能看到（Rust 的 stderr 在安卓上
        // 会进 logcat）。以前这里静默丢弃，于是「日志里一行原生侧的字都没有」
        // 这个现象本身就成了唯一的线索。
        eprintln!("[phira] {msg}");
        return;
    }
    for dir in dirs {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("phira-crash.log")) {
            use std::io::Write;
            let _ = writeln!(file, "[rust stage] {msg}");
        }
    }
}

/// 崩溃日志要写的位置。
/// Android：<filesDir>/data（Java 侧 CrashLog 会弹出来显示），外加一份到外部私有目录
/// （/sdcard/Android/data/<包名>/files，免 root 用文件管理器就能直接翻出来）。
/// iOS：除了 <Library>/data 之外再写一份到 Documents —— iOS 的 Library 在
/// 「文件」App 里看不到，写进 Documents 并开启 UIFileSharingEnabled 才能直接翻看。
fn crash_log_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(root) = DATA_PATH.lock().unwrap().clone() {
        // 只认绝对路径。JNI 那边一旦没读对（见 read_java_string 的注释），
        // root 会是 "<JNI Not Initialized>" 这种相对路径，写下去等于写到 cwd，
        // 静悄悄什么都留不下 —— 不如直接当成「没有日志目录」。
        let path = std::path::Path::new(&root);
        if path.is_absolute() {
            dirs.push(path.join("data"));
            #[cfg(target_os = "android")]
            if let Some(ext) = android_external_dir(&root) {
                if !dirs.contains(&ext) {
                    dirs.push(ext);
                }
            }
        }
    }
    #[cfg(target_os = "ios")]
    {
        use objc2_foundation::{NSSearchPathDirectory, NSSearchPathDomainMask, NSSearchPathForDirectoriesInDomains};

        let documents = NSSearchPathForDirectoriesInDomains(NSSearchPathDirectory::DocumentDirectory, NSSearchPathDomainMask::UserDomainMask, true);
        if let Some(path) = documents.firstObject() {
            dirs.push(std::path::PathBuf::from(path.to_string()));
        }
    }
    // ⚠️ 桌面（Windows / macOS / Linux）：`DATA_PATH` 永远是 `None`（那只有 JNI / ObjC 会设），
    // 所以上面那个分支在桌面**一条都不成立** → `dirs` 为空 → `diag()` 只能 `eprintln!`，
    // 而桌面上 stderr 通常没人看（不是从终端启动的话直接丢掉）。
    // 结果就是「Windows 上一个诊断文件都没有」，排查只能靠猜。这里补上：
    // 退化成 `dir::root()`（桌面 = `<cwd>/data`），日志落在存档目录里。
    #[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
    if dirs.is_empty() {
        if let Ok(root) = crate::dir::root() {
            dirs.push(std::path::PathBuf::from(root));
        }
    }
    dirs
}

/// 由 `/data/user/<user>/<pkg>/files` 推出 `/storage/emulated/<user>/Android/data/<pkg>/files`。
#[cfg(target_os = "android")]
fn android_external_dir(data_path: &str) -> Option<std::path::PathBuf> {
    let parts: Vec<&str> = data_path.split('/').collect();
    // ["", "data", "user", "<user>", "<pkg>", "files", ..]
    if parts.len() >= 5 && parts[1] == "data" && parts[2] == "user" {
        let (user, pkg) = (parts[3], parts[4]);
        if !user.is_empty() && !pkg.is_empty() {
            return Some(std::path::PathBuf::from(format!("/storage/emulated/{user}/Android/data/{pkg}/files")));
        }
    }
    None
}

#[no_mangle]
pub extern "C" fn quad_main() {
    #[cfg(target_os = "android")]
    install_panic_hook();
    if APP_STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    crash_log_stage("quad_main 进入，准备建窗口");
    macroquad::Window::from_config(build_global_window_conf(), async {
        // miniquad 的 start() 会把我们的 panic hook 顶掉（只写 logcat），这里重装回来
        #[cfg(target_os = "android")]
        reinstall_panic_hook();
        if let Err(err) = the_main().await {
            crash_log_stage(&format!("the_main 返回错误: {err:?}"));
            error!(?err, "global error");
        }
        crash_log_stage("游戏主循环结束");
    });
    cleanup_audio();
}

/// Android 外壳的兜底启动入口（导出名对应 quad_native.QuadNative.startApp）。
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_startApp(_env: EnvUnowned, _class: JClass) {
    quad_main();
}

fn on_pause_resume(pause: bool) {
    if let Some(tx) = MESSAGES_TX.lock().unwrap().as_mut() {
        let _ = tx.send(pause);
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_initializeEnvironment(env: EnvUnowned, _class: JClass) {
    install_panic_hook();
    unsafe {
        inputbox::backend::Android::initialize_raw(env.as_raw()).unwrap();
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_prprActivityOnPause(_env: EnvUnowned, _class: JClass) {
    if let Some(tx) = MESSAGES_TX.lock().unwrap().as_mut() {
        let _ = tx.send(true);
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_prprActivityOnResume(_env: EnvUnowned, _class: JClass) {
    if let Some(tx) = MESSAGES_TX.lock().unwrap().as_mut() {
        let _ = tx.send(false);
    }
    // ⚠️ 前后台切换后系统可能把帧率票收回去（省电/分屏/浮窗都会），恢复时重新申请。
    // 失败只记日志，不影响游戏继续跑。
    apply_preferred_refresh_rate();
}

/// Java `MainActivity.onCreate` 收到 `-e render` intent 时调这里（见 `record.rs`）。
///
/// 手机上没法传命令行参数，所以用 adb intent 触发离线渲染：
///   `adb shell am start -n org.flos.phira.modded/.MainActivity -e render "/sdcard/charts/X|X.mp4"`
#[cfg(target_os = "android")]
#[export_name = "Java_org_flos_phira_modded_MainActivity_nativeRenderArg"]
extern "system" fn android_render_arg(mut env: EnvUnowned, _: JClass, arg: JString) {
    let Some(s) = read_java_string(&mut env, &arg, "render arg") else { return };
    tracing::info!("[render] 收到 intent 渲染请求: {s}");
    *record::ANDROID_RENDER_ARG.lock().unwrap() = Some(s);
}

// ============ 刷新率（从 Phira-Pro 移植）============

/// 设备实际可达的最大刷新率（Hz）。Java 侧在发现变化时上报（只上报有变化的那个值）。
#[cfg(target_os = "android")]
pub static MAX_DEVICE_REFRESH_RATE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(any(target_os = "android", target_os = "ios"))]
pub(crate) fn set_refresh_rate(rate: u16) -> anyhow::Result<()> {
    #[cfg(target_os = "android")]
    set_android_refresh_rate(rate)?;
    Ok(())
}

/// 通过 JNI 调 `MainActivity.setRequestedFrameRate`（Java 侧会校验档位合法性）。
#[cfg(target_os = "android")]
pub(crate) fn set_android_refresh_rate(refresh_rate: u16) -> anyhow::Result<()> {
    use anyhow::Context;
    use jni::{jni_sig, jni_str, objects::JObject, vm::JavaVM};
    JavaVM::singleton()
        .context("Android Java VM is unavailable")?
        .attach_current_thread(|env| -> jni::errors::Result<()> {
            let activity = unsafe { JObject::from_raw(env, ndk_context::android_context().context() as _) };
            env.call_method(activity, jni_str!("setRequestedFrameRate"), jni_sig!("(I)V"), &[i32::from(refresh_rate).into()])?;
            Ok(())
        })
        .map_err(anyhow::Error::from)
}

/// 按用户配置申请刷新率。DATA 还没加载时用 120（与 Java 侧初始值一致）。
#[cfg(target_os = "android")]
#[allow(static_mut_refs)]
pub(crate) fn apply_preferred_refresh_rate() {
    // ⚠️ onResume 可能早于数据加载（Java 侧注释里明确说过），这里不能 panic：
    // 用裸的 DATA 判断有没有，没有就用 Java 侧一致的默认值 120。
    let rate = unsafe { DATA.as_ref() }.map_or(120, |d| d.config.preferred_refresh_rate);
    if let Err(err) = set_refresh_rate(rate) {
        tracing::warn!("申请刷新率 {rate}Hz 失败：{err:#}");
    }
}

/// Java 上报设备最大刷新率（含 alternativeRefreshRates）。
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setMaxRefreshRate(_env: EnvUnowned, _class: JClass, refresh_rate: jni::sys::jfloat) {
    if refresh_rate.is_finite() && refresh_rate > 0.0 {
        MAX_DEVICE_REFRESH_RATE.store(refresh_rate.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_prprActivityOnDestroy(_env: EnvUnowned, _class: JClass) {
    std::process::exit(0);
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setDataPath(mut env: EnvUnowned, _class: JClass, path: JString) {
    // 只能用 read_java_string 读（见它的注释：直接 to_string 会拿到占位字符串）
    if let Some(path) = read_java_string(&mut env, &path, "setDataPath") {
        *DATA_PATH.lock().unwrap() = Some(path.clone());
        // 设置完之后再记一笔，这样这行会落到正确的日志目录里
        crash_log_stage(&format!("数据目录已设置: {path}"));
    } else {
        crash_log_stage("setDataPath 没拿到有效路径，DATA_PATH 保持为空");
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setTempDir(mut env: EnvUnowned, _class: JClass, path: JString) {
    if let Some(path) = read_java_string(&mut env, &path, "setTempDir") {
        std::env::set_var("TMPDIR", path.clone());
        *CACHE_DIR.lock().unwrap() = Some(path.clone());
        crash_log_stage(&format!("临时目录已设置: {path}"));
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setDpi(_env: EnvUnowned, _class: JClass, dpi: jint) {
    prpr::core::DPI_VALUE.store(dpi as _, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setChosenFile(mut env: EnvUnowned, _class: JClass, file: JString) {
    use prpr::scene::CHOSEN_FILE;
    if let Some(file) = read_java_string(&mut env, &file, "setChosenFile") {
        CHOSEN_FILE.lock().unwrap().1 = Some(file);
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_markImport(_env: EnvUnowned, _class: JClass) {
    use prpr::scene::CHOSEN_FILE;

    CHOSEN_FILE.lock().unwrap().0 = Some("_import".to_owned());
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_markImportRespack(_env: EnvUnowned, _class: JClass) {
    use prpr::scene::CHOSEN_FILE;

    CHOSEN_FILE.lock().unwrap().0 = Some("_import_respack".to_owned());
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_setInputText(mut env: EnvUnowned, _class: JClass, text: JString) {
    use prpr::scene::INPUT_TEXT;
    if let Some(text) = read_java_string(&mut env, &text, "setInputText") {
        INPUT_TEXT.lock().unwrap().1 = Some(text);
    }
}

/// Android 传感器（加速度计）上报当前摇动强度，单位 g（线性加速度，已去掉重力）。
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_onMotion(_env: EnvUnowned, _class: JClass, level: jni::sys::jfloat) {
    prpr::scene::set_motion_level(level);
}

/// Android 姿态传感器上报设备倾斜（相对启动中位，已归一化到 [-1, 1]）。
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_onTilt(_env: EnvUnowned, _class: JClass, x: jni::sys::jfloat, y: jni::sys::jfloat) {
    prpr::scene::set_motion_tilt(x, y);
}

/// Android 输入法一次删除 N 个字符（`InputConnection.deleteSurroundingText` 的 beforeLength）。
///
/// 走 [`crate::ime`] 的**计数通道**，而不是在 Java 侧连发 N 次 `surfaceOnKeyDown(KEYCODE_DEL)`
/// —— 那 N 个事件会在同一帧入队，而 macroquad 的 `keys_pressed` 是 `HashSet`，
/// 同键多次按下会坍缩成一次，结果只删掉 1 个字符（原因详见 `ime::push_backspaces`）。
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_imeBackspace(_env: EnvUnowned, _class: JClass, n: jni::sys::jint) {
    crate::ime::push_backspaces(n.max(0) as usize);
}

/// Android 输入法提交一个字符（`InputConnection.commitText` 的 UTF-32 码点）。
///
/// 走 [`crate::ime`] 的 `IME_BUFFER` 互斥锁通道，而不是 macroquad 的 `chars_pressed`
/// 裸队列（`surfaceOnCharacter` 那条路）—— 后者在 IME 线程 push / 渲染线程 pop 之间无同步，
/// 是「移动端输入框有时把整个软件卡死」的根因。详见 `ime::push_char`。
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_imeChar(_env: EnvUnowned, _class: JClass, cp: jni::sys::jint) {
    crate::ime::push_char(cp.max(0) as u32);
}

/// Android 输入法「完成 / 确定」（`InputConnection.performEditorAction`）。
///
/// 走 [`crate::ime`] 的 `IME_BUFFER` 互斥锁通道，与 `imeChar` / `imeBackspace` 同一条
/// 安全路径。详见 `ime::push_confirm`。
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_imeConfirm(_env: EnvUnowned, _class: JClass) {
    crate::ime::push_confirm();
}

/* ------------------------- HYKB 凭据 ------------------------- */

/// Credentials obtained from the native HYKB (好游快爆) login SDK.
pub struct HykbCredential {
    /// SDK result code: 0 on success, otherwise an error / user cancellation.
    pub code: i32,
    pub uid: i64,
    pub nick: String,
    pub access_token: String,
}

impl HykbCredential {
    /// Map the SDK result code to an error, or yield the credential on success.
    /// Centralizes the code → user-facing message translation shared by every
    /// HYKB login/bind entry point.
    #[cfg(feature = "hykb")]
    pub fn ok_or_err(self) -> Result<Self> {
        if self.code == 0 {
            Ok(self)
        } else {
            // A non-zero code is any failure the HYKB SDK reports: 2001 auth
            // failed, 2002 login failed, 2003 cancelled, 2004 exception, 2005
            // developer-requested exit / account logout. A HYKB build mandates a
            // valid, matching HYKB session, so every one of these must tear the
            // in-game session down — otherwise cancelling the HYKB prompt during
            // a silent re-verify would leave the player signed in and bypass the
            // gate entirely.
            force_logout();
            anyhow::bail!("{}", crate::ttl!("hykb-login-cancelled"))
        }
    }
}

/// Slot for the pending HYKB login result. The native callback fulfills it.
static HYKB_TX: Mutex<Option<tokio::sync::oneshot::Sender<HykbCredential>>> = Mutex::new(None);

/// Call a no-arg `void` method on the Android host activity (the HYKB shell).
#[cfg(all(target_os = "android", feature = "hykb"))]
fn call_activity_void(method: &'static jni::strings::JNIStr) {
    use jni::{jni_sig, objects::JObject, vm::JavaVM};

    JavaVM::singleton()
        .unwrap()
        .attach_current_thread(|env| -> jni::errors::Result<()> {
            let ctx = unsafe { JObject::from_raw(env, ndk_context::android_context().context() as _) };
            env.call_method(ctx, method, jni_sig!("()V"), &[])?;
            Ok(())
        })
        .unwrap();
}

/// Ask the Android shell to pop the HYKB account picker (`MainActivity.hykbSwitchAccount`).
/// Used by the explicit login / switch-account flow.
#[cfg(all(target_os = "android", feature = "hykb"))]
fn request_hykb_login() {
    call_activity_void(jni::jni_str!("hykbSwitchAccount"));
}

#[cfg(not(all(target_os = "android", feature = "hykb")))]
fn request_hykb_login() {}

/// Ask the Android shell to sign in using the cached HYKB account without
/// popping the picker (`MainActivity.hykbLogin`). The credentials the SDK
/// reports flow back through `HYKB_TX`, so the caller can verify them against
/// the restored Phira session. Used by the silent startup restore.
#[cfg(all(target_os = "android", feature = "hykb"))]
fn request_hykb_login_silent() {
    call_activity_void(jni::jni_str!("hykbLogin"));
}

#[cfg(not(all(target_os = "android", feature = "hykb")))]
fn request_hykb_login_silent() {}

/// Tell the native HYKB SDK to sign out (`MainActivity.hykbLogout`). Called when the
/// player logs out from their profile.
#[cfg(all(target_os = "android", feature = "hykb"))]
pub fn hykb_logout() {
    call_activity_void(jni::jni_str!("hykbLogout"));
}

#[cfg(not(all(target_os = "android", feature = "hykb")))]
pub fn hykb_logout() {}

/// Tear down the local session: sign out of the native HYKB SDK, clear the
/// stored account and tokens, then re-sync. Shared by every path that must
/// reject a login — a failed/cancelled HYKB verification, a uid mismatch, or
/// the player logging out from their profile.
pub fn force_logout() {
    hykb_logout();
    get_data_mut().me = None;
    get_data_mut().tokens = None;
    let _ = save_data();
    sync_data();
}

/// Trigger the native HYKB login and await its credentials.
#[allow(unused)]
pub async fn obtain_hykb_credential() -> Result<HykbCredential> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    *HYKB_TX.lock().unwrap() = Some(tx);
    request_hykb_login();
    let cred = rx.await.map_err(|_| anyhow::anyhow!("hykb login cancelled"))?;
    Ok(cred)
}

/// Silently restore the HYKB session from the cached account and await its
/// credentials. Unlike [`obtain_hykb_credential`], this does not pop the account
/// picker; used by the blocking startup check to verify the restored session.
#[allow(unused)]
pub async fn obtain_hykb_credential_silent() -> Result<HykbCredential> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    *HYKB_TX.lock().unwrap() = Some(tx);
    request_hykb_login_silent();
    let cred = rx.await.map_err(|_| anyhow::anyhow!("hykb login cancelled"))?;
    Ok(cred)
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_quad_1native_QuadNative_hykbLoginCallback(
    mut env: EnvUnowned,
    _class: JClass,
    code: jint,
    uid: jni::sys::jlong,
    nick: JString,
    access_token: JString,
) {
    let nick = read_java_string(&mut env, &nick, "hykbLoginCallback(nick)").unwrap_or_default();
    let access_token = read_java_string(&mut env, &access_token, "hykbLoginCallback(access_token)").unwrap_or_default();
    if let Some(tx) = HYKB_TX.lock().unwrap().take() {
        let _ = tx.send(HykbCredential {
            code: code as i32,
            uid: uid as i64,
            nick,
            access_token,
        });
    } else if code == 2005 {
        // No login is in flight, so this is the SDK's asynchronous
        // anti-addiction "exit game" action: the player hit a play-time limit
        // and chose to quit from the SDK's own dialog. Honor it by exiting.
        // Other async codes (e.g. 2008 "continue playing") are handled inside
        // the SDK and need no response here. A request-less success (code 0, the
        // SDK switching accounts on its own) is likewise ignored: any signed-in
        // HYKB account is accepted, so a switch no longer tears the session down.
    }
}

#[cfg(target_env = "ohos")]
#[napi]
pub fn set_input_text(text: String) {
    use prpr::scene::INPUT_TEXT;
    INPUT_TEXT.lock().unwrap().1 = Some(text);
}

#[cfg(target_env = "ohos")]
#[napi]
pub fn set_chosen_file(file: String) {
    use prpr::scene::CHOSEN_FILE;
    CHOSEN_FILE.lock().unwrap().1 = Some(file);
}

#[cfg(target_env = "ohos")]
#[napi]
pub fn mark_auto_import() {
    use prpr::scene::CHOSEN_FILE;
    CHOSEN_FILE.lock().unwrap().0 = Some("_import_auto".to_owned());
}

#[cfg(target_env = "ohos")]
#[napi]
pub fn on_foreground() {
    if let Some(tx) = MESSAGES_TX.lock().unwrap().as_mut() {
        let _ = tx.send(false);
    }
}

#[cfg(target_env = "ohos")]
#[napi]
pub fn on_background() {
    if let Some(tx) = MESSAGES_TX.lock().unwrap().as_mut() {
        let _ = tx.send(true);
    }
}
