varying vec2 v_point;
uniform vec3 u_map_x, u_map_y, u_map_w;
void main() {
    vec3 at = vec3(v_point, 1.0);
    float w = dot(u_map_w, at);
    if (w == 0.0) discard;
    gl_FragColor = linear_source(vec2(dot(u_map_x, at), dot(u_map_y, at)) / w);
}
