varying vec2 v_point;
uniform sampler2D u_source;
uniform sampler2D u_backdrop;
uniform sampler2D u_lookup;
uniform sampler2D u_rule;
uniform vec2 u_source_origin;
uniform vec2 u_source_size;
uniform vec4 u_source_visible;
uniform vec2 u_backdrop_size;
uniform vec2 u_table_size;
uniform vec2 u_weights_size;
uniform vec2 u_frame;
uniform vec2 u_region;
uniform vec2 u_canvas;
uniform vec4 u_channel;
uniform float u_axis;
uniform float u_kind;
uniform float u_offset;
uniform float u_resample_kind;

void main() {
    vec2 point=floor(v_point);
    vec2 at=point-u_region;
    float sum=unpack_float(texture2D(u_backdrop,(point-u_frame+0.5)/u_backdrop_size));
    if(u_resample_kind!=0.0) {
        float row=u_axis!=0.0?point.y-u_frame.y:point.x-u_frame.x;
        float divisor=unpack_float(texture2D(u_rule,vec2((row+0.5)/u_weights_size.x,0.5)));
        // Each complete axis rounds to the low byte, including alpha. In
        // particular the legacy small-image kernel can produce values >255.
        float rounded=floor(sum/divisor+0.5);
        float value=rounded>=4294967296.0?255.0:mod(max(rounded,0.0),256.0);
        gl_FragColor=vec4(value/255.0);
        return;
    }
    float row=u_axis!=0.0?at.y:at.x;
    float extent=u_axis!=0.0?u_canvas.y:u_canvas.x;
    float middle=floor(u_kind/2.0);
    for(int k=0;k<64;k++) {
        float j=u_offset+float(k);
        float index=row+j-middle;
        if(j<u_kind && index>=0.0 && index<extent) {
            vec2 pos=u_axis!=0.0?vec2(at.x,index):vec2(index,at.y);
            if(any(lessThan(pos,u_source_visible.xy)) || any(greaterThanEqual(pos,u_source_visible.xy+u_source_visible.zw))) continue;
            vec2 local=pos-u_source_origin;
            if(all(greaterThanEqual(local,vec2(0.0))) && all(lessThan(local,u_source_size))) {
                // Preserve the plugin's shortened-image indexing rule.
                float weight_index=u_kind>extent?index:j;
                vec2 entry=vec2(mod(weight_index,u_table_size.x),floor(weight_index/u_table_size.x));
                float weight=unpack_float(texture2D(u_lookup,(entry+0.5)/u_table_size));
                vec4 color=floor(texture2D(u_source,(local+0.5)/u_source_size)*255.0+0.5);
                sum+=dot(color,u_channel)*weight;
            }
        }
    }
    gl_FragColor=pack_float(sum);
}
