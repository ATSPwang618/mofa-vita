#version 100
precision highp float;
attribute vec2 a_unit, a_uv;
uniform vec4 u_target;
uniform vec2 u_canvas;
varying vec2 v_uv;
void main() {
    vec2 point = (a_unit + 1.0) * 0.5 * u_canvas;
    gl_Position = vec4((point-u_target.xy)/u_target.zw*2.0-1.0, 0.0, 1.0);
    v_uv = a_uv;
}
