// Integer sampling conventions recovered from filter.dll; no alpha compositing.
struct Data { words: array<u32> }
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var previous: texture_2d<f32>;
@group(0) @binding(2) var<storage,read> data: Data;
@vertex fn vertex(@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32>{
    return vec4<f32>(f32((index<<1u)&2u)*2.0-1.0,f32(index&2u)*2.0-1.0,0.0,1.0);
}
fn pixel(at:vec2<i32>)->vec4<u32>{return vec4<u32>(round(textureLoad(source,at,0)*255.0));}
fn valid(at:vec2<i32>,next:bool)->bool {
    let size=vec2<i32>(i32(data.words[1]),i32(data.words[2]));
    return all(at>=vec2<i32>(0)) && all(at+vec2<i32>(select(0,1,next))<size);
}
fn weighted(at:vec2<i32>,weights:vec4<u32>)->vec4<u32>{
    return (pixel(at)*weights.x+pixel(at+vec2<i32>(1,0))*weights.y+
        pixel(at+vec2<i32>(0,1))*weights.z+pixel(at+vec2<i32>(1,1))*weights.w)>>vec4<u32>(8u);
}
@fragment fn fragment(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32>{
    let at=vec2<i32>(pos.xy);var out=vec4<u32>(0u);
    if data.words[0]==0u {
        let center=vec2<i32>(i32(data.words[3]/2u),i32(data.words[4]/2u));
        let offset=at-center;let distance=length(vec2<f32>(offset));let radius=bitcast<f32>(data.words[5]);
        if distance>=radius {return vec4<f32>(0.0,0.0,0.0,1.0);}
        let index=u32(distance/radius*8191.0);
        let radial=i32(f32(data.words[16u+min(index,8191u)])*radius);
        if radial==0 {if !valid(at,false){discard;}out=pixel(at);}
        else {
            var scale=i32(f32(radial)/distance);
            for(var p=1u;p<data.words[6];p++){scale=((scale>>1)*(scale>>1))>>14;}
            let fixed=offset*scale;let sample=(fixed>>vec2<u32>(16u))+center;
            if !valid(sample,true){discard;}
            let frac=vec2<u32>(fixed>>vec2<u32>(8u))&vec2<u32>(255u);
            // Negative quadrants use complemented fractions in the original kernel.
            let f=select(frac,255u-frac,offset<vec2<i32>(0));
            let both=(f.x*f.y)>>8u;let onlyx=f.x-both;let onlyy=f.y-both;
            var weights=vec4<u32>(256u-both-onlyx-onlyy,onlyx,onlyy,both);
            if offset.x<0 {weights=weights.yxwz;}
            if offset.y<0 {weights=weights.zwxy;}
            out=weighted(sample,weights);
        }
    } else if data.words[0]==1u {
        let size=vec2<i32>(i32(data.words[1]),i32(data.words[2]));
        if any(at>=size-vec2<i32>(1)){discard;}
        let center=size/2;let offset=at-center;
        let limit=max(center.x,center.y);let radius2=limit*limit;
        let shorter=min(size.x,size.y);
        let distance2=offset.x*offset.x+i32(f32(offset.y*offset.y)*f32(size.x*size.x)/f32(shorter*shorter));
        if radius2==0 || distance2>=radius2 {discard;}
        let strength=1.0-f32(distance2)/f32(radius2);
        let theta=strength*strength*strength*bitcast<f32>(data.words[5]);
        let c=cos(theta);let s=sin(theta);let d=vec2<f32>(offset);
        let fixed=vec2<i32>((vec2<f32>(d.x*c+d.y*s,d.y*c-d.x*s)+vec2<f32>(center))*32767.0);
        let sample=fixed>>vec2<u32>(15u);if !valid(sample,true){discard;}
        let f=vec2<u32>(fixed)&vec2<u32>(32767u);let inv=32767u-f;
        out=weighted(sample,vec4<u32>(inv.x*inv.y,f.x*inv.y,inv.x*f.y,f.x*f.y)>>vec4<u32>(22u));
    } else {
        let origin=vec2<i32>(i32(data.words[9]),i32(data.words[10]));
        let size=vec2<i32>(i32(data.words[11]),i32(data.words[12]));
        let delta=at-origin;if any(delta<vec2<i32>(0))||any(delta>=size){discard;}
        let step=(vec2<i32>(i32(data.words[7]),i32(data.words[8]))<<vec2<u32>(8u))/size;
        let fixed=(vec2<i32>(i32(data.words[5]),i32(data.words[6]))<<vec2<u32>(8u))+delta*step;
        let sample=fixed>>vec2<u32>(8u);let linear=all(step<=vec2<i32>(256));
        if !valid(sample,linear){discard;}
        let opacity=bitcast<i32>(data.words[13]);
        if linear {
            let f=vec2<u32>(fixed)&vec2<u32>(255u);
            let alpha=u32(select(256,opacity,opacity<255));
            let upper=(255u-f.y)*alpha;let lower=f.y*alpha;
            let a=(255u-f.x)*upper;let b=(255u-f.x)*lower;
            let weights=vec4<u32>(a>>16u,(upper-(a>>8u))>>8u,b>>16u,(lower-(b>>8u))>>8u);
            // Include the destination before the final shift, as the packed kernel does.
            var sum=pixel(sample)*weights.x+pixel(sample+vec2<i32>(1,0))*weights.y+pixel(sample+vec2<i32>(0,1))*weights.z+pixel(sample+vec2<i32>(1,1))*weights.w;
            if opacity<255 {sum+=vec4<u32>(round(textureLoad(previous,at-vec2<i32>(i32(data.words[14]),i32(data.words[15])),0)*255.0))*u32(255-opacity);}
            out=(sum>>vec4<u32>(8u))&vec4<u32>(255u);
        } else {
            out=pixel(sample);
            if opacity<255 {
                let old=vec4<u32>(round(textureLoad(previous,at-vec2<i32>(i32(data.words[14]),i32(data.words[15])),0)*255.0));
                out=((out*u32(opacity)+old*u32(255-opacity))>>vec4<u32>(8u))&vec4<u32>(255u);
            }
        }
    }
    return vec4<f32>(out)/255.0;
}
