struct Parameters {
    phase: vec4<i32>, // effect, phase, maximum, vague
    mode: vec4<i32>, // face, direction, stay, reserved
    size: vec4<i32>,
}
@group(0) @binding(0) var first: texture_2d<f32>;
@group(0) @binding(1) var second: texture_2d<f32>;
@group(0) @binding(2) var rule: texture_2d<f32>;
@group(0) @binding(3) var lookup: texture_2d<f32>;
@group(0) @binding(4) var<uniform> p: Parameters;
fn bytes(v:vec4<f32>)->vec4<i32>{return vec4<i32>(round(v*255.0));}
@vertex fn vertex(@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32>{
    let x=f32((index<<1u)&2u);let y=f32(index&2u);
    return vec4<f32>(x*2.0-1.0,y*2.0-1.0,0.0,1.0);
}
@fragment fn fragment(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32>{
    let at=vec2<i32>(position.xy);
    if p.phase.y==0 {return textureLoad(first,at,0);}
    if p.phase.y>=p.phase.z {return textureLoad(second,at,0);}
    if p.phase.x==2 {
        let horizontal=p.mode.y==0 || p.mode.y==2;
        let extent=select(p.size.y,p.size.x,horizontal);
        let sign=select(-1,1,p.mode.y<2);
        let a=select(sign*p.phase.y,0,p.mode.z==1);
        let b=select(sign*(p.phase.y-extent),0,p.mode.z==2);
        let coordinate=select(at.y,at.x,horizontal);
        let in_a=coordinate-a>=0 && coordinate-a<extent;
        let in_b=coordinate-b>=0 && coordinate-b<extent;
        let source=select(in_b,!in_a,p.mode.z==2);
        let offset=select(a,b,source);
        let point=at-select(vec2<i32>(0,offset),vec2<i32>(offset,0),horizontal);
        if source {return textureLoad(second,point,0);}
        return textureLoad(first,point,0);
    }
    let a=bytes(textureLoad(first,at,0));let b=bytes(textureLoad(second,at,0));
    var opacity=p.phase.y;
    if p.phase.x==1 {
        let level=bytes(textureLoad(rule,at,0)).r;
        let lower=p.phase.y-p.phase.w;
        if p.phase.w<512 {
            if level>=p.phase.y {return vec4<f32>(a)/255.0;}
            if level<lower {return vec4<f32>(b)/255.0;}
        }
        if level<lower {opacity=255;}
        else if level>=p.phase.y {opacity=0;}
        else {opacity=clamp(255-((level-lower)*255)/p.phase.w,0,255);}
    }
    var factor=opacity;var alpha=0;
    if p.mode.x==0 {
        let weight=opacity+select(0,1,p.phase.x==0 && opacity>127);
        let address=vec2<i32>((a.a*(256-weight))>>8u,(b.a*weight)>>8u);
        factor=bytes(textureLoad(lookup,address,0)).a;
        alpha=a.a+(((b.a-a.a)*weight)>>8u);
    } else if p.mode.x==4 {alpha=a.a+(((b.a-a.a)*opacity)>>8u);}
    return vec4<f32>(vec4<i32>(a.rgb+(((b.rgb-a.rgb)*factor)>>vec3<u32>(8u)),alpha))/255.0;
}
