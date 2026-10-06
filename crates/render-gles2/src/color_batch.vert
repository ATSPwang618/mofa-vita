#version 100
precision highp float;
attribute vec2 a_unit;
attribute vec4 a_uv;
uniform vec4 u_target;
varying vec2 v_point;
varying vec4 v_color;
void main() {
    v_point = a_unit;
    v_color = a_uv;
    vec2 clip = (a_unit - u_target.xy) / u_target.zw * 2.0 - 1.0;
    gl_Position = vec4(clip, 0.0, 1.0);
}
