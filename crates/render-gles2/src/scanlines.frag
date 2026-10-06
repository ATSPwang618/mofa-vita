varying vec2 v_point;
uniform sampler2D u_source;
uniform sampler2D u_backdrop;
uniform sampler2D u_lookup;
uniform sampler2D u_rule;
uniform vec2 u_source_origin;
uniform vec2 u_source_size;
uniform vec2 u_backdrop_origin;
uniform vec2 u_backdrop_size;
uniform vec2 u_table_size;
uniform vec4 u_extent;
uniform vec4 u_region;
uniform vec4 u_frame;
vec4 table_bytes(float index) {
    vec2 at=vec2(mod(index,u_table_size.x),floor(index/u_table_size.x));
    return floor(texture2D(u_lookup,(at+0.5)/u_table_size)*255.0+0.5);
}
float signed_word(vec4 b) {
    // Signed limb avoids subtracting 2^32 from an already rounded float.
    return b.x+b.y*256.0+b.z*65536.0+(b.w>=128.0?b.w-256.0:b.w)*16777216.0;
}
float word(float index) { return signed_word(table_bytes(index)); }
void read_row(float row,out vec4 span,out vec4 filter) {
    if(u_frame.z>0.0) {
        float index=4.0*row;
        vec4 a=table_bytes(index), b=table_bytes(index+1.0), c=table_bytes(index+2.0);
        span=vec4(a.x+a.y*256.0,a.z+a.w*256.0,b.x+b.y*256.0,b.z+b.w*256.0);
        filter=vec4(c.x,c.y-2.0,word(index+3.0),c.z);
    } else {
        float index=8.0*row;
        span=vec4(word(index),word(index+1.0),word(index+2.0),word(index+3.0));
        filter=vec4(word(index+4.0),word(index+5.0),word(index+6.0),word(index+7.0));
    }
}
vec2 normalize_point(vec2 q) { float carry=floor(q.x/u_extent.x); return vec2(q.x-carry*u_extent.x,q.y+carry); }
bool inside(vec2 q,vec2 origin,vec2 size) { return all(greaterThanEqual(q,origin)) && all(lessThan(q,origin+size)); }
vec4 sample_bytes(sampler2D image,vec2 q,vec2 origin,vec2 size) { return floor(texture2D(image,(q-origin+0.5)/size)*255.0+0.5); }
void main() {
    vec2 point=floor(v_point);
    vec4 span, filter;
    read_row(point.y-u_frame.y,span,filter);
    float dx=point.x-span.x;
    if(dx<0.0 || dx>=span.y) discard;
    vec2 logical=normalize_point(vec2(span.z+dx,span.w));
    vec2 physical=floor((logical+0.5)*u_extent.zw);
    if(!inside(physical,u_source_origin,u_source_size)) discard;
    vec4 color=sample_bytes(u_source,physical,u_source_origin,u_source_size);
    float mode=u_frame.x;
    float fraction=filter.x;
    float first=filter.y;
    float last=filter.z;
    bool neighbour=mode==1.0 && fraction!=0.0;
    bool blend_old=false;
    bool odd=mode>=2.0 && fraction!=0.0;
    if(mode>=2.0 && dx!=first) {
        if(odd) {
            if(filter.w!=0.0 || dx==first+1.0 || dx>last) discard;
            if(dx==last) blend_old=true;
            else neighbour=true;
        } else {
            if(dx>=last) discard;
            blend_old=mode==3.0;
        }
    }
    vec4 adjacent=vec4(0.0);
    if(neighbour) {
        vec2 q=normalize_point(logical+vec2(mode==1.0?1.0:-1.0,0.0));
        if(mode==1.0 && q.y>=u_extent.y) q=u_extent.xy-1.0;
        if(inside(q,vec2(0.0),u_extent.xy)) {
            q=floor((q+0.5)*u_extent.zw);
            if(!inside(q,u_region.xy,u_region.zw)) discard;
            adjacent=sample_bytes(u_rule,q,u_region.xy,u_region.zw);
        } else if(u_frame.w!=0.0) discard;
    } else if(u_frame.w!=0.0) discard;
    if(mode==1.0 && neighbour) color=floor((color*(256.0-fraction)+adjacent*fraction)/256.0);
    if(mode>=2.0 && dx!=first) {
        vec4 old=sample_bytes(u_backdrop,point,u_backdrop_origin,u_backdrop_size);
        if(blend_old) color=floor((old+color)/2.0);
        else if(odd && mode==3.0) color=floor(old/2.0)+floor(adjacent/4.0)+floor(color/4.0);
        else if(odd) color=floor((adjacent+color)/2.0);
    }
    gl_FragColor=color/255.0;
}
