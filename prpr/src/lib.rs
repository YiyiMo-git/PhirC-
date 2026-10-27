pub mod bin;
/// 自动倒打：按谱面设定的时间区间，用一段 0.8s 的旋转动画把画面翻过来/翻回去。
/// 纯状态机、与渲染无关，从 PhiraiAd 移植。
pub mod auto_flip;
/// 每谱面玩法设置（自动倒打区间 / 音符类型转换）——从 PhiraiAd 移植，
/// 与谱面数据一起持久化（PEC/PBC 走 `info.yml`，JSON 谱面嵌 `phiraReplica` 字段）。
pub mod chart_play;
pub mod config;
pub mod core;
pub mod dir;
pub mod ext;
pub mod fs;
pub mod health;
pub mod info;
/// 判定范围可视化（调试叠加层，只读、不改判定）——从 PhiraiAd 移植，
/// 数值来源换成本分支可配置的判定窗口。
pub mod judgement_range;
pub mod judge;
pub mod parse;
pub mod particle;
/// 练习模式的镜头控制（缩放 / 平移）——从 PhiraiAd 移植，纯几何。
pub mod practice_view;
/// 练习模式的流速曲线（分段线性，1x 为分界）——从 PhiraiAd 移植，纯数学。
pub mod practice_speed;
pub mod record;
pub mod replay;
pub mod scene;
pub mod spectrum;
pub mod task;
pub mod time;
/// 装饰性准度条（Timing Bar）——从 PhiraiAd 移植，按本分支的 `Config` 窗口重算边界。
pub mod timing_bar;
pub mod ui;

#[cfg(feature = "log")]
pub mod log;

#[rustfmt::skip]
#[cfg(closed)]
pub mod inner;

pub use scene::Main;

pub fn build_conf() -> macroquad::window::Conf {
    macroquad::window::Conf {
        window_title: "Phira".to_string(),
        window_width: 973,
        window_height: 608,
        ..Default::default()
    }
}
