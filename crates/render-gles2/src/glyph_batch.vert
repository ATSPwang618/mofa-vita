#version 100
precision highp float;
attribute vec2 a_unit;
attribute vec2 a_uv;
uniform vec4 u_target;
uniform float u_flip;
varying vec2 v_point;
varying vec2 v_atlas;
void main() {
    v_point = a_unit;
    v_atlas = a_uv;
    vec2 clip = (v_point - u_target.xy) / u_target.zw * 2.0 - 1.0;
    gl_Position = vec4(clip.x, clip.y * u_flip, 0.0, 1.0);
}
