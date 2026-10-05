//! CPU equivalent of the native mask, EdgeMask and GlowMask passes.
//! Bit rows keep the nine-tap dilation inexpensive without any offscreen FBOs.
use super::Zone;
use once_cell::sync::Lazy;

// 位移噪点纹理运行时读取；缺失时返回 None，noise() 退化为 0（区域仍可判定，仅视觉位移关闭）。
static DISPLACE: Lazy<Option<image::RgbImage>> = Lazy::new(|| {
    let bytes = crate::core::read_block_asset("assets/blockarea/BlockNoise1.png")?;
    let source = image::load_from_memory(&bytes).ok()?.to_rgb8();
    Some(image::imageops::flip_vertical(&source))
});

#[derive(Default)]
pub(super) struct Masks {
    pub rgba: Vec<u8>,
    /// Ready-only normal R, ready-only postprocessed subtract G, disabled compose, hover.
    pub aux_rgba: Vec<u8>,
    /// Point-sampled raw camera outputs, packed enabled N/S and disabled N/S.
    pub sources_rgba: Vec<u8>,
    pub raw_disabled_green: Vec<u8>,
    pub width: usize,
    pub height: usize,
    active: Vec<u64>,
    ping: Vec<u64>,
    pong: Vec<u64>,
    last_dim: (usize, usize, f32),
    last_zones: Vec<Zone>,
    last_time: Option<f32>,
    layers: [Vec<u8>; 6],
    ready_green: Vec<u8>,
    gray_ping: Vec<u8>,
    gray_pong: Vec<u8>,
}

/// 遮罩相机分辨率 = 视口 / 这个数（官方是 8，即 1/8 网格）。
///
/// 官方（手机）用 1/8，画面上就是 **8 像素一格**；溶解边 + 外侧辉光加起来只有约 5 个
/// 台阶，看起来偏"粗"。桌面端有算力，这里提到 **1/4（4 像素一格）**，台阶数翻倍、
/// 边缘更细。代价是逐像素工作量 ×4（灰阶路径只在噪区淡入的 0.5 秒内走，可接受）。
/// 想更细可以改 2，但要先优化 `render_gray_rings` 的逐像素膨胀。
const MASK_DIVISOR: usize = 4;

impl Masks {
    pub fn render_displaced(&mut self, width: usize, height: usize, aspect: f32, zones: &[Zone], time: f32) {
        let (bw, bh) = ((width / MASK_DIVISOR).max(1), (height / MASK_DIVISOR).max(1));
        let (ew, eh) = (bw * 2, bh * 2);
        if self.last_dim == (ew, eh, aspect) && self.last_zones == zones && self.last_time == Some(time) {
            return;
        }
        if self.last_dim != (ew, eh, aspect) || self.last_zones != zones {
            self.last_dim = (ew, eh, aspect);
            self.last_zones.clear();
            self.last_zones.extend_from_slice(zones);
            for layer in &mut self.layers {
                layer.resize(bw * bh, 0);
                layer.fill(0);
            }
            self.raw_disabled_green.resize(bw * bh, 0);
            self.raw_disabled_green.fill(0);
            self.ready_green.resize(bw * bh, 0);
            self.ready_green.fill(0);
            // The six official cameras capture enabled N/S, disabled N/S
            // (including ready), and ready-only N/S. BlockSprite blends
            // SrcAlpha, One; each subtract sprite has alpha .1.
            for z in zones {
                let layer = if z.active { usize::from(z.invert) } else { 2 + usize::from(z.invert) };
                let opacity = if z.invert { 0.1 } else { z.opacity };
                raster_rows(bw, bh, aspect, z, |y, first, last| {
                    for x in first..last {
                        let i = y * bw + x;
                        self.layers[layer][i] = unorm(self.layers[layer][i] as f32 / 255. + opacity);
                        if z.invert && !z.active {
                            self.raw_disabled_green[i] = unorm(self.raw_disabled_green[i] as f32 / 255. + 0.1 * z.opacity);
                        }
                        if z.ready {
                            let ready = 4 + usize::from(z.invert);
                            self.layers[ready][i] = unorm(self.layers[ready][i] as f32 / 255. + opacity);
                            if z.invert {
                                self.ready_green[i] = unorm(self.ready_green[i] as f32 / 255. + 0.1 * z.opacity);
                            }
                        }
                    }
                });
            }
            self.sources_rgba.resize(bw * bh * 4, 0);
            for i in 0..bw * bh {
                for c in 0..4 {
                    self.sources_rgba[i * 4 + c] = self.layers[c][i];
                }
            }
        }
        self.width = ew;
        self.height = eh;
        self.rgba.resize(ew * eh * 4, 0);
        self.rgba.fill(0);
        self.aux_rgba.resize(ew * eh * 4, 0);
        self.aux_rgba.fill(0);
        let stride = ew.div_ceil(64);
        self.active.resize(stride * eh, 0);
        self.active.fill(0);
        // `Compose` 的 d/t 每帧只算一次，不要丢进逐像素循环里反复重建。
        let compose = Compose::new(time);
        for y in 0..bh {
            for x in 0..bw {
                let uv = [(x as f32 + 0.5) / bw as f32, (y as f32 + 0.5) / bh as f32];
                let duv = compose.uv(uv);
                let sx = (duv[0] * bw as f32).floor().clamp(0., (bw - 1) as f32) as usize;
                let sy = (duv[1] * bh as f32).floor().clamp(0., (bh - 1) as f32) as usize;
                let i = y * bw + x;
                let di = sy * bw + sx;
                let mask = unorm((self.layers[0][di] as f32 / 255. - subtract_enabled(self.layers[1][di])).abs());
                let n = self.layers[2][i] as f32 / 255.;
                let (sr, sg) = subtract_disabled(self.layers[3][i], self.raw_disabled_green[i]);
                let disabled = unorm((sr * sg - n).abs());
                let (_, ready_sg) = subtract_disabled(self.layers[5][i], self.ready_green[i]);
                for dy in 0..2 {
                    for dx in 0..2 {
                        let (xx, yy) = (x * 2 + dx, y * 2 + dy);
                        let dst = (yy * ew + xx) * 4;
                        self.rgba[dst] = mask;
                        self.rgba[dst + 3] = disabled;
                        // The Active material's misleading "Disabled" names
                        // actually bind the ready-only camera outputs (Start).
                        self.aux_rgba[dst] = self.layers[4][i];
                        self.aux_rgba[dst + 1] = unorm(ready_sg);
                        self.aux_rgba[dst + 2] = disabled;
                        if mask != 0 {
                            self.active[yy * stride + xx / 64] |= 1 << (xx % 64);
                        }
                    }
                }
            }
        }
        self.render_rings();
        self.last_time = Some(time);
    }

    fn render_rings(&mut self) {
        let (width, height) = (self.width, self.height);
        if self.rgba.chunks_exact(4).any(|p| p[0] != 0 && p[0] != 255) {
            self.render_gray_rings();
            return;
        }
        let stride = width.div_ceil(64);
        self.ping.clone_from(&self.active);
        self.pong.resize(self.active.len(), 0);
        for (pass, weight) in glow_weights().into_iter().enumerate() {
            if weight < 0.01 {
                break;
            }
            dilate(&self.ping, &mut self.pong, width, height);
            for y in 0..height {
                for word in 0..stride {
                    let i = y * stride + word;
                    let mut ring = self.pong[i] & !self.ping[i];
                    while ring != 0 {
                        let bit = ring.trailing_zeros() as usize;
                        let x = word * 64 + bit;
                        if x < width {
                            let dst = (y * width + x) * 4;
                            if pass == 0 {
                                self.rgba[dst + 1] = 255;
                            }
                            self.rgba[dst + 2] = unorm(weight);
                        }
                        ring &= ring - 1;
                    }
                }
            }
            std::mem::swap(&mut self.ping, &mut self.pong);
        }
    }

    fn render_gray_rings(&mut self) {
        let (w, h) = (self.width, self.height);
        self.gray_ping.resize(w * h, 0);
        self.gray_pong.resize(w * h, 0);
        for (i, p) in self.rgba.chunks_exact(4).enumerate() {
            self.gray_ping[i] = p[0];
        }
        for (pass, weight) in glow_weights().into_iter().enumerate() {
            if weight < 0.01 {
                break;
            }
            // 可分离 max：水平 3 抽头 -> 竖直 3 抽头，等价于 3x3 方形邻域 max，
            // 每像素只读 6 次（原来 9 次）且去掉内层邻域双循环。多趟膨胀逐趟一致。
            for y in 0..h {
                let base = y * w;
                for x in 0..w {
                    let i = base + x;
                    let xl = x.saturating_sub(1);
                    let xr = (x + 1).min(w - 1);
                    self.gray_pong[i] = self.gray_ping[base + xl].max(self.gray_ping[i]).max(self.gray_ping[base + xr]);
                }
            }
            for y in 0..h {
                let base = y * w;
                let yl = y.saturating_sub(1) * w;
                let yr = (y + 1).min(h - 1) * w;
                for x in 0..w {
                    let i = base + x;
                    let maximum = self.gray_pong[yl + x].max(self.gray_pong[i]).max(self.gray_pong[yr + x]);
                    let delta = (maximum - self.gray_ping[i]) as f32 / 255.;
                    if pass == 0 {
                        self.rgba[i * 4 + 1] = maximum - self.gray_ping[i];
                    }
                    let outside = 1. - self.rgba[i * 4] as f32 / 255.;
                    self.rgba[i * 4 + 2] = unorm(weight * (outside * delta) + self.rgba[i * 4 + 2] as f32 / 255.);
                    self.gray_ping[i] = maximum;
                }
            }
        }
    }
}

fn glow_weights() -> [f32; 6] {
    let sum: f32 = (1..=6).map(|k| (k as f32).powf(2.65)).sum();
    std::array::from_fn(|pass| ((6 - pass) as f32).powf(2.65) / sum)
}

fn unorm(v: f32) -> u8 {
    (v.clamp(0., 1.) * 255.).round() as u8
}

fn subtract_enabled(red: u8) -> f32 {
    // Native SubtractBlockBlender pass 0: a threshold window, not parity.
    let r = red as f32 / 255.;
    if (0.09..0.12).contains(&r) {
        1.
    } else {
        0.
    }
}

fn subtract_disabled(red: u8, green: u8) -> (f32, f32) {
    let g = green as f32 / 255.;
    let t = ((g - 0.2) * -10.).clamp(0., 1.);
    let r = subtract_enabled(red) + t * t * (3. - 2. * t);
    // The intermediate is RG16, clamped and rounded after shader output.
    (unorm(r) as f32 / 255., unorm(g * r * 10.) as f32 / 255.)
}

/// 位移合成里**每帧只算一次**的量。
///
/// `d`（官方归一化方向）和 `t`（相位）原先在逐像素循环里反复重算，白白多两次
/// `medium()` 和两次乘法；这里提到每帧算一次，`Compose::uv` 只做真正随像素变化的部分。
#[derive(Clone, Copy)]
pub(super) struct Compose {
    /// mediump 精度的归一化方向
    d: f32,
    /// 相位：`time / 20. * 2.59`
    t: f32,
}

impl Compose {
    pub fn new(time: f32) -> Self {
        Self {
            d: medium(0.5 * medium((0.5_f32).sqrt().recip())),
            t: time / 20. * 2.59,
        }
    }

    /// BlockCompose pass 0, preserving the native normalized direction and ST.
    /// Native u_xlat16_* are mediump, not full-precision CPU floats. In the
    /// exported GLES shader the inverse length, direction, sampled noise and
    /// centered noise are separately rounded to binary16. Keeping the warp
    /// coordinates highp after those steps is essential at point boundaries.
    #[inline]
    pub fn uv(&self, uv: [f32; 2]) -> [f32; 2] {
        let (d, dt) = (self.d, self.d * self.t);
        let st = [uv[0] * 2.13, uv[1] * 1.02];
        let a = medium(medium(noise([dt + st[0], dt + st[1]])) - 0.5);
        let b = medium(medium(noise([-dt + st[0], dt + st[1]])) - 0.5);
        [(d * a + b * -d) * 0.1 + uv[0], (d * a + b * d) * 0.1 + uv[1]]
    }
}

fn medium(value: f32) -> f32 {
    // The normalized noise/direction intermediates are zero or normal f16
    // numbers. Round ten fraction bits with ties-to-even; no heap or new dep.
    let bits = value.to_bits();
    f32::from_bits((bits + 0xfff + ((bits >> 13) & 1)) & !0x1fff)
}

fn noise(uv: [f32; 2]) -> f32 {
    let Some(displace) = DISPLACE.as_ref() else {
        return 0.;
    };
    let (w, h) = (displace.width(), displace.height());
    let mirror = |v: f32, size: u32| {
        let v = v.rem_euclid(2.);
        let v = if v > 1. { 2. - v } else { v };
        (v * size as f32).floor().clamp(0., (size - 1) as f32) as u32
    };
    // Match the vertically flipped rows uploaded by the material renderer.
    displace.get_pixel(mirror(uv[0], w), mirror(uv[1], h))[0] as f32 / 255.
}

fn raster_rows(width: usize, height: usize, aspect: f32, zone: &Zone, mut write: impl FnMut(usize, usize, usize)) {
    if zone.half.x <= 0. || zone.half.y <= 0. {
        return;
    }
    let (s, c) = zone.angle.sin_cos();
    let y_half = s.abs() * zone.half.x + c.abs() * zone.half.y;
    let start = (((zone.center.y - y_half) * aspect + 1.) * height as f32 * 0.5 - 0.5)
        .ceil()
        .clamp(0., height as f32) as usize;
    let end = ((((zone.center.y + y_half) * aspect + 1.) * height as f32 * 0.5 - 0.5).floor() + 1.).clamp(0., height as f32) as usize;
    for y in start..end {
        let dy = ((y as f32 + 0.5) * 2. / height as f32 - 1.) / aspect - zone.center.y;
        let (mut lo, mut hi) = (-1.0_f32, 1.0_f32);
        for (a, b, half) in [(c, s * dy, zone.half.x), (-s, c * dy, zone.half.y)] {
            if a.abs() < 1e-7 {
                if b.abs() > half {
                    hi = lo - 1.;
                    break;
                }
            } else {
                let a0 = (-half - b) / a + zone.center.x;
                let a1 = (half - b) / a + zone.center.x;
                lo = lo.max(a0.min(a1));
                hi = hi.min(a0.max(a1));
            }
        }
        if lo > hi {
            continue;
        }
        let first = ((lo + 1.) * width as f32 * 0.5 - 0.5).ceil().clamp(0., width as f32) as usize;
        let last = (((hi + 1.) * width as f32 * 0.5 - 0.5).floor() + 1.).clamp(0., width as f32) as usize;
        write(y, first, last);
    }
}

fn dilate(source: &[u64], dest: &mut [u64], width: usize, height: usize) {
    let stride = width.div_ceil(64);
    let last_mask = u64::MAX >> ((64 - width % 64) % 64);
    for y in 0..height {
        for x in 0..stride {
            let mut value = 0;
            for row in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                let idx = row * stride + x;
                let mid = source[idx];
                let left = if x > 0 { source[idx - 1] >> 63 } else { 0 };
                let right = if x + 1 < stride { source[idx + 1] << 63 } else { 0 };
                value |= mid | (mid << 1) | left | (mid >> 1) | right;
            }
            dest[y * stride + x] = if x + 1 == stride { value & last_mask } else { value };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Vector;

    #[test]
    fn compose_uses_native_mediump_intermediate_rounding() {
        assert_eq!(medium(0.5 * medium((0.5_f32).sqrt().recip())), 0.70703125);
        assert_eq!(medium(medium(155. / 255.) - 0.5), 0.10791015625);
        assert_eq!(medium(1. + 1. / 2048.), 1., "ties round to even");
        assert_eq!(medium(1. + 3. / 2048.), 1.001953125);
    }

    fn zone(x: f32, y: f32, half_x: f32, half_y: f32, angle: f32, invert: bool) -> Zone {
        Zone {
            center: Vector::new(x, y),
            half: Vector::new(half_x, half_y),
            angle,
            invert,
            active: true,
            ready: false,
            opacity: 1.,
        }
    }

    #[test]
    fn nine_tap_dilation_matches_pixel_reference_across_word_boundaries() {
        for width in [1_usize, 63, 64, 65, 129] {
            let height = 9;
            let stride = width.div_ceil(64);
            let mut source = vec![0_u64; stride * height];
            for y in 0..height {
                for x in 0..width {
                    if (x * 11 + y * 7) % 17 == 0 {
                        source[y * stride + x / 64] |= 1 << (x % 64);
                    }
                }
            }
            let mut dest = vec![0; source.len()];
            dilate(&source, &mut dest, width, height);
            for y in 0..height {
                for x in 0..width {
                    let mut expected = false;
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let sx = (x as i32 + dx).clamp(0, width as i32 - 1) as usize;
                            let sy = (y as i32 + dy).clamp(0, height as i32 - 1) as usize;
                            expected |= source[sy * stride + sx / 64] & (1 << (sx % 64)) != 0;
                        }
                    }
                    assert_eq!(dest[y * stride + x / 64] & (1 << (x % 64)) != 0, expected, "{width}: ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn raw_camera_layers_match_resolved_rectangle_coverage() {
        let zones = [
            zone(0.1, -0.1, 0.45, 0.23, 0.57, false),
            zone(-0.5, 0.13, 0.33, 0.09, -1.17, false),
            zone(0.0, 0.0, 1.0, 0.6, 0.0, true),
            zone(0.15, 0.2, 0.12, 0.21, 1.32, true),
        ];
        let (width, height, aspect) = (129, 75, 16. / 9.);
        let mut masks = Masks::default();
        // 反过来凑输入尺寸，让相机网格正好是 (width, height)
        masks.render_displaced(width * MASK_DIVISOR, height * MASK_DIVISOR, aspect, &zones, 0.);
        for y in 0..height {
            for x in 0..width {
                let p = Vector::new((x as f32 + 0.5) * 2. / width as f32 - 1., ((y as f32 + 0.5) * 2. / height as f32 - 1.) / aspect);
                let mut normal = false;
                let mut subtract_count = 0_usize;
                for z in &zones {
                    let local = nalgebra::Rotation2::new(-z.angle) * (p - z.center);
                    if local.x.abs() <= z.half.x && local.y.abs() <= z.half.y {
                        if z.invert {
                            subtract_count += 1;
                        } else {
                            normal = true;
                        }
                    }
                }
                assert_eq!(masks.sources_rgba[(y * width + x) * 4] != 0, normal, "normal ({x}, {y})");
                assert_eq!(masks.sources_rgba[(y * width + x) * 4 + 1], [0, 26, 52][subtract_count], "subtract ({x}, {y})");
            }
        }
    }

    #[test]
    fn exterior_rings_leave_fill_and_union_interiors_unchanged() {
        let mut masks = Masks::default();
        masks.render_displaced(800, 800, 1., &[zone(-0.2, 0., 0.3, 0.3, 0., false), zone(0.2, 0., 0.3, 0.3, 0., false)], 1.);
        let p = ((masks.height / 2) * masks.width + masks.width / 2) * 4;
        assert_eq!(&masks.rgba[p..p + 4], &[255, 0, 0, 0]);
        for p in masks.rgba.chunks_exact(4) {
            if p[0] == 255 {
                assert_eq!(&p[1..3], &[0, 0]);
            }
            if p[1] == 255 {
                assert_eq!(p[0], 0);
                assert!(p[2] > 0);
            }
        }
        assert!(masks.rgba.chunks_exact(4).any(|p| p[1] == 255));
    }

    #[test]
    fn subtract_cancellation_has_no_phantom_edges_and_zone_count_is_unbounded() {
        let mut masks = Masks::default();
        masks.render_displaced(800, 800, 1., &[zone(0., 0., 0.5, 0.3, 0., true), zone(0., 0., 0.5, 0.3, 0., true)], 1.);
        assert!(masks.rgba.iter().all(|&v| v == 0));
        let mut zones: Vec<_> = (0..159).map(|_| zone(2., 2., 0.1, 0.1, 0., false)).collect();
        zones.push(zone(0., 0., 0.2, 0.2, 0., false));
        masks.render_displaced(800, 800, 1., &zones, 1.);
        let center = ((masks.height / 2) * masks.width + masks.width / 2) * 4;
        assert_eq!(masks.rgba[center], 255);
        let previous = masks.rgba.clone();
        masks.render_displaced(800, 800, 1., &zones, 1.);
        assert_eq!(masks.rgba, previous);
        masks.render_displaced(520, 560, 1., &zones, 1.);
        assert_eq!(
            (masks.width, masks.height),
            (520 / MASK_DIVISOR * 2, 560 / MASK_DIVISOR * 2)
        );
    }

    #[test]
    fn glow_weight_matches_native_material_and_skips_last_ring() {
        let weights = glow_weights();
        assert!((weights.iter().sum::<f32>() - 1.).abs() < 1e-6);
        assert!((weights[4] - 0.024947807).abs() < 1e-7);
        assert!(weights[5] < 0.01);
    }

    #[test]
    fn native_mask_and_effect_dimensions_match_start_and_point_upsampling() {
        let mut masks = Masks::default();
        masks.render_displaced(960, 540, 16. / 9., &[zone(0., 0., 0.5, 0.25, 0., false)], 1.);
        // 960/MASK_DIVISOR x 540/MASK_DIVISOR 的相机输出，最近邻 2 倍放大进 effect RT。
        let (cw, ch) = (960 / MASK_DIVISOR, 540 / MASK_DIVISOR);
        assert_eq!((masks.width, masks.height), (cw * 2, ch * 2));
        for y in (0..ch * 2).step_by(2) {
            for x in (0..cw * 2).step_by(2) {
                let sample = |px: usize, py: usize| masks.rgba[(py * masks.width + px) * 4];
                assert_eq!(sample(x, y), sample(x + 1, y));
                assert_eq!(sample(x, y), sample(x, y + 1));
            }
        }
    }

    #[test]
    fn disabled_subtract_does_not_clip_enabled_region() {
        let active = zone(0., 0., 0.5, 0.25, 0., false);
        let disabled = Zone {
            active: false,
            invert: true,
            ..active.clone()
        };
        let mut original = Masks::default();
        original.render_displaced(960, 540, 16. / 9., &[active.clone()], 1.);
        let mut mixed = Masks::default();
        mixed.render_displaced(960, 540, 16. / 9., &[active, disabled], 1.);
        assert!(original
            .rgba
            .chunks_exact(4)
            .zip(mixed.rgba.chunks_exact(4))
            .all(|(a, b)| a[..3] == b[..3]));
        assert!(mixed.rgba.chunks_exact(4).any(|p| p[0] == 255 && p[3] == 255));
    }

    #[test]
    fn visual_subtract_uses_threshold_window_including_three_overlaps() {
        let sub = zone(0., 0., 1., 1., 0., true);
        for (count, value) in [(1, 255), (2, 0), (3, 0)] {
            let mut mask = Masks::default();
            mask.render_displaced(128, 96, 4. / 3., &vec![sub.clone(); count], 2.);
            assert!(mask.rgba.chunks_exact(4).all(|p| p[0] == value));
        }
    }

    #[test]
    fn ready_inputs_use_ready_only_cameras_and_disabled_compose() {
        let disabled = Zone {
            active: false,
            ..zone(-0.5, 0., 0.2, 0.2, 0., false)
        };
        let ready = Zone {
            active: false,
            ready: true,
            ..zone(0.5, 0., 0.2, 0.2, 0., false)
        };
        let mut mask = Masks::default();
        mask.render_displaced(128, 96, 4. / 3., &[disabled, ready], 1.);
        // 两个区域分别在屏幕 25% / 75% 处，取它们中心那格（相机网格坐标）
        let (cw, ch) = (mask.width / 2, mask.height / 2);
        let at = |num: usize| {
            // effect 是相机网格的 2 倍，所以取样点也要 ×2：num=1 -> 25%，num=3 -> 75%
            let i = (ch * mask.width + (cw * num) / 2) * 4;
            [mask.aux_rgba[i], mask.aux_rgba[i + 1], mask.aux_rgba[i + 2]]
        };
        assert_eq!(at(1), [0, 0, 255]);
        assert_eq!(at(3), [255, 0, 255]);
    }

    #[test]
    fn compose_displacement_animates_only_enabled_masks_and_uses_red_channel() {
        // 官方位移贴图不在（干净检出 / CI）时跳过：没有它位移恒为 0，断言无从谈起。
        if DISPLACE.is_none() {
            eprintln!("skipping: assets/blockarea/BlockNoise1.png 不在，无法校验位移合成");
            return;
        }
        let active = zone(0., 0., 0.5, 0.2, 0., false);
        let disabled = Zone {
            active: false,
            ..active.clone()
        };
        let mut mask = Masks::default();
        mask.render_displaced(960, 540, 16. / 9., &[active.clone(), disabled.clone()], 1.);
        let before = mask.rgba.clone();
        mask.render_displaced(960, 540, 16. / 9., &[active, disabled], 2.);
        assert!(before.chunks_exact(4).zip(mask.rgba.chunks_exact(4)).any(|(a, b)| a[0] != b[0]));
        assert!(before.chunks_exact(4).zip(mask.rgba.chunks_exact(4)).all(|(a, b)| a[3] == b[3]));
        let pixel = DISPLACE.as_ref().unwrap().get_pixel(0, 0);
        assert_eq!(noise([0.0001, 0.0001]), pixel[0] as f32 / 255.);
        assert_ne!(pixel[0], pixel[1]);
    }

    #[test]
    fn initial_subtract_show_fades_green_while_alpha_stays_point_one() {
        let sub = Zone {
            active: false,
            opacity: 0.25,
            ..zone(0., 0., 1., 1., 0., true)
        };
        let mut mask = Masks::default();
        mask.render_displaced(128, 96, 4. / 3., &[sub], 1.);
        assert_eq!(mask.sources_rgba[3], 26);
        assert_eq!(mask.raw_disabled_green[0], 6);
        assert_eq!(mask.rgba[3], 120);
    }

    /// 诊断用（默认 ignore）：把**真实谱面**在某几个时刻的 CPU 遮罩导成 PNG，
    /// 并把每个噪区换算回屏幕百分比打印出来，用来核对噪区边界。
    ///
    /// ```text
    /// BLOCK_PROBE_CHART=<谱面.json> BLOCK_PROBE_OUT=<输出目录> \
    ///   cargo test -p prpr --lib -- --ignored --nocapture probe_dump_masks
    /// ```
    #[test]
    #[ignore = "diagnostic dump; needs BLOCK_PROBE_CHART / BLOCK_PROBE_OUT"]
    fn probe_dump_masks() {
        let (Ok(path), Ok(out)) = (std::env::var("BLOCK_PROBE_CHART"), std::env::var("BLOCK_PROBE_OUT")) else {
            eprintln!("set BLOCK_PROBE_CHART / BLOCK_PROBE_OUT");
            return;
        };
        let source = std::fs::read_to_string(&path).unwrap();
        let chart = crate::parse::parse_phigros(&source, Default::default()).unwrap();
        let aspect = 16.0f32 / 9.0;
        let (w, h) = (1920usize, 1080usize);
        std::fs::create_dir_all(&out).unwrap();
        let times: Vec<f64> = std::env::var("BLOCK_PROBE_TIMES")
            .ok()
            .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect())
            .unwrap_or_else(|| vec![85.6, 105.5, 108.0, 108.45, 110.0, 112.0, 116.0, 129.0]);
        for t in times {
            let zones: Vec<Zone> = chart.block_areas.iter().filter_map(|b| Zone::from_area(b, t, aspect)).collect();
            println!("t={t}  可见噪区 {} 个", zones.len());
            for z in &zones {
                println!(
                    "   center=({:.3}%,{:.3}%)  size=({:.3}%,{:.3}%)  ang={:.1}°  active={} subtract={} ready={} opacity={:.2}",
                    (z.center.x + 1.) * 50.,
                    (z.center.y * aspect + 1.) * 50.,
                    z.half.x * 100.,
                    z.half.y * aspect * 100.,
                    z.angle.to_degrees(),
                    z.active,
                    z.invert,
                    z.ready,
                    z.opacity
                );
            }
            let mut m = Masks::default();
            m.render_displaced(w, h, aspect, &zones, t as f32);
            let (mw, mh) = (m.width, m.height);
            for (idx, name) in ["compose", "edge", "glow", "disabled"].iter().enumerate() {
                let plane: Vec<u8> = m.rgba.chunks_exact(4).flat_map(|p| [p[idx], p[idx], p[idx], 255]).collect();
                image::RgbaImage::from_raw(mw as u32, mh as u32, plane)
                    .unwrap()
                    .save(format!("{out}/mask-{t}-{name}.png"))
                    .unwrap();
            }
            println!("   -> {out}/mask-{t}-*.png  ({mw}x{mh})");
        }
    }
}
