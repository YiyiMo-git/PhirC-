//! 开始页（`phira/ui/index.html` 及其素材）的内嵌资源表。
//!
//! 从 `webui_windows.rs` 里提出来共用 —— 原来这套 `include_str!` / `include_bytes!`
//! 只挂在 Windows 宿主上，Android 宿主没法复用（那就得把资源各拷一份）。
//! 资源是**编译期**打进来的，和平台无关，所以放在这里最合适。
//!
//! Windows 走 `webui://` 自定义协议、Android 走 Java 内存资源表，两边都从这里取字节。

/// 文本资源（`include_str!`，天然是 UTF-8）。
pub const TEXT_ASSETS: &[&str] = &["index.html", "style.css", "app.js", "bridge.js"];

/// 二进制资源（`include_bytes!`）。
pub const BINARY_ASSETS: &[&str] = &["assets/TriGridBase.png", "assets/phirc.png", "assets/3.png", "assets/cmdysj.ttf"];

/// 按相对路径取一份内嵌资源，连同它的 MIME 类型。
///
/// 路径与 `webui://` / `https://webui.local/` 里的路径一致（不带前导斜杠）。
pub fn load_asset(path: &str) -> Option<(Vec<u8>, &'static str)> {
    let (data, mime) = match path {
        "index.html" => (include_str!("../ui/index.html").as_bytes(), "text/html; charset=utf-8"),
        "style.css" => (include_str!("../ui/style.css").as_bytes(), "text/css; charset=utf-8"),
        "app.js" => (include_str!("../ui/app.js").as_bytes(), "text/javascript; charset=utf-8"),
        "bridge.js" => (include_str!("../ui/bridge.js").as_bytes(), "text/javascript; charset=utf-8"),
        // include_bytes! 各自带具体数组长度，直接用会在 match 里产生「长度不同的 &[u8; N]」
        // 冲突；用 .as_slice() 统一成 &[u8]。
        "assets/TriGridBase.png" => (include_bytes!("../ui/assets/TriGridBase.png").as_slice(), "image/png"),
        "assets/phirc.png" => (include_bytes!("../ui/assets/phirc.png").as_slice(), "image/png"),
        "assets/3.png" => (include_bytes!("../ui/assets/3.png").as_slice(), "image/png"),
        "assets/cmdysj.ttf" => (include_bytes!("../ui/assets/cmdysj.ttf").as_slice(), "font/ttf"),
        _ => return None,
    };
    Some((data.to_vec(), mime))
}
