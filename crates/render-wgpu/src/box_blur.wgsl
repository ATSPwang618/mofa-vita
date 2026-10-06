struct Parameters {
    clip: vec4<u32>,
    image: vec4<u32>,
    options: vec4<u32>,
}
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> sums: array<vec4<u32>>;
@group(0) @binding(2) var<uniform> p: Parameters;
@group(0) @binding(3) var destination: texture_storage_2d<rgba8unorm, write>;

fn pixel(x: u32, y: u32) -> vec4<u32> {
    var value = vec4<u32>(round(textureLoad(source, vec2<i32>(i32(x), i32(y)), 0) * 255.0));
    if p.options.z != 0u {
        let alpha = value.a + (value.a >> 7u);
        value = vec4<u32>((value.rgb * alpha) >> vec3<u32>(8u), value.a);
    }
    return value;
}

@compute @workgroup_size(64)
fn horizontal(@builtin(global_invocation_id) id: vec3<u32>) {
    let row = id.x;
    if row >= p.options.y { return; }
    let y = p.options.x + row;
    let radius = p.image.z;
    var left = u32(max(i32(p.clip.x) - i32(radius), 0));
    var right = min(p.clip.x + radius + 1u, p.image.x);
    var sum = vec4<u32>(0u);
    for (var x = left; x < right; x++) { sum += pixel(x, y); }
    for (var column = 0u; column < p.clip.z; column++) {
        sums[row * p.clip.z + column] = sum;
        let next = p.clip.x + column + 1u;
        let next_left = u32(max(i32(next) - i32(radius), 0));
        let next_right = min(next + radius + 1u, p.image.x);
        if next_left > left { sum -= pixel(left, y); }
        if next_right > right { sum += pixel(right, y); }
        left = next_left;
        right = next_right;
    }
}

fn row_sum(column: u32, y: u32) -> vec4<u32> {
    return sums[(y - p.options.x) * p.clip.z + column];
}

@compute @workgroup_size(64)
fn vertical(@builtin(global_invocation_id) id: vec3<u32>) {
    let column = id.x;
    if column >= p.clip.z { return; }
    let x = p.clip.x + column;
    let radius = p.image.w;
    var top = u32(max(i32(p.clip.y) - i32(radius), 0));
    var bottom = min(p.clip.y + radius + 1u, p.image.y);
    let columns = min(x + p.image.z + 1u, p.image.x)
        - u32(max(i32(x) - i32(p.image.z), 0));
    var sum = vec4<u32>(0u);
    for (var y = top; y < bottom; y++) { sum += row_sum(column, y); }
    for (var row = 0u; row < p.clip.w; row++) {
        let count = columns * (bottom - top);
        var value = (sum + vec4<u32>(count / 2u)) / count;
        if p.options.w != 0u {
            value = ((sum + vec4<u32>(count / 2u)) * (65536u / count)) >> vec4<u32>(16u);
        }
        if p.options.z != 0u {
            value = vec4<u32>(select(vec3<u32>(0u), min(value.rgb * 255u / max(value.a, 1u), vec3<u32>(255u)), value.a != 0u), value.a);
        }
        textureStore(destination, vec2<i32>(i32(x), i32(p.clip.y + row)), vec4<f32>(value) / 255.0);
        if row + 1u == p.clip.w { break; }
        let next = p.clip.y + row + 1u;
        let next_top = u32(max(i32(next) - i32(radius), 0));
        let next_bottom = min(next + radius + 1u, p.image.y);
        if next_top > top { sum -= row_sum(column, top); }
        if next_bottom > bottom { sum += row_sum(column, bottom); }
        top = next_top;
        bottom = next_bottom;
    }
}
