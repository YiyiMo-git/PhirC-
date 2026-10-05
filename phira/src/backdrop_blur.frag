// 可分离高斯模糊：水平 / 垂直两遍，sampler2D `Texture` 由 macroquad 在绘制时绑定
// 为「当前正在画的贴图」（与 trigrid 同约定）。`direction` 控制本遍沿 x 还是 y，
// `texel` 是一个纹素对应的 uv 步长（= 1 / 贴图尺寸）。
#version 100
precision highp float;

varying highp vec2 uv;
varying lowp vec4 color;

uniform sampler2D Texture;
uniform vec2 texel;
uniform vec2 direction;

void main() {
    // 9-tap 高斯（中心权重 + 4 对对称偏移），在半分辨率贴图上做，等效全分辨率更强。
    float w0 = 0.2270270270;
    float w1 = 0.1945945946;
    float w2 = 0.1216216216;
    float w3 = 0.0540540541;
    float w4 = 0.0162162162;

    float r = 2.5; // 偏移步长（纹素）

    vec2 o1 = direction * texel * (r * 1.0);
    vec2 o2 = direction * texel * (r * 2.0);
    vec2 o3 = direction * texel * (r * 3.0);
    vec2 o4 = direction * texel * (r * 4.0);

    vec4 sum = texture2D(Texture, uv) * w0;
    sum += (texture2D(Texture, uv + o1) + texture2D(Texture, uv - o1)) * w1;
    sum += (texture2D(Texture, uv + o2) + texture2D(Texture, uv - o2)) * w2;
    sum += (texture2D(Texture, uv + o3) + texture2D(Texture, uv - o3)) * w3;
    sum += (texture2D(Texture, uv + o4) + texture2D(Texture, uv - o4)) * w4;

    gl_FragColor = sum;
}
