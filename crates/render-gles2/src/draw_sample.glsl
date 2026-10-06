// Sampling mode is a preprocessor constant: no dynamic texture-read dispatcher.
varying vec2 v_point;
uniform sampler2D u_source,u_backdrop;
uniform vec2 u_source_origin,u_source_size,u_source_scale;
uniform vec4 u_source_visible;
uniform vec2 u_backdrop_origin,u_backdrop_size;
uniform vec3 u_map_x,u_map_y;
uniform vec4 u_color,u_operation,u_sampling,u_region,u_sample_bounds;
uniform float u_kind;
vec4 sampled(vec2 point) {
    point=clamp(point,u_sample_bounds.xy,u_sample_bounds.zw-1.0);
#if SAMPLE_KIND == 6 || SAMPLE_KIND == 7
    point=floor((point+0.5)*u_source_scale);
#endif
    return bytes(texture2D(u_source,(point-u_source_origin+0.5)/u_source_size));
}
vec4 draw_sample(out bool valid) {
    valid=true;
#if SAMPLE_KIND == 0
    return u_color;
#else
    vec2 point=floor(v_point);
    vec2 q=vec2(dot(u_map_x,vec3(point,1.0)),dot(u_map_y,vec3(point,1.0)));
#if SAMPLE_KIND == 5
    // Coordinates and periods are integers. Center the quotient in its unit
    // interval so reciprocal rounding cannot wrap an exact multiple backwards.
    vec2 wrapped=point+vec2(u_map_x.y,u_map_y.x);
    wrapped-=floor((wrapped+0.5)/u_color.xy)*u_color.xy;
    q=(wrapped+vec2(u_map_x.z,u_map_y.z)+0.5)*vec2(u_map_x.x,u_map_y.y)-0.5;
#endif
#if SAMPLE_KIND == 8 || SAMPLE_KIND == 9
    valid=all(greaterThanEqual(q,u_region.xy-0.5)) && all(lessThan(q,u_region.zw-0.5));
    q=clamp(q,u_sample_bounds.xy,u_sample_bounds.zw-1.0);
#if BLEND_MODE == 2 || BLEND_MODE >= 13 || SAMPLE_KIND == 9
    // Straight-alpha pixels need alpha-weighted color interpolation. Filtering
    // their stored RGB directly leaks invisible colors into the visible edge.
    vec2 base=floor(q), weight=fract(q);
    vec4 a=sampled(base), b=sampled(base+vec2(1.0,0.0));
    vec4 c=sampled(base+vec2(0.0,1.0)), d=sampled(base+1.0);
#if SAMPLE_KIND == 9
    vec3 sharp=sharpen_upscale(a,b,c,d,weight);
    float opaque=step(254.5,min(min(a.a,b.a),min(c.a,d.a)));
#endif
#if BLEND_MODE == 2 || BLEND_MODE >= 13
    a.rgb*=a.a; b.rgb*=b.a; c.rgb*=c.a; d.rgb*=d.a;
#endif
    vec4 result=mix(mix(a,b,weight.x),mix(c,d,weight.x),weight.y);
#if BLEND_MODE == 2 || BLEND_MODE >= 13
    result.rgb/=max(result.a,0.000001);
#endif
#if SAMPLE_KIND == 9
    // Keep alpha interpolation and transparent-edge colors unchanged.
    result.rgb=mix(result.rgb,sharp,opaque);
#endif
    return floor(result+0.5);
#else
    return bytes(texture2D(u_source,(q-u_source_origin+0.5)/u_source_size));
#endif
#elif SAMPLE_KIND == 2
    valid=all(greaterThanEqual(q,u_region.xy-0.5)) && all(lessThan(q,u_region.zw-0.5));
    q=floor((floor(q+0.5)+0.5)*u_source_scale);
    // Cropped canvas edits share the fill raster's pixel-center coverage.
    // A translated strip must not sample the empty texel just past its edge.
    vec2 first=ceil(u_sample_bounds.xy*u_source_scale-0.5);
    vec2 last=max(first,ceil(u_sample_bounds.zw*u_source_scale-0.5)-1.0);
    q=clamp(q,first,last);
#elif SAMPLE_KIND == 3 || SAMPLE_KIND == 4 || SAMPLE_KIND == 6 || SAMPLE_KIND == 7
    bool inside=all(greaterThanEqual(q,u_region.xy-0.5)) && all(lessThan(q,u_region.zw-0.5));
#if !SAMPLE_CLEAR
    valid=inside;
#endif
#if SAMPLE_KIND == 4 || SAMPLE_KIND == 7
    vec2 base=floor(q);
    vec2 fraction=floor(fract(q)*256.0);
    vec2 ratio=fraction+floor(fraction/128.0);
    if(u_sampling.w!=0.0 && all(greaterThanEqual(base,u_sample_bounds.xy)) && all(lessThan(base+1.0,u_sample_bounds.zw))) ratio.x=fraction.x;
    vec4 a=sampled(base);
    vec4 b=sampled(base+vec2(1.0,0.0));
    vec4 c=sampled(base+vec2(0.0,1.0));
    vec4 d=sampled(base+1.0);
    vec4 top=a+floor((b-a)*ratio.x/256.0);
    vec4 bottom=c+floor((d-c)*ratio.x/256.0);
    vec4 result=top+floor((bottom-top)*ratio.y/256.0);
#else
    vec4 result=sampled(floor(q+0.5));
#endif
#if SAMPLE_CLEAR
    return inside?result:u_color;
#else
    return result;
#endif
#else
    q=floor(q+0.5);
#endif
#if SAMPLE_KIND != 3 && SAMPLE_KIND != 4 && SAMPLE_KIND != 6 && SAMPLE_KIND != 7 && SAMPLE_KIND != 8 && SAMPLE_KIND != 9
    valid=valid && all(greaterThanEqual(q,u_source_visible.xy)) && all(lessThan(q,u_source_visible.xy+u_source_visible.zw));
    q-=u_source_origin;
    valid=valid && all(greaterThanEqual(q,vec2(0.0))) && all(lessThan(q,u_source_size));
    return bytes(texture2D(u_source,(q+0.5)/u_source_size));
#endif
#endif
}
