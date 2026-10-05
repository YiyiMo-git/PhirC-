//! 噪区（第 9 章噪域 / official "block area" / noise field）的**全部**实现。
//!
//! ⚠️ 名字容易混：本模块管的是官方 ActiveBlock/Ready 那套**噪域**，
//! 和 `core::block`（`BlockArea` / `block_touch_blocked`，被 `judge.rs` 使用的
//! **游戏判定块**）是两回事。那个文件留在 `core/` 下，不属于本目录。
//!
//! 子模块：
//! * [`shader`] —— 渲染主体：材质/帧纹理、`draw_zones` / `draw_disabled_zones`。
//!   它内部还有两个子模块（在 `shader.rs` 顶部声明，不在这里）：
//!   * `mask`  —— 遮罩的栅格化 / 膨胀 / 上传
//!   * `touch` —— 触摸与 hover 掩码
//! * [`audio`] —— 入区触摸时的 BGM 低通（官方 1500 Hz 的等价近似）
//!
//! 着色器源码（`shader_full.vert` / `shader_full.frag`）就在本目录，
//! 由 `shader.rs` 用 `include_str!` 在编译期嵌入；4 张贴图来自
//! `assets/blockarea/`（`include_bytes!`，同样编译期嵌入）。
//!
//! 历史上这些文件散落在 `core/` 根下、靠 `#[path]` 互相引用；集中到本目录后
//! 它们成为同级兄弟文件，`#[path]` 已不再需要。

mod audio;
mod shader;

pub(crate) use audio::BlockAudio;
pub(crate) use shader::reset_block_effects;
/// 噪区遮罩分辨率（设置里的「噪区精细度」滑块）。
pub use shader::{mask_divisor, set_mask_divisor};
pub use shader::{draw_disabled_zones, draw_zones, draw_zones_with_touches, Zone};
/// 噪区着色器的编译错误 / 强制初始化（供「设置 → 日志」页显示诊断）。
pub use shader::{force_init as force_block_shader_init, shader_error as block_shader_error};
