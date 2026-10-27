fn main() {
    // tauri-build 仅在包含 tauri 特性时运行（generate_context 依赖 tauri.conf.json）
    if std::env::var("CARGO_FEATURE_TAURI").is_ok() {
        tauri_build::build();
    }
}
