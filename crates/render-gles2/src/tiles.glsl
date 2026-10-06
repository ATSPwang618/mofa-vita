uniform sampler2D u_source, u_right, u_down, u_diagonal;
uniform vec4 u_source_rect, u_right_rect, u_down_rect, u_diagonal_rect;
uniform vec4 u_source_backing, u_right_backing, u_down_backing, u_diagonal_backing;
// xy logical dimensions, zw stored/logical ratios.
uniform vec4 u_extent;
bool tile_contains(vec2 p, vec4 r) {
    return all(greaterThanEqual(p, r.xy)) && all(lessThan(p, r.xy + r.zw));
}
vec2 stored_pixel(vec2 logical) {
    return floor((clamp(logical, vec2(0.0), u_extent.xy - 1.0) + 0.5) * u_extent.zw);
}
vec4 source_pixel(vec2 p) {
    if (tile_contains(p, u_source_rect))
        return texture2D(u_source, (p - u_source_backing.xy + 0.5) / u_source_backing.zw);
    if (tile_contains(p, u_right_rect))
        return texture2D(u_right, (p - u_right_backing.xy + 0.5) / u_right_backing.zw);
    if (tile_contains(p, u_down_rect))
        return texture2D(u_down, (p - u_down_backing.xy + 0.5) / u_down_backing.zw);
    return texture2D(u_diagonal, (p - u_diagonal_backing.xy + 0.5) / u_diagonal_backing.zw);
}
vec4 linear_source(vec2 point) {
    vec2 p = clamp(point - 0.5, vec2(0.0), u_extent.xy - 1.0);
    vec2 base = floor(p), f = p - base;
    vec2 at = stored_pixel(base);
    // Every output fragment belongs to exactly one first-tap tile. All four
    // taps are fetched in that draw, including corners shared by four tiles.
    if (!tile_contains(at, u_source_rect)) discard;
    vec4 a = source_pixel(at);
    vec4 b = source_pixel(stored_pixel(base + vec2(1.0, 0.0)));
    vec4 c = source_pixel(stored_pixel(base + vec2(0.0, 1.0)));
    vec4 d = source_pixel(stored_pixel(base + vec2(1.0)));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}
