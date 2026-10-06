// Fetch before per-pixel selection/discard. SGX's compiler cannot reliably
// lower nested early returns and texture fetches in divergent branches.
varying vec2 v_point;
uniform sampler2D u_source,u_backdrop,u_lookup,u_rule;
uniform vec2 u_source_origin,u_source_size,u_source_scale;
uniform vec2 u_backdrop_origin,u_backdrop_size,u_backdrop_scale;
uniform vec2 u_table_size,u_extent;
uniform vec2 u_canvas;
uniform vec4 u_data0,u_data1,u_data2,u_data3,u_color,u_frame;
vec4 bytes(vec4 value) { return floor(value*255.0+0.5); }
float v(int i) {
    if(i==0) return u_data0.x; if(i==1) return u_data0.y;
    if(i==3) return u_data0.w; if(i==4) return u_data1.x;
    if(i==5) return u_data1.y; if(i==6) return u_data1.z;
    if(i==7) return u_data1.w; if(i==8) return u_data2.x;
    return u_data2.y;
}
vec2 word(float index) {
    vec2 uv=(vec2(mod(index,u_table_size.x),floor(index/u_table_size.x))+0.5)/u_table_size;
    vec4 b=bytes(texture2D(u_rule,uv));
    float high=b.b+b.a*256.0; if(high>=32768.0) high-=65536.0;
    return vec2(high,b.r+b.g*256.0);
}
float table(float index) { vec2 parts=word(index); return parts.x*65536.0+parts.y; }
// Exact 16.16 scanline sampling, including negative steps and large x deltas.
float fixed_coordinate(float start,float step,float delta) {
    vec2 a=word(start),b=word(step);
    float low_product=mod(delta,256.0)*b.y;
    float high_product=floor(delta/256.0)*b.y;
    float low=a.y+mod(low_product,65536.0)+mod(high_product,256.0)*256.0;
    return a.x+delta*b.x+floor(low_product/65536.0)+floor(high_product/256.0)+floor(low/65536.0);
}
vec4 sample_source(float which,vec2 at,out bool valid) {
    vec2 q=clamp(at,vec2(0.0),u_extent-1.0);
    vec2 a=floor((q+0.5)*u_source_scale)-u_source_origin;
    vec2 b=floor((q+0.5)*u_backdrop_scale)-u_backdrop_origin;
    vec4 first=bytes(texture2D(u_source,(a+0.5)/u_source_size));
    vec4 second=bytes(texture2D(u_backdrop,(b+0.5)/u_backdrop_size));
    bool first_valid=all(greaterThanEqual(a,vec2(0.0))) && all(lessThan(a,u_source_size));
    bool second_valid=all(greaterThanEqual(b,vec2(0.0))) && all(lessThan(b,u_backdrop_size));
    valid=which==1.0?first_valid:second_valid;
    return which==1.0?first:second;
}
vec4 raw(vec4 a,vec4 b,float ratio) { return a+floor((b-a)*ratio/256.0); }
vec4 pair(vec2 at,bool alpha,out bool valid) {
    bool va,vb;
    vec4 a=sample_source(1.0,at,va),b=sample_source(2.0,at,vb);
    valid=va && vb;
    float ratio=v(1);
    float factor=ratio,opacity=0.0;
    float weight=ratio+(ratio>127.0?1.0:0.0);
    vec2 address=floor(vec2(a.a*(256.0-weight),b.a*weight)/256.0);
    float adjusted=bytes(texture2D(u_lookup,(address+0.5)/vec2(256.0,321.0))).a;
    if(u_frame.w==0.0) {
        factor=adjusted;
        opacity=a.a+floor((b.a-a.a)*weight/256.0);
    } else if(u_frame.w==4.0) opacity=a.a+floor((b.a-a.a)*ratio/256.0);
    vec4 blended=vec4(a.rgb+floor((b.rgb-a.rgb)*factor/256.0),opacity);
    return alpha?blended:raw(a,b,ratio);
}
float gloss(float phase) {
    if(phase<4.0) return 0.0;
    if(phase==4.0 || phase==12.0) return 16.0;
    if(phase==5.0 || phase==11.0) return 48.0;
    if(phase==6.0 || phase==10.0) return 80.0;
    if(phase==7.0 || phase==9.0) return 128.0;
    return 192.0;
}
vec4 effect(vec2 at,out bool valid) {
#if EXTRANS_MODE == 0
        float x=at.x-table(at.y);
        vec4 pixel=pair(vec2(x,at.y),true,valid);
        bool background=x<0.0 || x>=u_extent.x;
        valid=valid || background;
        return background?u_color:pixel;
#elif EXTRANS_MODE == 1
        float block=v(3); vec2 offset=vec2(v(4),v(5));
        vec2 divided=(at-offset)/block;
        vec2 q=sign(divided)*floor(abs(divided))*block+offset+floor(block/2.0);
        return pair(q,false,valid);
#elif EXTRANS_MODE == 2
        vec2 tile=floor(at/64.0),local=mod(at,64.0);
        float phase=clamp(v(3)-(tile.x-tile.y)*2.0,0.0,63.0);
        float base=(phase*64.0+local.y)*6.0;
        float start=table(base),count=table(base+1.0);
        float delta=local.x-start;
        vec2 q=tile*64.0+vec2(fixed_coordinate(base+2.0,base+4.0,delta),fixed_coordinate(base+3.0,base+5.0,delta));
        bool endpoint=phase==0.0 || phase==63.0;
        bool background=!endpoint && (local.x<start || local.x>=start+count || any(lessThan(q,vec2(0.0))) || any(greaterThanEqual(q,u_extent)));
        vec4 pixel=sample_source(phase>=32.0?2.0:1.0,endpoint?at:q,valid);
        if(phase>0.0 && phase<13.0) pixel=raw(pixel,vec4(255.0,255.0,255.0,0.0),gloss(phase));
        valid=valid || background;
        return background?u_color:pixel;
#elif EXTRANS_MODE == 3
        vec4 pixel=u_color;
        valid=true;
        for(int n=0;n<2;n++) {
            float which=n==1?v(3):3.0-v(3);
            float base=at.y*16.0+(which-1.0)*8.0;
            float start=table(base),end=table(base+1.0);
            float delta=at.x-start;
            vec2 q=vec2(fixed_coordinate(base+2.0,base+4.0,delta),fixed_coordinate(base+3.0,base+5.0,delta));
            bool sample_valid;
            vec4 sampled=sample_source(which,q,sample_valid);
            bool covered=at.x>=start && at.x<end;
            pixel=covered?sampled:pixel;
            valid=valid && (!covered || sample_valid);
        }
        return pixel;
#else
    vec2 center=vec2(v(3),v(4));
    vec2 folded=mix(at-center,center-at-1.0,vec2(lessThan(at,center)));
    float displacement=table(folded.y*v(6)+folded.x);
    float direction=mod(displacement,32.0),width=v(5),base=v(7);
    float wave=table(base+mod(floor(displacement/32.0)+v(8),width));
    float fd=floor(wave*(v(9)*256.0)/1024.0);
    float dx=floor(floor(table(base+width+direction)*fd/2048.0)/2048.0);
    float dy=floor(floor(table(base+width+32.0+direction)*fd/2048.0)/2048.0);
    vec2 q=abs(at+mix(-vec2(dx,dy),vec2(dx,dy),vec2(lessThan(at,center))));
    q=mix(q,u_extent*2.0-1.0-q,vec2(greaterThanEqual(q,u_extent)));
    return vec4(pair(q,false,valid).rgb,0.0);
#endif
}
void main() {
    bool valid;
    vec4 pixel=effect(floor((floor(v_point)+0.5)*u_canvas),valid);
    if(!valid) discard;
    gl_FragColor=pixel/255.0;
}
