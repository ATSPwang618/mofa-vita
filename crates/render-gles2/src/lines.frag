varying vec2 v_point;
uniform sampler2D u_source, u_lookup;
uniform vec2 u_source_origin, u_source_size;
uniform float u_kind;
uniform vec2 u_lookup_window;
vec4 record(float line,float column) {
    return floor(texture2D(u_lookup,vec2((column+0.5)/10.0,(line+u_lookup_window.x+0.5)/u_lookup_window.y))*255.0+0.5);
}
bool less_unsigned(vec4 a,vec4 b) {
    float ah=a.z+a.w*256.0,bh=b.z+b.w*256.0;
    return ah<bh || (ah==bh && a.x+a.y*256.0<b.x+b.y*256.0);
}
float divide_signed(vec4 n,vec4 d) {
    bool negative=n.w>=128.0;
    if(negative) n=word_negate(n);
    float q=floor(word_unsigned_value(n)/word_unsigned_value(d));
    vec4 product=word_multiply(d,word_unsigned(q));
    if(less_unsigned(n,product)) q-=1.0;
    else {
        vec4 next=word_add(product,d);
        if(!less_unsigned(next,product) && !less_unsigned(n,next)) q+=1.0;
    }
    return negative?-q:q;
}
// Unsigned channel subtraction followed by packing without masks spills one
// bit from blue into red, and green into alpha. Preserve the original result
// using exact signed 24-bit products instead of a generic alpha blend.
vec4 mix_pixel(vec4 old,vec4 ink,float weight) {
    vec4 value=ink+floor((old-ink)*weight/65536.0);
    if(weight>0.0) {
        if(old.b<ink.b) value.r+=1.0-mod(value.r,2.0);
        if(old.g<ink.g) value.a+=1.0-mod(value.a,2.0);
    }
    return value;
}
void main() {
    vec2 point=floor(v_point);
    vec4 color=floor(texture2D(u_source,(point-u_source_origin+0.5)/u_source_size)*255.0+0.5);
    for(int entry=0;entry<16;entry++) {
        if(float(entry)>=u_kind) break;
        float row=float(entry);
        float flags=record(row,5.0).x;
        bool aa=mod(flags,2.0)!=0.0,horizontal=mod(floor(flags/2.0),2.0)!=0.0;
        float start=word_signed_value(record(row,0.0));
        vec4 minor_start=record(row,1.0);
        float distance=word_unsigned_value(record(row,2.0));
        float index=((horizontal?point.x:point.y)-start)*(flags>=4.0?1.0:-1.0);
        if(index<0.0 || index>distance) continue;
        float pixel=horizontal?point.y:point.x;
        vec4 bytes=record(row,4.0);
        vec4 ink=bytes.zyxw;
        if(!aa) {
            if(distance==0.0) continue;
            bool from_start=index<=floor((distance+1.0)/2.0);
            float step=from_start?index:distance-index;
            vec4 minor=record(row,3.0);
            bool negative=minor.w>=128.0;
            vec4 magnitude=negative?word_negate(minor):minor;
            vec4 numerator=word_add(word_multiply(word_unsigned(step*2.0),magnitude),word_unsigned(distance));
            float shift=divide_signed(numerator,record(row,9.0));
            float direction=negative?-1.0:1.0;
            float y=word_signed_value(minor_start)+(from_start?shift*direction:word_signed_value(minor)-shift*direction);
            if(pixel==y) color=ink;
            continue;
        }
        vec4 position_word=word_add(word_multiply(minor_start,vec4(0.0,0.0,1.0,0.0)),word_multiply(word_unsigned(index),record(row,8.0)));
        float low=word_high_signed(position_word),fraction=position_word.x+position_word.y*256.0;
        float weight;
        if(pixel==low) weight=fraction;
        else if(pixel==low+1.0) weight=65535.0-fraction;
        else continue;
        float count=word_unsigned_value(record(row,6.0));
        if(count>0.0) ink.a=word_multiply(record(row,7.0),word_unsigned(min(index+1.0,count))).w;
        color=mix_pixel(color,ink,weight);
    }
    gl_FragColor=color/255.0;
}
