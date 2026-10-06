// Altered CxImage 7.0.2 / layerExImage kernels. See krkr-plugins/src/image/LICENSE.txt.
struct Parameters { mode: vec4<u32>, area: vec4<u32>, table: array<vec4<u32>, 256> }
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> p: Parameters;
fn word(i: u32) -> u32 { return p.table[i >> 2u][i & 3u]; }
fn rgba(at: vec2<i32>) -> vec4<u32> { return vec4<u32>(round(textureLoad(source, at, 0) * 255.0)); }
fn unpack(c: u32) -> vec3<u32> { return vec3<u32>((c >> 16u) & 255u, (c >> 8u) & 255u, c & 255u); }
fn hue_rgb(n1: f32, n2: f32, input: f32) -> i32 {
    var h = input;
    if h < 0.0 { h += 1.0; } else if h > 1.0 { h -= 1.0; }
    var c = n1;
    if h < 1.0/6.0 { c = n1 + (n2 - n1)*h*6.0; }
    else if h < 0.5 { c = n2; }
    else if h < 2.0/3.0 { c = n1 + (n2 - n1)*(2.0/3.0 - h)*6.0; }
    return i32(c * 255.0);
}
fn modulate(input: vec3<u32>) -> vec3<u32> {
    let rgb = vec3<f32>(input) / 255.0;
    let hi = max(max(rgb.r, rgb.g), rgb.b); let lo = min(min(rgb.r, rgb.g), rgb.b);
    let delta = hi - lo; let add = hi + lo;
    var l = add / 2.0; var s = 0.0; var h = 0.0;
    if delta != 0.0 {
        s = delta / select(2.0 - add, add, l < 0.5);
        if rgb.r == hi { h = (rgb.g - rgb.b) / delta; }
        else if rgb.g == hi { h = 2.0 + (rgb.b - rgb.r) / delta; }
        else { h = 4.0 + (rgb.r - rgb.g) / delta; }
        h /= 6.0;
    }
    h += bitcast<f32>(p.mode.y);
    // Reference wraps into inclusive [0,1]; keep positive exact integer at 1.
    if h < 0.0 { h += ceil(-h); } else if h > 1.0 { h -= ceil(h - 1.0); }
    let saturation = bitcast<f32>(p.mode.z); let luminance = bitcast<f32>(p.mode.w);
    s += select(s, 1.0 - s, saturation > 0.0) * saturation;
    l += select(l, 1.0 - l, luminance > 0.0) * luminance;
    var result = vec3<i32>(i32(l * 255.0));
    if s != 0.0 {
        let m2 = select(l + s - l*s, l*(1.0+s), l <= 0.5); let m1 = 2.0*l - m2;
        result = vec3<i32>(hue_rgb(m1,m2,h+1.0/3.0), hue_rgb(m1,m2,h), hue_rgb(m1,m2,h-1.0/3.0));
    }
    return vec3<u32>(result) & vec3<u32>(255u);
}
fn advance(input: u32, count: u32) -> u32 {
    var state = input; var n = count; var a = 214013u; var c = 2531011u;
    while n != 0u {
        if (n & 1u) != 0u { state = state*a+c; }
        c *= a+1u; a *= a; n >>= 1u;
    }
    return state;
}
fn blur(at: vec2<i32>) -> vec4<u32> {
    let vertical = p.mode.x == 6u; let count = i32(p.mode.y); let middle = count/2;
    let extent = i32(select(p.area.z, p.area.w, vertical)); let row = select(at.x, at.y, vertical);
    let first = max(0, middle-row); let end = min(count, extent-row+middle);
    var sum = vec4<f32>(0.0); var scale = 0.0;
    // Retain the reference's distinct small-image branch (its numerator uses
    // the source index as weight index); do not substitute a box/GPU Gaussian.
    for (var j = first; j < end; j++) {
        let index = row+j-middle;
        var pos = at; if vertical { pos.y = index; } else { pos.x = index; }
        let weight = bitcast<f32>(word(u32(select(j, index, count > extent))));
        sum += vec4<f32>(rgba(pos)) * weight;
        scale += bitcast<f32>(word(u32(j)));
    }
    if row < middle || row >= extent-middle || count > extent { sum /= scale; }
    return vec4<u32>(sum + vec4<f32>(0.5)) & vec4<u32>(255u);
}
fn noise_advance(input: u32, count: u32) -> u32 {
    var state = input; var n = count; var a = select(0x7d2b89ddu,0x5d588b65u,word(5u)!=0u); var c = 1u;
    while n != 0u {
        if (n & 1u) != 0u { state = state*a+c; }
        c *= a+1u; a *= a; n >>= 1u;
    }
    return state;
}
fn smudge(at: vec2<i32>) -> vec4<u32> {
    let center = rgba(at); var sum = vec4<u32>(0u);
    let size = vec2<i32>(p.area.zw);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            if x == 0 && y == 0 { continue; }
            let pos = at + vec2<i32>(x,y);
            if any(pos < vec2<i32>(0)) || any(pos >= size) { sum += center; }
            else { sum += rgba(pos); }
        }
    }
    return sum >> vec4<u32>(3u);
}
fn dither(color: vec4<u32>, at: vec2<i32>) -> vec4<u32> {
    let c = (color.a << 24u) | (color.r << 16u) | (color.g << 8u) | color.b;
    let thresholds = array<u32,4>(0x010101u,0x040404u,0x030303u,0x020202u);
    let index = ((p.mode.y-u32(at.x)) & 1u) | (((p.mode.z-u32(at.y)) & 1u) << 1u);
    let v = (c & 0xfcfcfcu) + (((c & 0xffff0707u | 0xfffc0404u)-thresholds[index]) & 0x040404u);
    let overflow = v & 0x01010100u;
    return vec4<u32>(unpack((overflow-(overflow>>8u)) | (v & 0xfcfcfcu)),color.a);
}
fn noise_channel(bits: u32) -> u32 {
    return u32((bitcast<i32>(bits*p.mode.w) >> 16u) + bitcast<i32>(p.mode.z)) & 255u;
}
fn multiply_high(a:u32,b:u32)->u32 {
    let first=(a&65535u)*(b&65535u);
    let middle=(a>>16u)*(b&65535u)+(first>>16u);
    let cross=(a&65535u)*(b>>16u)+(middle&65535u);
    return (a>>16u)*(b>>16u)+(middle>>16u)+(cross>>16u);
}
fn random_fill(position: vec2<i32>, original: vec4<u32>) -> vec4<u32> {
    let at = vec2<u32>(position-vec2<i32>(vec2<u32>(word(2u),word(3u))));
    let width = word(4u); let mono = word(0u) != 0u; let hold = word(1u) != 0u;
    let full = p.mode.w == 255u;
    let legacy=word(5u)!=0u;
    var alpha=select(255u,original.a,hold);
    var steps = width; var index = at.x;
    if mono {
        if full { steps = width/3u+1u; index = at.x/3u; }
        else { steps = (width+1u)/2u; index = at.x/2u; }
    } else if full {
        if hold { steps += width/4u; index += at.x/4u; }
        if legacy && width>=4u {
            let tail=width%4u;
            if at.x>=width-tail {return original;}
            if at.x<tail {index=width-tail+at.x;}
        }
    } else { steps = (width/2u)*3u+(width%2u)*2u; index = (at.x/2u)*3u+(at.x%2u); }
    let state = noise_advance(p.mode.y, at.y*steps+index+1u);
    var rgb: vec3<u32>;
    if mono {
        var gray: u32;
        if full {
            var part = at.x%3u;
            // Native fallthrough emits the middle byte for a one-pixel tail.
            if legacy {
                if at.x>=width-width%3u {part=select(1u,2u,at.x==width-1u);}
                else {alpha=multiply_high(noise_advance(p.mode.y,at.y*steps+index),0x5d588b65u)>>24u;}
            } else if at.x == width-1u && width%3u == 1u { part = 1u; }
            gray = (state >> (24u-part*8u)) & 255u;
        } else {
            let high = at.x%2u != 0u || at.x == width-1u;
            gray = noise_channel((state >> select(0u,16u,high)) & 65535u);
        }
        rgb = vec3<u32>(gray);
    } else if full { rgb = unpack(state >> 8u); }
    else {
        let next = state*select(0x7d2b89ddu,0x5d588b65u,legacy)+1u;
        if at.x%2u == 0u {
            rgb = vec3<u32>(noise_channel(state&65535u),noise_channel(state>>16u),noise_channel(next&65535u));
        } else {
            rgb = vec3<u32>(noise_channel(state>>16u),noise_channel(next&65535u),noise_channel(next>>16u));
        }
    }
    return vec4<u32>(rgb,alpha);
}
@vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return vec4<f32>(f32((index << 1u) & 2u)*2.0-1.0, f32(index & 2u)*2.0-1.0, 0.0, 1.0);
}
@fragment fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let at = vec2<i32>(position.xy) - vec2<i32>(p.area.xy);
    if p.mode.x == 5u || p.mode.x == 6u { return vec4<f32>(blur(at))/255.0; }
    if p.mode.x == 7u && word(1u) == 0u && word(5u)==0u { return vec4<f32>(random_fill(vec2<i32>(position.xy),vec4<u32>(255u)))/255.0; }
    var color = rgba(at);
    if p.mode.x == 7u { return vec4<f32>(random_fill(vec2<i32>(position.xy),color))/255.0; }
    switch p.mode.x {
        case 8u: { color = smudge(at); }
        case 9u: { color ^= vec4<u32>(unpack(p.mode.y),p.mode.y>>24u); }
        case 10u: { color = dither(color,at); }
        case 0u: { color = vec4<u32>(word(color.r), word(color.g), word(color.b), color.a); }
        case 1u: {
            let lightness = (max(max(color.r,color.g),color.b)+min(min(color.r,color.g),color.b)+1u)/2u;
            let tinted = unpack(word(lightness)); let amount = p.mode.y;
            color = vec4<u32>((tinted*amount + color.rgb*(256u-amount))>>vec3<u32>(8u),color.a);
        }
        case 2u: { color = vec4<u32>(modulate(color.rgb),color.a); }
        case 3u: {
            var state = advance(p.mode.y, 3u*(u32(at.y)*p.area.z+u32(at.x))+1u);
            for(var channel=0; channel<3; channel++) {
                let rand = (state >> 16u) & 32767u;
                let offset = i32((f32(rand)/32767.0 - 0.5)*f32(bitcast<i32>(p.mode.z)));
                color[2-channel] = u32(clamp(i32(color[2-channel])+offset,0,255));
                state = state*214013u+2531011u;
            }
        }
        default: {
            let state = advance(p.mode.y,u32(at.y)*p.area.z+u32(at.x)+1u);
            let gray = (((state >> 16u)&32767u)/128u)&255u;
            color = vec4<u32>(vec3<u32>(gray),color.a);
        }
    }
    return vec4<f32>(color)/255.0;
}
