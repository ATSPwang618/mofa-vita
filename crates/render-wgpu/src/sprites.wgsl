// Appended to blend.wgsl: shares the engine's exact integer blend functions.
@group(0) @binding(5) var<storage,read> sprite_words:array<u32>;
fn word4(i:u32)->vec4<i32>{return vec4<i32>(i32(sprite_words[i]),i32(sprite_words[i+1u]),i32(sprite_words[i+2u]),i32(sprite_words[i+3u]));}
@fragment fn sprite_fragment(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32>{
    let point=vec2<i32>(position.xy);
    let tile=vec2<u32>(point)/32u-vec2<u32>(sprite_words[1],sprite_words[2]);
    let range=16u+2u*(tile.y*sprite_words[0]+tile.x);
    let begin=sprite_words[range];let end=sprite_words[range+1u];
    var pixel=bytes(textureLoad(destination,point-vec2<i32>(i32(sprite_words[8]),i32(sprite_words[9])),0));
    let mode=i32(sprite_words[5]);let face=i32(sprite_words[6]);let hold=sprite_words[7]!=0u;
    for(var i=begin;i<end;i++){
        let r=sprite_words[3]+16u*sprite_words[sprite_words[4]+i];
        let bounds=word4(r+1u);
        if any(point<bounds.xy)||any(point>=bounds.zw){continue;}
        if sprite_words[r]==0u{
            if face==2{pixel.a=0;}else{pixel=vec4<i32>(0,0,0,select(0,pixel.a,face==1&&hold));}continue;
        }
        let xy=vec3<f32>(vec2<f32>(point),1.);
        let a=vec3<f32>(bitcast<f32>(sprite_words[r+9u]),bitcast<f32>(sprite_words[r+10u]),bitcast<f32>(sprite_words[r+11u]));
        let b=vec3<f32>(bitcast<f32>(sprite_words[r+12u]),bitcast<f32>(sprite_words[r+13u]),bitcast<f32>(sprite_words[r+14u]));
        let q=vec2<f32>(dot(a,xy),dot(b,xy));let region=word4(r+5u);
        if any(q<vec2<f32>(region.xy)-0.5)||any(q>=vec2<f32>(region.zw)-0.5){continue;}
        let sample=bytes(textureLoad(source,vec2<i32>(floor(q+0.5)),0));
        pixel=clamp(blend(pixel,sample,mode,face,i32(sprite_words[r+15u]),hold),vec4<i32>(0),vec4<i32>(255));
    }
    return vec4<f32>(pixel)/255.;
}
