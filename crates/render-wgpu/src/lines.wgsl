@group(0) @binding(0) var background: texture_2d<f32>;
@group(0) @binding(1) var<storage, read> data: array<u32>;

@vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let points = array(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(points[index], 0.0, 1.0);
}
fn packed(rgba: vec4<u32>) -> u32 {
    return (rgba.a << 24u) | (rgba.r << 16u) | (rgba.g << 8u) | rgba.b;
}
fn unpacked(color: u32) -> vec4<u32> {
    return vec4((color >> 16u) & 255u, (color >> 8u) & 255u, color & 255u, color >> 24u);
}
// Keep the DLL's unsigned packed-channel interpolation, including its truncation.
fn mix_pixel(old: u32, color: u32, weight: u32) -> u32 {
    let src = unpacked(color);
    let value = src + (((unpacked(old) - src) * weight) >> vec4(16u));
    return packed(value);
}
@fragment fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let point = vec2<i32>(position.xy);
    let origin = vec2<i32>(i32(data[0]), i32(data[1]));
    let local = vec2<u32>(point - origin);
    let tile = (local.y / 32u) * data[2] + local.x / 32u;
    let range = 8u + tile * 2u;
    let read_origin = vec2<i32>(i32(data[6]), i32(data[7]));
    var color = packed(vec4<u32>(round(textureLoad(background, point - read_origin, 0) * 255.0)));
    for (var at = data[range]; at < data[range] + data[range + 1u]; at++) {
        let line = data[4] + data[at] * 8u;
        let start = vec2<i32>(i32(data[line]), i32(data[line + 1u]));
        let end = vec2<i32>(i32(data[line + 2u]), i32(data[line + 3u]));
        let delta = end - start;
        let horizontal = abs(delta.x) >= abs(delta.y);
        let major = select(delta.y, delta.x, horizontal);
        let minor = select(delta.x, delta.y, horizontal);
        let distance = select(abs(major), max(abs(major), 1), data[line + 5u] != 0u);
        let index = (select(point.y, point.x, horizontal) - select(start.y, start.x, horizontal)) * select(-1, 1, major >= 0);
        if index < 0 || index > distance { continue; }
        let pixel_minor = select(point.x, point.y, horizontal);
        let start_minor = select(start.x, start.y, horizontal);
        var ink = data[line + 4u];
        if data[line + 5u] == 0u {
            if distance == 0 { continue; }
            // Symmetric Bresenham: ties are approached from the nearer endpoint.
            let from_start = index <= (distance + 1) / 2;
            let step = select(distance - index, index, from_start);
            let shift = (step * abs(minor) * 2 + distance) / max(distance * 2, 1);
            let direction = select(-1, 1, minor >= 0);
            let y = select(start_minor + minor - shift * direction, start_minor + shift * direction, from_start);
            if pixel_minor == y { color = ink; }
            continue;
        }
        let step = (minor * 65536) / max(distance, 1);
        let fixed = start_minor * 65536 + index * step;
        let low = fixed >> 16;
        let fraction = u32(fixed) & 65535u;
        var weight: u32;
        if pixel_minor == low { weight = fraction; }
        else if pixel_minor == low + 1 { weight = 65535u - fraction; }
        else { continue; }
        let fade = data[line + 6u];
        if fade > 0u {
            let count = min(u32(distance), fade);
            let alpha = ((ink & 0xff000000u) / fade) * min(u32(index + 1), count);
            ink = (ink & 0xffffffu) | (alpha & 0xff000000u);
        }
        color = mix_pixel(color, ink, weight);
    }
    return vec4<f32>(unpacked(color)) / 255.0;
}
