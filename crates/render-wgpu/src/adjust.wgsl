struct Parameters { mode: vec4<u32>, table: array<vec4<u32>, 256> }
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> parameters: Parameters;
fn unpack(color: u32) -> vec4<u32> { return vec4<u32>((color>>16u)&255u, (color>>8u)&255u, color&255u, color>>24u); }
fn hsv_rgb(hsv: vec3<f32>) -> vec3<u32> {
    let h = select(hsv.x, 0.0, hsv.x == 360.0);
    let s = hsv.y/100.0; let v = hsv.z/100.0;
    if s == 0.0 { return vec3<u32>(vec3<i32>(i32(v*255.0))) & vec3<u32>(255u); }
    // Use one quotient for both the sector and its fraction. GPU division can
    // round h just below 120/240/360 while h/60 rounds to the next integer;
    // deriving the sector separately from floor(h) selects a different color.
    let quotient = h/60.0;
    let sector = floor(quotient);
    let f = quotient - sector;
    let p = v*(1.0-s); let q = v*(1.0-f*s); let t = v*(1.0-(1.0-f)*s);
    var rgb = vec3<f32>(v,p,q);
    switch i32(sector)%6 {
        case 0: { rgb=vec3<f32>(v,t,p); } case 1: { rgb=vec3<f32>(q,v,p); }
        case 2: { rgb=vec3<f32>(p,v,t); } case 3: { rgb=vec3<f32>(p,q,v); }
        case 4: { rgb=vec3<f32>(t,p,v); } default: {}
    }
    return vec3<u32>(vec3<i32>(rgb*255.0)) & vec3<u32>(255u);
}
fn gradient(x: u32, length: u32) -> vec4<u32> {
    let a=unpack(parameters.table[0].x); let b=unpack(parameters.table[0].y);
    let denominator=max(1u,length-1u);
    return (a*(denominator-min(x,denominator))+b*min(x,denominator))/denominator;
}
@vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return vec4<f32>(f32((index << 1u) & 2u)*2.0-1.0, f32(index & 2u)*2.0-1.0, 0.0, 1.0);
}
@fragment fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    var at = vec2<i32>(position.xy);
    let mode = parameters.mode.x;
    if mode == 6u {
        if parameters.mode.y == 0u {
            var rgb=vec3<u32>();
            for(var channel=0u; channel<3u; channel++) {
                let axis=parameters.table[0][channel];
                if axis==1u { rgb[channel]=255u*u32(at.x)/max(1u,parameters.mode.z-1u); }
                else if axis==2u { rgb[channel]=255u*(parameters.mode.w-1u-u32(at.y))/max(1u,parameters.mode.w-1u); }
                else { rgb[channel]=parameters.table[1][channel]&255u; }
            }
            return vec4<f32>(vec3<f32>(rgb)/255.0,1.0);
        }
        var c = vec3<f32>();
        for(var channel=0u; channel<3u; channel++) {
            let axis=parameters.table[0][channel];
            let limit=select(255.0,select(100.0,360.0,channel==0u),parameters.mode.y!=0u);
            if axis == 1u { c[channel]=limit*f32(at.x)/f32(max(1u,parameters.mode.z-1u)); }
            else if axis == 2u { c[channel]=limit*f32(i32(parameters.mode.w)-1-at.y)/f32(max(1u,parameters.mode.w-1u)); }
            else { c[channel]=bitcast<f32>(parameters.table[1][channel]); }
        }
        var rgb=vec3<u32>(vec3<i32>(c)) & vec3<u32>(255u);
        if parameters.mode.y!=0u { rgb=hsv_rgb(c); }
        return vec4<f32>(vec3<f32>(rgb)/255.0,1.0);
    }
    if mode == 3u { at.x = i32(parameters.mode.z) - 1 - at.x; }
    if mode == 4u { at.y = i32(parameters.mode.w) - 1 - at.y; }
    if mode == 3u || mode == 4u { return textureLoad(source, at, 0); }
    if mode == 5u {
        let vertical=parameters.mode.y!=0u;
        let bounds=parameters.table[1];
        let length=select(bounds.z,bounds.w,vertical);
        let x=u32(select(at.x-bitcast<i32>(bounds.x),at.y-bitcast<i32>(bounds.y),vertical));
        var color=gradient(x,length);
        if parameters.mode.z!=0u {
            let value=textureLoad(source,at-vec2<i32>(parameters.table[2].xy),0);
            let old=vec4<u32>(round(value*255.0));
            if vertical {
                color=vec4<u32>((old.rgb*(255u-color.a)+color.rgb*color.a)>>vec3<u32>(8u),old.a);
            } else {
                // krkrz's selected SSE2 implementation interpolates all four
                // channels; the generic C implementation instead clears alpha.
                let clip=parameters.table[3];
                let first=(clip.x+3u)&~3u;
                let end=(clip.x+clip.z)&~3u;
                let group=u32(at.x)&~3u;
                var opaque=color.a==255u;
                if opaque && u32(at.x)>=first && u32(at.x)<end {
                    opaque=gradient(group-bounds.x,length).a==255u && gradient(group+3u-bounds.x,length).a==255u;
                }
                if !opaque {
                    let mixed=vec4<i32>(old)+((vec4<i32>(color)-vec4<i32>(old))*i32(color.a)>>vec4<u32>(8u));
                    color=vec4<u32>(mixed);
                }
            }
        }
        return vec4<f32>(color)/255.0;
    }
    let value = textureLoad(source, at - vec2<i32>(parameters.mode.yz), 0);
    var p = vec4<u32>(round(value * 255.0));
    if mode == 2u {
        let gray = (p.r * 54u + p.g * 183u + p.b * 19u) >> 8u;
        return vec4<f32>(vec3<f32>(f32(gray)), f32(p.a))/255.0;
    }
    if mode == 0u && p.a == 0u { return value; }
    for(var channel=0u; channel<3u; channel++) {
        let color = p[channel];
        if mode == 0u || p.a == 255u { p[channel] = parameters.table[color][channel]; }
        else {
            let adjusted = p.a + (p.a >> 7u);
            if color > p.a { p[channel] = (parameters.table[255u][channel]*adjusted >> 8u) + color - p.a; }
            else {
                let reciprocal = min(65535u, 65536u/max(1u,p.a));
                let straight = min(255u, reciprocal*color >> 8u);
                p[channel] = parameters.table[straight][channel]*adjusted >> 8u;
            }
        }
    }
    return vec4<f32>(p)/255.0;
}
