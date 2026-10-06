// extNagano pixel kernels. Fetches precede discard for the SGX compiler.
varying vec2 v_point;
uniform sampler2D u_source,u_backdrop,u_rule;
uniform vec2 u_source_origin,u_source_size,u_source_scale;
uniform vec2 u_backdrop_origin,u_backdrop_size,u_backdrop_scale;
uniform vec2 u_table_size,u_extent,u_canvas;
uniform vec4 u_data0,u_data1,u_data2,u_data3;
uniform vec4 u_color;
vec4 bytes(vec4 c) { return floor(c*255.0+0.5); }
float table(float n) {
    vec4 b=bytes(texture2D(u_rule,(vec2(mod(n,u_table_size.x),floor(n/u_table_size.x))+0.5)/u_table_size));
    float high=b.b+b.a*256.0; if(high>=32768.0) high-=65536.0;
    return high*65536.0+b.r+b.g*256.0;
}
vec4 rule_pixel(float n) {
    return bytes(texture2D(u_rule,(vec2(mod(n,u_table_size.x),floor(n/u_table_size.x))+0.5)/u_table_size));
}
vec4 sample_source(float which,vec2 q,out bool valid) {
    q=clamp(q,vec2(0.0),u_extent-1.0);
    vec2 a=floor((q+0.5)*u_source_scale)-u_source_origin;
    vec2 b=floor((q+0.5)*u_backdrop_scale)-u_backdrop_origin;
    vec4 ca=bytes(texture2D(u_source,(a+0.5)/u_source_size));
    vec4 cb=bytes(texture2D(u_backdrop,(b+0.5)/u_backdrop_size));
    bool va=all(greaterThanEqual(a,vec2(0.0)))&&all(lessThan(a,u_source_size));
    bool vb=all(greaterThanEqual(b,vec2(0.0)))&&all(lessThan(b,u_backdrop_size));
    valid=which==1.0?va:vb; return which==1.0?ca:cb;
}
vec4 blend(vec4 a,vec4 b,float r) {
#if NAGANO_MODE == 5 || NAGANO_MODE == 6 || NAGANO_MODE == 10
    // SGX rejects the integer-style floor blend in these control-flow paths.
    // Use ordinary interpolation; visual fidelity here is intentionally approximate.
    return mix(a,b,r/255.0);
#else
    return a+floor((b-a)*r/256.0);
#endif
}
vec4 darken(vec4 a,float r) { return vec4(a.rgb+floor(-a.rgb*r/256.0),a.a); }
vec4 lighten(vec4 a,float r) { return vec4(a.rgb+floor((255.0-a.rgb)*r/256.0),a.a); }
#if NAGANO_MODE == 9
vec3 warp_rule(vec2 q) {
    vec2 at=clamp(floor(q),vec2(0.0),vec2(u_data0.w,u_data1.x)-1.0);
    return rule_pixel(1024.0+at.y*u_data0.w+at.x).rgb;
}
vec2 warp_position(vec2 q,float phase,float side) {
    vec2 source=q;
    for(int i=0;i<3;i++) {
        vec3 rule=warp_rule(source);
        float dt=max(phase-rule.r,0.0),movement=table(dt*4.0+2.0+side);
        vec2 direction=vec2(table(rule.b*4.0),table(rule.b*4.0+1.0));
        source=q-floor(direction*rule.g*movement/524288.0);
    }
    return source;
}
#endif
#if NAGANO_MODE == 11
vec4 blurred(float which,vec2 q,vec2 radius,out bool valid) {
    valid=true;
    vec4 sum=vec4(0.0);
    if(u_data1.w==1.0) {
        vec2 step=radius*2.0+1.0,lo=floor(q/step)*step,hi=min(lo+step,u_extent-1.0);
        vec2 frac=(q-lo)/max(hi-lo,vec2(1.0));
        bool va,vb,vc,vd;
        vec4 a=sample_source(which,lo,va),b=sample_source(which,vec2(hi.x,lo.y),vb);
        vec4 c=sample_source(which,vec2(lo.x,hi.y),vc),d=sample_source(which,hi,vd);
        valid=va&&vb&&vc&&vd;return floor(mix(mix(a,b,frac.x),mix(c,d,frac.x),frac.y));
    }
    // Bounded 5x5 box approximation: cost is independent of a game's radius.
    vec2 lo=max(q-radius,vec2(0.0)),hi=min(q+radius,u_extent-1.0);
    for(int y=0;y<5;y++) for(int x=0;x<5;x++) {
        bool v;sum+=sample_source(which,floor(mix(lo,hi,vec2(float(x),float(y))/4.0)+0.5),v);valid=valid&&v;
    }
    return floor(sum/25.0);
}
#endif
vec4 effect(vec2 q,out bool valid) {
    float phase=u_data0.y;
#if NAGANO_MODE >= 6
    if(u_data3.w==1.0) return sample_source(1.0,q,valid);
    if(u_data3.w==2.0) return sample_source(2.0,q,valid);
#endif
#if NAGANO_MODE == 0
    vec2 a=vec2(table(q.x*2.0),table((u_extent.x+q.y)*2.0));
    vec2 b=vec2(table(q.x*2.0+1.0),table((u_extent.x+q.y)*2.0+1.0));
    bool va,vb;
    vec4 ca=sample_source(1.0,a,va),cb=sample_source(2.0,b,vb);
    valid=va&&vb;
    vec4 result=blend(ca,cb,phase);
    if(cb.a==0.0) result=vec4(ca.rgb,floor(ca.a*(255.0-phase)/256.0));
    else if(ca.a==0.0) result=vec4(cb.rgb,floor(cb.a*phase/256.0));
    return result;
#elif NAGANO_MODE == 1
    float shift=u_data0.w;
    float x=q.x-shift;
    if(mod(q.y,2.0)!=0.0) x=q.x+shift;
    bool incoming=x<0.0||x>=u_extent.x;
    return sample_source(incoming?2.0:1.0,vec2(mod(x,u_extent.x),q.y),valid);
#elif NAGANO_MODE == 2
    bool va,vb;vec4 a=sample_source(1.0,q,va),b=sample_source(2.0,q,vb);valid=va&&vb;
    vec4 r=u_data1;
    vec4 c=a+floor((b-a)*r/256.0);
    // Original byte multiplications deliberately wrap in the zero-alpha path.
    if(b.a==0.0) c=vec4(mod(a.rgb*r.rgb,256.0),a.a-floor(a.a*r.a/256.0));
    else if(a.a==0.0) c=vec4(mod(a.rgb*r.rgb,256.0),floor(b.a*r.a/256.0));
    if(u_data3.w==1.0) c=a;
    return c;
#elif NAGANO_MODE == 3
    float p=u_data0.w,w=u_extent.x,x=q.x,dir=u_data1.x;
    float shade=u_data1.y,hg=floor(shade/2.0),m=min(p,16.0);
    float seg=0.0,f=0.0,sx=x;
    if(dir==0.0) {
        float b1=w-2.0*p-m,b2=w-2.0*p,b3=floor(w-3.0*p/2.0),b4=w-p;
        if(x<b1) seg=0.0;
        else if(x<b2) {seg=1.0;f=floor(floor((x-b2+m)*shade/max(m,1.0))/2.0);}
        else if(x<b3) {seg=2.0;f=floor((b3-x)*shade*2.0/max(p,1.0));sx=x+2.0*p-w;}
        else if(x<b4) {seg=3.0;f=floor(((x-b4)*2.0+p)*shade/max(p,1.0));sx=x+2.0*p-w;}
        else {seg=4.0;f=floor((w-x)*shade/max(p,1.0));}
    } else {
        float b1=p,b2=floor(3.0*p/2.0),b3=2.0*p,b4=2.0*p+m;
        if(x<b1) {seg=4.0;f=floor((b1-x)*shade/max(p,1.0));}
        else if(x<b2) {seg=3.0;f=floor((b2-x)*shade*2.0/max(p,1.0));sx=x+w-2.0*p;}
        else if(x<b3) {seg=2.0;f=max(0.0,floor((floor(p/2.0)+(x-b3)*2.0)*shade/max(p,1.0)));sx=x+w-2.0*p;}
        else if(x<b4) {seg=1.0;f=floor(floor((b4-x)*shade/max(m,1.0))/2.0);}
    }
    bool va,vb;vec4 a=sample_source(1.0,q,va),b=sample_source(2.0,vec2(sx,q.y),vb);
    vec4 folded=seg==2.0?lighten(b,f):darken(b,f);
    vec4 c=blend(darken(a,hg),folded,b.a);c.a=a.a;
    valid=va&&vb;
    if(seg==0.0) {c=a;valid=va;}
    else if(seg==1.0) {c=darken(a,f);valid=va;}
    else if(seg==4.0) {c=darken(b,f);valid=vb;}
    if(p==0.0) {c=a;valid=va;}
    return c;
#elif NAGANO_MODE == 4
    float sx=table(q.x*3.0),slope=table(q.x*3.0+1.0),which=table(q.x*3.0+2.0);
    float sy=floor(slope*(q.y-floor(u_extent.y/2.0))/256.0)+floor(u_extent.y/2.0);
    vec4 c=sample_source(which,vec2(sx,sy),valid);
    if(which==0.0||sy<0.0||sy>=u_extent.y) {valid=true;return vec4(0.0);}
    return c;
#elif NAGANO_MODE == 5
    bool va,vb;vec4 a=sample_source(1.0,q,va),b=sample_source(2.0,q,vb);
    float th=u_data0.w,edge=table(q.y),rx=q.x-th,rw=u_data1.y;
    vec4 decoration=rule_pixel(u_extent.y+min(q.y,u_data1.z-1.0)*rw+clamp(rx,0.0,rw-1.0));
    float reveal=1.0-step(th+edge,q.x);
    if(u_data1.x!=0.0) reveal=1.0-reveal;
    vec4 c=mix(a,b,reveal);valid=va&&vb;
    float band=step(0.0,rx)*(1.0-step(edge,rx));
    return blend(c,decoration,decoration.a*band);
#elif NAGANO_MODE == 6
    float slip=u_data1.x,fold=u_extent.x+u_extent.y-u_data0.w;
    float side=q.x+q.y-fold;
    vec2 old=q-vec2(slip);
    bool paper=side<0.0&&all(greaterThanEqual(old,vec2(0.0)));
    bool folded=side>=0.0&&side<max(16.0,min(u_extent.x,u_extent.y)*0.28);
    if(folded) old=vec2(fold-q.y,fold-q.x)-vec2(slip);
    bool va,vb;vec4 a=sample_source(1.0,old,va),b=sample_source(2.0,q,vb);
    vec4 back=vec4(bytes(u_color).rgb,255.0);
    float backAlpha=bytes(u_color).a;
    vec4 c=blend(a,back,backAlpha);
    c=darken(c,clamp(side*2.0,0.0,160.0));
    c=blend(b,c,u_data1.z);
    valid=folded?va&&vb:paper?va:vb;
    return folded?c:paper?a:b;
#elif NAGANO_MODE == 7
    float s=u_data0.w,twist=u_data1.x,order=u_data1.y,dir=u_data1.z;
    float aq=(0.577350269*q.x-q.y/3.0)/s,ar=(2.0*q.y/3.0)/s;
    vec3 cube=vec3(aq,-aq-ar,ar),cell=floor(cube+0.5),err=abs(cell-cube);
    if(err.x>err.y&&err.x>err.z) cell.x=-cell.y-cell.z;
    else if(err.y>err.z) cell.y=-cell.x-cell.z;else cell.z=-cell.x-cell.y;
    vec2 center=vec2(s*1.732050808*(cell.x+cell.z/2.0),s*1.5*cell.z);
    vec2 n=center/max(u_extent-1.0,vec2(1.0));
    float dx=mod(order-1.0,3.0)-1.0,dy=1.0-floor((order-1.0)/3.0);
    float delay=length(n*2.0-1.0)/1.414213562;
    if(order!=5.0) delay=((dx==0.0?0.0:dx>0.0?n.x:1.0-n.x)+(dy==0.0?0.0:dy>0.0?n.y:1.0-n.y))/max(abs(dx)+abs(dy),1.0);
    float local=clamp(phase-floor(clamp(delay,0.0,1.0)*192.0+0.5),0.0,63.0);
    float scale=local<32.0?(32.0-local)/32.0:(local-31.0)/32.0;
    float halfWidth=s*0.866025404*clamp(2.0*(s-abs(q.y-center.y))/s,0.0,1.0);
    float rel=q.x-center.x;
    if(dir==1.0||dir==4.0||dir==7.0) rel=-rel;
    vec2 at=vec2(floor(center.x+rel/max(scale,0.0001)+twist/s*(q.y-center.y)*(1.0-scale)+0.5),q.y);
    if(local==0.0||local==63.0) at=q;
    vec4 c=sample_source(local<32.0?1.0:2.0,at,valid);
    if(local>0.0&&local<63.0&&(abs(rel)>halfWidth*scale||at.x<0.0||at.x>=u_extent.x)) {valid=true;c=vec4(0.0);}
    return c;
#elif NAGANO_MODE == 8
    float count=u_data0.w,travel=u_data1.x,roundness=u_data1.y/65536.0,unit=u_data1.z;
    float amount=0.0,shift=0.0;
    for(int i=0;i<20;i++) {
        if(float(i)>=count) break;
        float at=float(i)*4.0;
        vec2 delta=q-vec2(table(at),table(at+1.0));delta.y*=roundness;
        float wc=table(at+2.0)-floor(length(delta)+0.5);
        if(wc>=travel) amount+=unit;
        else if(wc>=0.0) {amount+=table(count*4.0+wc*2.0+1.0);shift-=floor(table(count*4.0+wc*2.0)*table(at+3.0)/256.0);}
    }
    vec2 at=vec2(q.x,clamp(q.y+shift,0.0,u_extent.y-1.0));
    bool va,vb;vec4 a=sample_source(1.0,at,va),b=sample_source(2.0,at,vb);valid=va&&vb;
    if(mod(floor(amount/256.0),2.0)!=0.0) {vec4 tmp=a;a=b;b=tmp;}
    return vec4(blend(a,b,mod(amount,256.0)).rgb,255.0);
#elif NAGANO_MODE == 9
    vec2 qa=warp_position(q,phase,0.0),qb=warp_position(q,255.0-phase,1.0);
    bool va,vb;vec4 a=sample_source(1.0,qa,va),b=sample_source(2.0,qb,vb);
    bool ina=all(greaterThanEqual(qa,vec2(0.0)))&&all(lessThan(qa,u_extent));
    bool inb=all(greaterThanEqual(qb,vec2(0.0)))&&all(lessThan(qb,u_extent));
    if(!ina) a=vec4(0.0);if(!inb) b=vec4(0.0);valid=(!ina||va)&&(!inb||vb);
    float start=warp_rule(q).r,ratio=clamp((phase-start)*255.0/max(255.0-start,1.0),0.0,255.0);
    return blend(a,b,ratio);
#elif NAGANO_MODE == 10
    float row=q.y*u_data0.w,lo=0.0,hi=table(row);
    for(int i=0;i<10;i++) {if(lo>=hi) break;float mid=floor((lo+hi)/2.0);if(q.x<table(row+1.0+mid*2.0)) hi=mid;else lo=mid+1.0;}
    float id=table(row+2.0+lo*2.0);vec2 qa=q,qb=q;
    if(id>=0.0) {
        float at=u_data1.x+id*18.0;
        vec2 a=vec2(table(at),table(at+1.0)),b=vec2(table(at+2.0),table(at+3.0)),c=vec2(table(at+4.0),table(at+5.0));
        float den=(b.y-c.y)*(a.x-c.x)+(c.x-b.x)*(a.y-c.y);
        if(abs(den)>0.0001) {
            float u=((b.y-c.y)*(q.x-c.x)+(c.x-b.x)*(q.y-c.y))/den;
            float v=((c.y-a.y)*(q.x-c.x)+(a.x-c.x)*(q.y-c.y))/den;
            qa=floor(u*vec2(table(at+6.0),table(at+7.0))+v*vec2(table(at+8.0),table(at+9.0))+(1.0-u-v)*vec2(table(at+10.0),table(at+11.0))+0.5);
            qb=floor(u*vec2(table(at+12.0),table(at+13.0))+v*vec2(table(at+14.0),table(at+15.0))+(1.0-u-v)*vec2(table(at+16.0),table(at+17.0))+0.5);
        }
    }
    bool va,vb;vec4 a=sample_source(1.0,qa,va),b=sample_source(2.0,qb,vb);valid=va&&vb;return blend(a,b,phase);
#elif NAGANO_MODE == 11
    bool va,vb;vec4 a=blurred(1.0,q,vec2(u_data0.w,u_data1.x),va),b=blurred(2.0,q,u_data1.yz,vb);valid=va&&vb;
    return blend(a,b,phase);
#endif
}
void main() {
    bool valid;vec2 q=floor((floor(v_point)+0.5)*u_canvas);
    vec4 color=effect(q,valid);
    if(!valid) discard;
    gl_FragColor=color/255.0;
}
