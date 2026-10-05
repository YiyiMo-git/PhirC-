// 「光波」(TriGrid) —— 由 kaichang/shaders/trigrid.glsl（用户提供的权威实现）逐行移植。
//
// 语义（横向扭曲 + mod 平铺 + sin 扫描波 p + 用 texColor 的 r/g/b 三通道
// 分别驱动 grid/glow/face 三层亮度）与原版完全一致，只加了两个 uniform：
//
//   aspect      —— 宽高比校正，见下面的注释；
//   alpha_scale —— 原版恒为 1.0；浅色主题需要更「实」的墨色时才 > 1。
//                  （原版 alpha 由亮度推出来，浅底上想画暗色就必然偏淡。）
#version 100
precision highp float;

varying highp vec2 uv;
varying lowp vec4 color;

// macroquad 会把「当前正在画的贴图」绑到 `Texture` 这个名字上（保留字）。
uniform sampler2D Texture;

uniform vec2 tex_size;

uniform vec4 base_color;

uniform vec4 face_color;
uniform float face_brightness;

uniform vec4 grid_color;
uniform float grid_brightness;

uniform vec4 glow_color;
uniform float glow_brightness;

uniform float always_on;
uniform float glow_ooroff;

uniform float t;
uniform float aspect;
uniform float alpha_scale;

void main() {
    // ---------- 照搬 kaichang/index.html 原版：**不做宽高比校正** ----------
    // 原版 canvas 的 uv 在 x / y 两个方向都是 0..1 铺满整块画布（`uv = a_pos*0.5+0.5`），
    // 我们的 `uv` 也一样（trigrid.rs 里显式用 `ScaleType::Fit` 画满屏矩形）。所以
    // 这里直接拿 uv 采样 tile，格子像素尺寸 = `tex_size × 视口尺寸`，与原版逐像素一致。
    // ⚠️ 之前默认的 `ScaleType::CropCenter` 会按视口宽高比把 uv 裁一刀，格子会被拉伸，
    // 那才是"背景比例不对"的根因 —— 不要退回去。
    vec2 uv2 = uv;

    vec2 w = uv2 - 0.5;
    uv2.x += pow(abs(w.y), 2.0) * (-w.x); // 原版核心：中间的横向扭曲
    uv2.x += tex_size.x / 2.0;            // 水平方向居中

    vec2 new_uv = mod(uv2 / tex_size, 1.0);
    vec4 texColor = texture2D(Texture, new_uv);

    // 扫描波：只有 sin 超过 k 的那条窄带会被点亮，随时间上下扫
    float k = 0.8;
    float k2 = 1.6;
    float p = max(0.0, min(1.0, (sin((uv2.y + t) * k2) - k) * (1.0 / (1.0 - k))));

    if (always_on > 0.5) {
        p = 1.0;
    }

    float k3 = 2.0;
    float k4 = 5.0;
    float k5 = 1.0;

    float grid_w = 1.0;
    float face_w = p * k3;
    float glow_w = p * k4;

    if (glow_ooroff > 0.5) {
        grid_w *= p;
    }

    vec4 finalColor =
        base_color +
        (
            grid_color * grid_brightness * texColor.r * grid_w +
            face_color * face_brightness * texColor.b * face_w +
            glow_color * pow(glow_brightness * texColor.g * glow_w, k5)
        );

    // 原版：alpha 由「最终颜色的平均亮度」推出来，所以暗的地方几乎是透明的，
    // 底图（beijing）能从下面透上来 —— 光波是叠在底图之上的一层，不是盖住它。
    finalColor.a = texColor.a * (finalColor.r + finalColor.g + finalColor.b) / 3.0 * 0.4 * alpha_scale;

    // 乘上顶点色（`Ui` 传白，`ui.alpha` 揉在里面）。
    gl_FragColor = finalColor * color;
}
