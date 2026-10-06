#version 100
precision highp float;
attribute vec2 a_unit;
uniform vec4 u_target;
uniform vec4 u_rectangle;
uniform float u_flip;
varying vec2 v_point;
void main() {
    v_point = u_rectangle.xy + a_unit * u_rectangle.zw;
    vec2 clip = (v_point - u_target.xy) / u_target.zw * 2.0 - 1.0;
    gl_Position = vec4(clip.x, clip.y * u_flip, 0.0, 1.0);
}
