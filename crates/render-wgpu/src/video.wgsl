@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> offset: vec4<u32>;
@vertex fn vertex(@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32>{
    return vec4<f32>(f32((index<<1u)&2u)*2.0-1.0,f32(index&2u)*2.0-1.0,0.0,1.0);
}
@fragment fn fragment(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32>{
    let at=vec2<i32>(position.xy);
    let rgb=textureLoad(source,at,0).rgb;
    let alpha=textureLoad(source,at+vec2<i32>(i32(offset.x),0),0).b;
    return vec4<f32>(rgb,alpha);
}
