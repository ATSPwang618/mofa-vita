//! Backend-independent rendering data shared by the desktop and Vita implementations.

use bytemuck::{Pod, Zeroable};

pub const CANVAS_W: u32 = 1920;
pub const CANVAS_H: u32 = 1080;
pub const RENDER_W: u32 = 960;
pub const RENDER_H: u32 = 540;

pub mod experiments;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    /// STRA haze (nexl_nxshader_haze): x and y each (amplitude in uv, waves per uv.y, phase, unused)
    pub haze: [[f32; 4]; 2],
    /// soft clip mask (see `SoftClip`): inner rect, (pivot, sin, cos), (radius, LUT row, texels, -);
    /// radius 0 = no mask
    pub soft: [[f32; 4]; 3],
}

// Native shader streams consume this ABI. Keep the assertions in the library
// so a Vita `cargo check --lib` verifies ARM layout without a target test harness.
const _: () = {
    assert!(core::mem::size_of::<Vertex>() == 112);
    assert!(core::mem::align_of::<Vertex>() == 4);
    assert!(core::mem::offset_of!(Vertex, pos) == 0);
    assert!(core::mem::offset_of!(Vertex, uv) == 8);
    assert!(core::mem::offset_of!(Vertex, color) == 16);
    assert!(core::mem::offset_of!(Vertex, haze) == 32);
    assert!(core::mem::offset_of!(Vertex, soft) == 64);
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Blend {
    #[default]
    Alpha,
    Add,
    Sub,
    Multiply,
    Screen,
    /// Replace destination (no blending).
    Opaque,
    /// A blend computed in the shader from a copy of the target taken just before the quad is drawn (the
    /// exe's blend state 7, which captures the framebuffer per draw).
    Ps(PsOp),
    /// Normal (premultiplied) colour with the source alpha added to the target's: the exe's blend state 0 (colour
    /// SrcAlpha / OneMinusSrcAlpha, alpha One / One), which a draw into a cleared group surface needs so the
    /// surface keeps the drawn alpha (`Alpha` leaves the target's alpha alone).
    Over,
    /// Colour kept, source alpha added to the target's (the exe's blend state 0 alpha One / One, for the layer
    /// group mask sub_9FE00 / sub_A4BC0 accumulate).
    Footprint,
    /// Colour kept, the target's alpha multiplied by the source alpha (the group mask applied to the group
    /// pixels: sub_37EA0's exalpha draw).
    MaskAlpha,
}

/// Backdrop-reading blends of the exe's layer shaders (shader/nexl_nxshader_*.bnsh). With s = texture rgb *
/// colour rgb, a = texture alpha * colour alpha and d = the backdrop, each writes an opaque colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PsOp {
    /// ps_add: mix(d, min(d + s, 1), a)
    Add = 1,
    /// ps_darken: mix(d, min(d, s), a)
    Darken,
    /// ps_lighten: mix(d, max(d, s), a)
    Lighten,
    /// ps_sub: mix(d, max(d + s - 1, 0), a)
    LinearBurn,
    /// ps_burn: mix(d, s > 0 ? max(1 - (1 - d) / s, 0) : 0, a)
    Burn,
    /// ps_dodge: mix(d, 1 - s > 0 ? min(d / (1 - s), 1) : 1, a)
    Dodge,
    /// dodge / ps_dodge5: d > 0 ? min(d / max(1 - a s, 0), 1) : 0
    DodgeAlpha,
    /// differnce: mix(d, |d - s|, a)
    Difference,
    /// differnce5: |d - a s|
    DifferenceAlpha,
    /// exclusion: mix(d, d + s - 2 d s, a)
    Exclusion,
    /// hardlight: mix(d, s < 0.5 ? 2 d s : 1 - 2 (1 - s)(1 - d), a)
    HardLight,
    /// hardlight_ex: hardlight of min(s / texture alpha, 1)
    HardLightEx,
    /// overlay: mix(d, d < 0.5 ? 2 d s : 1 - 2 (1 - s)(1 - d), a)
    Overlay,
    /// softlight: mix(d, s < 0.5 ? d^(2 - 2 s) : d^(1 / (2 s)), a)
    SoftLight,
    /// sub: max(d - a (1 - s), 0)
    Sub,
    /// add (ignores texture alpha): min(d + s * colour alpha, 1)
    AddNoAlpha,
    /// sub_noalpha (ignores texture alpha): max(d - colour alpha (1 - s), 0)
    SubNoAlpha,
    /// noalpha: s, opaque
    NoAlpha,
    /// normal: mix(d * backdrop alpha, s, a), opaque (where a = 0 the backdrop, alpha included, stays)
    Normal,
    /// darken (Witch on the Holy Night's STBR 4): mix(d, min(d, s), colour alpha) where a > 0
    DarkenColorAlpha,
    /// lighten (STBR 6; texture alpha ignored): mix(d, max(d, s), colour alpha) while the colour alpha > 0
    LightenColorAlpha,
    /// screen (STBR 9; texture alpha ignored): mix(d, 1 - (1 - d)(1 - s ca), ca), ca the colour alpha
    ScreenColorAlpha,
    /// the default shader through blend state 3 (DstColor / 1-SrcAlpha, STBR 7): min(d s + (1 - a) d, 1)
    /// where a > 0
    MultiplyDst,
    /// the default shader with blending off (blend state 7, STBR 26 / 29): s where a > 0
    Copy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Shade {
    #[default]
    Sprite,
    Fill,
    Text,
    /// glyph coverage with its rim composited in one pass (fill over the rim taps), so a faded glyph blends
    /// once like nn::font's bordered glyph; `Quad::haze` = [uv clamp rect, (u, v per canvas px, rim alpha, rim
    /// colour 0xRRGGBB: 0 black)]
    RimText,
    Mono,
    Nega,
    /// The texture's rgb with alpha 1 (shader noalpha: the exe draws a clipped layer of a layer group this way)
    NoAlpha,
    /// The texture's rgb with the colour's alpha alone (shader 17 forcealpha)
    ForceAlpha,
}

/// How a blur pass takes its taps (see fs_blur).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlurMode {
    /// Taps offset in uv, edges clamped, rgb only (overseas sub_9CCD0)
    Uv = 0,
    /// Shifted quads of shader 3 fulladd (sub_A0A90 on a picture)
    FullAdd = 1,
    /// Shifted quads of shader 51 whiteness (sub_A0A90 on a coverage texture)
    White = 2,
}

/// The clip inset mask (sub_32E30): a rounded rectangle whose edge band of `radius` target pixels fades out
/// along the radial profile of an alphagradation texture (BORDER_PARTS, LUT `row`, `texels` = its radius).
/// `inner` (x0, y0, x1, y1) is relative to `pivot` before the rectangle is turned by `rot` (radians).
#[derive(Clone, Copy, Debug)]
pub struct SoftClip {
    pub inner: [f32; 4],
    pub pivot: [f32; 2],
    pub rot: f32,
    pub radius: f32,
    pub row: u32,
    pub texels: u32,
    /// alpha inside the inner rectangle
    pub fill: f32,
}

impl SoftClip {
    /// Encode the mask into the three `Vertex::soft` attributes at the target pixel scale.
    pub fn vertex_data(&self, scale: [f32; 2]) -> [[f32; 4]; 3] {
        let (s, c) = self.rot.sin_cos();
        let inner = core::array::from_fn(|i| self.inner[i] * scale[i % 2]);
        [inner, [self.pivot[0] * scale[0], self.pivot[1] * scale[1], s, c],
            [self.radius * scale[0].min(scale[1]), self.row as f32, self.texels as f32, 1.0 - self.fill]]
    }
}

/// Rows of the soft clip LUT: alpha along the middle row of each alphagradation texture, edge first.
pub const SOFT_LUT_ROWS: u32 = 18;
pub const SOFT_LUT_W: u32 = 257;

#[cfg(test)]
mod tests {
    use super::Vertex;

    #[test]
    fn vertex_stream_layout_is_stable() {
        let vertex = Vertex {
            pos: [1.0, 2.0],
            uv: [3.0, 4.0],
            color: [5.0, 6.0, 7.0, 8.0],
            haze: [[9.0, 10.0, 11.0, 12.0], [13.0, 14.0, 15.0, 16.0]],
            soft: [
                [17.0, 18.0, 19.0, 20.0],
                [21.0, 22.0, 23.0, 24.0],
                [25.0, 26.0, 27.0, 28.0],
            ],
        };
        let stream: &[f32] = bytemuck::cast_slice(core::slice::from_ref(&vertex));
        assert_eq!(stream.len(), 28);
        for (index, value) in stream.iter().enumerate() {
            assert_eq!(*value, (index + 1) as f32, "vertex attribute at float {index}");
        }
    }
}
