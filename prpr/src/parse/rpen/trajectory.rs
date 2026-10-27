//! RPEN 轨迹（`moveXEvents` 里带 `trajectory` 的那种）求值。
//!
//! 从上游 `Phira-Pro` 的 `parse::rpen::trajectory` 移植。支持：
//! - 直角坐标（`xExpression` / `yExpression`，参数 `t`）
//! - 极坐标（`radiusExpression` + `angleStart/angleEnd`，参数 `theta`）
//! - `shape: star / random-polygon`
//! - `trimStart/trimEnd`、`scaleX/scaleY`、`rotation`、`alignStart`
//! - `tangentRotation`（沿切线取角度，跨 360° 做连续性修正）
//!
//! 采样点全部**预计算/预校验**（构造时跑一遍 0..1 的 65 个点），坏谱面在解析期就报错，
//! 不会在打歌时逐帧 panic。

use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::collections::HashMap;

use super::curve::curve;
use super::{flag, number};
use super::expression::Expr;

pub struct Trajectory {
    options: Value,
    vars: HashMap<String, f64>,
    expressions: HashMap<&'static str, Expr>,
    first: [f64; 3],
    /// `tangentRotation` 时预采样的角度表（1025 个点），用来做跨圈连续性修正。
    angles: Vec<f64>,
}

impl Trajectory {
    pub fn new(o: &Value) -> Result<Self> {
        let mut vars = HashMap::new();
        if let Some(p) = o["parameters"].as_object() {
            ensure!(p.len() <= 32, "too many trajectory parameters");
            for (k, v) in p {
                vars.insert(k.clone(), v.as_f64().context("non-numeric trajectory parameter")?);
            }
        }
        vars.insert("seed".into(), number(&o["seed"], 1.));
        vars.insert("randomness".into(), number(&o["randomness"], 0.));
        let polar = o["mode"] == "polar";
        let mut expressions = HashMap::new();
        for (k, d) in [
            ("begin", if polar { "0" } else { "0" }),
            ("finish", if polar { "2*pi" } else { "1" }),
            ("xExpression", "200*cos(2*pi*t)"),
            ("yExpression", "200*sin(2*pi*t)"),
            ("radiusExpression", "200"),
        ] {
            let key = match k {
                "begin" => {
                    if polar {
                        "angleStart"
                    } else {
                        "parameterStart"
                    }
                }
                "finish" => {
                    if polar {
                        "angleEnd"
                    } else {
                        "parameterEnd"
                    }
                }
                _ => k,
            };
            expressions.insert(k, Expr::parse(o[key].as_str().unwrap_or(d))?);
        }
        if let Some(s) = o["rotationExpression"].as_str().filter(|s| !s.trim().is_empty()) {
            expressions.insert("rotationExpression", Expr::parse(s)?);
        }
        let mut t = Self {
            options: o.clone(),
            vars,
            expressions,
            first: [0.; 3],
            angles: Vec::new(),
        };
        ensure!(
            number(&o["trimStart"], 0.) >= 0.
                && number(&o["trimEnd"], 1.) <= 1.
                && number(&o["trimEnd"], 1.) > number(&o["trimStart"], 0.),
            "invalid trajectory trim"
        );
        t.first = t.raw(0.)?;
        if flag(&o["tangentRotation"]) {
            for i in 0..=1024 {
                let last = t.angles.last().copied().unwrap_or(0.);
                let angle = t.tangent(i as f64 / 1024.).unwrap_or(last);
                t.angles.push(angle + 360. * ((last - angle) / 360.).round());
            }
        }
        // 预跑一遍：坏表达式在这里就炸，别留到逐帧渲染时。
        for i in 0..=64 {
            t.raw(i as f64 / 64.)?;
        }
        Ok(t)
    }

    fn raw(&self, p: f64) -> Result<[f64; 3]> {
        let o = &self.options;
        let u = number(&o["trimStart"], 0.)
            + (number(&o["trimEnd"], 1.) - number(&o["trimStart"], 0.)) * p;
        let begin = self.expressions["begin"].eval(&self.vars)?;
        let end = self.expressions["finish"].eval(&self.vars)?;
        let coordinate =
            |axis| begin + (end - begin) * curve(number(&o[axis], 1.) as u8, u);
        let mut v = self.vars.clone();
        v.insert("u".into(), p);
        let (x, y) = if let Some(shape) = o["shape"].as_str() {
            ensure!(
                shape == "star" || shape == "random-polygon",
                "unknown trajectory shape"
            );
            let param = |k, d| number(&o["parameters"][k], d);
            let n = param("points", 5.).round().clamp(3., 40.)
                * if shape == "star" { 2. } else { 1. };
            let a = u.rem_euclid(1.) * n;
            let i = a.floor();
            let vertex = |i: f64| {
                let i = i % n;
                let random = (i * 127.1 + number(&o["seed"], 1.) * 311.7).sin() * 43758.5453;
                let r = param("radius", 200.)
                    * if shape == "star" {
                        if i % 2. != 0. {
                            param("inner", 0.4)
                        } else {
                            1.
                        }
                    } else {
                        1. - number(&o["randomness"], 0.) * random.rem_euclid(1.)
                    };
                let angle = 2. * std::f64::consts::PI * i / n;
                [r * angle.cos(), r * angle.sin()]
            };
            let l = vertex(i);
            let r = vertex(i + 1.);
            (
                l[0] + (r[0] - l[0]) * (a - i),
                l[1] + (r[1] - l[1]) * (a - i),
            )
        } else if o["mode"] == "polar" {
            let theta = coordinate("easingX");
            v.insert("theta".into(), theta);
            v.insert("t".into(), u);
            let r = self.expressions["radiusExpression"].eval(&v)?;
            (r * theta.cos(), r * theta.sin())
        } else {
            v.insert("t".into(), coordinate("easingX"));
            let x = self.expressions["xExpression"].eval(&v)?;
            v.insert("t".into(), coordinate("easingY"));
            (x, self.expressions["yExpression"].eval(&v)?)
        };
        v.insert(
            "t".into(),
            if o["mode"] == "polar" { u } else { coordinate("easingX") },
        );
        v.insert("theta".into(), coordinate("easingX"));
        let rot = self
            .expressions
            .get("rotationExpression")
            .map(|e| e.eval(&v))
            .transpose()?
            .unwrap_or(0.);
        let a = number(&o["rotation"], 0.).to_radians();
        let x = x * number(&o["scaleX"], 1.);
        let y = y * number(&o["scaleY"], 1.);
        Ok([
            x * a.cos() - y * a.sin(),
            x * a.sin() + y * a.cos(),
            rot,
        ])
    }

    fn tangent(&self, p: f64) -> Option<f64> {
        // 步长由小到大试，够大能算出方向就返回。
        for d in [0.00001, 0.0001, 0.001, 0.01] {
            let a = self.raw((p - d).max(0.)).ok()?;
            let b = self.raw((p + d).min(1.)).ok()?;
            let (x, y) = (b[0] - a[0], b[1] - a[1]);
            if x.hypot(y) > 1e-9 {
                return Some(-y.atan2(x).to_degrees());
            }
        }
        None
    }

    /// 取 `p`（0..1）处的 `[x, y, rotation]`。
    pub fn value(&self, p: f64) -> [f64; 3] {
        let p = p.clamp(0., 1.);
        let mut v = self.raw(p).unwrap_or(self.first);
        if !self.angles.is_empty() {
            // 预采样角度做「跨圈连续性」基准，再叠加真实切线角
            let a = self.angles[(p * 1024.).round() as usize];
            let t = self.tangent(p).unwrap_or(a);
            v[2] = t + 360. * ((a - t) / 360.).round();
        }
        if flag(&self.options["alignStart"]) {
            v[0] += number(&self.options["startX"], 0.) - self.first[0];
            v[1] += number(&self.options["startY"], 0.) - self.first[1];
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::Trajectory;
    use serde_json::json;

    #[test]
    fn linear_trajectory_interpolates() {
        // 默认表达式是圆；这里换成 t 的线性映射验证求值链路
        let t = Trajectory::new(&json!({
            "xExpression": "100*t",
            "yExpression": "0",
            "parameterStart": "0",
            "parameterEnd": "1"
        }))
        .unwrap();
        let a = t.value(0.);
        let b = t.value(1.);
        assert!(a[0].abs() < 1e-6, "起点 x 应为 0，实际 {}", a[0]);
        assert!((b[0] - 100.).abs() < 1e-6, "终点 x 应为 100，实际 {}", b[0]);
        let m = t.value(0.5);
        assert!((m[0] - 50.).abs() < 1e-6, "中点 x 应为 50，实际 {}", m[0]);
    }

    #[test]
    fn tangent_rotation_follows_direction() {
        // 沿 +x 直线走，切线角应为 0
        let t = Trajectory::new(&json!({
            "xExpression": "100*t",
            "yExpression": "0",
            "parameterStart": "0",
            "parameterEnd": "1",
            "tangentRotation": true
        }))
        .unwrap();
        assert!(t.value(0.5)[2].abs() < 1e-3, "沿 +x 切线角应≈0");
    }

    #[test]
    fn bad_trim_and_bad_expression_are_rejected() {
        assert!(Trajectory::new(&json!({ "trimStart": 0.9, "trimEnd": 0.1 })).is_err());
        assert!(Trajectory::new(&json!({ "xExpression": "1+" })).is_err());
    }
}
