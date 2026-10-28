//! 官方视频回落目录：APK / 发行包里可以不带 86MB 的内置视频（`assets/video/`）。
//!
//! 回落链：谱包自带 → [`read`]（数据目录 `video/`）→ 内置 assets。
//! 想在精简包上恢复解锁动画 / 谱面视频：把 mp4 放进 `<数据目录>/video/` 即可，
//! 或打包时用 `-WithVideos` 把它们重新塞回去。

use std::path::PathBuf;
use std::sync::Mutex;

static FALLBACK_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 启动时设置一次（phira 用 `<数据根>/video`）。
pub fn set_fallback_dir(path: impl Into<PathBuf>) {
    if let Ok(mut g) = FALLBACK_DIR.lock() {
        *g = Some(path.into());
    }
}

/// 按文件名从回落目录读视频；目录没设置或文件不存在都返回 None。
pub fn read(name: &str) -> Option<Vec<u8>> {
    // 挡路径分隔符：name 来自谱面 / info.yml，只允许纯文件名
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    let dir = FALLBACK_DIR.lock().ok()?.clone()?;
    std::fs::read(dir.join(name)).ok()
}
