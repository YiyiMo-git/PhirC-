//! RPEN（RPE Next / Phira-Pro）扩展特性探测。
//!
//! ⚠️ 这一版**只做识别，不改引擎**：给导入警告和「RPEN特性」开关提供依据。
//! 真正的特性适配（曲线轨迹 / 分层噪域 / shader 事件 / globalEvents…）在后续批次里做，
//! 到时候统一读 `ChartInfo::use_rpen` 决定走新路还是回退旧行为。
//!
//! 探测逻辑与上游 `Phira-Pro` 的 `parse::rpen::features` 保持一致：只看 JSON 形态，
//! 不依赖任何引擎结构，所以可以安全地在 `lint()` 里跑。

use serde_json::Value;

pub mod curve;
pub mod noise;
pub mod expression;
pub mod trajectory;

pub use curve::curve;
pub use noise::{parse_layers as parse_noise_layers, parse_next_areas as parse_noise_areas};
pub use expression::Expr;
pub use trajectory::Trajectory;

/// 数组非空或对象非空。
pub fn nonempty(v: &Value) -> bool {
    v.as_array().is_some_and(|a| !a.is_empty()) || v.as_object().is_some_and(|a| !a.is_empty())
}

/// 取 JSON 里的数字，缺省给 `def`。
pub fn number(v: &Value, def: f64) -> f64 {
    v.as_f64().filter(|n| n.is_finite()).unwrap_or(def)
}

/// RPE 里的布尔量既可能是 `true/false`，也可能是 `1/0`。
pub fn flag(v: &Value) -> bool {
    v.as_bool().unwrap_or_else(|| v.as_i64().unwrap_or(0) != 0)
}

/// 谱面是否使用了 RPEN 扩展特性。
///
/// 判定项（与上游一致）：
/// 1. `META.RPEVersion >= 230` 且 `blockAreaList` 里出现 `bottomLeft`（新版块区格式）
/// 2. 顶层出现 `noiseAreaLayers` / `globalEvents` / `effects` / `shaderEvents`，
///    或 `META.effects` 非空
/// 3. 任一判定线上：`bpmfactor` ≠ 1、`skewControl` 里有非 0 的 `skew`、
///    `extended.paintEvents` 里有 shader 事件、`eventLayers` 里的 `moveXEvents` 带 `trajectory`
/// 4. `blockAreaList` 里出现 `layerId`（分层块区）
pub fn features(chart: &Value) -> bool {
    if number(&chart["META"]["RPEVersion"], 0.) >= 230.
        && chart["blockAreaList"].as_array().is_some_and(|a| a.iter().any(|a| a.get("bottomLeft").is_some()))
    {
        return true;
    }
    if ["noiseAreaLayers", "globalEvents", "effects", "shaderEvents"].iter().any(|k| nonempty(&chart[k]))
        || nonempty(&chart["META"]["effects"])
    {
        return true;
    }
    chart["judgeLineList"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|line| {
            line["bpmfactor"].as_f64().is_some_and(|v| (v - 1.).abs() > 1e-12)
                || line["skewControl"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|p| p["skew"].as_f64().unwrap_or(0.) != 0.))
                || line["extended"]["paintEvents"].as_array().is_some_and(|a| a.iter().any(shader_event))
                || line["eventLayers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|l| l["moveXEvents"].as_array().into_iter().flatten().any(|e| e["trajectory"].is_object()))
        })
        || chart["blockAreaList"].as_array().into_iter().flatten().any(|a| a.get("layerId").is_some())
}

/// 一个 paint 事件是不是「shader 事件」（而不是传统数字 Paint 渲染器的那类）。
pub fn shader_event(v: &Value) -> bool {
    v.get("shader").is_some() || v.get("shaderType").is_some() || v.get("name").is_some()
}

/// 把 shader paint 事件从源里剔掉，避免它们被传统数字 Paint 渲染器误读。
///
/// 现在还没有 shader 渲染通道，所以导入时先按上游的做法剥离，等第 5 批接了
/// shader 事件再放开。
pub fn legacy_source(chart: &Value) -> Value {
    let mut result = chart.clone();
    for line in result["judgeLineList"].as_array_mut().into_iter().flatten() {
        if let Some(events) = line["extended"]["paintEvents"].as_array_mut() {
            events.retain(|v| !shader_event(v));
        }
        if line["extended"]["paintEvents"].as_array().is_some_and(|v| v.is_empty()) {
            line["extended"]["paintEvents"] = Value::Null;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_chart_has_no_rpen() {
        let chart = json!({
            "META": { "RPEVersion": 180 },
            "judgeLineList": [
                { "eventLayers": [{ "moveXEvents": [{ "start": 0, "end": 1, "start2": 0, "end2": 1 }] }] }
            ]
        });
        assert!(!features(&chart));
    }

    #[test]
    fn detects_bpmfactor() {
        let chart = json!({ "judgeLineList": [{ "bpmfactor": 1.5 }] });
        assert!(features(&chart));
    }

    #[test]
    fn detects_skew_control() {
        let chart = json!({ "judgeLineList": [{ "skewControl": [{ "skew": 0.3 }] }] });
        assert!(features(&chart));
    }

    #[test]
    fn detects_trajectory() {
        let chart = json!({
            "judgeLineList": [{ "eventLayers": [{ "moveXEvents": [{ "trajectory": { "type": "line" } }] }] }]
        });
        assert!(features(&chart));
    }

    #[test]
    fn detects_shader_paint_event() {
        let chart = json!({ "judgeLineList": [{ "extended": { "paintEvents": [{ "shader": "x" }] } }] });
        assert!(features(&chart));
    }

    #[test]
    fn detects_noise_layers_and_global_events() {
        assert!(features(&json!({ "noiseAreaLayers": [{ "id": 1 }] })));
        assert!(features(&json!({ "globalEvents": [{ "name": "cam" }] })));
        assert!(features(&json!({ "META": { "effects": [{ "a": 1 }] } })));
    }

    #[test]
    fn detects_layered_block_area() {
        assert!(features(&json!({ "blockAreaList": [{ "layerId": 3 }] })));
        assert!(!features(&json!({ "blockAreaList": [{ "x": 1 }] })));
    }

    #[test]
    fn legacy_source_strips_shader_events() {
        let chart = json!({
            "judgeLineList": [{ "extended": { "paintEvents": [
                { "shader": "x" },
                { "color": 1 }
            ] } }]
        });
        let out = legacy_source(&chart);
        let events = out["judgeLineList"][0]["extended"]["paintEvents"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].get("shader").is_none());
    }
}

// ============================================================
//  轨迹事件 → AnimFloat（从上游 `parse::rpen` 移植）
// ============================================================

/// 拍号三元组 `[整数, 分子, 分母]` → 拍数（也接受直接给数字）。
pub fn beat(v: &Value) -> anyhow::Result<f64> {
    use anyhow::{ensure, Context};
    let n = if let Some(v) = v.as_f64() {
        v
    } else {
        let a = v.as_array().context("expected beat triple")?;
        ensure!(a.len() == 3, "invalid beat triple");
        let d = a[2].as_f64().context("invalid beat denominator")?;
        ensure!(d != 0., "zero beat denominator");
        a[0].as_f64().context("invalid beat integer")? + a[1].as_f64().context("invalid beat numerator")? / d
    };
    ensure!(n.is_finite(), "non-finite beat");
    Ok(n)
}

/// 事件缓动：支持贝塞尔（`bezier` + `bezierPoints`）与 `easingType`（RPE 1..29）。
pub fn ease(e: &Value, p: f64) -> f64 {
    let p = p.clamp(0., 1.);
    if flag(&e["bezier"]) {
        let pts = e["bezierPoints"].as_array();
        let v = |i, d| pts.and_then(|v| v.get(i)).map_or(d, |v| number(v, d));
        let (x1, y1, x2, y2) = (v(0, 0.), v(1, 0.), v(2, 1.), v(3, 1.));
        let c = |t: f64, a, b| 3. * t * (1. - t).powi(2) * a + 3. * t * t * (1. - t) * b + t.powi(3);
        // 二分反解 x(t)=p，再取 y(t)
        let (mut lo, mut hi) = (0., 1.);
        for _ in 0..40 {
            let m = (lo + hi) / 2.;
            if c(m, x1, x2) < p {
                lo = m
            } else {
                hi = m
            }
        }
        c((lo + hi) / 2., y1, y2)
    } else {
        let id = number(&e["easingType"], 1.) as u8;
        let f = |p| curve(id, p);
        // easingLeft/Right 把曲线裁剪到子区间后再归一化
        let l = number(&e["easingLeft"], 0.);
        let r = number(&e["easingRight"], 1.);
        let d = f(r) - f(l);
        if d.abs() < 1e-12 {
            p
        } else {
            (f(l + (r - l) * p) - f(l)) / d
        }
    }
}

/// 单个事件的取值器：带 `trajectory` 就按轨迹采样，否则在 start/end 之间按缓动插值。
struct MotionTween {
    trajectory: Option<std::rc::Rc<Trajectory>>,
    event: Value,
    axis: usize,
    phase: f64,
}

impl crate::core::TweenFunction for MotionTween {
    fn y(&self, p: f32) -> f32 {
        let p = p as f64 * self.phase;
        if let Some(t) = &self.trajectory {
            t.value(p)[self.axis] as f32
        } else {
            let start = number(&self.event["start"], 0.);
            (start + (number(&self.event["end"], 0.) - start) * ease(&self.event, p)) as f32
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// 把一条轨道的事件（可能含 `trajectory`）编译成 `AnimFloat`。
///
/// `axis`：0 = x，1 = y，2 = 旋转。时间按 `bpm` 换算成秒并乘 `factor`（`bpmfactor` 已经
/// 通过改 BPM 消化掉了，这里传 1.）。
pub fn motion(
    events: &[Value],
    axis: usize,
    bpm: &mut crate::core::BpmList,
    factor: f64,
) -> anyhow::Result<crate::core::AnimFloat> {
    use crate::core::{AnimFloat, Keyframe, TweenFunction};
    use anyhow::ensure;
    use std::rc::Rc;

    let mut entries = events
        .iter()
        .map(|e| Ok((bpm.time_beats(beat(&e["startTime"])?) * factor, bpm.time_beats(beat(&e["endTime"])?) * factor, e)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    entries.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut kfs = Vec::new();
    for (i, &(start, end, e)) in entries.iter().enumerate() {
        ensure!(start <= end, "invalid trajectory time range");
        let trajectory = if e["trajectory"].is_object() {
            ensure!(e["trajectory"]["version"] == 1, "unsupported trajectory version");
            Some(Rc::new(Trajectory::new(&e["trajectory"]["options"])?))
        } else {
            None
        };
        // 下一个事件开始时就截断当前事件（避免重叠区间互相覆盖）
        let cut = entries.get(i + 1).map_or(end, |n| end.min(n.0));
        let phase = if end <= start {
            1.
        } else {
            ((cut - start) / (end - start)).clamp(0., 1.)
        };
        let tween = Rc::new(MotionTween {
            trajectory,
            event: e.clone(),
            axis,
            phase,
        });
        if kfs.is_empty() {
            kfs.push(Keyframe::new(start.min(0.) - 1., tween.y(0.), 0));
        }
        if cut > start {
            kfs.push(Keyframe {
                time: start,
                value: 0.,
                tween: tween.clone(),
            });
            kfs.push(Keyframe::new(cut, 1., 0));
        }
        kfs.push(Keyframe::new(cut, tween.y(1.), 0));
    }
    if kfs.is_empty() {
        return Ok(AnimFloat::fixed(0.));
    }
    let mut anim = AnimFloat::new(kfs);
    // 第一个关键帧之前也要有值（`NEG_INFINITY` 起点 + 立刻 set_time(0)）
    anim.time = f64::NEG_INFINITY;
    anim.set_time(0.);
    Ok(anim)
}

/// 一条**全局**事件轨道（`globalEvents` 里的列）。
///
/// 与 `motion()` 不同：这里的轨道可以有时间间隙（间隙取 fallback）、
/// 支持反向 seek（`before_default` 决定间隙前的默认值）。
pub struct Track {
    events: Vec<(f64, f64, Value)>,
    fallback: Value,
    before_default: bool,
}

impl Track {
    pub fn new(
        events: &[Value],
        bpm: &mut crate::core::BpmList,
        factor: f64,
        fallback: Value,
        before_default: bool,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(factor.is_finite() && factor > 0., "invalid bpmfactor");
        let mut parsed = Vec::new();
        for e in events {
            let start = bpm.time_beats(beat(&e["startTime"])?) * factor;
            let end = bpm.time_beats(beat(&e["endTime"])?) * factor;
            anyhow::ensure!(start <= end, "RPEN event ends before start");
            parsed.push((start, end, e.clone()));
        }
        parsed.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(Self {
            events: parsed,
            fallback,
            before_default,
        })
    }

    /// 取 `time` 处的值。落在间隙里返回 fallback；命中事件则按缓动在 start/end 间插值
    /// （数字和数组都能插）。
    pub fn value(&self, time: f64) -> Value {
        let n = self.events.partition_point(|v| v.0 <= time);
        if self.events.is_empty() || (n == 0 && self.before_default) {
            return self.fallback.clone();
        }
        let (a, b, e) = &self.events[n.saturating_sub(1)];
        let p = if b <= a {
            1.
        } else {
            ((time - a) / (b - a)).clamp(0., 1.)
        };
        let p = ease(e, p);
        fn mix(a: &Value, b: &Value, p: f64) -> Value {
            if let (Some(a), Some(b)) = (a.as_f64(), b.as_f64()) {
                return Value::from(a + (b - a) * p);
            }
            if let (Some(a), Some(b)) = (a.as_array(), b.as_array()) {
                return Value::Array(a.iter().enumerate().map(|(i, a)| mix(a, b.get(i).unwrap_or(a), p)).collect());
            }
            if p >= 1. {
                b.clone()
            } else {
                a.clone()
            }
        }
        mix(&e["start"], &e["end"], p)
    }
}
