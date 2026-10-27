use super::{
    draw_disabled_zones, draw_zones_with_touches, reset_block_effects, BlockArea, BlockRenderer, BpmList, Effect, JudgeLine, JudgeLineCache, JudgeLineKind, Matrix,
    NoteKind, Resource, UIElement, Vector, Zone,
};
use crate::{config::NoteMap, core::Object, fs::FileSystem, judge::JudgeStatus, ui::Ui};
use anyhow::{Context, Result};
use macroquad::prelude::*;
use nalgebra::Rotation2;
use sasa::AudioClip;
use std::{cell::RefCell, collections::HashMap};

#[derive(Default)]
pub struct ChartExtra {
    pub effects: Vec<Effect>,
    pub global_effects: Vec<Effect>,
    #[cfg(feature = "video")]
    pub videos: Vec<(super::Video, Option<super::VideoAttach>)>,
}

#[derive(Default)]
pub struct ChartSettings {
    pub pe_alpha_extension: bool,
    pub hold_partial_cover: bool,
    /// 判定线长度与厚度以 Y 轴（屏幕纵横比）为参考；PGR（官方谱面）默认为真。
    pub line_reference_y_axis: bool,
}

pub type HitSoundMap = HashMap<String, AudioClip>;

pub struct Chart {
    pub offset: f32,
    pub lines: Vec<JudgeLine>,
    pub bpm_list: RefCell<BpmList>,

    pub settings: ChartSettings,
    pub extra: ChartExtra,

    /// Line order according to z-index, lines with attach_ui will be removed from this list
    ///
    /// Store the index of the line in z-index ascending order
    pub order: Vec<usize>,
    /// TODO: docs from RPE
    pub attach_ui: [Option<usize>; 7],

    pub hitsounds: HitSoundMap,

    /// Phigros 第 9 章的触摸阻挡区（`blockAreaList`）。
    pub block_areas: Vec<BlockArea>,
    /// RPEN 全局轨道与相机（`globalEvents`）。非 RPEN 谱面为 None。
    pub rpen: Option<super::rpen_scene::Scene>,
    /// RPEN 新格式 `blockAreaList`（带 `layerId` / `isInvert`）。
    /// **和 `block_areas` 分开**：旧数据来自噪域线，新数据来自列表，两套互不干扰。
    pub next_block_areas: Vec<super::rpe_next_block::Area>,
    /// RPEN 新版反转规则：某层「恰好 1 个反转区」才翻转（旧版是「奇数个」）。
    pub single_inversion: bool,
    /// 每层独立的噪域渲染器（掩码 / 纹理缓存按层隔离）。
    pub next_layer_renderers: Vec<BlockRenderer>,
    /// 每层的自定义皮肤（`noiseAreaLayers[].skin`）。`None` = 用原生噪域材质。
    pub next_layer_skins: Vec<Option<super::rpen_skin::Skin>>,
    /// 单层（无 `noiseAreaLayers`）时共用的渲染器。
    next_block_renderer: BlockRenderer,
    next_block_frame: RefCell<(Option<(f64, f32, usize)>, Vec<super::rpe_next_block::Sample>)>,
    /// 本帧被阻挡区挡下的触摸（谱面坐标）。
    pub blocked_touches: Vec<(u64, Vector)>,
}

impl Chart {
    pub fn new(offset: f32, lines: Vec<JudgeLine>, bpm_list: BpmList, settings: ChartSettings, extra: ChartExtra, hitsounds: HitSoundMap) -> Self {
        let mut attach_ui = [None; 7];
        let mut order = (0..lines.len())
            .filter(|it| {
                if let Some(element) = lines[*it].attach_ui {
                    attach_ui[element as usize - 1] = Some(*it);
                    false
                } else {
                    true
                }
            })
            .collect::<Vec<_>>();
        order.sort_by_key(|it| (lines[*it].z_index, *it));
        Self {
            offset,
            lines,
            bpm_list: RefCell::new(bpm_list),
            settings,
            extra,

            order,
            attach_ui,

            hitsounds,

            block_areas: Vec::new(),
            next_block_areas: Vec::new(),
            single_inversion: false,
            next_layer_renderers: Vec::new(),
            next_layer_skins: Vec::new(),
            next_block_renderer: BlockRenderer::default(),
            next_block_frame: RefCell::default(),
            rpen: None,
            blocked_touches: Vec::new(),
        }
    }

    #[inline]
    pub fn with_element<R>(
        &self,
        ui: &mut Ui,
        res: &Resource,
        element: UIElement,
        scale_point: Option<(f32, f32)>,
        rotation_point: (f32, f32),
        f: impl FnOnce(&mut Ui, Color) -> R,
    ) -> R {
        let scale_point = scale_point.unwrap_or(rotation_point);
        if let Some(id) = self.attach_ui[element as usize - 1] {
            let lines = &self.lines;
            let line = &lines[id];
            let obj = &line.object;
            let mut tr = line.fetch_pos(res, lines);
            tr.y = -tr.y;
            let color = self.lines[id].color.now_opt().unwrap_or(WHITE);
            let scale = obj.now_scale(Vector::new(scale_point.0, scale_point.1));
            let ro =
                Object::new_rotation_wrt_point(Rotation2::new(-obj.rotation.now().to_radians()), Vector::new(rotation_point.0, rotation_point.1));
            ui.with(Matrix::new_translation(&tr) * ro * scale, |ui| ui.alpha(obj.now_alpha().max(0.), |ui| f(ui, color)))
        } else {
            f(ui, WHITE)
        }
    }

    pub async fn load_textures(&mut self, fs: &mut dyn FileSystem) -> Result<()> {
        for line in &mut self.lines {
            if let JudgeLineKind::Texture(tex, path) = &mut line.kind {
                *tex = image::load_from_memory(&fs.load_file(path).await.with_context(|| format!("failed to load illustration {path}"))?)?.into();
            }
        }
        Ok(())
    }

    /// 应用「音符映射」：把四类音符整体换成配置里指定的类型。
    ///
    /// 必须在谱面解析完成（`JudgeLineCache::new` 已跑过）之后、`Judge::new` 之前调用：
    /// 改完类型后缓存里「普通 / 非普通音符」的划分会失效，所以这里要重建缓存。
    /// `hold_secs` 是「变成**长条**」时新长条的时长（秒）。
    pub fn apply_note_map(&mut self, map: &NoteMap, hold_secs: f64) {
        if map.is_identity() {
            return;
        }
        for line in &mut self.lines {
            // 判定线高度动画的副本：变成长条时用它算 end_height，不污染渲染状态。
            let mut height = line.height.clone();
            let mut changed = false;
            for note in &mut line.notes {
                let target = match note.kind {
                    NoteKind::Click => map.tap,
                    NoteKind::Drag => map.drag,
                    NoteKind::Flick => map.flick,
                    NoteKind::Hold { .. } => map.hold,
                };
                changed |= note.apply_map(target, hold_secs, &mut height);
            }
            if changed {
                line.cache = JudgeLineCache::new(&mut line.notes);
            }
        }
    }

    pub fn reset(&mut self) {
        self.blocked_touches.clear();
        reset_block_effects();
        self.next_block_frame.get_mut().0 = None;
        self.next_block_renderer.reset();
        for renderer in &mut self.next_layer_renderers {
            renderer.reset();
        }
        for skin in self.next_layer_skins.iter().flatten() {
            skin.reset();
        }
        self.lines
            .iter_mut()
            .flat_map(|it| it.notes.iter_mut())
            .for_each(|note| note.judge = JudgeStatus::NotJudged);
        for line in &mut self.lines {
            line.cache.reset(&mut line.notes);
        }
        #[cfg(feature = "video")]
        for (video, _) in &mut self.extra.videos {
            if let Err(err) = video.reset() {
                use crate::parse::{ptl, L10N_LOCAL};
                crate::scene::show_error(err.context(ptl!("video-load-failed", "path" => video.video_file.path().to_string_lossy())));
            }
        }
    }

    pub fn update(&mut self, res: &mut Resource) {
        for line in &mut self.lines {
            line.object.set_time(res.time);
        }
        // TODO optimize
        let trs = self.lines.iter().map(|it| it.now_transform(res, &self.lines)).collect::<Vec<_>>();
        let rotations = self.lines.iter().map(|it| it.fetch_rot(&self.lines)).collect::<Vec<_>>();
        for ((line, tr), rot) in self.lines.iter_mut().zip(trs).zip(rotations) {
            line.update(res, tr, rot);
        }
        for effect in &mut self.extra.effects {
            effect.update(res);
        }
        #[cfg(feature = "video")]
        for (video, _) in &mut self.extra.videos {
            if let Err(err) = video.update(res.time) {
                tracing::warn!("video error: {err:?}");
            }
        }
    }

    /// RPEN 相机矩阵：没有相机时是单位阵（对普通谱面完全无影响）。
    pub fn camera_matrix(&self, time: f64, aspect: f32) -> Matrix {
        self.rpen
            .as_ref()
            .and_then(|r| r.camera.as_ref())
            .map_or_else(Matrix::identity, |c| c.matrix(time, aspect))
    }

    /// 是否用了投影相机（渲染层据此决定要不要 `clip_model`）。
    pub fn projected_chart(&self) -> bool {
        self.rpen.as_ref().is_some_and(|r| r.camera.is_some())
    }

    /// 把屏幕上的触点反投影回谱面坐标。
    ///
    /// 有相机时触点要经过 `screen_matrix` 的逆变换；超出可见深度（1..=150000）
    /// 返回 `None`，调用方应把该触点当作「不在谱面内」忽略。
    pub fn unproject_touch(&self, point: Vector, time: f64, aspect: f32) -> Option<Vector> {
        let matrix = self
            .rpen
            .as_ref()
            .and_then(|r| r.camera.as_ref())
            .map_or_else(Matrix::identity, |c| c.screen_matrix(time, aspect));
        let inverse = matrix.try_inverse()?;
        let p = inverse * nalgebra::Vector3::new(point.x, point.y, 1.);
        if !p.iter().all(|v| v.is_finite()) || p.z.abs() < 1e-6 {
            return None;
        }
        let p = nalgebra::Vector3::new(p.x / p.z, p.y / p.z, 1.);
        let depth = (matrix * p).z;
        (depth >= 0.001 && depth <= 150.).then_some(Vector::new(p.x, p.y))
    }

    pub fn render(&self, ui: &mut Ui, res: &mut Resource) {
        #[cfg(feature = "video")]
        for (video, attach) in &self.extra.videos {
            if let Some(attach) = attach {
                let line = &self.lines[attach.line];
                let color = line.color.now_opt().unwrap_or(res.judge_line_color);
                let mat = self.lines[attach.line].object.now(res);
                res.apply_model_of(&mat, |res| {
                    video.render(res.time, res.aspect_ratio, color);
                });
            } else {
                video.render(res.time, res.aspect_ratio, WHITE);
            }
        }
        res.apply_model_of(&Matrix::identity().append_nonuniform_scaling(&Vector::new(if res.config.flip_x() { -1. } else { 1. }, -1.)), |res| {
            // 官方排序层：Background 层放阻挡区（第 2 层），判定线第 3 层。
            let zones = self.block_zones(res);
            draw_disabled_zones(res, res.aspect_ratio, &zones);
            // RPEN 新格式噪区的 Disabled 层同样在判定线之前。
            let next = self.next_block_samples(res.time, res.aspect_ratio);
            if !next.is_empty() {
                self.draw_next_layers(res, &next, true);
            }
            drop(next);
            // RPEN 相机：有的话用投影矩阵（clip_model 保留 W 通道做透视），
            // 没有就是单位阵 —— 对普通谱面零影响。
            let projection = self.camera_matrix(res.time, res.aspect_ratio);
            let camera_glm = if self.projected_chart() {
                crate::core::rpen_scene::clip_model(&projection)
            } else {
                crate::ext::nalgebra_to_glm(&projection)
            };
            let gl = unsafe { get_internal_gl() }.quad_gl;
            gl.push_model_matrix(camera_glm);
            let mut guard = self.bpm_list.borrow_mut();
            for id in &self.order {
                self.lines[*id].render(ui, res, &self.lines, &mut guard, &self.settings, *id);
            }
            drop(guard);
            res.note_buffer.borrow_mut().draw_all();
            gl.pop_model_matrix();
            if res.config.sample_count > 1 {
                unsafe { get_internal_gl() }.flush();
                if let Some(target) = &res.chart_target {
                    target.blit();
                }
            }
            if !res.no_effect {
                let render = |res: &mut Resource| {
                    for effect in &self.extra.effects {
                        effect.render(res);
                    }
                };
                if res.config.flip_x() {
                    res.apply_model_of(&Matrix::identity().append_nonuniform_scaling(&Vector::new(-1., 1.)), render);
                } else {
                    render(res);
                }
            }
        });
    }

    /// 噪区前景层：在音符 / HUD 之后绘制（官方 ActiveBlock 是后处理层）。
    pub fn render_block_overlay(&self, res: &mut Resource) {
        let flip_x = res.config.flip_x();
        let zones = self.block_zones(res);
        res.apply_model_of(&Matrix::identity().append_nonuniform_scaling(&Vector::new(if flip_x { -1. } else { 1. }, -1.)), |res| {
            draw_zones_with_touches(res, res.aspect_ratio, &zones, &self.blocked_touches, flip_x);
            if !self.next_block_areas.is_empty() {
                let next = self.next_block_samples(res.time, res.aspect_ratio);
                self.draw_next_layers(res, &next, false);
            }
        });
    }

    /// RPEN 是否用了自定义噪域皮肤（供加载阶段决定是否预编译）。
    pub fn has_rpen_skins(&self) -> bool {
        self.next_layer_skins.iter().any(Option::is_some)
    }

    fn draw_next_layers(&self, res: &mut Resource, next: &[super::rpe_next_block::Sample], disabled: bool) {
        let flip_x = res.config.flip_x();
        let flip = Matrix::identity().append_nonuniform_scaling(&Vector::new(if flip_x { -1. } else { 1. }, -1.));
        let parent = crate::ext::nalgebra_to_glm(&flip);
        // 有相机时被挡触点要先投影到屏幕空间，皮肤/噪域材质都按屏幕 UV 工作。
        let matrix = self.camera_matrix(res.time, res.aspect_ratio);
        let touches: Vec<_> = self
            .blocked_touches
            .iter()
            .filter_map(|&(id, p)| {
                let p = matrix * nalgebra::Vector3::new(p.x, p.y, 1.);
                (p.z >= 0.001 && p.z <= 150.).then_some((id, Vector::new(p.x / p.z, p.y / p.z)))
            })
            .collect();
        if self.next_layer_renderers.is_empty() {
            let zones: Vec<_> = next.iter().map(|s| s.zone.clone()).collect();
            self.next_block_renderer.draw_next(
                res,
                res.aspect_ratio,
                &zones,
                self.single_inversion,
                disabled,
                if disabled { &[] } else { &touches },
                flip_x,
            );
        } else {
            for (id, renderer) in self.next_layer_renderers.iter().enumerate() {
                let zones: Vec<_> = next.iter().filter(|s| s.layer == id).map(|s| s.zone.clone()).collect();
                if !res.no_effect {
                    if let Some(skin) = self.next_layer_skins.get(id).and_then(Option::as_ref) {
                        if !disabled && skin.render(res, &zones, &touches, parent) {
                            continue;
                        }
                        // Disabled 层由皮肤自己的 disabled.glsl 画；皮肤已失败时退回原生材质。
                        if disabled && !skin.failed() {
                            continue;
                        }
                    }
                }
                renderer.draw_next(res, res.aspect_ratio, &zones, true, disabled, if disabled { &[] } else { &touches }, flip_x);
            }
        }
    }

    /// 当前时间的 RPEN 新格式噪区采样（带缓存；含相机投影）。
    pub fn next_block_samples(&self, time: f64, aspect: f32) -> std::cell::Ref<'_, [super::rpe_next_block::Sample]> {
        let mut cache = self.next_block_frame.borrow_mut();
        let key = (time, aspect, self.next_block_areas.len());
        if cache.0 != Some(key) {
            cache.1.clear();
            cache.1.extend(self.next_block_areas.iter().filter_map(|area| area.sample(time, aspect)));
            if self.rpen.as_ref().is_some_and(|r| r.camera.is_some()) {
                let projection = self.camera_matrix(time, aspect);
                for sample in &mut cache.1 {
                    sample.zone.projection = Some(projection);
                }
            }
            cache.0 = Some(key);
        }
        drop(cache);
        std::cell::Ref::map(self.next_block_frame.borrow(), |cache| cache.1.as_slice())
    }

    fn block_zones(&self, res: &Resource) -> Vec<Zone> {
        let aspect = res.aspect_ratio;
        let t = res.time;
        self.block_areas.iter().filter_map(|b| Zone::from_area(b, t, aspect)).collect()
    }
}
