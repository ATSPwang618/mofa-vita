@group(0) @binding(0) var source: texture_2d<f32>;
struct Parameters { offset: vec4<i32>, extent: vec4<i32>, reserved: vec4<i32> }
@group(0) @binding(1) var<uniform> parameters: Parameters;
@vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    return vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let offset = parameters.offset;
    var at = vec2<i32>(position.xy) + offset.xy;
    if parameters.extent.z != 0 {
        let extent = parameters.extent.xy;
        at = ((at % extent) + extent) % extent + offset.zw;
    }
    return textureLoad(source, at, 0);
}
