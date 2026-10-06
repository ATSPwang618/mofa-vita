varying vec2 v_point;
uniform sampler2D u_source;
uniform vec2 u_source_origin, u_source_size, u_canvas, u_output_scale, u_source_scale;
uniform vec4 u_operation;
void main() {
    vec2 at=floor((floor(v_point)+0.5)*u_output_scale), radius=u_operation.xy;
    vec2 width=radius*2.0+1.0;
    vec4 sum=vec4(0.0);
    vec2 offset=vec2(0.0);
    for(int k=0;k<81;k++) {
        if(float(k)>=width.x*width.y) break;
        vec2 p=at-radius+offset;
        // Always fetch before applying edge coverage. Divergent texture fetches
        // at canvas borders produce undefined-gradient warnings on PVR.
        float inside=step(0.0,p.x)*step(0.0,p.y)*(1.0-step(u_canvas.x,p.x))*(1.0-step(u_canvas.y,p.y));
        vec2 stored=floor((p+0.5)*u_source_scale);
        vec4 value=floor(texture2D(u_source,(stored-u_source_origin+0.5)/u_source_size)*255.0+0.5);
        if(u_operation.z!=0.0) value.rgb=floor(value.rgb*(value.a+floor(value.a/128.0))/256.0);
        sum+=value*inside;
        offset.x+=1.0;
        if(offset.x>=width.x) { offset.x=0.0; offset.y+=1.0; }
    }
    vec2 extent=min(at+radius+1.0,u_canvas)-max(at-radius,vec2(0.0));
    float count=extent.x*extent.y;
    // Keep the reference's truncated 16-bit reciprocal, including edge kernels.
    // The product is below 2^24, so every integer remains exactly representable.
    vec4 value=floor((sum+floor(count/2.0))*floor(65536.0/count)/65536.0);
    if(u_operation.z!=0.0) {
        vec3 n=value.rgb*255.0;
        float d=max(1.0,value.a);
        vec3 q=floor(n/d);
        q-=vec3(greaterThan(q*d,n));
        q+=vec3(lessThanEqual((q+1.0)*d,n));
        value.rgb=value.a==0.0?vec3(0.0):min(q,vec3(255.0));
    }
    gl_FragColor=value/255.0;
}
