// Each executable contains one blend mode and one destination face. In
// particular, lookup reads never sit behind a dynamic mode/face dispatch.
#if BLEND_MODE == 22
// PS Dodge 5 indexes the dodge table with an opacity-scaled source byte.
// Evaluate the integer table directly to avoid three dependent lookup reads
// after source sampling.
// The bias is smaller than the smallest nonzero fractional remainder (1/255),
// but protects exact integer quotients from floating-point reciprocal rounding.
vec3 table_rgb(vec3 d,vec3 s) {
    vec3 divisor=255.0-s;
    vec3 quotient=floor(d*255.0/max(divisor,vec3(1.0))+1.0/1024.0);
    return mix(quotient,vec3(255.0),vec3(lessThanEqual(divisor,d)));
}
#elif BLEND_MODE == 20 || BLEND_MODE == 21 || BLEND_MODE == 23
float table_channel(float d,float s) {
    vec4 v=bytes(texture2D(u_lookup,(vec2(d,s)+0.5)/vec2(256.0,321.0)));
#if BLEND_MODE == 20
    return v.r;
#elif BLEND_MODE == 23
    return v.b;
#else
    return v.g;
#endif
}
vec3 table_rgb(vec3 d,vec3 s) {
    return vec3(table_channel(d.r,s.r),table_channel(d.g,s.g),table_channel(d.b,s.b));
}
#endif
#if BLEND_MODE == 18 || BLEND_MODE == 19
vec3 overlay(vec3 d,vec3 s) {
    vec3 even=s-mod(s,2.0);
    vec3 product=floor(d*(even+1.0)/128.0);
    vec3 high=2.0*(even+d)-255.0-product;
    return mix(high,product,vec3(lessThan(d,vec3(128.0))));
}
#endif
vec4 apply_blend(vec4 d,vec4 s,float opacity,bool hold) {
#if SOLID_COLOR
    float a=max(opacity,0.0);
#if BLEND_FACE == 0
    vec4 result=straight_over(d,vec4(s.rgb,a),true);
    if(opacity<0.0) result=vec4(d.rgb,floor(d.a*(255.0+opacity)/256.0));
#elif BLEND_FACE == 1
    vec4 result=vec4(floor((d.rgb*(255.0-a)+s.rgb*a)/256.0),d.a);
#else
    vec4 result=premul_over(d,vec4(mul256(s.rgb,a),a));
#endif
    if(a==255.0) result=vec4(s.rgb,BLEND_FACE==1?d.a:255.0);
#else
    float a=opacity==255.0?s.a:floor(s.a*opacity/256.0);
#if BLEND_MODE == 1
#if BLEND_FACE == 0
    vec4 result=straight_over(d,vec4(s.rgb,opacity),false);
#elif BLEND_FACE == 4
    vec4 result=premul_over(d,vec4(s.rgb,opacity));
#else
    float alpha=d.a+floor((s.a-d.a)*opacity/256.0);
    vec4 result=vec4(lerp256(d.rgb,s.rgb,opacity),hold?d.a:alpha);
#endif
    if(opacity==255.0) result=vec4(s.rgb,BLEND_FACE==1?(hold?d.a:s.a):255.0);
#elif BLEND_MODE == 2
#if BLEND_FACE == 0
    vec4 result=straight_over(d,vec4(s.rgb,a),false);
#elif BLEND_FACE == 4
    vec4 result=premul_over(d,vec4(mul256(s.rgb,a),a));
#else
    float alpha=d.a+floor((s.a-d.a)*a/256.0);
    vec4 result=vec4(lerp256(d.rgb,s.rgb,a),hold?d.a:alpha);
    if(s.a==255.0 && opacity==255.0 && !hold) result=s;
#endif
#elif BLEND_MODE == 12
#if BLEND_FACE == 0
    vec4 result=d;
#else
    vec4 value=opacity==255.0?s:floor(s*opacity/256.0);
    vec4 result=premul_over(d,value);
#if BLEND_FACE == 1
    result.a=hold?d.a:value.a;
#endif
#endif
#elif BLEND_MODE == 16
    float weight=floor(s.a/2.0);
    weight=floor(weight*(opacity==255.0?256.0:opacity)/256.0);
    vec4 mixed=floor(d*s/256.0);
    vec4 result=d+floor((mixed-d)*weight/128.0);
    result.a=hold?d.a:result.a;
#else
    vec3 rgb=vec3(0.0);
    float alpha=0.0;
#if BLEND_MODE >= 13
#if BLEND_MODE == 17
    rgb=d.rgb+mul256(s.rgb-floor(d.rgb*s.rgb/256.0),a);
#elif BLEND_MODE == 22
    rgb=table_rgb(d.rgb,mul256(s.rgb,a));
#elif BLEND_MODE == 27
    rgb=abs(d.rgb-mul256(s.rgb,a));
    alpha=d.a;
#elif BLEND_MODE == 28
    rgb=d.rgb+mul256(s.rgb-floor(d.rgb*s.rgb/128.0),a);
#else
    vec3 mixed=s.rgb;
#if BLEND_MODE == 14
    mixed=min(d.rgb+s.rgb,vec3(255.0));
#elif BLEND_MODE == 15
    mixed=max(d.rgb+s.rgb-255.0,vec3(0.0));
#elif BLEND_MODE == 18
    mixed=overlay(d.rgb,s.rgb);
#elif BLEND_MODE == 19
    mixed=overlay(s.rgb,d.rgb);
#elif BLEND_MODE == 20 || BLEND_MODE == 21 || BLEND_MODE == 23
    mixed=table_rgb(d.rgb,s.rgb);
#elif BLEND_MODE == 24
    mixed=max(d.rgb,s.rgb);
#elif BLEND_MODE == 25
    mixed=min(d.rgb,s.rgb);
#elif BLEND_MODE == 26
    mixed=abs(d.rgb-s.rgb);
#endif
    rgb=lerp256(d.rgb,mixed,a);
#endif
#else
    vec3 value=s.rgb;
#if BLEND_MODE == 4 || BLEND_MODE == 5
    if(opacity!=255.0) value=255.0-mul256(255.0-value,opacity);
#elif BLEND_MODE == 3 || BLEND_MODE == 8 || BLEND_MODE == 11
    if(opacity!=255.0) value=mul256(value,opacity);
#endif
#if BLEND_MODE == 3
    rgb=min(d.rgb+value,vec3(255.0));
    alpha=opacity==255.0?min(d.a+s.a,255.0):d.a;
#elif BLEND_MODE == 4
    rgb=max(d.rgb+value-255.0,vec3(0.0));
    alpha=opacity==255.0?max(d.a+s.a-255.0,0.0):d.a;
#elif BLEND_MODE == 5
    rgb=floor(d.rgb*value/256.0);
#elif BLEND_MODE == 8
    rgb=min(floor(d.rgb*floor(65536.0/max(255.0-value,vec3(1.0)))/256.0),vec3(255.0));
#elif BLEND_MODE == 9 || BLEND_MODE == 10
#if BLEND_MODE == 9
    rgb=min(d.rgb,value);
    if(opacity==255.0) alpha=min(d.a,s.a);
#else
    rgb=max(d.rgb,value);
    if(opacity==255.0) alpha=max(d.a,s.a);
#endif
    if(opacity!=255.0) rgb=lerp256(d.rgb,rgb,opacity);
#elif BLEND_MODE == 11
    rgb=255.0-floor((255.0-d.rgb)*(255.0-value)/256.0);
    alpha=255.0;
#endif
#endif
#if BLEND_MODE == 22
    vec4 result=vec4(rgb,hold?d.a:alpha);
#else
    vec4 result=vec4(mod(rgb,256.0),hold?d.a:alpha);
#endif
#endif
#endif
    return opacity==0.0?d:result;
}
