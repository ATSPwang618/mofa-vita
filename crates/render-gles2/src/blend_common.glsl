// Byte-domain implementations match the shared WGPU legacy blend rules. GLSL
// ES 1.00 has no bitwise operators; floor divisions preserve signed shifts.
uniform sampler2D u_lookup;
vec4 bytes(vec4 value) { return floor(value * 255.0 + 0.5); }
vec3 lerp256(vec3 d, vec3 s, float a) { return d + floor((s-d)*a/256.0); }
vec3 mul256(vec3 v, float a) { return floor(v*a/256.0); }
float over_alpha(float da, float sa) {
    float a=da+sa-floor(da*sa/256.0);
    return a-floor(a/256.0);
}
vec4 premul_over(vec4 d, vec4 s) {
    return vec4(min(vec3(255.0),s.rgb+mul256(d.rgb,255.0-s.a)),over_alpha(d.a,s.a));
}
vec4 straight_over(vec4 d, vec4 s, bool color_fill) {
    float ratio=bytes(texture2D(u_lookup,(vec2(d.a,s.a)+0.5)/vec2(256.0,321.0))).a;
    float divisor=color_fill?256.0:255.0;
    float a=255.0-floor((255.0-d.a)*(255.0-s.a)/divisor);
    return vec4(lerp256(d.rgb,s.rgb,ratio),a);
}
