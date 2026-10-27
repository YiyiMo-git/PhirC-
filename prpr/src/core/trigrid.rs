//! 「光波」背景（TriGrid）。
//!
//! 移植自用户提供并亲自确认过的 Web 实现：
//! `kaichang/shaders/trigrid.glsl` + `kaichang/trigrid_preview.html`。
//!
//! 原版每帧的管线是：
//! ```text
//! ctx.clear();
//! ctx.drawImage(TriGridBase.png -> 铺满 canvas);      // ① Base 铺满画布
//! shaderPass(canvas 自身, TRIGRID_GLSL, uniforms);    // ② 对画布做全屏 pass
//! ctx.globalCompositeOperation = "destination-over";
//! ctx.drawImage(beijing -> 铺满 canvas);              // ③ 底图垫到**下面**
//! ```
//! 也就是说：**光波是一层半透明的叠加层，垫在 beijing 之上**（它的 alpha 由自身
//! 亮度推出来，暗处几乎是透明的，底图能从下面透上来）。而且 ① 里铺满 canvas 的
//! Base 贴图会被 ② 用 `mod(uv2 / tex_size, 1.)` 重新平铺采样 —— 等价于「直接把
//! `TriGridBase.png` 当平铺单元采样」。所以这里省掉 render target，直接把 Base
//! 当材质的 `Texture`，数学结果一致。
//!
//! 与 Web 版的两点差异（见 `trigrid.frag`）：
//!   * `alpha_scale`，浅色主题用（原版恒等于 1）；
//!   * `always_on` / `glow_ooroff` 两个 uniform 来自 `trigrid.glsl`（index.html 内联版没有），
//!     本实现恒传 0（→ 与原版内联 shader 同一条分支）。
//!
//! uv 语义：与原版 canvas 完全一致 —— **x / y 两个方向都是 0..1 铺满整个视口**，
//! 所以 `tex_size`（0.22 / 0.27）直接就是「格宽 = 0.22 视口宽、格高 = 0.27 视口高」。
//!
//! 纹理或 shader 不可用时整体降级：只打一条 warn，返回 `false`，由调用方
//! 退回原来的背景装饰，绝不 panic。

use crate::{ext::ScaleType, ui::Ui};
use macroquad::prelude::*;
use miniquad::{BlendFactor, BlendState, BlendValue, Equation, UniformType};
use once_cell::sync::Lazy;

/// 平铺单元（tile）尺寸，单位是「整屏 uv」。
///
/// **照搬 `kaichang/index.html` 内联的原版 shader** —— 那里面
/// `gl.uniform2f(u_tex_size, 0.22, 0.27)`（**不是** `trigrid.glsl` / 预览页里的
/// 0.16/0.2，那两个是旧的默认值）。原版 canvas 的 uv 在 x / y 两个方向都是
/// 0..1 铺满整个画布，所以格子像素尺寸就是：
///
/// ```text
/// 格宽 = 0.22 × 视口宽      格高 = 0.27 × 视口高
/// ```
///
/// 实测对照（见交付报告）：1280x720 下参照件格高 195px（= 0.27 × 720 = 194.4）、
/// 1024x768 下 208px（= 0.27 × 768 = 207.4），本实现改前是 0.16/0.2 且 uv 被
/// `ScaleType::CropCenter` 裁过（y 方向只覆盖 0.5625 个贴图），格子比例完全不对。
///
/// ⚠️ 2026-10：用户反馈「光波太密」，按用户要求**调疏**（不再是 kaichang 原值
/// 0.22/0.27）。值越大 = 平铺单元越大 = 格线越疏。这里放大到 0.30/0.36
/// （约 1.36 倍间距）。**若以后要还原成严格照搬原件，改回 `[0.22, 0.27]` 即可。**
/// 注意：这里只管 Rust 原生界面的背景。（原先还有一份内联在 `phira/ui/index.html`
/// 里的同名 shader，那份随 WebView 界面在 2026-10-09 一起删掉了。）
const TEX_SIZE: [f32; 2] = [0.30, 0.36];

/// 一套「配色 + 亮度」参数，对应原版那串 uniforms。
#[derive(Clone, Copy)]
struct Look {
    base_color: [f32; 4],
    face_color: [f32; 4],
    face_brightness: f32,
    grid_color: [f32; 4],
    grid_brightness: f32,
    glow_color: [f32; 4],
    glow_brightness: f32,
    alpha_scale: f32,
}

/// 深色主题：原版那套参数，逐字照抄。
const DARK: Look = Look {
    base_color: [0.018, 0.018, 0.018, 0.4],
    face_color: [0.702, 0.9449554, 1., 1.],
    face_brightness: 0.1,
    grid_color: [1., 1., 1., 1.],
    grid_brightness: 0.4,
    glow_color: [0.383, 0.8843222, 1., 1.],
    glow_brightness: 0.71,
    alpha_scale: 1.,
};

/// 浅色主题：底是 #f3f3f3 这种亮底，原版那种「加法发光」在上面完全看不见，
/// 所以反过来用**暗色的墨**画网格/光带（网格线压成蓝灰、光带压成蓝色），
/// 并把 alpha 放大 2.2 倍 —— 原版的 alpha 是由亮度推出来的，暗色必然偏淡。
/// 数值是在 1920x1080 / 2560x1080 / 1440x1080 三种比例下逐帧对着看定的。
const LIGHT: Look = Look {
    base_color: [0.02, 0.03, 0.05, 1.],
    face_color: [0.15, 0.35, 0.6, 1.],
    face_brightness: 0.05,
    grid_color: [0.24, 0.34, 0.5, 1.],
    grid_brightness: 0.5,
    glow_color: [0.05, 0.25, 0.6, 1.],
    glow_brightness: 0.62,
    alpha_scale: 2.2,
};

/// 运行时读取 `assets/trigrid_base.png`（和 `block_shader.rs` 一样的做法：
/// 不走 macroquad 的资源系统，缺文件时返回 `None` 而不是 panic）。
fn load_tile(path: &str) -> Option<Texture2D> {
    let bytes = std::fs::read(path).ok()?;
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let texture = Texture2D::from_rgba8(image.width() as u16, image.height() as u16, image.as_raw());
    // 原版也是 LINEAR 且没有 mipmap；平铺取整靠 shader 里的 mod()，
    // 采样本身仍要线性插值，否则细网格会变成锯齿。
    texture.set_filter(FilterMode::Linear);
    Some(texture)
}

static TILE: Lazy<Option<Texture2D>> = Lazy::new(|| {
    let tile = load_tile("assets/trigrid_base.png");
    if tile.is_none() {
        tracing::warn!("TriGrid 背景：assets/trigrid_base.png 读取失败，光波背景降级为原来的背景装饰");
    }
    tile
});

fn load_trigrid_material() -> Result<Material, miniquad::ShaderError> {
    load_material(
        include_str!("trigrid.vert"),
        include_str!("trigrid.frag"),
        MaterialParams {
            // 原版 `__enableAlpha` 走的就是 (SRC_ALPHA, ONE_MINUS_SRC_ALPHA)，
            // 这里必须显式给上：miniquad 的 PipelineParams::default() 是**不混合**的。
            pipeline_params: PipelineParams {
                color_blend: Some(BlendState::new(
                    Equation::Add,
                    BlendFactor::Value(BlendValue::SourceAlpha),
                    BlendFactor::OneMinusValue(BlendValue::SourceAlpha),
                )),
                ..Default::default()
            },
            uniforms: vec![
                ("tex_size".to_owned(), UniformType::Float2),
                ("base_color".to_owned(), UniformType::Float4),
                ("face_color".to_owned(), UniformType::Float4),
                ("face_brightness".to_owned(), UniformType::Float1),
                ("grid_color".to_owned(), UniformType::Float4),
                ("grid_brightness".to_owned(), UniformType::Float1),
                ("glow_color".to_owned(), UniformType::Float4),
                ("glow_brightness".to_owned(), UniformType::Float1),
                ("always_on".to_owned(), UniformType::Float1),
                ("glow_ooroff".to_owned(), UniformType::Float1),
                ("t".to_owned(), UniformType::Float1),
                ("aspect".to_owned(), UniformType::Float1),
                ("alpha_scale".to_owned(), UniformType::Float1),
            ],
            ..Default::default()
        },
    )
}

static MATERIAL: Lazy<Option<Material>> = Lazy::new(|| {
    if TILE.is_none() {
        return None;
    }
    load_trigrid_material()
        .map_err(|e| {
            tracing::warn!("TriGrid 背景：着色器编译失败（{e}），光波背景降级为原来的背景装饰");
        })
        .ok()
});

/// 光波是否可用（贴图 + 材质都就绪）。第一次调用会做一次懒加载。
pub fn available() -> bool {
    TILE.is_some() && MATERIAL.is_some()
}

/// 在 `rect` 上画一层半透明光波。
///
/// * `rect` —— 世界坐标下要铺满的矩形（整屏用 `Ui::screen_rect()`）；
/// * `t` —— 秒级时间（`get_time()` / `TimeManager::real_time()`），驱动扫描波；
/// * `dark` —— 深浅主题各一套参数；
///
/// 返回 `false` 表示光波不可用（已降级），调用方应保留自己的兜底背景。
pub fn draw(ui: &mut Ui<'_>, rect: Rect, t: f32, dark: bool) -> bool {
    let (Some(tile), Some(material)) = (TILE.as_ref(), MATERIAL.as_ref()) else {
        return false;
    };
    let look = if dark { DARK } else { LIGHT };

    // 世界坐标里「屏幕宽 = 2」，所以 rect.w / rect.h 就是真实宽高比。
    let aspect = rect.w / rect.h;

    material.set_uniform("tex_size", vec2(TEX_SIZE[0], TEX_SIZE[1]));
    material.set_uniform("base_color", vec4(look.base_color[0], look.base_color[1], look.base_color[2], look.base_color[3]));
    material.set_uniform("face_color", vec4(look.face_color[0], look.face_color[1], look.face_color[2], look.face_color[3]));
    material.set_uniform("face_brightness", look.face_brightness);
    material.set_uniform("grid_color", vec4(look.grid_color[0], look.grid_color[1], look.grid_color[2], look.grid_color[3]));
    material.set_uniform("grid_brightness", look.grid_brightness);
    material.set_uniform("glow_color", vec4(look.glow_color[0], look.glow_color[1], look.glow_color[2], look.glow_color[3]));
    material.set_uniform("glow_brightness", look.glow_brightness);
    material.set_uniform("always_on", 0.0_f32);
    material.set_uniform("glow_ooroff", 0.0_f32);
    material.set_uniform("t", t);
    material.set_uniform("aspect", aspect);
    material.set_uniform("alpha_scale", look.alpha_scale);

    // `Ui::fill_rect` 默认把 `(Texture2D, Rect)` 解析成 `ScaleType::CropCenter`
    // —— 那会把 uv **按视口宽高比裁一刀**（16:9 下 y 方向只剩 0.5625 个贴图高度），
    // 于是格子被非等比拉伸，比例必然和原版对不上。
    //
    // 原版 WebGL 里 `uv = a_pos * 0.5 + 0.5` 是**两个方向都 0..1 铺满画布**的，
    // 所以这里显式指定 `ScaleType::Fit`（`source_of_image` 返回 None → 取 (0,0,1,1)），
    // 拿到的 uv 就是「矩形内的 0..1」，与 canvas 语义逐字一致。
    gl_use_material(*material);
    ui.fill_rect(rect, (*tile, rect, ScaleType::Fit));
    gl_use_default_material();
    true
}
