//! Exact-alpha support for one admitted immutable effect. Rectangles were built
//! from exact DXT5 alpha-palette positivity (before u8 rounding), dilated by two
//! source texels, then merged
//! without overlap. This is one vertex stream, not one draw per rectangle.

use super::{Blend, PsOp, Quad, Shade, SparseCoveragePlan};

pub(super) const IMG0436_PHYSICAL: [u32; 2] = [624, 415];
pub(super) const IMG0436_RECTS: &[[u16; 4]] = &[
    [144, 0, 208, 16],
    [320, 0, 352, 16],
    [144, 16, 224, 32],
    [96, 32, 160, 48],
    [240, 16, 272, 48],
    [464, 16, 512, 48],
    [96, 48, 192, 64],
    [416, 48, 432, 64],
    [112, 64, 192, 80],
    [464, 48, 496, 80],
    [128, 80, 144, 96],
    [256, 64, 320, 96],
    [448, 80, 496, 96],
    [576, 64, 608, 96],
    [112, 96, 160, 112],
    [240, 96, 304, 112],
    [448, 96, 480, 112],
    [208, 112, 304, 128],
    [352, 96, 400, 128],
    [544, 112, 576, 128],
    [592, 96, 624, 128],
    [192, 128, 288, 144],
    [544, 128, 624, 144],
    [32, 128, 96, 160],
    [160, 144, 288, 160],
    [464, 128, 496, 160],
    [544, 144, 560, 160],
    [576, 144, 608, 160],
    [128, 160, 192, 176],
    [512, 160, 544, 176],
    [128, 176, 176, 192],
    [480, 176, 560, 192],
    [32, 160, 64, 208],
    [128, 192, 160, 208],
    [192, 176, 240, 208],
    [416, 192, 464, 208],
    [480, 192, 528, 208],
    [80, 192, 112, 224],
    [368, 160, 400, 224],
    [416, 208, 480, 224],
    [0, 224, 32, 240],
    [176, 208, 272, 240],
    [432, 224, 464, 240],
    [0, 240, 48, 256],
    [176, 240, 192, 256],
    [352, 224, 384, 256],
    [32, 256, 48, 272],
    [96, 256, 128, 272],
    [80, 272, 144, 288],
    [528, 256, 560, 288],
    [96, 288, 144, 304],
    [384, 272, 416, 304],
    [448, 272, 464, 304],
    [112, 304, 144, 320],
    [496, 304, 544, 320],
    [368, 304, 400, 336],
    [464, 320, 496, 336],
    [512, 320, 544, 336],
    [160, 336, 192, 352],
    [464, 336, 528, 352],
    [160, 352, 208, 368],
    [320, 336, 400, 368],
    [416, 336, 448, 368],
    [480, 352, 512, 384],
    [256, 384, 288, 400],
    [256, 400, 336, 415],
    [352, 400, 384, 415],
    [400, 384, 432, 415],
];

/// Invalid material/UV mappings retain the original single-quad path. In
/// particular, source-pixel rectangles cannot be mapped as parametric fractions
/// through mirrored/cropped UVs: that would move the alpha support to the wrong
/// screen region. Only the exact original 0..1 UV orientation is admitted.
pub(super) fn rectangles(q: &Quad) -> Option<&'static [[u16; 4]]> {
    if q.tex.sparse_coverage != Some(SparseCoveragePlan::Img0436)
        || q.tex.physical_size() != IMG0436_PHYSICAL
        || q.tex.native.render_target
        || q.tex.native.content_version.get() != 0
        || q.clip.is_some() || q.rule.is_some() || q.soft.is_some() || q.masked
        || q.haze != [[0.; 4]; 2] || q.shade != Shade::Sprite
        || q.blend != Blend::Ps(PsOp::DodgeAlpha) || q.color[3] <= 0.
        || q.uv != [[0., 0.], [1., 0.], [1., 1.], [0., 1.]]
        || q.pos.iter().flatten().any(|v| !v.is_finite())
    { return None; }
    for j in 0..2 {
        if (f64::from(q.pos[0][j]) + f64::from(q.pos[2][j])
            - f64::from(q.pos[1][j]) - f64::from(q.pos[3][j])).abs() > 1. / 4096.
        { return None; }
    }
    let a = [f64::from(q.pos[1][0]) - f64::from(q.pos[0][0]),
        f64::from(q.pos[1][1]) - f64::from(q.pos[0][1])];
    let b = [f64::from(q.pos[3][0]) - f64::from(q.pos[0][0]),
        f64::from(q.pos[3][1]) - f64::from(q.pos[0][1])];
    if (a[0] * b[1] - a[1] * b[0]).abs() < 1.0e-8 { return None; }
    Some(IMG0436_RECTS)
}

pub(super) fn point(q: &Quad, uv: [f32; 2]) -> [f32; 4] {
    let p = core::array::from_fn::<_, 2, _>(|j| q.pos[0][j]
        + (q.pos[1][j] - q.pos[0][j]) * uv[0]
        + (q.pos[3][j] - q.pos[0][j]) * uv[1]);
    [p[0], p[1], uv[0], uv[1]]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_support_rectangles_are_bounded_and_disjoint() {
        assert_eq!(IMG0436_RECTS.len(), 68);
        let mut covered = 0u32;
        for (i, &[x0, y0, x1, y1]) in IMG0436_RECTS.iter().enumerate() {
            assert!(x0 < x1 && y0 < y1 && u32::from(x1) <= IMG0436_PHYSICAL[0]
                && u32::from(y1) <= IMG0436_PHYSICAL[1]);
            covered += u32::from(x1 - x0) * u32::from(y1 - y0);
            for &[a0, b0, a1, b1] in &IMG0436_RECTS[..i] {
                assert!(x0 >= a1 || a0 >= x1 || y0 >= b1 || b0 >= y1,
                    "overlapping support would apply the PS blend twice");
            }
        }
        assert_eq!(covered, 67_696);
    }
}
