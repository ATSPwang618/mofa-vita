varying vec2 v_point;
uniform vec4 u_target;
uniform sampler2D u_source, u_lookup;
uniform vec2 u_source_origin, u_source_size;
uniform float u_kind;
uniform vec4 u_data0, u_data1, u_data2, u_data3;

vec4 old_pixel(vec2 at) {
    return floor(texture2D(u_source,(at-u_source_origin+0.5)/u_source_size)*255.0+0.5);
}

// Division only needs a byte quotient. Correct a rounded float estimate with
// exact word products before quantizing, including large gradient bounds.
bool unsigned_less(vec4 a,vec4 b) {
    float ah=a.z+a.w*256.0,bh=b.z+b.w*256.0;
    return ah<bh || (ah==bh && a.x+a.y*256.0<b.x+b.y*256.0);
}
float byte_divide(vec4 a,vec4 b) {
    float q=floor(word_unsigned_value(a)/word_unsigned_value(b));
    if(q>=256.0) return 255.0;
    vec4 product=word_multiply(b,word_unsigned(q));
    if(unsigned_less(a,product)) q-=1.0;
    else {
        vec4 next=word_add(product,b);
        if(q<255.0 && !unsigned_less(next,product) && !unsigned_less(a,next)) q+=1.0;
    }
    return min(q,255.0);
}

#if ADJUST_KIND == 0
void main() {
    vec4 p=old_pixel(floor(v_point));
    float gray=floor(dot(p.rgb,vec3(54.0,183.0,19.0))/256.0);
    gl_FragColor=vec4(vec3(gray),p.a)/255.0;
}
#elif ADJUST_KIND == 3 || ADJUST_KIND == 4
vec4 gamma_word(float index,float channel) {
    return floor(texture2D(u_lookup,vec2((index+0.5)/256.0,(channel+0.5)/3.0))*255.0+0.5);
}
#if ADJUST_KIND == 4
float additive_gamma(float color,float channel,float alpha,float reciprocal,float adjusted) {
    float index=min(255.0,floor(reciprocal*color/256.0));
    index=max(index,step(alpha+0.5,color)*255.0);
    vec4 lookup=gamma_word(index,channel);
    // At alpha 255 the reciprocal index equals color. Multiplication by
    // 256 preserves the low 24 bits; a nonzero high byte saturates output.
    float opaque_overflow=step(254.5,alpha)*step(0.5,lookup.w)*255.0;
    // The alpha factor is at most 256. Carry each byte in exact float
    // integers, discard the low product byte and wrap at 32 bits.
    vec3 product=lookup.yzw*adjusted;
    product.x+=floor(lookup.x*adjusted/256.0);
    product.y+=floor(product.x/256.0);
    product.z+=floor(product.y/256.0);
    float mapped=dot(mod(product,256.0),vec3(1.0,256.0,65536.0))+max(0.0,color-alpha);
    return max(mapped,opaque_overflow);
}
#endif
void main() {
    vec4 p=old_pixel(floor(v_point));
    vec3 result;
#if ADJUST_KIND == 4
    float adjusted=p.a+floor(p.a/128.0);
    float reciprocal=min(65535.0,floor(65536.0/max(1.0,p.a)));
#endif
    for(int c=0;c<3;c++) {
        float color=p[c];
#if ADJUST_KIND == 3
        // Keep all lookups outside per-pixel branches, including alpha zero.
        float mapped=word_unsigned_value(gamma_word(color,float(c)));
        result[c]=mix(color,mapped,step(0.5,p.a));
#else
        result[c]=additive_gamma(color,float(c),p.a,reciprocal,adjusted);
#endif
    }
    gl_FragColor=vec4(result,p.a)/255.0;
}
#elif ADJUST_KIND == 1
uniform vec2 u_operation;
uniform vec4 u_region;
vec4 gradient(vec4 index) {
    vec4 x=unsigned_less(index,u_data2)?index:u_data2;
    // Ordinary game canvases fit exact 24-bit integer products. Keep the
    // byte-word path only for oversized script-supplied gradient bounds.
    if(u_data2.z==0.0 && u_data2.w==0.0) {
        float length=u_data2.x+u_data2.y*256.0;
        float position=x.x+x.y*256.0;
        return floor((u_data0*(length-position)+u_data1*position)/length);
    }
    vec4 remaining=word_subtract(u_data2,x);
    vec4 color;
    for(int c=0;c<4;c++) color[c]=byte_divide(word_add(
        word_multiply(word_unsigned(u_data0[c]),remaining),
        word_multiply(word_unsigned(u_data1[c]),x)),u_data2);
    return color;
}
void main() {
    vec2 at=floor(v_point);
    float delta=u_operation.x!=0.0?at.y-u_target.y:at.x-u_target.x;
    vec4 index=word_add(u_data3,word_unsigned(delta));
    vec4 color=gradient(index);
    if(u_operation.y!=0.0) {
        vec4 old=old_pixel(at);
        if(u_operation.x!=0.0) color=vec4(floor((old.rgb*(255.0-color.a)+color.rgb*color.a)/256.0),old.a);
        else {
            bool opaque=color.a==255.0;
            float first=ceil(u_region.x/4.0)*4.0;
            float end=floor((u_region.x+u_region.z)/4.0)*4.0;
            if(opaque && at.x>=first && at.x<end) {
                vec4 group=word_subtract(index,word_unsigned(mod(at.x,4.0)));
                opaque=gradient(group).a==255.0 && gradient(word_add(group,vec4(3.0,0.0,0.0,0.0))).a==255.0;
            }
            if(!opaque) color=old+floor((color-old)*color.a/256.0);
        }
    }
    gl_FragColor=color/255.0;
}
#else
uniform vec2 u_canvas;
uniform vec3 u_axis, u_color;
vec3 hsv_rgb(vec3 hsv) {
    float h=hsv.x==360.0?0.0:hsv.x;
    float s=hsv.y/100.0,v=hsv.z/100.0;
    if(s==0.0) return vec3(word_from_signed(v*255.0).x);
    float quotient=h/60.0,sector=floor(quotient),f=quotient-sector;
    float p=v*(1.0-s),q=v*(1.0-f*s),t=v*(1.0-(1.0-f)*s);
    vec4 n=word_from_signed(sector);
    bool negative=n.w>=128.0;
    if(negative) n=word_negate(n);
    float which=mod(n.x+4.0*(n.y+n.z+n.w),6.0)*(negative?-1.0:1.0);
    vec3 rgb=vec3(v,p,q);
    if(which==0.0) rgb=vec3(v,t,p);
    else if(which==1.0) rgb=vec3(q,v,p);
    else if(which==2.0) rgb=vec3(p,v,t);
    else if(which==3.0) rgb=vec3(p,q,v);
    else if(which==4.0) rgb=vec3(t,p,v);
    return vec3(word_from_signed(rgb.r*255.0).x,word_from_signed(rgb.g*255.0).x,word_from_signed(rgb.b*255.0).x);
}
void main() {
    vec2 at=floor(v_point);
    vec3 color=u_color;
    if(u_kind==0.0) {
        float x=byte_divide(word_add(u_data2,word_unsigned((at.x-u_target.x)*255.0)),u_data0);
        float y=byte_divide(word_subtract(u_data3,word_unsigned((at.y-u_target.y)*255.0)),u_data1);
        for(int c=0;c<3;c++) {
            if(u_axis[c]==1.0) color[c]=x;
            else if(u_axis[c]==2.0) color[c]=y;
        }
    } else {
        for(int c=0;c<3;c++) {
            float limit=c==0?360.0:100.0;
            if(u_axis[c]==1.0) color[c]=limit*at.x/max(1.0,u_canvas.x-1.0);
            else if(u_axis[c]==2.0) color[c]=limit*(u_canvas.y-1.0-at.y)/max(1.0,u_canvas.y-1.0);
        }
        color=hsv_rgb(color);
    }
    gl_FragColor=vec4(color,255.0)/255.0;
}
#endif
