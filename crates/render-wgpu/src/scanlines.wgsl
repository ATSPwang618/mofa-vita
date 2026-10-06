struct Data { words: array<i32> }
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<storage,read> data: Data;
@group(0) @binding(2) var previous: texture_2d<f32>;
@vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return vec4<f32>(f32((index << 1u) & 2u)*2.0-1.0, f32(index & 2u)*2.0-1.0, 0.0, 1.0);
}
fn sample_pixel(index: i32) -> vec4<u32> {
    let width = data.words[3];
    let pos = vec2<i32>(index%width,index/width)-vec2<i32>(data.words[6],data.words[7]);
    return vec4<u32>(round(textureLoad(source,pos,0)*255.0));
}
@fragment fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let y = i32(position.y)-data.words[0];
    if y < 0 || y >= data.words[1] { discard; }
    let row = 8+8*y; let x = i32(position.x)-data.words[row];
    if x < 0 || x >= data.words[row+1] { discard; }
    let index = data.words[row+3]*data.words[3]+data.words[row+2]+x;
    let length = data.words[3]*data.words[4];
    if index < 0 || index >= length { discard; }
    var color = sample_pixel(index);
    if data.words[2]>=2 {
        let span=data.words[row+1];let odd=data.words[row+4]!=0;
        if x==0 {return vec4<f32>(color)/255.0;}
        let old=vec4<u32>(round(textureLoad(previous,vec2<i32>(i32(position.x),i32(position.y)-data.words[5]),0)*255.0));
        if odd {
            // Original Duff-loop leaves pixel one and the final three pixels intact.
            let last=span-4;
            if span<6 || x==1 || x>last {discard;}
            if x==last {color=(old+color)>>vec4<u32>(1u);}
            else {
                let left=sample_pixel(index-1);
                if data.words[2]==3 {color=(old>>vec4<u32>(1u))+(left>>vec4<u32>(2u))+(color>>vec4<u32>(2u));}
                else {color=(left+color)>>vec4<u32>(1u);}
            }
        } else {
            if x>=span-1 {discard;}
            if data.words[2]==3 {color=(old+color)>>vec4<u32>(1u);}
        }
        return vec4<f32>(color)/255.0;
    }
    if data.words[2] != 0 {
        let fraction = u32(data.words[row+4]);
        color = (color*(256u-fraction)+sample_pixel(min(index+1,length-1))*fraction)>>vec4<u32>(8u);
    }
    return vec4<f32>(color)/255.0;
}
