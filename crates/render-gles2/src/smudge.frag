varying vec2 v_point;
uniform sampler2D u_source;
uniform vec2 u_source_origin;
uniform vec2 u_source_size;
uniform vec2 u_canvas;
uniform vec2 u_region;

vec4 pixel(vec2 at) {
    return floor(texture2D(u_source,(at-u_source_origin+0.5)/u_source_size)*255.0+0.5);
}
void main() {
    vec2 at=floor(v_point)-u_region;
    vec4 center=pixel(at);
    vec4 sum=vec4(0.0);
    for(int y=-1;y<=1;y++) {
        for(int x=-1;x<=1;x++) {
            if(x!=0 || y!=0) {
                vec2 neighbor=at+vec2(float(x),float(y));
                if(any(lessThan(neighbor,vec2(0.0))) || any(greaterThanEqual(neighbor,u_canvas))) sum+=center;
                else sum+=pixel(neighbor);
            }
        }
    }
    gl_FragColor=floor(sum/8.0)/255.0;
}
