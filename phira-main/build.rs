// Windows 下给 exe 嵌入图标（资源已预编译为 assets/app.res，MSVC link 直接接受；
// 预编译命令：mingw windres assets/app.rc -O res -o assets/app.res）。
// 其它平台空操作。
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let res = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/app.res");
    if res.exists() {
        println!("cargo:rerun-if-changed={}", res.display());
        println!("cargo:rustc-link-arg-bins={}", res.display());
    }
}
