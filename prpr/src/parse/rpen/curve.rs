//! RPE 缓动曲线求值（0..1 归一化）。
//!
//! 从上游 `Phira-Pro` 的 `core::rpe_next_block::curve` 原样移植。
//! 之所以不复用我们的 `RPE_TWEEN_MAP`：那边是 tween 对象、按「在 x..y 之间插值」用，
//! 这里是「给定 0..1 进度求缓动值」，语义不同；照搬能保证和上游数值逐位一致。

use std::f64::consts::PI;

/// `id` = RPE 的 `easingType`（1..=29），`p` = 0..1 进度。
pub fn curve(id: u8, p: f64) -> f64 {
    let (family, direction) = match id {
        1 => return p,
        2 => (1, 1),
        3 => (1, 0),
        4 => (2, 1),
        5 => (2, 0),
        6 => (1, 2),
        7 => (2, 2),
        8 => (3, 1),
        9 => (3, 0),
        10 => (4, 1),
        11 => (4, 0),
        12 => (3, 2),
        13 => (4, 2),
        14 => (5, 1),
        15 => (5, 0),
        16 => (6, 1),
        17 => (6, 0),
        18 => (7, 1),
        19 => (7, 0),
        20 => (8, 1),
        21 => (8, 0),
        22 => (7, 2),
        23 => (8, 2),
        24 => (9, 1),
        25 => (9, 0),
        26 => (10, 1),
        27 => (10, 0),
        28 => (10, 2),
        29 => (9, 2),
        _ => return p,
    };
    let bounce_out = |x: f64| {
        let (shift, offset) = if x < 4. / 11. {
            (0., 0.)
        } else if x < 8. / 11. {
            (6. / 11., 0.75)
        } else if x < 10. / 11. {
            (9. / 11., 0.9375)
        } else {
            (21. / 22., 0.984375)
        };
        121. / 16. * (x - shift).powi(2) + offset
    };
    let inside = |x: f64| match family {
        1 => 1. - (x * PI / 2.).cos(),
        2..=5 => x.powi(family),
        6 => {
            if x == 0. {
                0.
            } else {
                2_f64.powf(10. * (x - 1.))
            }
        }
        7 => 1. - (1. - x * x).abs().sqrt(),
        8 => x * x * ((1.70158 + 1.) * x - 1.70158),
        9 => {
            if x == 0. || x == 1. {
                x
            } else {
                -2_f64.powf(10. * x - 10.) * ((10. * x - 10.75) * 2. * PI / 3.).sin()
            }
        }
        10 => 1. - bounce_out(1. - x),
        _ => x,
    };
    match direction {
        0 => inside(p),
        1 => 1. - inside(1. - p),
        _ if family == 8 => {
            let k = 1.70158 * 1.525;
            if p < 0.5 {
                let x = 2. * p;
                x * x * ((k + 1.) * x - k) / 2.
            } else {
                let x = 2. * p - 2.;
                (x * x * ((k + 1.) * x + k) + 2.) / 2.
            }
        }
        _ if family == 9 => {
            if p == 0. || p == 1. {
                p
            } else {
                let wave = ((20. * p - 11.125) * 2. * PI / 4.5).sin() / 2.;
                if p < 0.5 {
                    -2_f64.powf(20. * p - 10.) * wave
                } else {
                    2_f64.powf(10. - 20. * p) * wave + 1.
                }
            }
        }
        _ => {
            if p < 0.5 {
                inside(2. * p) / 2.
            } else {
                1. - inside(2. - 2. * p) / 2.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::curve;

    #[test]
    fn linear_and_endpoints() {
        assert!((curve(1, 0.37) - 0.37).abs() < 1e-12, "id 1 是线性");
        // 所有缓动在两端都必须落在 0 / 1 附近
        for id in 1..=29u8 {
            assert!(curve(id, 0.).abs() < 1e-6, "id {id} 起点应为 0");
            assert!((curve(id, 1.) - 1.).abs() < 1e-6, "id {id} 终点应为 1");
        }
    }

    #[test]
    fn monotonic_families_are_in_range() {
        // 只检查几个「单调」家族，回弹/弹性类会超出 0..1
        for id in [1u8, 2, 3, 4, 5, 6, 7] {
            for i in 0..=10 {
                let v = curve(id, i as f64 / 10.);
                assert!((-1e-6..=1. + 1e-6).contains(&v), "id {id} 越界：{v}");
            }
        }
    }
}
