struct Parameters {
    offsets: vec4<i32>, // source offset xy; copied destination origin zw
    operation: vec4<i32>, // legacy mode, draw face, opacity, flags
    color: vec4<i32>,
    basis_x: vec4<f32>,
    basis_y: vec4<f32>,
    region: vec4<i32>,
    sample_bounds: vec4<i32>,
}
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var destination: texture_2d<f32>;
@group(0) @binding(2) var lookup: texture_2d<f32>;
@group(0) @binding(3) var<uniform> p: Parameters;
@group(0) @binding(4) var<storage, read> coefficients: array<vec4<f32>>;

fn bytes(v: vec4<f32>) -> vec4<i32> { return vec4<i32>(round(v * 255.0)); }
fn lerp256(d: vec3<i32>, s: vec3<i32>, a: i32) -> vec3<i32> {
    return d + (((s - d) * a) >> vec3<u32>(8u));
}
fn mul256(v: vec3<i32>, a: i32) -> vec3<i32> { return (v * a) >> vec3<u32>(8u); }
fn over_alpha(da: i32, sa: i32) -> i32 {
    let a = da + sa - ((da * sa) >> 8u);
    return a - (a >> 8u);
}
fn premul_over(d: vec4<i32>, s: vec4<i32>) -> vec4<i32> {
    return vec4<i32>(min(vec3<i32>(255), s.rgb + mul256(d.rgb, 255 - s.a)), over_alpha(d.a, s.a));
}
fn straight_over(d: vec4<i32>, s: vec4<i32>, color_fill: bool) -> vec4<i32> {
    let ratio = bytes(textureLoad(lookup, vec2<i32>(d.a, s.a), 0)).a;
    var a = 255 - ((255 - d.a) * (255 - s.a)) / 255;
    if color_fill { a = 255 - (((255 - d.a) * (255 - s.a)) >> 8u); }
    return vec4<i32>(lerp256(d.rgb, s.rgb, ratio), a);
}
fn table_rgb(d: vec3<i32>, s: vec3<i32>, channel: u32) -> vec3<i32> {
    return vec3<i32>(bytes(textureLoad(lookup, vec2<i32>(d.r, s.r), 0))[channel],
        bytes(textureLoad(lookup, vec2<i32>(d.g, s.g), 0))[channel],
        bytes(textureLoad(lookup, vec2<i32>(d.b, s.b), 0))[channel]);
}
fn overlay(d: vec3<i32>, s: vec3<i32>) -> vec3<i32> {
    let product = (d * (s | vec3<i32>(1))) >> vec3<u32>(7u);
    return select(2 * ((s & vec3<i32>(254)) + d) - vec3<i32>(255) - product,
        product, d < vec3<i32>(128));
}
fn solid(d: vec4<i32>, s: vec4<i32>, face: i32, opacity: i32) -> vec4<i32> {
    if opacity == 0 { return d; }
    if face == 0 && opacity < 0 {
        return vec4<i32>(d.rgb, (d.a * (255 + opacity)) >> 8u);
    }
    let a = max(opacity, 0);
    if a == 255 { return vec4<i32>(s.rgb, select(255, d.a, face == 1)); }
    if face == 1 {
        return vec4<i32>((d.rgb * (255 - a) + s.rgb * a) >> vec3<u32>(8u), d.a);
    }
    if face == 0 { return straight_over(d, vec4<i32>(s.rgb, a), true); }
    return premul_over(d, vec4<i32>(mul256(s.rgb, a), a));
}
fn blend(d: vec4<i32>, s: vec4<i32>, mode: i32, face: i32, opacity: i32, hold: bool) -> vec4<i32> {
    if opacity == 0 { return d; }
    var a = s.a;
    if opacity != 255 { a = (a * opacity) >> 8u; }
    if mode == 1 { // omOpaque / bmCopy, bmCopyOnAlpha, bmCopyOnAddAlpha
        if opacity == 255 {
            if face == 1 { return vec4<i32>(s.rgb, select(s.a, d.a, hold)); }
            return vec4<i32>(s.rgb, 255);
        }
        if face == 0 { return straight_over(d, vec4<i32>(s.rgb, opacity), false); }
        if face == 4 { return premul_over(d, vec4<i32>(s.rgb, opacity)); }
        // TVPConstAlphaBlend's MMX/SSE path interpolates all four bytes.
        let alpha = d.a + (((s.a - d.a) * opacity) >> 8u);
        return vec4<i32>(lerp256(d.rgb, s.rgb, opacity), select(alpha, d.a, hold));
    }
    if mode == 2 { // omAlpha
        if face == 0 { return straight_over(d, vec4<i32>(s.rgb, a), false); }
        if face == 4 { return premul_over(d, vec4<i32>(mul256(s.rgb, a), a)); }
        // dfOpaque ignores destination alpha when computing RGB, but does not
        // erase it. AnimationLayer may switch back to ltAlpha after this draw.
        // Match TVPAlphaBlend's opaque shortcut and four-byte MMX/SSE blend.
        if s.a == 255 && opacity == 255 && !hold { return s; }
        let alpha = d.a + (((s.a - d.a) * a) >> 8u);
        return vec4<i32>(lerp256(d.rgb, s.rgb, a), select(alpha, d.a, hold));
    }
    if mode == 12 { // The stock bmAddAlphaOnAlpha path performs no pixel work.
        if face == 0 { return d; }
        var input = s;
        if opacity != 255 { input = (s * opacity) >> vec4<u32>(8u); }
        var out = premul_over(d, input);
        if face == 1 { out.a = select(input.a, d.a, hold); }
        return out;
    }
    if mode == 16 {
        // Kirikiri2 tvpps_asm.nas MulBlend: multiply/interpolate RGBA with
        // seven-bit source opacity. The generic C fallback discards alpha;
        // games tinting transparent text rely on the commonly used MMX path.
        var weight = s.a >> 1u;
        if opacity != 255 { weight = (weight * opacity) >> 8u; }
        let mixed = (d * s) >> vec4<u32>(8u);
        let result = d + (((mixed - d) * weight) >> vec4<u32>(7u));
        return vec4<i32>(result.rgb, select(result.a, d.a, hold));
    }
    var rgb = vec3<i32>(0);
    var alpha = 0;
    if mode >= 13 { // PS modes use source alpha even on dfOpaque/dfMask.
        var mixed = s.rgb;
        switch mode {
            case 14: { mixed = min(d.rgb + s.rgb, vec3<i32>(255)); }
            case 15: { mixed = max(d.rgb + s.rgb - vec3<i32>(255), vec3<i32>(0)); }
            case 17: { return vec4<i32>((d.rgb + mul256(s.rgb - ((d.rgb * s.rgb) >> vec3<u32>(8u)), a)) & vec3<i32>(255), select(0, d.a, hold)); }
            case 18: { mixed = overlay(d.rgb, s.rgb); }
            case 19: { mixed = overlay(s.rgb, d.rgb); }
            case 20: { mixed = table_rgb(d.rgb, s.rgb, 0u); }
            case 21: { mixed = table_rgb(d.rgb, s.rgb, 1u); }
            case 22: { return vec4<i32>(table_rgb(d.rgb, mul256(s.rgb, a), 1u), select(0, d.a, hold)); }
            case 23: { mixed = table_rgb(d.rgb, s.rgb, 2u); }
            case 24: { mixed = max(d.rgb, s.rgb); }
            case 25: { mixed = min(d.rgb, s.rgb); }
            case 26: { mixed = abs(d.rgb - s.rgb); }
            case 27: { return vec4<i32>(abs(d.rgb - mul256(s.rgb, a)), d.a); }
            case 28: { return vec4<i32>((d.rgb + mul256(s.rgb - ((d.rgb * s.rgb) >> vec3<u32>(7u)), a)) & vec3<i32>(255), select(0, d.a, hold)); }
            default: {}
        }
        rgb = lerp256(d.rgb, mixed, a);
    } else {
        var input = s.rgb;
        if opacity != 255 {
            if mode == 4 || mode == 5 { input = vec3<i32>(255) - mul256(vec3<i32>(255) - input, opacity); }
            else if mode == 3 || mode == 8 || mode == 11 { input = mul256(input, opacity); }
        }
        switch mode {
            case 3: { rgb = min(d.rgb + input, vec3<i32>(255)); alpha = select(d.a, min(d.a + s.a, 255), opacity == 255); }
            case 4: { rgb = max(d.rgb + input - vec3<i32>(255), vec3<i32>(0)); alpha = select(d.a, max(d.a + s.a - 255, 0), opacity == 255); }
            case 5: { rgb = (d.rgb * input) >> vec3<u32>(8u); }
            case 8: {
                let divisor = max(vec3<i32>(255) - input, vec3<i32>(1));
                // TVPRecipTable256[0] and [1] are both 65536.
                rgb = min((d.rgb * (vec3<i32>(65536) / divisor)) >> vec3<u32>(8u), vec3<i32>(255));
            }
            case 9: { rgb = min(d.rgb, input); if opacity == 255 { alpha = min(d.a, s.a); } else { rgb = lerp256(d.rgb, rgb, opacity); } }
            case 10: { rgb = max(d.rgb, input); if opacity == 255 { alpha = max(d.a, s.a); } else { rgb = lerp256(d.rgb, rgb, opacity); } }
            case 11: { rgb = vec3<i32>(255) - (((vec3<i32>(255) - d.rgb) * (vec3<i32>(255) - input)) >> vec3<u32>(8u)); alpha = 255; }
            default: {}
        }
    }
    return vec4<i32>(rgb & vec3<i32>(255), select(alpha, d.a, hold));
}
@vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    return vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let point = vec2<i32>(position.xy);
    let op = p.operation;
    if (op.w & 128) != 0 {
        var mask = bytes(textureLoad(source, point + p.offsets.xy, 0)).r;
        let d = bytes(textureLoad(destination, point - p.offsets.zw, 0));
        let gray65 = (op.w & 256) != 0;
        let shift = select(8u, 6u, gray65);
        if op.z < 0 {
            let opacity = -op.z;
            var a: i32;
            if opacity == 255 { a = (d.a * (select(255,64,gray65) - mask)) >> shift; }
            else {
                let adjusted = opacity + select(0,1,opacity > 127);
                a = (d.a * (select(65535,16384,gray65) - mask*adjusted)) >> (shift+8u);
            }
            return vec4<f32>(vec4<i32>(d.rgb,a)) / 255.0;
        }
        if op.z != 255 { mask = (mask * op.z) >> 8u; }
        var result: vec4<i32>;
        if op.y == 1 {
            result = vec4<i32>(d.rgb + (((p.color.rgb-d.rgb)*mask) >> vec3<u32>(shift)), select(0,d.a,(op.w&1)!=0));
        } else if op.y == 0 {
            let alpha = min(select(mask,mask*4,gray65),255);
            let ratio = bytes(textureLoad(lookup,vec2<i32>(d.a,select(mask,256+mask,gray65)),0)).a;
            result = vec4<i32>(lerp256(d.rgb,p.color.rgb,ratio),255-(255-d.a)*(255-alpha)/255);
        } else {
            var alpha = mask << (8u-shift); alpha -= alpha >> 8u;
            result = premul_over(d,vec4<i32>((p.color.rgb*mask)>>vec3<u32>(shift),alpha));
        }
        return vec4<f32>(result) / 255.0;
    }
    var s = p.color;
    if (op.w & 96) != 0 {
        let vertical = (op.w & 32) != 0;
        let index = select(point.x - p.sample_bounds.x, point.y, vertical);
        let row = coefficients[u32(index)];
        var sum = vec4<f32>(0.0);
        for (var k = 0; k < i32(row.z); k++) {
            let offset = u32(row.y) + u32(k);
            let weight = coefficients[offset / 4u][offset % 4u];
            var coordinate = vec2<i32>(i32(row.x) + k + p.offsets.x, point.y + p.offsets.y);
            if vertical { coordinate = vec2<i32>(point.x + p.offsets.x, i32(row.x) + k); }
            sum += vec4<f32>(bytes(textureLoad(source, coordinate, 0))) * weight;
        }
        // The generic resampler quantizes each axis to bytes before the next
        // axis or the blend stage; filtering does not premultiply alpha.
        s = vec4<i32>(clamp(sum, vec4<f32>(0.0), vec4<f32>(255.0)));
    } else if (op.w & 4) != 0 {
        // Deferred regions keep the original global pixel centers and inverse.
        // Rebasing the inverse translation would change f32 rounding at edges.
        let xy = vec3<f32>(vec2<f32>(point) + vec2<f32>(p.basis_x.w, p.basis_y.w), 1.0);
        let q = vec2<f32>(dot(p.basis_x.xyz, xy), dot(p.basis_y.xyz, xy));
        let lower = vec2<f32>(p.region.xy) - vec2<f32>(0.5);
        let upper = vec2<f32>(p.region.zw) - vec2<f32>(0.5);
        if any(q < lower) || any(q >= upper) {
            if (op.w & 16) == 0 { discard; }
        } else if (op.w & 8) != 0 {
            let base = vec2<i32>(floor(q));
            let fractional = vec2<i32>(floor(fract(q) * 256.0));
            var ratio = fractional + (fractional >> vec2<u32>(7u));
            let lo = p.sample_bounds.xy;
            let hi = p.sample_bounds.zw - vec2<i32>(1);
            // Native horizontal interior loops use an unadjusted x ratio;
            // edge pixels and general affine loops adjust both components.
            if p.basis_y.x == 0.0 && all(base >= lo) && all(base + vec2<i32>(1) <= hi) {
                ratio.x = fractional.x;
            }
            let a = bytes(textureLoad(source, clamp(base, lo, hi) - p.offsets.xy, 0));
            let b = bytes(textureLoad(source, clamp(base + vec2<i32>(1,0), lo, hi) - p.offsets.xy, 0));
            let c = bytes(textureLoad(source, clamp(base + vec2<i32>(0,1), lo, hi) - p.offsets.xy, 0));
            let d = bytes(textureLoad(source, clamp(base + vec2<i32>(1,1), lo, hi) - p.offsets.xy, 0));
            let top = a + (((b - a) * ratio.x) >> vec4<u32>(8u));
            let bottom = c + (((d - c) * ratio.x) >> vec4<u32>(8u));
            s = top + (((bottom - top) * ratio.y) >> vec4<u32>(8u));
        } else {
            s = bytes(textureLoad(source, vec2<i32>(floor(q + vec2<f32>(0.5))) - p.offsets.xy, 0));
        }
    } else if (op.w & (2 | 512)) == 0 {
        let src = point + p.offsets.xy;
        var size = vec2<i32>(textureDimensions(source));
        if all(p.sample_bounds.zw > vec2<i32>(0)) { size = p.sample_bounds.zw; }
        if any(src < vec2<i32>(0)) || any(src >= size) { discard; }
        s = bytes(textureLoad(source, src, 0));
    }
    // Raw copies and fully opaque draws don't read the destination snapshot.
    if op.x < 0 { return vec4<f32>(s) / 255.0; }
    if op.x == 1 && op.z == 255 {
        // RGB-only writes preserve destination alpha without a backdrop copy.
        return vec4<f32>(vec4<i32>(s.rgb, select(255, s.a, op.y == 1))) / 255.0;
    }
    let d = bytes(textureLoad(destination, point - p.offsets.zw, 0));
    var result: vec4<i32>;
    if (op.w & 2) != 0 { result = solid(d, s, op.y, op.z); }
    else { result = blend(d, s, op.x, op.y, op.z, (op.w & 1) != 0); }
    return vec4<f32>(result) / 255.0;
}
