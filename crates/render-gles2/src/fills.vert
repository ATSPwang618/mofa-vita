#version 100
precision highp float;
attribute vec2 a_unit;
attribute vec4 a_uv;
uniform vec4 u_target;
varying highp vec4 v_color;
void main() {
    vec2 point = (a_unit - u_target.xy) / u_target.zw;
    gl_Position = vec4(point * 2.0 - 1.0, 0.0, 1.0);
    v_color = a_uv;
}
