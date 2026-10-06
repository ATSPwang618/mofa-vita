// CxImage / layerExImage point kernels. See krkr-plugins/src/image/LICENSE.txt.
varying vec2 v_point;
uniform sampler2D u_source,u_lookup;
uniform vec2 u_source_origin,u_source_size,u_table_size;
uniform vec2 u_stream_origin;
uniform float u_kind,u_offset;
uniform vec4 u_data0,u_data1,u_data2,u_data3;
vec4 old_pixel(vec2 at){return floor(texture2D(u_source,(at+0.5)/u_source_size)*255.0+0.5);}
vec4 lookup(float index){return floor(texture2D(u_lookup,vec2((index+0.5)/256.0,0.5))*255.0+0.5);}
#if FILTER_KIND == 0
void main(){
    vec4 color=old_pixel(floor(v_point)-u_source_origin);
    if(u_kind==0.0){
        color.rgb=vec3(word_unsigned_value(lookup(color.r)),word_unsigned_value(lookup(color.g)),word_unsigned_value(lookup(color.b)));
    }else{
        float lightness=floor((max(max(color.r,color.g),color.b)+min(min(color.r,color.g),color.b)+1.0)/2.0);
        vec3 tinted=lookup(lightness).zyx;
        vec3 numerator=tinted*u_offset+color.rgb*(256.0-u_offset);
        for(int c=0;c<3;c++) color[c]=numerator[c]<0.0?255.0:floor(numerator[c]/256.0);
    }
    gl_FragColor=color/255.0;
}
#elif FILTER_KIND == 1
uniform vec3 u_color;
float hue_rgb(float n1,float n2,float input_h){
    float h=input_h;
    if(h<0.0)h+=1.0;else if(h>1.0)h-=1.0;
    float c=n1;
    if(h<1.0/6.0)c=n1+(n2-n1)*h*6.0;
    else if(h<0.5)c=n2;
    else if(h<2.0/3.0)c=n1+(n2-n1)*(2.0/3.0-h)*6.0;
    return word_from_signed(c*255.0).x;
}
vec3 modulate(vec3 input_rgb){
    vec3 rgb=input_rgb/255.0;
    float hi=max(max(rgb.r,rgb.g),rgb.b),lo=min(min(rgb.r,rgb.g),rgb.b);
    float delta=hi-lo,add=hi+lo,l=add/2.0,s=0.0,h=0.0;
    if(delta!=0.0){
        s=delta/(l<0.5?add:2.0-add);
        if(rgb.r==hi)h=(rgb.g-rgb.b)/delta;
        else if(rgb.g==hi)h=2.0+(rgb.b-rgb.r)/delta;
        else h=4.0+(rgb.r-rgb.g)/delta;
        h/=6.0;
    }
    h+=u_color.x;
    if(h<0.0)h+=ceil(-h);else if(h>1.0)h-=ceil(h-1.0);
    s+=(u_color.y>0.0?1.0-s:s)*u_color.y;
    l+=(u_color.z>0.0?1.0-l:l)*u_color.z;
    if(s==0.0)return vec3(word_from_signed(l*255.0).x);
    float m2=l<=0.5?l*(1.0+s):l+s-l*s,m1=2.0*l-m2;
    return vec3(hue_rgb(m1,m2,h+1.0/3.0),hue_rgb(m1,m2,h),hue_rgb(m1,m2,h-1.0/3.0));
}
void main(){vec4 color=old_pixel(floor(v_point)-u_source_origin);color.rgb=modulate(color.rgb);gl_FragColor=color/255.0;}
#elif FILTER_KIND == 3
uniform vec4 u_color;
uniform vec2 u_operation,u_region;
vec4 byte_xor(vec4 a,vec4 b){
    vec4 result=vec4(0.0);float power=1.0;
    for(int bit=0;bit<8;bit++){
        result+=mod(mod(a,2.0)+mod(b,2.0),2.0)*power;
        a=floor(a/2.0);b=floor(b/2.0);power*=2.0;
    }
    return result;
}
void main(){
    vec4 color=old_pixel(floor(v_point)-u_source_origin);
    if(u_kind==0.0)color=byte_xor(color,u_color);
    else{
        vec2 parity=mod(u_operation-mod(floor(v_point)-u_region,2.0)+2.0,2.0);
        float index=parity.x+parity.y*2.0;
        float threshold=index==0.0?1.0:(index==1.0?4.0:(index==2.0?3.0:2.0));
        vec3 rounded=floor(color.rgb/4.0)*4.0+step(vec3(threshold),mod(color.rgb,4.0))*4.0;
        color.rgb=min(rounded,vec3(255.0));
    }
    gl_FragColor=color/255.0;
}
#else
vec4 stream_word(float index,float row){return floor(texture2D(u_lookup,vec2((index+0.5)/u_table_size.x,(row+0.5)/4.0))*255.0+0.5);}
vec4 stream_base(vec2 at){return word_add(word_multiply(stream_word(at.y,2.0),stream_word(at.x,0.0)),stream_word(at.x,1.0));}
vec4 advance_one(vec4 state){return word_add(word_multiply(state,u_data0),u_data1);}
#if FILTER_KIND == 2
void main(){
    vec2 at=floor(v_point)-u_stream_origin;
    vec4 color=old_pixel(floor(v_point)-u_source_origin),state=advance_one(stream_base(at));
    if(u_kind==0.0){color.rgb=vec3(floor(state.z/128.0)+mod(state.w,128.0)*2.0);}
    else{
        for(int channel=0;channel<3;channel++){
            float random=state.z+mod(state.w,128.0)*256.0;
            float value=(random/32767.0-0.5)*u_offset;
            float offset=sign(value)*floor(abs(value));
            color[2-channel]=clamp(color[2-channel]+offset,0.0,255.0);
            state=advance_one(state);
        }
    }
    gl_FragColor=color/255.0;
}
#else
uniform vec4 u_operation;
float product_high_byte(vec4 a,vec4 b){
    float c=floor(a.x*b.x/256.0);
    c=floor((a.x*b.y+a.y*b.x+c)/256.0);
    c=floor((a.x*b.z+a.y*b.y+a.z*b.x+c)/256.0);
    c=floor((a.x*b.w+a.y*b.z+a.z*b.y+a.w*b.x+c)/256.0);
    c=floor((a.y*b.w+a.z*b.z+a.w*b.y+c)/256.0);
    c=floor((a.z*b.w+a.w*b.z+c)/256.0);
    return floor((a.w*b.w+c)/256.0);
}
float noise_channel(vec2 bits){
    vec4 product=word_multiply(vec4(bits,0.0,0.0),u_data2);
    return mod(word_high_signed(product)+u_data3.x,256.0);
}
void main(){
    vec2 at=floor(v_point)-u_stream_origin;
    vec4 old=old_pixel(floor(v_point)-u_source_origin),control=stream_word(at.x,3.0);
    if(control.b!=0.0){gl_FragColor=old/255.0;return;}
    vec4 base=stream_base(at),state=advance_one(base);
    float alpha=u_operation.z!=0.0?old.a:255.0;
    vec3 rgb;
    if(u_operation.x!=0.0){
        float gray;
        if(u_operation.y!=0.0){
            gray=control.r==0.0?state.w:(control.r==1.0?state.z:state.y);
            if(control.g!=0.0)alpha=product_high_byte(base,u_data0);
        }else gray=noise_channel(control.r!=0.0?state.zw:state.xy);
        rgb=vec3(gray);
    }else if(u_operation.y!=0.0)rgb=state.wzy;
    else{
        vec4 next=advance_one(state);
        if(control.r==0.0)rgb=vec3(noise_channel(state.xy),noise_channel(state.zw),noise_channel(next.xy));
        else rgb=vec3(noise_channel(state.zw),noise_channel(next.xy),noise_channel(next.zw));
    }
    gl_FragColor=vec4(rgb,alpha)/255.0;
}
#endif
#endif
