// 「光波」(TriGrid) 全屏 pass 的顶点着色器。
//
// 属性名 / 顺序必须和 macroquad（miniquad）的 Vertex 布局一致：
// position(3) / texcoord(2) / color0(4)。这里只把 texcoord 原样传给片元，
// uv 就是「所画矩形内的 0..1」。
#version 100
attribute vec3 position;
attribute vec2 texcoord;
attribute vec4 color0;

uniform mat4 Model;
uniform mat4 Projection;

// highp：lowp 的 10bit 定点精度会让平铺的 mod() 出现台阶/摩尔纹。
varying highp vec2 uv;
varying lowp vec4 color;

void main() {
    gl_Position = Projection * Model * vec4(position, 1.0);
    uv = texcoord;
    color = color0 / 255.0;
}
