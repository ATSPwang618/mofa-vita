//! A bounded stack of alpha layers, preserving every legacy byte rounding step.
pub const MAPS: [&str; 4] = [
    "u_batch_map0",
    "u_batch_map1",
    "u_batch_map2",
    "u_batch_map3",
];
pub const SIZES: [&str; 4] = [
    "u_batch_size0",
    "u_batch_size1",
    "u_batch_size2",
    "u_batch_size3",
];
pub const SCALES: [&str; 4] = [
    "u_batch_scale0",
    "u_batch_scale1",
    "u_batch_scale2",
    "u_batch_scale3",
];
pub const CLIPS: [&str; 4] = [
    "u_batch_clip0",
    "u_batch_clip1",
    "u_batch_clip2",
    "u_batch_clip3",
];
pub const SAMPLERS: [&str; 4] = [
    "u_batch_source0",
    "u_batch_source1",
    "u_batch_source2",
    "u_batch_source3",
];
pub const UNITS: [u32; 4] = [0, 3, 4, 5];

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub layers: usize,
    pub face: u8,
    pub constant: bool,
    pub clipped: bool,
    pub display: bool,
    pub sharpen: bool,
}
pub fn fragment(key: Key) -> String {
    use std::fmt::Write;
    assert!((2..=4).contains(&key.layers));
    let mut text = format!(
        "#define BLEND_MODE 2\n#define BLEND_FACE {}\n#define SOLID_COLOR 0\n",
        key.face
    );
    text.push_str(include_str!("blend_common.glsl"));
    text.push_str(include_str!("blend_draw.glsl"));
    if key.sharpen {
        text.push_str(include_str!("upscale_sharpen.glsl"));
        text.push_str("uniform vec4 u_batch_sharpen;\n");
    }
    text.push_str("varying vec2 v_point;\nuniform vec4 u_batch_opacity;\n");
    if key.constant {
        text.push_str("uniform vec4 u_backdrop_color;\n");
    } else {
        text.push_str(
            "uniform sampler2D u_backdrop;\nuniform vec2 u_backdrop_origin,u_backdrop_size;\n",
        );
    }
    for i in 0..key.layers {
        writeln!(
            text,
            "uniform sampler2D {};\nuniform vec4 {},{},{};",
            SAMPLERS[i], MAPS[i], SIZES[i], SCALES[i]
        )
        .unwrap();
        if key.clipped {
            writeln!(text, "uniform vec4 {};", CLIPS[i]).unwrap();
        }
        if key.display {
            writeln!(text, "vec4 sample{i}(vec2 p) {{ return bytes(texture2D({},(clamp(p,vec2(0.0),{}.zw-1.0)-{}.zw+0.5)/{}.xy)); }}", SAMPLERS[i], SIZES[i], SCALES[i], SIZES[i]).unwrap();
        }
    }
    text.push_str("void main() { vec2 point=floor(v_point);\n");
    // All implicit-gradient samples precede control flow and depend only on
    // coordinates, never another texture. No intermediate framebuffer exists.
    for i in 0..key.layers {
        writeln!(text, "vec2 q{i}=point*{}.xy+{}.zw;", MAPS[i], MAPS[i]).unwrap();
        writeln!(
            text,
            "bool valid{i}=all(greaterThanEqual(q{i},vec2(-0.5)))&&all(lessThan(q{i},{}.zw-0.5));",
            SIZES[i]
        )
        .unwrap();
        if key.clipped {
            writeln!(text, "valid{i}=valid{i}&&all(greaterThanEqual(point,{}.xy))&&all(lessThan(point,{}.zw));", CLIPS[i], CLIPS[i]).unwrap();
        }
        if key.display {
            // Match Display's alpha-weighted interpolation and byte rounding.
            // Sampling stays at texel centers, with no texture filter changes.
            writeln!(text, "q{i}=clamp(q{i},vec2(0.0),{}.zw-1.0);", SIZES[i]).unwrap();
            writeln!(
                text,
                "vec2 base{i}=floor(q{i}), weight{i}=fract(q{i});
vec4 a{i}=sample{i}(base{i}), b{i}=sample{i}(base{i}+vec2(1.0,0.0));
vec4 c{i}=sample{i}(base{i}+vec2(0.0,1.0)), e{i}=sample{i}(base{i}+1.0);
{}a{i}.rgb*=a{i}.a; b{i}.rgb*=b{i}.a; c{i}.rgb*=c{i}.a; e{i}.rgb*=e{i}.a;
vec4 s{i}=mix(mix(a{i},b{i},weight{i}.x),mix(c{i},e{i},weight{i}.x),weight{i}.y);
s{i}.rgb/=max(s{i}.a,0.000001);
{}s{i}=floor(s{i}+0.5);",
                if key.sharpen { format!("vec3 sharp{i}=sharpen_upscale(a{i},b{i},c{i},e{i},weight{i});\nfloat opaque{i}=step(254.5,min(min(a{i}.a,b{i}.a),min(c{i}.a,e{i}.a)));\n") } else { String::new() },
                if key.sharpen { format!("s{i}.rgb=mix(s{i}.rgb,sharp{i},opaque{i}*u_batch_sharpen.{});\n", ["x","y","z","w"][i]) } else { String::new() }
            )
            .unwrap();
        } else {
            writeln!(
                text,
                "vec2 at{i}=floor((floor(q{i}+0.5)+0.5)*{}.xy)-{}.zw;",
                SCALES[i], SCALES[i]
            )
            .unwrap();
            writeln!(
                text,
                "vec4 s{i}=bytes(texture2D({},(at{i}+0.5)/{}.xy));",
                SAMPLERS[i], SIZES[i]
            )
            .unwrap();
        }
    }
    if key.constant {
        text.push_str("vec4 d=u_backdrop_color;\n");
    } else {
        text.push_str(
            "vec4 d=bytes(texture2D(u_backdrop,(point-u_backdrop_origin+0.5)/u_backdrop_size));\n",
        );
    }
    for i in 0..key.layers {
        let component = ["x", "y", "z", "w"][i];
        writeln!(text, "vec4 next{i}=floor(clamp(apply_blend(d,s{i},u_batch_opacity.{component},false),0.0,255.0)+0.5);\nd=mix(d,next{i},float(valid{i}));").unwrap();
    }
    text.push_str("gl_FragColor=d/255.0; }\n");
    text
}
