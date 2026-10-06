struct Parameters {
    matrix: array<vec4f,3>,
    points: array<vec4f,4>,
    output_size: vec4f,
    source: vec4f,
    bounds: vec4f,
};
@group(0) @binding(0) var<uniform> p: Parameters;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var linear: sampler;

@vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
    let indices = array<u32,6>(0,1,2,1,2,3);
    let point = p.points[indices[index]].xy / p.output_size.xy;
    return vec4f(point.x*2.0-1.0, 1.0-point.y*2.0, 0.0, 1.0);
}
@fragment fn fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let v = vec3f(position.xy,1.0);
    let w = dot(p.matrix[2].xyz,v);
    if w == 0.0 { discard; }
    let point = vec2f(dot(p.matrix[0].xyz,v),dot(p.matrix[1].xyz,v))/w;
    // Snapshot origin is removed after projection; edge clamp is equivalent
    // to sampling the complete source because the snapshot includes all taps.
    var pixel = point-p.source.xy;
    if any(p.bounds.xy != p.source.zw) {
        pixel = clamp(pixel, vec2f(0.5), p.bounds.xy-vec2f(0.5));
    }
    return textureSampleLevel(source, linear, pixel/p.source.zw, 0.0);
}
