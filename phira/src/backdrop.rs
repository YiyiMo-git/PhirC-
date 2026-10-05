//! **真·背景模糊**（Real Backdrop Blur）管线。
//!
//! 思路（与「抓当前 framebuffer 再写回」那条老路**无关**，老路在多数驱动上会把
//! 文字 / 图标吞成黑块，见 `newui.rs` 顶部说明）：
//!
//! 1. 每帧把「开始界面那套背景」（深黑底 + TriGrid 光波 + 三团环境光 + 暗角）渲到一张
//!    **半分辨率 RenderTarget**（`scene`）里 —— 背景是低频光波，半分辨率几乎无损；
//! 2. 对 `scene` 做两遍**可分离高斯模糊**（水平 → `blur_a`，垂直 → `blur_b`）；
//! 3. 把 `blur_b` 这张**已经模糊好的贴图整屏铺底**，替代原来那张锐利的背景。
//!
//! 于是所有玻璃面板（`acrylic` 用的半透明 `th.bg_elev`）底下透出来的就是**货真价实**
//! 的模糊背景 —— 这才是「卡片有模糊效果」的本意。整条管线零状态、零 framebuffer
//! 回读，任何驱动都不会出现黑块。
//!
//! 全部失败（RT 创建 / 着色器编译）都**静默降级**：`texture()` 返回 `None`，
//! 上层照常走原来的锐利背景 + 磨砂配方，绝不 panic、绝不黑屏。

use macroquad::prelude::*;
use prpr::ext::ScaleType;
use prpr::ui::Ui;
use std::cell::RefCell;

// 当前帧可用的「已模糊背景」贴图（半分辨率）。`None` = 降级到锐利背景。
thread_local! {
    static TEX: RefCell<Option<Texture2D>> = RefCell::new(None);
}

pub fn set_texture(t: Option<Texture2D>) {
    TEX.with(|it| *it.borrow_mut() = t);
}

pub fn texture() -> Option<Texture2D> {
    TEX.with(|it| it.borrow().clone())
}

pub struct Backdrop {
    /// 半分辨率尺寸（与视口同比例，避免拉伸）。
    dim: (u32, u32),
    /// 背景快照（锐利）。
    scene: Option<RenderTarget>,
    /// 模糊中间缓冲（水平结果）。
    blur_a: Option<RenderTarget>,
    /// 模糊结果（垂直结果）—— 这就是最终要铺底的贴图。
    blur_b: Option<RenderTarget>,
    /// 高斯模糊材质。
    blur_mat: Option<Material>,
}

impl Default for Backdrop {
    fn default() -> Self {
        Self {
            dim: (0, 0),
            scene: None,
            blur_a: None,
            blur_b: None,
            blur_mat: None,
        }
    }
}

impl Drop for Backdrop {
    fn drop(&mut self) {
        if let Some(rt) = self.scene.take() {
            rt.delete();
        }
        if let Some(rt) = self.blur_a.take() {
            rt.delete();
        }
        if let Some(rt) = self.blur_b.take() {
            rt.delete();
        }
    }
}

impl Backdrop {
    /// 懒创建 RT 与模糊材质。`(w, h)` 为视口物理像素；尺寸变化时会重建。
    /// 返回 `false` 表示资源不可用（已 warn），上层应降级。
    pub fn ensure(&mut self, w: u32, h: u32) -> bool {
        let w = (w / 2).max(1);
        let h = (h / 2).max(1);
        if self.dim != (w, h) || self.scene.is_none() {
            // 尺寸变了：丢掉旧的再建
            if let Some(rt) = self.scene.take() {
                rt.delete();
            }
            if let Some(rt) = self.blur_a.take() {
                rt.delete();
            }
            if let Some(rt) = self.blur_b.take() {
                rt.delete();
            }
            self.dim = (w, h);
            self.scene = Some(render_target(w, h));
            self.blur_a = Some(render_target(w, h));
            self.blur_b = Some(render_target(w, h));
            // 模糊采样要线性过滤，边缘钳制（默认即 ClampToEdge，这里只保证线性）。
            if let Some(rt) = &self.scene {
                rt.texture.set_filter(FilterMode::Linear);
            }
            if let Some(rt) = &self.blur_a {
                rt.texture.set_filter(FilterMode::Linear);
            }
            if let Some(rt) = &self.blur_b {
                rt.texture.set_filter(FilterMode::Linear);
            }
        }
        if self.blur_mat.is_none() {
            match load_material(
                include_str!("backdrop_blur.vert"),
                include_str!("backdrop_blur.frag"),
                MaterialParams {
                    uniforms: vec![
                        ("texel".to_owned(), UniformType::Float2),
                        ("direction".to_owned(), UniformType::Float2),
                    ],
                    ..Default::default()
                },
            ) {
                Ok(mat) => self.blur_mat = Some(mat),
                Err(e) => {
                    tracing::warn!("背景模糊：高斯模糊着色器编译失败（{e}），降级为锐利背景");
                    return false;
                }
            }
        }
        true
    }

    /// 把 `draw_bg` 渲进 `scene`，再高斯模糊到 `blur_b`，并发布到全局。
    /// 成功返回 `true`；任何一步不可用则返回 `false`（全局置 `None` → 降级）。
    pub fn capture_and_blur(&mut self, ui: &mut Ui, draw_bg: impl FnOnce(&mut Ui)) -> bool {
        let (w, h) = self.dim;
        // 1) 渲背景到 scene
        push_camera_state();
        {
            let mut cam = ui.camera();
            cam.viewport = Some((0, 0, w as i32, h as i32));
            cam.render_target = self.scene;
            set_camera(&cam);
            clear_background(Color::new(0.051, 0.055, 0.063, 1.));
            draw_bg(ui);
        }
        pop_camera_state();

        let (Some(mat), Some(scene), Some(blur_a), Some(blur_b)) =
            (self.blur_mat.as_ref(), self.scene, self.blur_a, self.blur_b)
        else {
            set_texture(None);
            return false;
        };

        let sr = ui.screen_rect();

        // 2) 水平模糊：scene -> blur_a
        gl_use_material(*mat);
        mat.set_uniform("texel", vec2(1. / w as f32, 1. / h as f32));
        mat.set_uniform("direction", vec2(1., 0.));
        push_camera_state();
        {
            let mut cam = ui.camera();
            cam.viewport = Some((0, 0, w as i32, h as i32));
            cam.render_target = Some(blur_a);
            set_camera(&cam);
            ui.fill_rect(sr, (scene.texture, sr, ScaleType::Fit));
        }
        pop_camera_state();
        gl_use_default_material();

        // 3) 垂直模糊：blur_a -> blur_b
        gl_use_material(*mat);
        mat.set_uniform("texel", vec2(1. / w as f32, 1. / h as f32));
        mat.set_uniform("direction", vec2(0., 1.));
        push_camera_state();
        {
            let mut cam = ui.camera();
            cam.viewport = Some((0, 0, w as i32, h as i32));
            cam.render_target = Some(blur_b);
            set_camera(&cam);
            ui.fill_rect(sr, (blur_a.texture, sr, ScaleType::Fit));
        }
        pop_camera_state();
        gl_use_default_material();

        set_texture(Some(blur_b.texture));
        true
    }
}
