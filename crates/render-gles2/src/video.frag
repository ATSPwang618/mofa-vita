varying vec2 v_point;
uniform sampler2D u_source,u_backdrop;
uniform vec2 u_source_size;
uniform float u_nv12;
void main(){
    vec2 at=floor(v_point);
    float y=1.164383*(texture2D(u_source,(at+0.5)/u_source_size).r-16.0/255.0);
    float u,v;
    if(u_nv12>0.5){
        vec2 chroma=(floor(at/2.0)+0.5)/(u_source_size/2.0);
        vec4 uv=texture2D(u_backdrop,chroma);
        u=uv.r-128.0/255.0;
        v=uv.a-128.0/255.0;
    }else{
        vec2 chroma=(floor(at/2.0)+0.5)/vec2(u_source_size.x/2.0,u_source_size.y);
        v=texture2D(u_backdrop,chroma).r-128.0/255.0;
        u=texture2D(u_backdrop,chroma+vec2(0.0,0.5)).r-128.0/255.0;
    }
    gl_FragColor=vec4(y+1.596027*v,y-0.391762*u-0.812968*v,y+2.017232*u,1.0);
}
