varying vec2 v_point;
uniform vec4 u_frame;
bool valid_source(vec2 q,bool next) {
    return all(greaterThanEqual(q,vec2(0.0))) && all(lessThan(q+(next?1.0:0.0),u_extent.xy));
}
vec4 raw_pixel(vec2 q) { return floor(source_pixel(stored_pixel(q))*255.0+0.5); }
void select_source(vec2 q) { if(!tile_contains(stored_pixel(q),u_source_rect)) discard; }
vec4 weighted_pixels(vec2 q,vec4 w) {
    return raw_pixel(q)*w.x+raw_pixel(q+vec2(1.0,0.0))*w.y+
        raw_pixel(q+vec2(0.0,1.0))*w.z+raw_pixel(q+vec2(1.0))*w.w;
}
#if WARP_KIND == 0
uniform vec2 u_canvas,u_table_size;
uniform sampler2D u_mask;
vec4 lens_word(float index) {
    return floor(texture2D(u_mask,(vec2(mod(index,u_table_size.x),floor(index/u_table_size.x))+0.5)/u_table_size)*255.0+0.5);
}
void main() {
    vec2 at=floor(v_point),offset=at-u_canvas;
    float distance=length(offset),radius=u_frame.y;
    if(distance>=radius) {
        if(u_frame.w!=0.0) discard;
        gl_FragColor=vec4(0.0,0.0,0.0,1.0);return;
    }
    float index=min(floor(distance/radius*8191.0),8191.0);
    vec4 radial=word_from_signed(word_unsigned_value(lens_word(index))*radius);
    if(all(equal(radial,vec4(0.0)))) {
        if(!valid_source(at,false)) discard;
        select_source(at);gl_FragColor=raw_pixel(at)/255.0;return;
    }
    vec4 scale=word_from_signed(word_signed_value(radial)/distance);
    for(int p=0;p<45;p++) {
        if(float(p)>=u_frame.z) break;
        vec4 half_scale=word_asr_one(scale);
        vec4 next=word_from_signed(word_asr_fourteen(word_multiply(half_scale,half_scale)));
        if(all(equal(scale,next))) break;
        scale=next;
    }
    vec4 x=word_multiply(word_from_signed(offset.x),scale);
    vec4 y=word_multiply(word_from_signed(offset.y),scale);
    vec2 q=vec2(word_high_signed(x),word_high_signed(y))+u_canvas;
    if(!valid_source(q,true)) discard;
    select_source(q);
    vec2 f=vec2(offset.x<0.0?255.0-x.y:x.y,offset.y<0.0?255.0-y.y:y.y);
    float both=floor(f.x*f.y/256.0),only_x=f.x-both,only_y=f.y-both;
    vec4 weights=vec4(256.0-both-only_x-only_y,only_x,only_y,both);
    if(offset.x<0.0) weights=weights.yxwz;
    if(offset.y<0.0) weights=weights.zwxy;
    gl_FragColor=floor(weighted_pixels(q,weights)/256.0)/255.0;
}
#elif WARP_KIND == 1
uniform vec2 u_canvas;
uniform vec4 u_color,u_data0;
void main() {
    vec2 at=floor(v_point);
    if(any(greaterThanEqual(at,u_extent.xy-1.0))) discard;
    vec2 offset=at-u_canvas;
    vec4 x=word_from_signed(offset.x),y=word_from_signed(offset.y);
    vec4 dx=word_multiply(x,x),dy=word_multiply(y,y);
    vec4 distance=word_add(dx,word_from_signed(word_signed_value(dy)*u_data0.x/u_data0.y));
    if(all(equal(u_color,vec4(0.0))) || !word_signed_less(distance,u_color)) discard;
    float strength=1.0-word_signed_value(distance)/word_signed_value(u_color);
    float theta=strength*strength*strength*u_frame.y,c=cos(theta),s=sin(theta);
    vec2 rotated=vec2(offset.x*c+offset.y*s,offset.y*c-offset.x*s)+u_canvas;
    x=word_from_signed(rotated.x*32767.0);y=word_from_signed(rotated.y*32767.0);
    vec2 q=vec2(word_asr_fifteen(x),word_asr_fifteen(y));
    if(!valid_source(q,true)) discard;
    select_source(q);
    vec2 f=vec2(x.x+mod(x.y,128.0)*256.0,y.x+mod(y.y,128.0)*256.0);
    vec2 inv=32767.0-f;
    vec4 weights=vec4(
        word_shr_twenty_two(word_multiply(word_unsigned(inv.x),word_unsigned(inv.y))),
        word_shr_twenty_two(word_multiply(word_unsigned(f.x),word_unsigned(inv.y))),
        word_shr_twenty_two(word_multiply(word_unsigned(inv.x),word_unsigned(f.y))),
        word_shr_twenty_two(word_multiply(word_unsigned(f.x),word_unsigned(f.y))));
    gl_FragColor=floor(weighted_pixels(q,weights)/256.0)/255.0;
}
#else
uniform vec4 u_target,u_data0,u_data1,u_data2,u_data3,u_color;
uniform sampler2D u_previous;
uniform vec2 u_previous_size,u_backdrop_origin;
float upper_weight(vec4 a) { return a.z+a.w*256.0; }
float side_weight(vec4 row,vec4 product) {
    vec4 value=word_subtract(row,vec4(product.yzw,0.0));
    return value.y+value.z*256.0;
}
void main() {
    vec2 at=floor(v_point),local=at-u_target.xy;
    vec4 x=word_add(u_data0,word_multiply(word_from_signed(local.x),u_data2));
    vec4 y=word_add(u_data1,word_multiply(word_from_signed(local.y),u_data3));
    vec2 q=vec2(word_asr_eight(x),word_asr_eight(y));
    bool linear=u_frame.z!=0.0;
    if(!valid_source(q,linear)) discard;
    select_source(q);
    float alpha_low=u_color.x+u_color.y*256.0;
    vec4 old=vec4(0.0);
    if(u_frame.y!=0.0) old=floor(texture2D(u_previous,(at-u_backdrop_origin+0.5)/u_previous_size)*255.0+0.5);
    vec4 sum;
    if(linear) {
        vec4 upper=word_multiply(word_unsigned(255.0-y.x),u_color);
        vec4 lower=word_multiply(word_unsigned(y.x),u_color);
        vec4 a=word_multiply(word_unsigned(255.0-x.x),upper);
        vec4 b=word_multiply(word_unsigned(255.0-x.x),lower);
        vec4 weights=vec4(upper_weight(a),side_weight(upper,a),upper_weight(b),side_weight(lower,b));
        // Only bits 8..15 of the final wrapped sum are observed. Reduce each
        // exact 24-bit product first so addition cannot erode these bits.
        sum=mod(raw_pixel(q)*weights.x,65536.0)+mod(raw_pixel(q+vec2(1.0,0.0))*weights.y,65536.0)
            +mod(raw_pixel(q+vec2(0.0,1.0))*weights.z,65536.0)+mod(raw_pixel(q+vec2(1.0))*weights.w,65536.0);
    } else {
        sum=raw_pixel(q);
        if(u_frame.y==0.0) {gl_FragColor=sum/255.0;return;}
        sum=mod(sum*alpha_low,65536.0);
    }
    if(u_frame.y!=0.0) sum+=mod(old*mod(255.0-alpha_low,65536.0),65536.0);
    gl_FragColor=floor(mod(sum,65536.0)/256.0)/255.0;
}
#endif
