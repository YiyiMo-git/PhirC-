use std::path::Path;

fn main() {
    let libs_dir = std::env::var("PRPR_AVC_LIBS").unwrap_or_else(|_| format!("{}/static-lib", std::env::var("CARGO_MANIFEST_DIR").unwrap()));
    let libs_path = Path::new(&libs_dir).join(std::env::var("TARGET").unwrap());
    let libs_path = libs_path.display();
    println!("cargo:rustc-link-search={libs_path}");
    // ⚠️ `-lz` 是 Unix 写法：MSVC 上没有 `z.lib`（LNK1181），Windows 的预编译包
    // 也没带 zlib（FFmpeg 的 zlib 依赖在该包里是关闭/内置的）。上游 CI 没有 Windows
    // 任务，所以这个问题一直没暴露；本地 Windows 构建必须跳过。
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        println!("cargo:rustc-link-lib=z");
    } else {
        // FFmpeg 的 av_get_random_seed 在 Windows 上走 BCrypt（BCryptOpenAlgorithmProvider 等），
        // 必须显式链 bcrypt.lib —— 否则单独构建 prpr-avc（测试/其它消费方）会 LNK2019。
        println!("cargo:rustc-link-lib=bcrypt");
    }
    println!("cargo:rerun-if-changed={libs_path}");
}
