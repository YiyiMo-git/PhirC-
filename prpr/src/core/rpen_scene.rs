//! RPEN 全局轨道与相机几何。
//!
//! 从上游 `core::rpen_scene` 移植。与场景导航 / 应用 HUD 无关。
//!
//! 相机三组轴（`globalEvents.types`）：
//! * `spatial-camera/position` → x / y / z（z 默认 1000）
//! * `spatial-camera/rotation` → yaw / pitch / roll（pitch 钳到 ±89° 防止万向锁）
//! * `spatial-camera/lens` → fov（默认按 675×900 半高视角推出）
//!
//! `followChart`（lens.settings）目前只存下来，供触点反投影等使用。
//!
//! 其它第三方全局事件**保留原始 JSON**（`Scene::global_events`），只记 warn ——
//! 它们需要各自的 provider 适配（后续批次/上游补齐）。

use super::{BpmList, Matrix};
use crate::parse::rpen::{number, Track};
use anyhow::{ensure, Result};
use macroquad::prelude::*;
use serde_json::Value;

/// 把平面单应矩阵的 W 保留下来，并强制协议要求的 1..=150000 深度范围。
///
/// 相机矩阵是**投影**矩阵（W ≠ 1），直接交给 GL 会和后处理的深度假设冲突；
/// 这里把 W 通道显式写回 model 矩阵，保证几何在进入后处理前就透视正确。
pub fn clip_model(matrix: &Matrix) -> Mat4 {
    let mut model = crate::ext::nalgebra_to_glm(matrix);
    let projection = unsafe { get_internal_gl() }.quad_gl.get_projection_matrix();
    let zz = projection.z_axis.z;
    if zz.abs() > 1e-9 {
        let a = (150000_f32 + 1.) / (150000. - 1.);
        let b = -2. * 150000. / (150000. - 1.) / 1000.;
        let k = (a - projection.w_axis.z) / zz;
        model.x_axis.z = matrix[(2, 0)] * k;
        model.y_axis.z = matrix[(2, 1)] * k;
        model.w_axis.z = matrix[(2, 2)] * k + b / zz;
    }
    model
}

pub struct Scene {
    /// 原始 `globalEvents` JSON。没有适配到 provider 的事件也留在这里，方便后续接入。
    pub global_events: Value,
    /// `spatial-camera` 就绪时才有相机。
    pub camera: Option<PlaneCamera>,
}

pub struct PlaneCamera {
    axes: Vec<Track>,
    pub follow_chart: bool,
}

impl Scene {
    pub fn new(raw: &Value, bpm: &mut BpmList) -> Result<Self> {
        let events = raw.get("globalEvents").cloned().unwrap_or(Value::Null);
        if !events.is_null() {
            ensure!(events["version"] == 1, "unsupported globalEvents version");
        }
        let types = &events["types"];
        let mut camera = false;
        let mut axes = Vec::new();
        for (kind, id, default) in [
            ("position", "x", 0.),
            ("position", "y", 0.),
            ("position", "z", 1000.),
            ("rotation", "yaw", 0.),
            ("rotation", "pitch", 0.),
            ("rotation", "roll", 0.),
            ("lens", "fov", 2. * (450_f64 / 1000.).atan().to_degrees()),
        ] {
            let key = format!("spatial-camera/{kind}");
            camera |= types.get(&key).is_some();
            let events = types[&key]["columns"][id]["events"].as_array().cloned().unwrap_or_default();
            axes.push(Track::new(&events, bpm, 1., Value::from(default), true)?);
        }
        // 其它第三方全局事件：没有对应 provider 的话只保留数据 + 记日志。
        if let Some(types) = types.as_object() {
            for key in types.keys().filter(|k| !k.starts_with("spatial-camera/")) {
                tracing::warn!(event_type = %key, "RPEN global event has no registered runtime; source data retained");
            }
        }
        let follow_chart = types["spatial-camera/lens"]["settings"]["followChart"].as_bool().unwrap_or(false);
        Ok(Self {
            global_events: events,
            camera: camera.then_some(PlaneCamera { axes, follow_chart }),
        })
    }
}

impl PlaneCamera {
    /// 谱面 Y-up 坐标 → 投影 Y-up 坐标的单应矩阵。
    /// W 通道由 GPU 接管，所以几何在进后处理前就完成透视除法。
    pub fn matrix(&self, time: f64, aspect: f32) -> Matrix {
        let v: Vec<_> = self.axes.iter().map(|t| number(&t.value(time), 0.) as f32).collect();
        let c = vec3(
            v[0].clamp(-50000., 50000.),
            v[1].clamp(-50000., 50000.),
            v[2].clamp(-50000., 50000.),
        );
        let (sy, cy) = v[3].to_radians().sin_cos();
        let (sp, cp) = v[4].clamp(-89., 89.).to_radians().sin_cos();
        let (sr, cr) = v[5].to_radians().sin_cos();
        let right = vec3(cy, 0., sy);
        let up = vec3(-sy * sp, cp, cy * sp);
        let forward = vec3(sy * cp, sp, -cy * cp);
        let (right, up) = (right * cr + up * sr, up * cr - right * sr);
        let f = 450. / (v[6].clamp(10., 150.).to_radians() / 2.).tan();
        let y = 450. * aspect;
        // W 统一除以 1000：无相机时矩阵仍是单位阵，且浮点精度稳定。
        Matrix::new(
            right.x * f / 1000.,
            right.y * y * f / 675. / 1000.,
            -c.dot(right) * f / 675. / 1000.,
            up.x * 675. * f / y / 1000.,
            up.y * f / 1000.,
            -c.dot(up) * f / y / 1000.,
            forward.x * 675. / 1000.,
            forward.y * y / 1000.,
            -c.dot(forward) / 1000.,
        )
    }

    pub fn screen_matrix(&self, time: f64, aspect: f32) -> Matrix {
        let flip = Matrix::new_nonuniform_scaling(&nalgebra::Vector2::new(1., -1.));
        flip * self.matrix(time, aspect) * flip
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().build().unwrap()
    }

    #[test]
    fn no_global_events_means_no_camera() {
        let raw = json!({ "META": { "RPEVersion": 240 } });
        let mut bpm = BpmList::new(vec![(0., 120.)]);
        let scene = Scene::new(&raw, &mut bpm).unwrap();
        assert!(scene.camera.is_none());
        assert!(scene.global_events.is_null());
    }

    #[test]
    fn camera_defaults_are_stable_without_events() {
        let raw = json!({ "globalEvents": { "version": 1, "types": { "spatial-camera/position": { "columns": {} } } } });
        let mut bpm = BpmList::new(vec![(0., 120.)]);
        let scene = Scene::new(&raw, &mut bpm).unwrap();
        let cam = scene.camera.as_ref().expect("声明了 position 就该有相机");
        // 无任何事件时相机矩阵应该是**单位阵**（W 除以 1000 后的对角恒等）
        let m = cam.matrix(0., 16. / 9.);
        for i in 0..3 {
            for j in 0..3 {
                let want = if i == j { 1. } else { 0. };
                assert!((m[(i, j)] - want).abs() < 1e-5, "({}, {}) = {} ≠ {}", i, j, m[(i, j)], want);
            }
        }
    }

    #[test]
    fn position_events_move_the_camera() {
        let runtime = runtime();
        let raw = json!({
            "globalEvents": { "version": 1, "types": {
                "spatial-camera/position": { "columns": {
                    "x": { "events": [{ "startTime": 0, "endTime": 2, "start": 0, "end": 675 }] }
                } }
            } }
        });
        let mut bpm = BpmList::new(vec![(0., 120.)]); // 1 拍 = 0.5s → 2 拍 = 1s
        let scene = runtime.block_on(async { Scene::new(&raw, &mut bpm).unwrap() });
        let cam = scene.camera.as_ref().unwrap();
        // t=0 在起点，t=1 在终点（x 平移 675 会体现在第三列）
        let at_start = cam.matrix(0., 16. / 9.);
        let at_end = cam.matrix(1., 16. / 9.);
        assert!(
            (at_start[(0, 2)] - at_end[(0, 2)]).abs() > 1e-6 && at_end[(0, 2)] < at_start[(0, 2)],
            "x 平移应把相机矩阵第三列往负向推（start={} end={})",
            at_start[(0, 2)],
            at_end[(0, 2)]
        );
    }

    #[test]
    fn other_providers_are_preserved_and_warned() {
        let raw = json!({ "globalEvents": { "version": 1, "types": {
            "some-plugin/thing": { "columns": {} },
            "spatial-camera/position": { "columns": {} }
        } } });
        let mut bpm = BpmList::new(vec![(0., 120.)]);
        let scene = Scene::new(&raw, &mut bpm).unwrap();
        assert!(scene.global_events["types"].get("some-plugin/thing").is_some(), "第三方事件必须保留原始数据");
    }
}
