// 背景模糊 pass 的顶点着色器（照搬 trigrid.vert 的属性布局 / 投影约定）。
#version 100
attribute vec3 position;
attribute vec2 texcoord;
attribute vec4 color0;

uniform mat4 Model;
uniform mat4 Projection;

varying highp vec2 uv;
varying lowp vec4 color;

void main() {
    gl_Position = Projection * Model * vec4(position, 1.0);
    uv = texcoord;
    color = color0 / 255.0;
}
