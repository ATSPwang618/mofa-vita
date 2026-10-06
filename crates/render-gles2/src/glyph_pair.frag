// Composite a shadow mask and its glyph against one frozen backdrop. This is
// the common full-opacity alpha text path; both legacy blend steps retain
// their byte rounding, while the work surface needs only one draw and store.
varying vec2 v_point;
uniform sampler2D u_source, u_source2, u_backdrop, u_lookup;
uniform vec2 u_source_size, u_source_size2;
uniform vec2 u_backdrop_origin, u_backdrop_size;
uniform vec3 u_map_x, u_map_y, u_map_x2, u_map_y2;
uniform vec4 u_color, u_color2, u_area0, u_area1;

void main() {
    vec2 point = floor(v_point);
    vec3 p = vec3(point, 1.0);
    vec2 q0 = vec2(dot(u_map_x, p), dot(u_map_y, p));
    vec2 q1 = vec2(dot(u_map_x2, p), dot(u_map_y2, p));
    vec2 at0 = floor(q0 + 0.5);
    vec2 at1 = floor(q1 + 0.5);
    // Fetch before testing coverage so the SGX compiler sees uniform control
    // flow for both atlas textures and the blend lookup table.
    float coverage0 = floor(texture2D(u_source, (q0 + 0.5) / u_source_size).r * 255.0 + 0.5);
    float coverage1 = floor(texture2D(u_source2, (q1 + 0.5) / u_source_size2).r * 255.0 + 0.5);
    vec4 d = floor(texture2D(u_backdrop, (point - u_backdrop_origin + 0.5) / u_backdrop_size) * 255.0 + 0.5);
    float divisor = u_color.a == 65.0 ? 64.0 : 256.0;
    float row = u_color.a == 65.0 ? 256.0 : 0.0;

    float ratio0 = floor(texture2D(u_lookup, (vec2(d.a, row + coverage0) + 0.5) / vec2(256.0, 321.0)).a * 255.0 + 0.5);
    float alpha0 = min(coverage0 * (256.0 / divisor), 255.0);
    vec4 first = vec4(
        d.rgb + floor((u_color.rgb - d.rgb) * ratio0 / 256.0),
        255.0 - floor((255.0 - d.a) * (255.0 - alpha0) / 255.0)
    );
    bool valid0 = all(greaterThanEqual(point, u_area0.xy)) && all(lessThan(point, u_area0.zw))
        && all(greaterThanEqual(at0, vec2(0.0))) && all(lessThan(at0, u_source_size));
    d = mix(d, first, float(valid0));

    float ratio1 = floor(texture2D(u_lookup, (vec2(d.a, row + coverage1) + 0.5) / vec2(256.0, 321.0)).a * 255.0 + 0.5);
    float alpha1 = min(coverage1 * (256.0 / divisor), 255.0);
    vec4 second = vec4(
        d.rgb + floor((u_color2.rgb - d.rgb) * ratio1 / 256.0),
        255.0 - floor((255.0 - d.a) * (255.0 - alpha1) / 255.0)
    );
    bool valid1 = all(greaterThanEqual(point, u_area1.xy)) && all(lessThan(point, u_area1.zw))
        && all(greaterThanEqual(at1, vec2(0.0))) && all(lessThan(at1, u_source_size2));
    d = mix(d, second, float(valid1));
    gl_FragColor = d / 255.0;
}
