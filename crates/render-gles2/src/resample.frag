varying vec2 v_point;
uniform sampler2D u_source;
uniform sampler2D u_backdrop;
uniform sampler2D u_lookup;
uniform vec4 u_rectangle;
uniform vec2 u_source_origin;
uniform vec2 u_source_size;
uniform vec4 u_source_visible;
uniform vec2 u_source_scale;
uniform vec2 u_weights_size;
uniform vec2 u_offset;
uniform vec4 u_channel;
uniform float u_axis;
uniform float u_resample_kind;

void main() {
    vec2 point=floor(v_point);
    float sum=unpack_float(texture2D(u_backdrop,(point-u_rectangle.xy+0.5)/u_rectangle.zw));
    if(u_resample_kind!=0.0) {
        // Legacy separable filters truncate after each whole axis, not each
        // source tile or fixed-size tap batch. RGB and alpha remain straight.
        gl_FragColor=vec4(floor(clamp(sum,0.0,255.0))/255.0);
        return;
    }
    float row=u_axis!=0.0?point.y-u_rectangle.y:point.x-u_rectangle.x;
    float start=unpack_float(texture2D(u_lookup,vec2(0.5,row+0.5)/u_weights_size));
    float count=unpack_float(texture2D(u_lookup,vec2(1.5,row+0.5)/u_weights_size));
    // A compile-time loop bound is valid on strict GLSL ES 1.00 compilers.
    // Larger kernels are submitted as additional batches, retaining float sums.
    for(int k=0;k<64;k++) {
        if(float(k)<count) {
            vec2 source_point=u_axis!=0.0?vec2(point.x,start+float(k)):vec2(start+float(k),point.y);
            source_point=floor((source_point+u_offset+0.5)*u_source_scale);
            if(any(lessThan(source_point,u_source_visible.xy)) || any(greaterThanEqual(source_point,u_source_visible.xy+u_source_visible.zw))) continue;
            source_point-=u_source_origin;
            if(all(greaterThanEqual(source_point,vec2(0.0))) && all(lessThan(source_point,u_source_size))) {
                float weight=unpack_float(texture2D(u_lookup,vec2(float(k)+2.5,row+0.5)/u_weights_size));
                vec4 pixel=floor(texture2D(u_source,(source_point+0.5)/u_source_size)*255.0+0.5);
                sum+=dot(pixel,u_channel)*weight;
            }
        }
    }
    gl_FragColor=pack_float(sum);
}
