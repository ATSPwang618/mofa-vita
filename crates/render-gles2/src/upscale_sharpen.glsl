// Reuse the bilinear footprint. Clamp to its color range to avoid ringing.
vec3 sharpen_upscale(vec4 a, vec4 b, vec4 c, vec4 d, vec2 weight) {
    vec3 center=mix(mix(a.rgb,b.rgb,weight.x),mix(c.rgb,d.rgb,weight.x),weight.y);
    vec3 low=min(min(a.rgb,b.rgb),min(c.rgb,d.rgb));
    vec3 high=max(max(a.rgb,b.rgb),max(c.rgb,d.rgb));
    vec3 range=high-low;
    vec3 amount=0.25*range/(range+16.0);
    return clamp(center+amount*(center-(a.rgb+b.rgb+c.rgb+d.rgb)*0.25),low,high);
}
