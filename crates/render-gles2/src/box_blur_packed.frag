// Two RGBA8 surfaces retain all four 16-bit horizontal sums. Average once,
// after the vertical sum, so this path has the same rounding as the wide kernel.
varying vec2 v_point;
uniform sampler2D u_source, u_backdrop;
uniform vec2 u_source_origin, u_source_size, u_canvas, u_output_scale, u_source_scale;
uniform vec4 u_operation;
uniform float u_kind;
void main() {
#if PACKED_STAGE == 0
    vec2 at=floor(v_point);
    vec4 sum=vec4(0.0);
    for(int k=0;k<129;k++) {
        if(float(k)>u_operation.x*2.0) break;
        vec2 p=at+vec2(float(k)-u_operation.x,0.0);
        vec2 stored=floor((p+0.5)*u_source_scale);
        vec4 value=floor(texture2D(u_source,(stored-u_source_origin+0.5)/u_source_size)*255.0+0.5);
        if(u_operation.z!=0.0) value.rgb=floor(value.rgb*(value.a+floor(value.a/128.0))/256.0);
        float inside=step(0.0,p.x)*(1.0-step(u_canvas.x,p.x));
        sum+=value*inside;
    }
    vec4 high=floor(sum/256.0);
    gl_FragColor=(u_kind==0.0 ? sum-high*256.0 : high)/255.0;
#else
    vec2 at=floor((floor(v_point)+0.5)*u_output_scale);
    vec4 sum=vec4(0.0);
    for(int k=0;k<129;k++) {
        if(float(k)>u_operation.y*2.0) break;
        vec2 p=at+vec2(0.0,float(k)-u_operation.y);
        vec2 uv=(p-u_source_origin+0.5)/u_source_size;
        vec4 low=floor(texture2D(u_source,uv)*255.0+0.5);
        vec4 high=floor(texture2D(u_backdrop,uv)*255.0+0.5);
        float inside=step(0.0,p.y)*(1.0-step(u_canvas.y,p.y));
        sum+=(low+high*256.0)*inside;
    }
    vec2 extent=min(at+u_operation.xy+1.0,u_canvas)-max(at-u_operation.xy,vec2(0.0));
    float count=extent.x*extent.y;
    vec4 n=sum+floor(count/2.0), value;
    if(u_operation.w!=0.0) value=floor(n*floor(65536.0/count)/65536.0);
    else {
        value=floor(n/count);
        // All products are exact below 2^24; correct a rounded float quotient.
        value-=vec4(greaterThan(value*count,n));
        value+=vec4(lessThanEqual((value+1.0)*count,n));
    }
    if(u_operation.z!=0.0) {
        vec3 n=value.rgb*255.0;
        float d=max(1.0,value.a);
        vec3 q=floor(n/d);
        q-=vec3(greaterThan(q*d,n));
        q+=vec3(lessThanEqual((q+1.0)*d,n));
        value.rgb=value.a==0.0?vec3(0.0):min(q,vec3(255.0));
    }
    gl_FragColor=value/255.0;
#endif
}
