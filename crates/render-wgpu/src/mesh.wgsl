struct Parameters { color: vec4<f32>, options: vec4<f32>, size: vec4<f32> }
@group(0) @binding(0) var<uniform> parameters: Parameters;
@group(0) @binding(1) var image: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;
@group(0) @binding(3) var mask: texture_2d<f32>;
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}
@vertex fn vertex(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(position.x, -position.y, 0.0, 1.0);
    out.uv = uv;
    return out;
}
@fragment fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var uv = in.uv;
    if any(parameters.size.xy != parameters.size.zw) {
        uv = clamp(uv * parameters.size.xy, vec2<f32>(0.5), parameters.size.xy-vec2<f32>(0.5)) / parameters.size.zw;
    }
    var color = textureSample(image, linear_sampler, uv);
    if parameters.options.y != 0.0 {
        color = vec4<f32>(parameters.color.rgb, color.a * parameters.color.a);
    } else {
        color *= parameters.color;
    }
    color.a = clamp(color.a * parameters.options.x, 0.0, 1.0);
    if parameters.options.w != 0.0 {
        color.a = floor(color.a * 255.0 * (255.0 / 256.0)) / 255.0;
    }
    if parameters.options.z != 0.0 && textureLoad(mask, vec2<i32>(in.position.xy), 0).a < (128.0 / 255.0) {
        discard;
    }
    return color;
}
