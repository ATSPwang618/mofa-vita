varying vec2 v_point;
uniform sampler2D u_source;
uniform sampler2D u_lookup;
uniform vec2 u_source_origin;
uniform vec2 u_source_size;
uniform vec2 u_source_scale;
uniform vec2 u_weights_size;
uniform vec2 u_offset;
uniform float u_axis;

void main() {
    vec2 point=floor(v_point);
    float row=u_axis!=0.0?point.y:point.x;
    float start=unpack_float(texture2D(u_lookup,vec2(0.5,row+0.5)/u_weights_size));
    float count=unpack_float(texture2D(u_lookup,vec2(1.5,row+0.5)/u_weights_size));
    vec4 sum=vec4(0.0);
    for(int k=0;k<FILTER_TAPS;k++) {
        if(float(k)<count) {
            vec2 q=u_axis!=0.0?vec2(point.x,start+float(k)):vec2(start+float(k),point.y);
            q=floor((q+u_offset+0.5)*u_source_scale)-u_source_origin;
            float weight=unpack_float(texture2D(u_lookup,vec2(float(k)+2.5,row+0.5)/u_weights_size));
            vec4 pixel=floor(texture2D(u_source,(q+0.5)/u_source_size)*255.0+0.5);
            sum+=pixel*weight;
        }
    }
    // Match the legacy u8 intermediate after each complete filter axis.
    gl_FragColor=floor(clamp(sum,0.0,255.0))/255.0;
}
