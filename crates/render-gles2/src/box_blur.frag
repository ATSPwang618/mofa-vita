varying vec2 v_point;
uniform sampler2D u_source, u_backdrop, u_lookup;
uniform vec2 u_source_origin, u_source_size, u_backdrop_origin, u_backdrop_size, u_source_scale, u_canvas, u_table_size;
uniform float u_offset, u_kind;
uniform vec4 u_channel;
uniform vec4 u_source_visible;
#if BOX_STAGE == 0
uniform vec3 u_operation;
void main() {
    vec2 at=floor(v_point);
    vec4 sum=floor(texture2D(u_backdrop,(at-u_backdrop_origin+0.5)/u_backdrop_size)*255.0+0.5);
    float scalar=0.0;
    for(int k=0;k<64;k++) {
        if(float(k)>=u_kind) break;
        vec2 p=at+(u_operation.x!=0.0?vec2(u_offset+float(k),0.0):vec2(0.0,u_offset+float(k)));
        if(p.x<0.0 || p.y<0.0 || p.x>=u_canvas.x || p.y>=u_canvas.y) continue;
        vec2 physical=floor((p+0.5)*u_source_scale);
        if(any(lessThan(physical,u_source_visible.xy)) || any(greaterThanEqual(physical,u_source_visible.xy+u_source_visible.zw))) continue;
        vec2 local=physical-u_source_origin;
        if(local.x<0.0 || local.y<0.0 || local.x>=u_source_size.x || local.y>=u_source_size.y) continue;
        vec4 value=floor(texture2D(u_source,(local+0.5)/u_source_size)*255.0+0.5);
        if(u_operation.y!=0.0) {
            if(u_operation.z!=0.0) value.rgb=floor(value.rgb*(value.a+floor(value.a/128.0))/256.0);
            scalar+=dot(value,u_channel);
        } else sum=word_add(sum,value);
    }
    gl_FragColor=word_add(sum,word_unsigned(scalar))/255.0;
}
#else
uniform vec4 u_operation;
uniform vec2 u_output_scale;
bool less_unsigned(vec4 a,vec4 b) {
    float ah=a.z+a.w*256.0,bh=b.z+b.w*256.0;
    return ah<bh || (ah==bh && a.x+a.y*256.0<b.x+b.y*256.0);
}
float average(vec4 sum,float count) {
    vec4 n=word_add(sum,word_unsigned(floor(count/2.0)));
    if(u_operation.z!=0.0) {
        vec4 value=word_multiply(n,word_unsigned(floor(65536.0/count)));
        return value.z+value.w*256.0;
    }
    float q=floor(word_unsigned_value(n)/count);
    vec4 d=word_unsigned(count),product=word_multiply(d,word_unsigned(q));
    if(less_unsigned(n,product)) q-=1.0;
    else if(!less_unsigned(n,word_add(product,d))) q+=1.0;
    return q;
}
void main() {
    vec2 at=floor(v_point*u_output_scale),local=at-u_source_origin;
    vec4 sum=floor(texture2D(u_source,(local+0.5)/u_source_size)*255.0+0.5);
    vec2 count=min(at+u_operation.xy+1.0,u_canvas)-max(at-u_operation.xy,vec2(0.0));
    float value=average(sum,count.x*count.y);
    if(u_operation.w!=0.0) {
        float alpha=floor(texture2D(u_lookup,(local+0.5)/u_table_size).r*255.0+0.5);
        float n=value*255.0,d=max(1.0,alpha),q=floor(n/d);
        if(q*d>n) q-=1.0;
        else if((q+1.0)*d<=n) q+=1.0;
        value=alpha==0.0?0.0:min(q,255.0);
    }
    gl_FragColor=vec4(value/255.0);
}
#endif
