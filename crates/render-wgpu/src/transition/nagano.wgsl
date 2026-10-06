struct Parameters { phase:vec4<u32>, mode:vec4<u32>, size:vec4<u32>, data:array<vec4<u32>,4> }
@group(0) @binding(0) var first:texture_2d<f32>;
@group(0) @binding(1) var second:texture_2d<f32>;
@group(0) @binding(2) var rule:texture_2d<f32>;
@group(0) @binding(3) var lookup:texture_2d<f32>;
@group(0) @binding(4) var<uniform> p:Parameters;
@group(0) @binding(5) var<storage,read> table:array<i32>;
fn v(n:u32)->i32 { return bitcast<i32>(p.data[n/4u][n%4u]); }
fn bytes(c:vec4<f32>)->vec4<i32> { return vec4<i32>(round(c*255.0)); }
fn color(c:vec4<i32>)->vec4<f32> { return vec4<f32>(c)/255.0; }
fn sample_source(which:i32,q:vec2<i32>)->vec4<i32> {
    let at=clamp(q,vec2<i32>(0),vec2<i32>(p.size.xy)-1);
    if which==1 {return bytes(textureLoad(first,at,0));}
    return bytes(textureLoad(second,at,0));
}
fn blend(a:vec4<i32>,b:vec4<i32>,r:i32)->vec4<i32> {
    // Match the SGX-friendly interpolation used by these GLES kernels.
    let mode=v(0u);
    if mode==5 || mode==6 || mode==10 {
        return vec4<i32>(round(mix(vec4<f32>(a),vec4<f32>(b),f32(r)/255.0)));
    }
    return a+(((b-a)*r)>>vec4<u32>(8u));
}
fn darken(a:vec4<i32>,r:i32)->vec4<i32> {return vec4<i32>(a.rgb+((-a.rgb*r)>>vec3<u32>(8u)),a.a);}
fn lighten(a:vec4<i32>,r:i32)->vec4<i32> {return vec4<i32>(a.rgb+(((255-a.rgb)*r)>>vec3<u32>(8u)),a.a);}
fn unpack(n:i32)->vec4<i32> {
    let rgba=bitcast<u32>(n);return vec4<i32>(i32(rgba&255u),i32((rgba>>8u)&255u),i32((rgba>>16u)&255u),i32(rgba>>24u));
}
fn warp_rule(q:vec2<f32>)->vec3<i32> {
    let at=clamp(vec2<i32>(floor(q)),vec2<i32>(0),vec2<i32>(v(3u),v(4u))-1);
    return unpack(table[1024+at.y*v(3u)+at.x]).rgb;
}
fn warp_position(q:vec2<f32>,phase:i32,side:i32)->vec2<i32> {
    var source=q;
    for(var i=0;i<3;i++) {
        let rule=warp_rule(source);let dt=max(phase-rule.r,0);
        let movement=f32(table[dt*4+2+side]);let direction=vec2<f32>(f32(table[rule.b*4]),f32(table[rule.b*4+1]));
        source=q-floor(direction*f32(rule.g)*movement/524288.0);
    }
    return vec2<i32>(source);
}
fn blurred(which:i32,q:vec2<i32>,radius:vec2<i32>)->vec4<i32> {
    if v(7u)==1 {
        let step=radius*2+1;let lo=(q/step)*step;let hi=min(lo+step,vec2<i32>(p.size.xy)-1);
        let frac=vec2<f32>(q-lo)/vec2<f32>(max(hi-lo,vec2<i32>(1)));
        let a=vec4<f32>(sample_source(which,lo));let b=vec4<f32>(sample_source(which,vec2<i32>(hi.x,lo.y)));
        let c=vec4<f32>(sample_source(which,vec2<i32>(lo.x,hi.y)));let d=vec4<f32>(sample_source(which,hi));
        return vec4<i32>(floor(mix(mix(a,b,frac.x),mix(c,d,frac.x),frac.y)));
    }
    let lo=max(q-radius,vec2<i32>(0));let hi=min(q+radius,vec2<i32>(p.size.xy)-1);var sum=vec4<i32>(0);
    for(var y=0;y<5;y++) {for(var x=0;x<5;x++) {
        let at=vec2<i32>(floor(mix(vec2<f32>(lo),vec2<f32>(hi),vec2<f32>(f32(x),f32(y))/4.0)+0.5));
        sum+=sample_source(which,at);
    }}
    return sum/25;
}
@vertex fn vertex(@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32> {
    let x=f32((index<<1u)&2u);let y=f32(index&2u);return vec4<f32>(x*2.0-1.0,y*2.0-1.0,0.0,1.0);
}
@fragment fn fragment(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
    let q=vec2<i32>(position.xy);let mode=v(0u);let phase=v(1u);let w=i32(p.size.x);
    if p.phase.y==0xffffffffu {return textureLoad(second,q,0);}
    if mode>=6 && v(15u)==1 {return color(sample_source(1,q));}
    if mode>=6 && v(15u)==2 {return color(sample_source(2,q));}
    if mode==0 {
        let a=sample_source(1,vec2<i32>(table[q.x*2],table[(w+q.y)*2]));
        let b=sample_source(2,vec2<i32>(table[q.x*2+1],table[(w+q.y)*2+1]));
        if b.a==0 {return color(vec4<i32>(a.rgb,(a.a*(255-phase))>>8u));}
        if a.a==0 {return color(vec4<i32>(b.rgb,(b.a*phase)>>8u));}
        return color(blend(a,b,phase));
    }
    if mode==1 {
        let x=q.x+select(-v(3u),v(3u),(q.y%2)!=0);
        return color(sample_source(select(1,2,x<0||x>=w),vec2<i32>((x+w)%w,q.y)));
    }
    if mode==2 {
        let a=sample_source(1,q);let b=sample_source(2,q);
        if v(15u)==1 {return color(a);}
        let r=vec4<i32>(v(4u),v(5u),v(6u),v(7u));
        if b.a==0 {return color(vec4<i32>((a.rgb*r.rgb)&vec3<i32>(255),a.a-((a.a*r.a)>>8u)));}
        if a.a==0 {return color(vec4<i32>((a.rgb*r.rgb)&vec3<i32>(255),(b.a*r.a)>>8u));}
        return color(a+(((b-a)*r)>>vec4<u32>(8u)));
    }
    if mode==3 {
        let phase_x=v(3u);let x=q.x;let dir=v(4u);let shade=v(5u);let hg=shade/2;let m=min(phase_x,16);let den=max(phase_x,1);
        var seg=0;var f=0;var sx=x;
        if dir==0 {
            let b1=w-2*phase_x-m;let b2=w-2*phase_x;let b3=(2*w-3*phase_x)/2;let b4=w-phase_x;
            if x<b1 {seg=0;}
            else if x<b2 {seg=1;f=((x-b2+m)*shade/max(m,1))/2;}
            else if x<b3 {seg=2;f=(b3-x)*shade*2/den;sx=x+2*phase_x-w;}
            else if x<b4 {seg=3;f=((x-b4)*2+phase_x)*shade/den;sx=x+2*phase_x-w;}
            else {seg=4;f=(w-x)*shade/den;}
        }else {
            let b1=phase_x;let b2=3*phase_x/2;let b3=2*phase_x;let b4=2*phase_x+m;
            if x<b1 {seg=4;f=(b1-x)*shade/den;}
            else if x<b2 {seg=3;f=(b2-x)*shade*2/den;sx=x+w-2*phase_x;}
            else if x<b3 {seg=2;f=max(0,(phase_x/2+(x-b3)*2)*shade/den);sx=x+w-2*phase_x;}
            else if x<b4 {seg=1;f=((b4-x)*shade/max(m,1))/2;}
        }
        let a=sample_source(1,q);let b=sample_source(2,vec2<i32>(sx,q.y));
        if phase_x==0||seg==0 {return color(a);}
        if seg==1 {return color(darken(a,f));}
        if seg==4 {return color(darken(b,f));}
        let folded=select(darken(b,f),lighten(b,f),seg==2);
        return color(vec4<i32>(blend(darken(a,hg),folded,b.a).rgb,a.a));
    }
    if mode==4 {
        let sx=table[q.x*3];let slope=table[q.x*3+1];let which=table[q.x*3+2];
        let half=i32(p.size.y)/2;
        let sy=((slope*(q.y-half))>>8u)+half;
        if which==0||sy<0||sy>=i32(p.size.y) {return vec4<f32>(0.0);}
        return color(sample_source(which,vec2<i32>(sx,sy)));
    }
    if mode==5 {
        let edge=table[q.y];let rx=q.x-v(3u);let rw=v(5u);
        let left=select(2,1,v(4u)!=0);
        let c=sample_source(select(3-left,left,rx<edge),q);
        if rx>=0&&rx<edge {
            let rgba=bitcast<u32>(table[i32(p.size.y)+min(q.y,v(6u)-1)*rw+rx]);
            let decoration=vec4<i32>(i32(rgba&255u),i32((rgba>>8u)&255u),i32((rgba>>16u)&255u),i32(rgba>>24u));
            return color(blend(c,decoration,decoration.a));
        }
        return color(c);
    }
    if mode==6 {
        let slip=v(4u);let fold=i32(p.size.x+p.size.y)-v(3u);let side=q.x+q.y-fold;
        var old=q-vec2<i32>(slip);
        let paper=side<0&&all(old>=vec2<i32>(0));let folded=side>=0&&f32(side)<max(16.0,f32(min(p.size.x,p.size.y))*0.28);
        if folded {old=vec2<i32>(fold-q.y,fold-q.x)-vec2<i32>(slip);}
        let a=sample_source(1,old);let b=sample_source(2,q);
        if folded {
            let argb=unpack(v(2u));let back=vec4<i32>(argb.bgr,255);
            return color(blend(b,darken(blend(a,back,argb.a),clamp(side*2,0,160)),v(6u)));
        }
        return color(select(b,a,paper));
    }
    if mode==7 {
        let s=f32(v(3u));let twist=f32(v(4u));let order=v(5u);let dir=v(6u);let at=vec2<f32>(q);
        let aq=(0.577350269*at.x-at.y/3.0)/s;let ar=(2.0*at.y/3.0)/s;
        let cube=vec3<f32>(aq,-aq-ar,ar);var cell=floor(cube+0.5);let err=abs(cell-cube);
        if err.x>err.y&&err.x>err.z {cell.x=-cell.y-cell.z;}
        else if err.y>err.z {cell.y=-cell.x-cell.z;} else {cell.z=-cell.x-cell.y;}
        let center=vec2<f32>(s*1.732050808*(cell.x+cell.z/2.0),s*1.5*cell.z);
        let n=center/max(vec2<f32>(p.size.xy)-1.0,vec2<f32>(1.0));
        let dx=(order-1)%3-1;let dy=1-(order-1)/3;
        var delay=length(n*2.0-1.0)/1.414213562;
        if order!=5 {
            let px=select(select(1.0-n.x,n.x,dx>0),0.0,dx==0);
            let py=select(select(1.0-n.y,n.y,dy>0),0.0,dy==0);
            delay=(px+py)/f32(max(abs(dx)+abs(dy),1));
        }
        let local=clamp(phase-i32(floor(clamp(delay,0.0,1.0)*192.0+0.5)),0,63);
        if local==0 {return color(sample_source(1,q));}if local==63 {return color(sample_source(2,q));}
        let scale=select(f32(local-31)/32.0,f32(32-local)/32.0,local<32);
        let halfWidth=s*0.866025404*clamp(2.0*(s-abs(at.y-center.y))/s,0.0,1.0);
        var rel=at.x-center.x;if dir==1||dir==4||dir==7 {rel=-rel;}
        let sx=i32(floor(center.x+rel/max(scale,0.0001)+twist/s*(at.y-center.y)*(1.0-scale)+0.5));
        if abs(rel)>halfWidth*scale||sx<0||sx>=w {return vec4<f32>(0.0);}
        return color(sample_source(select(2,1,local<32),vec2<i32>(sx,q.y)));
    }
    if mode==8 {
        let count=v(3u);let travel=v(4u);let roundness=f32(v(5u))/65536.0;let unit=v(6u);
        var amount=0;var shift=0;
        for(var i=0;i<20;i++) {
            if i>=count {break;}let at=i*4;
            let delta=vec2<f32>(q-vec2<i32>(table[at],table[at+1]))*vec2<f32>(1.0,roundness);
            let wc=table[at+2]-i32(floor(length(delta)+0.5));
            if wc>=travel {amount+=unit;}
            else if wc>=0 {amount+=table[count*4+wc*2+1];shift-=(table[count*4+wc*2]*table[at+3])>>8u;}
        }
        let at=vec2<i32>(q.x,clamp(q.y+shift,0,i32(p.size.y)-1));
        let a=sample_source(1,at);let b=sample_source(2,at);
        var result=blend(a,b,amount&255);if (amount&256)!=0 {result=blend(b,a,amount&255);}
        return color(vec4<i32>(result.rgb,255));
    }
    if mode==9 {
        let qa=warp_position(vec2<f32>(q),phase,0);let qb=warp_position(vec2<f32>(q),255-phase,1);
        var a=sample_source(1,qa);var b=sample_source(2,qb);
        if any(qa<vec2<i32>(0))||any(qa>=vec2<i32>(p.size.xy)) {a=vec4<i32>(0);}
        if any(qb<vec2<i32>(0))||any(qb>=vec2<i32>(p.size.xy)) {b=vec4<i32>(0);}
        let start=warp_rule(vec2<f32>(q)).r;let ratio=clamp((phase-start)*255/max(255-start,1),0,255);
        return color(blend(a,b,ratio));
    }
    if mode==10 {
        let row=q.y*v(3u);var lo=0;var hi=table[row];
        for(var i=0;i<10;i++) {if lo>=hi {break;}let mid=(lo+hi)/2;if q.x<table[row+1+mid*2] {hi=mid;}else {lo=mid+1;}}
        let id=table[row+2+lo*2];var qa=q;var qb=q;
        if id>=0 {
            let at=v(4u)+id*18;
            let a=vec2<f32>(f32(table[at]),f32(table[at+1]));let b=vec2<f32>(f32(table[at+2]),f32(table[at+3]));let c=vec2<f32>(f32(table[at+4]),f32(table[at+5]));
            let den=(b.y-c.y)*(a.x-c.x)+(c.x-b.x)*(a.y-c.y);
            if abs(den)>0.0001 {
                let pt=vec2<f32>(q);let u=((b.y-c.y)*(pt.x-c.x)+(c.x-b.x)*(pt.y-c.y))/den;
                let vtx=((c.y-a.y)*(pt.x-c.x)+(a.x-c.x)*(pt.y-c.y))/den;
                qa=vec2<i32>(floor(u*vec2<f32>(f32(table[at+6]),f32(table[at+7]))+vtx*vec2<f32>(f32(table[at+8]),f32(table[at+9]))+(1.0-u-vtx)*vec2<f32>(f32(table[at+10]),f32(table[at+11]))+0.5));
                qb=vec2<i32>(floor(u*vec2<f32>(f32(table[at+12]),f32(table[at+13]))+vtx*vec2<f32>(f32(table[at+14]),f32(table[at+15]))+(1.0-u-vtx)*vec2<f32>(f32(table[at+16]),f32(table[at+17]))+0.5));
            }
        }
        return color(blend(sample_source(1,qa),sample_source(2,qb),phase));
    }
    if mode==11 {return color(blend(blurred(1,q,vec2<i32>(v(3u),v(4u))),blurred(2,q,vec2<i32>(v(5u),v(6u))),phase));}
    return vec4<f32>(0.0);
}
