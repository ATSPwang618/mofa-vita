//! Shared GLSL generation for runtime compilation and host-side SGX binaries.
pub fn fragment_source(fragment: &str) -> String {
    format!("#version 100\nprecision highp float;\nprecision highp int;\n{fragment}")
}
pub fn glyph_batch() -> String {
    format!("#define GLYPH_BATCH\n{}", include_str!("glyph.frag"))
}
pub fn color_batch(face: u8) -> String {
    format!(
        "#define SOLID_COLOR 1\n#define BLEND_MODE 0\n#define BLEND_FACE {face}\n{}\n{}\n\
         varying vec2 v_point; varying vec4 v_color;\n\
         uniform sampler2D u_backdrop;\n\
         uniform vec2 u_backdrop_origin,u_backdrop_size;\n\
         void main() {{\n\
         vec4 d=bytes(texture2D(u_backdrop,(floor(v_point)-u_backdrop_origin+0.5)/u_backdrop_size));\n\
         vec4 c=floor(v_color+0.5);\n\
         gl_FragColor=apply_blend(d,vec4(c.rgb,255.0),c.a,false)/255.0; }}\n",
        include_str!("blend_common.glsl"),
        include_str!("blend_draw.glsl"),
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Raw,
    PremultipliedDisplay,
    Fill,
    Solid,
    Glyph,
    Blend,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sampling {
    Constant,
    Nearest,
    Logical,
    Affine,
    Linear,
    Wrapped,
    LogicalAffine,
    LogicalLinear,
    Display,
    SharpenedDisplay,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub kind: Kind,
    pub sampling: Sampling,
    pub mode: u8,
    pub face: u8,
    pub clear: bool,
    pub constant_backdrop: bool,
    /// CPU coverage proof permits an opaque SGX pass without fragment kill.
    pub covered: bool,
}
impl Key {
    pub fn raw() -> Self {
        Self {
            kind: Kind::Raw,
            sampling: Sampling::Nearest,
            mode: 0,
            face: 0,
            clear: false,
            constant_backdrop: false,
            covered: false,
        }
    }
}

pub fn fragment(key: Key) -> String {
    if key.kind == Kind::Glyph {
        return include_str!("glyph.frag").into();
    }
    let mut fragment = format!(
        "#define SAMPLE_KIND {}\n#define SAMPLE_CLEAR {}\n#define BLEND_MODE {}\n#define BLEND_FACE {}\n#define SOLID_COLOR {}\n",
        key.sampling as u8,
        u8::from(key.clear),
        key.mode,
        key.face,
        u8::from(key.kind == Kind::Solid),
    );
    fragment.push_str(include_str!("blend_common.glsl"));
    if matches!(key.kind, Kind::Solid | Kind::Blend) {
        fragment.push_str(include_str!("blend_draw.glsl"));
    }
    if key.sampling == Sampling::SharpenedDisplay {
        fragment.push_str(include_str!("upscale_sharpen.glsl"));
    }
    fragment.push_str(include_str!("draw_sample.glsl"));
    if key.constant_backdrop {
        fragment.push_str("uniform vec4 u_backdrop_color;\n");
    }
    fragment.push_str("void main() { bool valid; vec4 s=draw_sample(valid);\n");
    if key.kind == Kind::Raw {
        fragment
            .push_str("if(u_kind==6.0) s.a=255.0; else if(u_kind==8.0) s=vec4(0.0,0.0,0.0,s.b);\n");
    } else if key.kind == Kind::PremultipliedDisplay {
        fragment.push_str("if(u_operation.z!=255.0) s=floor(s*u_operation.z/256.0);\n");
    } else if key.kind != Kind::Fill {
        if key.constant_backdrop {
            fragment.push_str("vec4 d=u_backdrop_color;\n");
        } else {
            fragment.push_str("vec4 d=bytes(texture2D(u_backdrop,(floor(v_point)-u_backdrop_origin+0.5)/u_backdrop_size));\n");
        }
        fragment.push_str("s=apply_blend(d,s,u_operation.z,u_operation.w!=0.0);\n");
    }
    // SGX's compiler needs implicit-gradient reads before fragment rejection,
    // including the backdrop and blend lookup textures.
    if !key.covered {
        fragment.push_str("if(!valid) discard; ");
    }
    fragment.push_str("gl_FragColor=s/255.0; }\n");
    fragment
}
