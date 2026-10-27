//! bp-host —— 赛事主持工具（Ban/Pick 选图 + 干扰互动层）。
//!
//! 独立 workspace：不并进 phira 主 workspace，避免打 App / CI 被牵连。
//! 模块拓扑（无环）：
//!
//! ```text
//! model ─→ {net/protocol, bp/interference} ─→ bp/state_machine ─→ service
//! config ─→ service / net-server / persistence
//! service ─→ net/{server,session} + commands（Tauri）/ persistence/sqlite
//! ```

pub mod bp;
pub mod config;
pub mod model;
pub mod net;
pub mod persistence;
pub mod service;

#[cfg(feature = "tauri")]
pub mod commands;

/// 进程入口。Tauri 构建产物走桌面应用；`--no-default-features` 构建产物走
/// standalone 服务器（同 service 层，等价于「只有服务器、无本地控制台」的运行形态）。
pub fn main_entry() {
    #[cfg(feature = "tauri")]
    {
        commands::run_tauri();
    }
    #[cfg(not(feature = "tauri"))]
    {
        run_standalone();
    }
}

/// standalone 运行形态（快速联调 / 无 Tauri 环境）：
/// 从配置读监听地址 + 房间码，起服务与计时器；缺配置文件直接退出并给指引。
pub fn run_standalone() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("构建 tokio runtime 失败");
    rt.block_on(async_standalone());
}

async fn async_standalone() {
    let path = config::default_config_path();
    let cfg = match config::load_exact(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let db_path = cfg
        .db_path
        .clone()
        .map(std::path::PathBuf::from)
        .or_else(|| cfg.data_dir.clone().map(|d| std::path::PathBuf::from(d).join("bp-host.db")))
        .unwrap_or_else(|| {
            path.parent()
                .map(|p| p.join("bp-host.db"))
                .unwrap_or_else(|| std::path::PathBuf::from("bp-host.db"))
        });
    let store = std::sync::Arc::new(
        persistence::SqliteStore::open(&db_path)
            .map_err(|e| format!("SQLite 初始化失败：{e}"))
            .unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(2); }),
    );
    let svc = match service::BpService::new(cfg.clone(), store) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[bp-host] 服务初始化失败：{e}");
            std::process::exit(2);
        }
    };
    net::server::spawn_timer_ticker(svc.clone());
    println!(
        "[bp-host] 房间 {} 监听 {}（前端目录 {:?}）",
        svc.room_code().await,
        cfg.bind_target(),
        cfg.frontend_dir
    );
    if let Err(e) = net::server::serve(cfg, svc).await {
        eprintln!("[bp-host] {e}");
        std::process::exit(1);
    }
}
