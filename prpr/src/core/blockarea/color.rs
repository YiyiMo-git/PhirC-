//! 彩色噪域：每个像素记住「最近的那个可见矩形」，再把它自己的 RGB 写进颜色贴图。
//!
//! 移植自 Phira-Pro-0.8.2-pro.11 的 block_color.rs。要点：
//!
//! * **颜色与覆盖是解耦的** —— tint 不影响 subtract 判定、输入、opacity，
//!   也不影响边缘/辉光掩码。这里只负责产出一张 RGB 贴图给 frag 采样。
//! * 只有**在原生合成里存活下来的那一侧**才播种颜色（Masks::color_source）：
//!   被抵消掉的矩形不能在洞里留下 tint。
//! * 播种完做**两趟 chamfer 扫描**（正向 + 反向）把最近的种子往外扩 ——
//!   这样位移填充和外侧辉光取到的是「最近的可见矩形」的颜色，而不是黑色或
//!   被抵消掉那个矩形的颜色。O(像素)，不是「每个像素测所有矩形」。
//!
//! 两层：0 = 激活、1 = 未激活（含 ready）。分别对应 frag 的
//! uActiveColors / uDisabledColors。

use super::{
    mask::{raster_rows, Masks},
    Zone,
};
use crate::core::DEFAULT_BLOCK_COLOR;

#[derive(Default)]
pub(super) struct Colors {
    /// [激活, 未激活] 两张 RGBA 颜色贴图。
    pub rgba: [Vec<u8>; 2],
    pub width: usize,
    pub height: usize,
    /// 内容版本号；上传方靠它判断要不要重传。
    pub revision: u64,
    aspect: f32,
    zones: Vec<Zone>,
    seeds: Vec<usize>,
    owners: Vec<usize>,
}

impl Colors {
    /// 重新生成两张颜色贴图。返回 false 表示**这一帧没有彩色噪域**
    /// （调用方应当把 uColored 置 0，走原生红色路径）。
    pub fn update(&mut self, masks: &Masks, aspect: f32, zones: &[Zone]) -> bool {
        let (width, height) = (masks.width, masks.height);
        let colored = zones.iter().any(|z| z.opacity > 0. && z.color != DEFAULT_BLOCK_COLOR);
        if !colored {
            return false;
        }
        if (self.width, self.height, self.aspect) == (width, height, aspect) && self.zones == zones {
            return true;
        }
        self.width = width;
        self.height = height;
        self.aspect = aspect;
        self.zones.clear();
        self.zones.extend_from_slice(zones);
        self.seeds.resize(width * height, usize::MAX);
        self.owners.resize(width * height, usize::MAX);

        for layer in 0..2 {
            self.seeds.fill(usize::MAX);
            self.owners.fill(usize::MAX);
            // 只有存活的那一侧播种。同类里**后画的 z-order 更大**。
            for invert in [true, false] {
                for (id, z) in zones
                    .iter()
                    .enumerate()
                    .filter(|(_, z)| z.active == (layer == 0) && z.opacity > 0. && z.invert == invert)
                {
                    let (seeds, owners) = (&mut self.seeds, &mut self.owners);
                    raster_rows(width, height, aspect, z, |y, first, last| {
                        for i in y * width + first..y * width + last {
                            if masks.color_source(layer, i) == Some(invert) {
                                seeds[i] = i;
                                owners[i] = id;
                            }
                        }
                    });
                }
            }
            // 两趟 chamfer：把最近的种子往洞里和边界外扩。
            for reverse in [false, true] {
                for n in 0..width * height {
                    let i = if reverse { width * height - 1 - n } else { n };
                    if self.seeds[i] == i {
                        continue;
                    }
                    let (x, y) = (i % width, i / width);
                    let distance = |seed: usize| -> usize {
                        if seed == usize::MAX {
                            usize::MAX
                        } else {
                            x.abs_diff(seed % width).pow(2) + y.abs_diff(seed / width).pow(2)
                        }
                    };
                    let mut best = self.seeds[i];
                    let mut dist = distance(best);
                    let dy = if reverse { 1_isize } else { -1 };
                    for (dx, dy) in [(dy, 0), (-1, dy), (0, dy), (1, dy)] {
                        let (nx, ny) = (x as isize + dx, y as isize + dy);
                        if nx < 0 || ny < 0 || nx >= width as isize || ny >= height as isize {
                            continue;
                        }
                        let seed = self.seeds[ny as usize * width + nx as usize];
                        let d = distance(seed);
                        if d < dist {
                            best = seed;
                            dist = d;
                        }
                    }
                    self.seeds[i] = best;
                }
            }
            let bytes = &mut self.rgba[layer];
            bytes.resize(width * height * 4, 0);
            for (pixel, &seed) in bytes.chunks_exact_mut(4).zip(&self.seeds) {
                let rgb = if seed == usize::MAX {
                    DEFAULT_BLOCK_COLOR
                } else {
                    zones[self.owners[seed]].color
                };
                for (dst, c) in pixel[..3].iter_mut().zip(rgb) {
                    *dst = (c.clamp(0., 1.) * 255.).round() as u8;
                }
                pixel[3] = 255;
            }
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Vector;

    fn zone(x: f32, color: [f32; 3], active: bool, invert: bool) -> Zone {
        Zone {
            projection: None,
            line2area: false,
            y_scale: 1.,
            color,
            center: Vector::new(x, 0.),
            half: Vector::new(0.2, 0.2),
            angle: 0.,
            invert,
            active,
            ready: false,
            opacity: 1.,
        }
    }

    fn masks(w: usize, h: usize) -> Masks {
        // ⚠️ 不能写 `Masks { width, height, ..Default::default() }` —— 它有一堆私有
        // 字段，`..Default::default()` 要求所有字段都可见。先 default 再赋公开字段。
        let mut m = Masks::default();
        m.width = w;
        m.height = h;
        m
    }

    /// 没有彩色噪域时直接返回 false（调用方走原生红色路径）。
    #[test]
    fn plain_red_zones_report_uncolored() {
        let mut colors = Colors::default();
        assert!(!colors.update(&masks(8, 8), 1., &[zone(0., DEFAULT_BLOCK_COLOR, true, false)]));
    }

    /// 有彩色噪域时生成贴图，且像素颜色来自该矩形自己。
    #[test]
    fn colored_zone_paints_its_own_rgb() {
        let mut m = masks(16, 16);
        m.layers[0].resize(16 * 16, 255);
        let mut colors = Colors::default();
        assert!(colors.update(&m, 1., &[zone(0., [0., 1., 0.], true, false)]));
        assert_eq!(&colors.rgba[0][0..3], &[0, 255, 0], "应当画成绿色");
        assert_eq!(colors.rgba[0][3], 255);
        assert_eq!((colors.width, colors.height), (16, 16));
    }

    /// 被抵消掉的那一侧不播种：整层都是「反转占上风」时，普通矩形不该上色。
    #[test]
    fn canceled_side_does_not_seed_color() {
        let mut m = masks(16, 16);
        m.layers[0].resize(16 * 16, 255);
        m.layers[1].resize(16 * 16, 26); // 阈值窗内 → inverse 占上风
        let mut colors = Colors::default();
        assert!(colors.update(&m, 1., &[zone(0., [0., 1., 0.], true, false)]));
        assert_eq!(&colors.rgba[0][0..3], &[255, 84, 84], "没有种子 → 回落默认红");
    }
}
