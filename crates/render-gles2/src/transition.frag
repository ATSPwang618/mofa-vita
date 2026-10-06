varying vec2 v_point;
uniform sampler2D u_source,u_backdrop,u_lookup,u_rule,u_curve;
uniform vec4 u_frame; // kind, phase, reserved, face
uniform vec4 u_patch;
uniform float u_direct;
uniform vec2 u_canvas,u_extent,u_source_size,u_backdrop_size,u_rule_size;
uniform vec4 u_source_bounds,u_backdrop_bounds;
vec4 bytes(vec4 color) { return floor(color*255.0+0.5); }
void main() {
    vec2 uv=(floor(v_point)-u_patch.xy+0.5)/u_patch.zw;
    vec2 source_uv=uv,backdrop_uv=uv,rule_uv=uv;
#if TRANSITION_DIRECT
        vec2 center=floor((floor(v_point)+0.5)*u_canvas)+0.5;
        source_uv=(floor(center*u_source_size/u_extent)+0.5-u_source_bounds.xy)/u_source_bounds.zw;
        backdrop_uv=(floor(center*u_backdrop_size/u_extent)+0.5-u_backdrop_bounds.xy)/u_backdrop_bounds.zw;
        rule_uv=(floor(center*u_rule_size/u_extent)+0.5)/u_rule_size;
#endif
    // All texture fetches are outside data-dependent branches. PVR's ES2
    // compiler does not need gradients through early returns or integer loops.
    vec4 a=bytes(texture2D(u_source,source_uv));
    vec4 b=bytes(texture2D(u_backdrop,backdrop_uv));
#if TRANSITION_RULE
    float level=bytes(texture2D(u_rule,rule_uv)).r;
    vec4 curve=bytes(texture2D(u_curve,vec2((level+0.5)/256.0,0.5)));
    float opacity=curve.r;
#else
    float opacity=u_frame.y;
#endif
    float factor=opacity,alpha=0.0;
#if TRANSITION_FACE == 0
    float weight=opacity;
#if !TRANSITION_RULE
    weight+=step(128.0,opacity);
#endif
    vec2 address=floor(vec2(a.a*(256.0-weight),b.a*weight)/256.0);
    float adjusted=bytes(texture2D(u_lookup,(address+0.5)/vec2(256.0,321.0))).a;
    factor=adjusted;
    alpha=a.a+floor((b.a-a.a)*weight/256.0);
#elif TRANSITION_FACE == 4
    alpha=a.a+floor((b.a-a.a)*opacity/256.0);
#endif
    vec4 result=vec4(a.rgb+floor((b.rgb-a.rgb)*factor/256.0),alpha);
#if TRANSITION_RULE
    if(curve.g==1.0) result=a;
    if(curve.g==2.0) result=b;
#endif
    gl_FragColor=result/255.0;
}
