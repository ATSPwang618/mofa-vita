// Pixel half of krkr.extrans.v1. CPU preparation follows Kirikiri2 extrans;
// the six-word turn table and 16.16 rotation scanlines retain original sampling.
struct Parameters { phase:vec4<u32>, mode:vec4<u32>, size:vec4<u32>, data:array<vec4<u32>,4> }
@group(0) @binding(0) var first:texture_2d<f32>;
@group(0) @binding(1) var second:texture_2d<f32>;
@group(0) @binding(2) var rule:texture_2d<f32>;
@group(0) @binding(3) var lookup:texture_2d<f32>;
@group(0) @binding(4) var<uniform> p:Parameters;
@group(0) @binding(5) var<storage,read> table:array<i32>;
fn v(i:u32)->i32 { return bitcast<i32>(p.data[i/4u][i%4u]); }
fn rgba(c:u32)->vec4<i32> { return vec4<i32>(i32((c>>16u)&255u),i32((c>>8u)&255u),i32(c&255u),i32(c>>24u)); }
fn bytes(c:vec4<f32>)->vec4<i32> { return vec4<i32>(round(c*255.0)); }
fn color(c:vec4<i32>)->vec4<f32> { return vec4<f32>(c)/255.0; }
fn raw(a:vec4<i32>,b:vec4<i32>,ratio:i32)->vec4<i32> { return a+(((b-a)*ratio)>>vec4<u32>(8u)); }
fn sample_source(which:i32,at:vec2<i32>)->vec4<i32> {
    let q=clamp(at,vec2<i32>(0),vec2<i32>(p.size.xy)-1);
    if which==1 { return bytes(textureLoad(first,q,0)); }
    return bytes(textureLoad(second,q,0));
}
fn pair(at:vec2<i32>,alpha:bool)->vec4<i32> {
    let a=sample_source(1,at);let b=sample_source(2,at);let ratio=v(1u);
    if !alpha { return raw(a,b,ratio); }
    var factor=ratio;var opacity=0;
    if p.mode.x==0u {
        let weight=ratio+select(0,1,ratio>127);
        let address=vec2<i32>((a.a*(256-weight))>>8u,(b.a*weight)>>8u);
        factor=bytes(textureLoad(lookup,address,0)).a;
        opacity=a.a+(((b.a-a.a)*weight)>>8u);
    }else if p.mode.x==4u {opacity=a.a+(((b.a-a.a)*ratio)>>8u);}
    return vec4<i32>(a.rgb+(((b.rgb-a.rgb)*factor)>>vec3<u32>(8u)),opacity);
}
@vertex fn vertex(@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32> {
    let x=f32((index<<1u)&2u);let y=f32(index&2u);return vec4<f32>(x*2.0-1.0,y*2.0-1.0,0.0,1.0);
}
@fragment fn fragment(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
    let at=vec2<i32>(position.xy);let mode=v(0u);let background=rgba(p.data[0].z);
    if p.phase.y==0xffffffffu {return textureLoad(second,at,0);}
    if mode==0 {
        let x=at.x-table[at.y];
        if x<0 || x>=i32(p.size.x) {return color(background);}
        return color(pair(vec2<i32>(x,at.y),true));
    }
    if mode==1 {
        let block=v(3u);let offset=vec2<i32>(v(4u),v(5u));
        let q=((at-offset)/block)*block+offset+vec2<i32>(block>>1u);
        return color(pair(q,false));
    }
    if mode==2 {
        let tile=at/64;let local=at%64;
        let phase=clamp(v(3u)-(tile.x-tile.y)*2,0,63);
        if phase==0 {return textureLoad(first,at,0);}
        if phase==63 {return textureLoad(second,at,0);}
        let base=(phase*64+local.y)*6;
        let start=table[base];let count=table[base+1];
        if local.x<start || local.x>=start+count {return color(background);}
        let delta=local.x-start;
        let q=tile*64+vec2<i32>((table[base+2]+delta*table[base+4])>>16u,(table[base+3]+delta*table[base+5])>>16u);
        if any(q<vec2<i32>(0)) || any(q>=vec2<i32>(p.size.xy)) {return color(background);}
        var pixel=sample_source(select(1,2,phase>=32),q);
        let gloss=array<i32,13>(0,0,0,0,16,48,80,128,192,128,80,48,16);
        if phase<13 {pixel=raw(pixel,vec4<i32>(255,255,255,0),gloss[phase]);}
        return color(pixel);
    }
    if mode==3 {
        var pixel=background;
        // Paint rear source first. Each row has an independent start and steps.
        for(var n=0;n<2;n++) {
            let which=select(3-v(3u),v(3u),n==1);
            let base=at.y*16+(which-1)*8;
            let start=table[base];let end=table[base+1];
            if at.x>=start && at.x<end {
                let delta=at.x-start;
                let q=vec2<i32>((table[base+2]+delta*table[base+4])>>16u,(table[base+3]+delta*table[base+5])>>16u);
                pixel=sample_source(which,q);
            }
        }
        return color(pixel);
    }
    // Ripple: folded distance/direction map plus original quantized wave and
    // direction tables. No transcendental work or full-frame upload per tick.
    let center=vec2<i32>(v(3u),v(4u));
    let folded=select(at-center,center-at-1,at<center);
    let displacement=table[folded.y*v(6u)+folded.x];
    let direction=displacement%32;let width=v(5u);let base=v(7u);
    let wave=table[base+((displacement/32+v(8u))&(width-1))];
    let fd=(wave*(v(9u)*256))>>10u;
    let dx=((table[base+width+direction]*fd)>>11u)>>11u;
    let dy=((table[base+width+32+direction]*fd)>>11u)>>11u;
    var q=at+select(-vec2<i32>(dx,dy),vec2<i32>(dx,dy),at<center);
    q=abs(q);
    q=select(q,vec2<i32>(p.size.xy)*2-1-q,q>=vec2<i32>(p.size.xy));
    return color(vec4<i32>(pair(q,false).rgb,0));
}
