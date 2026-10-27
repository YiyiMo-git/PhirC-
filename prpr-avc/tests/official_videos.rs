//! 官方内置视频解码冒烟测试。
//!
//! 验证 `assets/video/`（从原游戏 UI 包导出的 9 个 mp4）能被 prpr-avc 解码：
//! 打开 → 流格式（宽高/时长）→ 后台解码线程真的产出帧。
//! 不依赖 GL 上下文，只走 FFmpeg 解码链路。
//!
//! 运行：`PHIRA_VIDEO_DIR=D:/Phigros/UI/VideoClip cargo test -p prpr-avc --test official_videos -- --nocapture`
//! 目录不存在时跳过（CI 上没有原游戏素材）。

use prpr_avc::{AVPixelFormat, Video};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[test]
fn official_videos_decode() {
    let dir = std::env::var("PHIRA_VIDEO_DIR").unwrap_or_else(|_| "../UI/VideoClip".to_owned());
    let dir = Path::new(&dir);
    if !dir.exists() {
        eprintln!("SKIP: {dir:?} 不存在（设 PHIRA_VIDEO_DIR 指向 mp4 目录）");
        return;
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("读目录失败")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4")))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "{dir:?} 里没有 mp4");

    let mut checked = 0usize;
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        eprintln!("[1] open {name}");
        let video = Video::open(path.to_str().unwrap(), AVPixelFormat::YUV420P).unwrap_or_else(|e| panic!("{name}: 打开失败 {e:?}"));
        eprintln!("[2] opened {name}");
        let fmt = video.stream_format();
        eprintln!("[3] fmt {}x{}", fmt.width, fmt.height);
        assert!(fmt.width > 0 && fmt.height > 0, "{name}: 流尺寸异常 {}x{}", fmt.width, fmt.height);
        let dur = video.duration();
        eprintln!("[4] dur {dur}");
        assert!(dur.is_finite() && dur > 0.1, "{name}: 时长异常 {dur}");

        // 后台解码线程异步产帧：轮询 with_frame，统计不同 pts
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen_pts: Vec<i64> = Vec::new();
        while Instant::now() < deadline && seen_pts.len() < 3 {
            video.with_frame(|_frame, pts| {
                if !seen_pts.contains(&pts) {
                    seen_pts.push(pts);
                }
            });
            if seen_pts.len() < 3 {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        assert!(!seen_pts.is_empty(), "{name}: 5 秒内一帧都没解出来（解码链路坏了？）");
        println!("OK {name}: {}x{} {:.2}s 解出 {} 帧", fmt.width, fmt.height, dur, seen_pts.len());
        checked += 1;
    }
    assert_eq!(checked, files.len(), "有文件没被测到");
    println!("全部 {checked} 个官方视频解码通过");
}

// ABI 探针：确认静态库的 FFmpeg 版本 + 最基本的上下文分配能不能跑通
#[test]
fn abi_probe() {
    extern "C" {
        fn avformat_version() -> u32;
        fn avcodec_version() -> u32;
        fn avformat_alloc_context() -> *mut std::ffi::c_void;
        fn avformat_free_context(s: *mut std::ffi::c_void);
    }
    unsafe {
        let v = avformat_version();
        let c = avcodec_version();
        eprintln!("avformat_version={} ({}.{}.{})", v, v >> 16, (v >> 8) & 0xff, v & 0xff);
        eprintln!("avcodec_version={} ({}.{}.{})", c, c >> 16, (c >> 8) & 0xff, c & 0xff);
        let ctx = avformat_alloc_context();
        eprintln!("alloc_context={ctx:p}");
        assert!(!ctx.is_null(), "avformat_alloc_context 返回空");
        avformat_free_context(ctx);
        eprintln!("free_context ok");
    }
}

// 内存布局探针：dump AVStream 前 64 字节
#[test]
fn layout_probe() {
    extern "C" {
        fn avformat_alloc_context() -> *mut u8;
        fn avformat_open_input(ctx: *mut *mut u8, url: *const std::os::raw::c_char, fmt: *const u8, opts: *mut *mut u8) -> i32;
        fn avformat_find_stream_info(ctx: *mut u8, opts: *mut *mut u8) -> i32;
    }
    let path = std::env::var("PHIRA_VIDEO_DIR").unwrap_or_else(|_| "../UI/VideoClip".to_owned());
    let file = format!("{path}/Unlock.mp4");
    unsafe {
        let mut ctx = avformat_alloc_context();
        let url = std::ffi::CString::new(file).unwrap();
        let r = avformat_open_input(&mut ctx, url.as_ptr(), std::ptr::null(), std::ptr::null_mut());
        eprintln!("open_input ret={r}");
        let r2 = avformat_find_stream_info(ctx, std::ptr::null_mut());
        eprintln!("find_stream_info ret={r2}");
        // AVFormatContext: nb_streams 在 0x2c，streams 在 0x30（FFmpeg 5.x x64）
        let nb = *(ctx.add(0x2c) as *const u32);
        let streams = *(ctx.add(0x30) as *const *const *const u8);
        eprintln!("nb_streams={nb} streams={streams:p}");
        for i in 0..nb.min(2) {
            let st = *streams.add(i as usize);
            eprintln!("stream[{i}] ptr={st:p}");
            let q = st as *const u64;
            let vals: Vec<u64> = (0..8).map(|k| *q.add(k)).collect();
            eprintln!("  qwords: {:016x?}", vals);
            // AVStream: av_class(0) index(8) id(12) codecpar(16)
            let index = *(st.add(8) as *const i32);
            let id = *(st.add(12) as *const i32);
            let cp = *(st.add(16) as *const *const u8);
            eprintln!("  index={index} id={id} codecpar={cp:p}");
            if !cp.is_null() {
                let ctype = *(cp as *const i32);
                eprintln!("  codecpar->codec_type={ctype}");
            }
        }
    }
}
