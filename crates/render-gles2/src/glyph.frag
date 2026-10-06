// Glyphs use hardware-linear atlas sampling. Keep all texture fetches outside
// control flow: the Vita PVR compiler rejects the generic sampling dispatcher
// combined with glyph blending. This pass needs none of its image filters.
varying vec2 v_point;
#ifdef GLYPH_BATCH
varying vec2 v_atlas;
#endif
uniform sampler2D u_source;
uniform sampler2D u_backdrop;
uniform sampler2D u_lookup;
uniform vec2 u_source_origin;
uniform vec2 u_source_size;
uniform vec2 u_backdrop_origin;
uniform vec2 u_backdrop_size;
uniform vec3 u_map_x;
uniform vec3 u_map_y;
uniform vec4 u_color;
uniform vec4 u_operation;
void main() {
    vec2 point=floor(v_point);
    vec3 p=vec3(point.x,point.y,1.0);
#ifdef GLYPH_BATCH
    vec2 q=vec2(dot(vec3(u_map_x.xy,v_atlas.x),p),dot(vec3(u_map_y.xy,v_atlas.y),p));
#else
    vec2 q=vec2(dot(u_map_x,p),dot(u_map_y,p));
#endif
    vec2 at=floor(q+0.5)-u_source_origin;
    vec2 sample_at=q-u_source_origin;
    float coverage=floor(texture2D(u_source,(sample_at+0.5)/u_source_size).r*255.0+0.5);
    vec4 d=floor(texture2D(u_backdrop,(point-u_backdrop_origin+0.5)/u_backdrop_size)*255.0+0.5);
    float divisor=256.0;
    float row_offset=0.0;
    if(u_color.a==65.0) { divisor=64.0; row_offset=256.0; }
    float mask=coverage;
    if(u_operation.z!=255.0) mask=floor(coverage*max(u_operation.z,0.0)/256.0);
    float ratio=floor(texture2D(u_lookup,(vec2(d.a,row_offset+mask)+0.5)/vec2(256.0,321.0)).a*255.0+0.5);
    if(any(lessThan(at,vec2(0.0))) || any(greaterThanEqual(at,u_source_size))) discard;
    vec4 result=d;
    float alpha=0.0;
    if(u_operation.z<0.0) {
        float weight=-u_operation.z;
        float maximum=255.0;
        if(u_color.a==65.0) maximum=64.0;
        if(weight==255.0) alpha=floor(d.a*(maximum-coverage)/divisor);
        else {
            float adjusted=weight;
            if(weight>127.0) adjusted+=1.0;
            float product=65535.0;
            if(u_color.a==65.0) product=16384.0;
            alpha=floor(d.a*(product-coverage*adjusted)/(divisor*256.0));
        }
        result.a=alpha;
    } else if(u_operation.y==1.0) {
        result.rgb=d.rgb+floor((u_color.rgb-d.rgb)*mask/divisor);
        if(u_operation.w==0.0) result.a=0.0;
    } else if(u_operation.y==0.0) {
        alpha=min(mask*(256.0/divisor),255.0);
        result.rgb=d.rgb+floor((u_color.rgb-d.rgb)*ratio/256.0);
        result.a=255.0-floor((255.0-d.a)*(255.0-alpha)/255.0);
    } else {
        alpha=mask*(256.0/divisor);
        alpha-=floor(alpha/256.0);
        result.rgb=min(vec3(255.0),floor(u_color.rgb*mask/divisor)+floor(d.rgb*(255.0-alpha)/256.0));
        alpha=d.a+alpha-floor(d.a*alpha/256.0);
        result.a=alpha-floor(alpha/256.0);
    }
    gl_FragColor=result/255.0;
}
