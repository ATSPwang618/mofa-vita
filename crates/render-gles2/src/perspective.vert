#version 100
precision highp float;
attribute vec2 a_unit;
uniform vec4 u_target;
uniform vec4 u_data0, u_data1, u_data2, u_data3;
varying vec2 v_point;
void main() {
    v_point = mix(mix(u_data0.xy, u_data1.xy, a_unit.x),
                  mix(u_data2.xy, u_data3.xy, a_unit.x), a_unit.y);
    gl_Position = vec4((v_point - u_target.xy) / u_target.zw * 2.0 - 1.0, 0.0, 1.0);
}
