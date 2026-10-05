//! 开始页（`phira/ui/index.html` 及其素材）的内嵌资源表。
//!
//! 从 `webui_windows.rs` 里提出来共用 —— 原来这套 `include_str!` / `include_bytes!`
//! 只挂在 Windows 宿主上，Android 宿主没法复用（那就得把资源各拷一份）。
//! 资源是**编译期**打进来的，和平台无关，所以放在这里最合适。
//!
//! Windows 走 `webui://` 自定义协议、Android 走 Java 内存资源表，两边都从这里取字节。

/// 文本资源（`include_str!`，天然是 UTF-8）。
///
/// 只有 **Android** 宿主用得到这张清单（它要把每份资源逐个推给 Java 的内存资源表）；
/// Windows 走 `webui://` 自定义协议、iOS 走内联单文件 HTML，都不需要遍历。
#[cfg(target_os = "android")]
pub const TEXT_ASSETS: &[&str] = &["index.html", "style.css", "app.js", "bridge.js"];

/// 二进制资源（`include_bytes!`）。用途同 [`TEXT_ASSETS`]，仅 Android 需要。
#[cfg(target_os = "android")]
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

/// 把开始页做成**完全自包含的单文件 HTML**：所有 `./assets/*` 引用换成 base64 data URI。
///
/// # 为什么 iOS 需要这个
///
/// iOS 上没法像 Windows（`webui://` 自定义协议）或 Android（`shouldInterceptRequest`）
/// 那样给 WebView 喂「自定义来源」的资源 —— `WKWebView` 那套要写 `WKURLSchemeHandler`
/// 协议实现，成本与风险都高。而 `file://` 也不行：
///
/// `index.html:415` 有 `gl.texImage2D(gl.TEXTURE_2D, ..., img)`，其中 `img.src` 是
/// `./assets/TriGridBase.png`（`index.html:428`）。`file://` 下 WebKit 把它判成跨源，
/// `texImage2D` 直接抛 SecurityError，**整个背景光波消失** —— 这个坑工程文档里已经记过一次。
///
/// 内联成 data URI 后，页面是同源文档，既没有跨源问题也不需要任何自定义协议。
/// 代价是字符串变大（字体 13MB → base64 ~18MB），只在加载那一刻占内存。
///
/// 内联逻辑刻意放在 Rust 侧（而不是 ObjC 里），是为了**能在 Windows 上编译验证**。
///
/// 只有 iOS 宿主用得到（Windows 走自定义协议、Android 走 Java 内存表），所以按平台门控。
#[cfg(target_os = "ios")]
pub fn build_inlined_index_html() -> Option<String> {
    let (html_bytes, _) = load_asset("index.html")?;
    let mut out = String::from_utf8(html_bytes).ok()?;

    // 引用形式与 `index.html` 里实际出现的写法一一对应：
    //   `url('./assets/cmdysj.ttf')`（CSS font-face）
    //   `src="./assets/3.png"` / `src="./assets/phirc.png"`（img 标签）
    //   `img.src = './assets/TriGridBase.png';`（JS）
    for name in ["TriGridBase.png", "phirc.png", "3.png", "cmdysj.ttf"] {
        let Some((data, mime)) = load_asset(&format!("assets/{name}")) else {
            continue;
        };
        let uri = format!("data:{mime};base64,{}", base64_encode(&data));
        out = out.replace(&format!("'./assets/{name}'"), &format!("'{uri}'"));
        out = out.replace(&format!("\"./assets/{name}\""), &format!("\"{uri}\""));
    }
    Some(out)
}

/// 标准 base64（无换行）。不引额外依赖：就这几行。仅 iOS 内联用，故按平台门控。
#[cfg(target_os = "ios")]
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = ((chunk[0] as u32) << 16) | (b1 << 8) | b2;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}
