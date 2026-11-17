//! Conservative support for exact, immutable BC3 sources. Exact DXT5 alpha
//! palette positivity is dilated by two source texels before merging cells.
//! Each source still emits a single vertex stream and samples the original GXT.

use super::{Blend, DeferredKind, EffectShader, GxtFormat, LiveAlphaCoverage, OpaqueGroupMask,
    PsOp, Quad, Shade, SparseCoveragePlan, TexRef};
use knife_perf::optimizations::{self, COVERAGE_MATERIALS, GROUP_OPAQUE_MASK,
    LIVE_ALPHA_COVERAGE, SPARSE_COVERAGE};

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

struct LivePlan {
    index: usize,
    entry: &'static str,
    logical: [u32; 2],
    physical: [u32; 2],
    bytes: usize,
    sha256: &'static [u8; 64],
    rects: &'static [[u16; 4]],
    extended: bool,
}

include!("vita_live_alpha_data.rs");

struct OpaqueMask {
    index: usize,
    entry: &'static str,
    logical: [u32; 2],
    physical: [u32; 2],
    sha256: &'static [u8; 64],
    // Every decoded BC3 alpha in this half-open rectangle is exactly 255.
    rect: [u32; 4],
}

const OPAQUE_MASKS: [OpaqueMask; 2] = [
    OpaqueMask {
        index: 97422, entry: "KQ014710003F8000003F800000.NXZ",
        logical: [1420, 860], physical: [1065, 645],
        sha256: b"bd6587961a63188daeeb5ca04209e68582b7de6bc1879319ba310730f8c79440",
        rect: [286, 160, 776, 441],
    },
    OpaqueMask {
        index: 98016, entry: "KQ0075D0003F8000003F800000.NXZ",
        logical: [1400, 840], physical: [1050, 630],
        sha256: b"c4a767ecfde836a17e9281f37e3d6d06366579cb42bc1b703442caf995486b8f",
        rect: [29, 23, 1013, 582],
    },
];

impl SparseCoveragePlan {
    /// Tie coverage to the exact archive locator and the hash of its current
    /// in-memory KIMG envelope.
    pub fn live_alpha_source(
        archive: &str,
        index: usize,
        entry: &str,
        logical: [u32; 2],
        physical: [u32; 2],
        encoded: &[u8],
        hash_envelope: impl FnOnce(&[u8]) -> Option<[u8; 64]>,
    ) -> Option<Self> {
        if archive != "jallpac" {
            return None;
        }
        if optimizations::enabled(GROUP_OPAQUE_MASK)
            && let Ok(id) = OPAQUE_MASKS.binary_search_by_key(&index, |source| source.index)
        {
            let source = &OPAQUE_MASKS[id];
            return (entry == source.entry && logical == source.logical && physical == source.physical
                && hash_envelope(encoded).as_ref() == Some(source.sha256))
                .then_some(Self::OpaqueMask(OpaqueGroupMask(id)));
        }
        let id = LIVE_PLANS.binary_search_by_key(&index, |source| source.index).ok()?;
        let source = &LIVE_PLANS[id];
        if (source.extended && !optimizations::enabled(COVERAGE_MATERIALS))
            || entry != source.entry
            || logical != source.logical
            || physical != source.physical
            || encoded.len() != source.bytes
        {
            return None;
        }
        (hash_envelope(encoded).as_ref() == Some(source.sha256))
            .then_some(Self::Live(LiveAlphaCoverage(id)))
    }

    pub(super) fn enabled(self) -> bool {
        match self {
            Self::Img0436 => optimizations::enabled(SPARSE_COVERAGE),
            Self::Live(LiveAlphaCoverage(id)) => optimizations::enabled(LIVE_ALPHA_COVERAGE)
                && (!LIVE_PLANS[id].extended || optimizations::enabled(COVERAGE_MATERIALS)),
            Self::OpaqueMask(_) => false,
        }
    }
}

pub(super) fn verify_stage_alpha_upload(id: LiveAlphaCoverage, bytes: &[u8], gxt: &super::GxtTexture<'_>) -> bool {
    if !optimizations::enabled(optimizations::STAGE_ALPHA_REGION) { return false; }
    verify_stage_alpha_bytes(id, bytes, gxt, |encoded| {
        #[cfg(target_os = "vita")]
        return knife_platform_vita::image::sha256_hex(encoded);
        #[cfg(not(target_os = "vita"))]
        { let _ = encoded; None }
    })
}

fn verify_stage_alpha_bytes(id: LiveAlphaCoverage, bytes: &[u8], gxt: &super::GxtTexture<'_>,
    digest: impl FnOnce(&[u8]) -> Option<[u8; 64]>) -> bool
{
    let source = &LIVE_PLANS[id.0];
    matches!(source.index, 96272 | 96328 | 98107)
        && bytes.len() == source.bytes && gxt.format == GxtFormat::Dxt5
        && gxt.texture_type == 0xa000_0000
        && [u32::from(gxt.width), u32::from(gxt.height)] == source.physical
        && digest(bytes).as_ref() == Some(source.sha256)
}

pub(super) fn stage_alpha_region_source(tex: &super::NativeTexture) -> Option<super::stage_alpha_region::Source> {
    let deferred = tex.deferred.as_ref()?;
    let id = deferred.stage_alpha_region.get()?;
    let source = &LIVE_PLANS[id.0];
    if !deferred.stage_alpha_region_verified.get() || tex.id.borrow().is_none()
        || !SparseCoveragePlan::Live(id).enabled() || tex.render_target || tex.srgb
        || tex.content_version.get() != 0 || tex.size != source.physical
        || !matches!(&deferred.kind, DeferredKind::GxtAltered { info, .. }
            if info.format == GxtFormat::Dxt5 && info.layout == 0xa000_0000)
    {
        return None;
    }
    Some(super::stage_alpha_region::Source { index: source.index, size: source.physical, support: source.rects })
}

fn covering_rect(points: [[f32; 2]; 4], [w, h]: [u32; 2]) -> Option<[f64; 4]> {
    let [[l, t], [r, rt], [rb, b], [lb, bb]] = points;
    (points.iter().flatten().all(|p| p.is_finite()) &&
        l == lb && r == rb && t == rt && b == bb &&
        l <= 0. && t <= 0. && r >= w as f32 && b >= h as f32)
        .then_some([l, t, r, b].map(f64::from))
}

pub(super) fn opaque_group_mask(q: &Quad, target: [u32; 2], white: &TexRef) -> bool {
    if target.contains(&0) || q.blend != Blend::Opaque || q.shade != Shade::Sprite
        || q.color[3] != 1. || q.masked || q.soft.is_some() || q.rule.is_some()
        || q.haze != [[0.; 4]; 2] || q.uv != [[0., 0.], [1., 0.], [1., 1.], [0., 1.]]
        || q.clip.is_some_and(|corners| covering_rect(corners, target).is_none())
    { return false; }
    let Some([l, t, r, b]) = covering_rect(q.pos, target) else { return false; };
    if std::sync::Arc::ptr_eq(&q.tex, white) { return q.tex.native.content_version.get() == 0; }
    let Some(SparseCoveragePlan::OpaqueMask(OpaqueGroupMask(id))) = q.tex.sparse_coverage else {
        return false;
    };
    let source = &OPAQUE_MASKS[id];
    if q.tex.physical_size() != source.physical || q.tex.native.srgb
        || q.tex.native.content_version.get() != 0 || !q.tex.story_tiles.is_empty()
        || !q.tex.native.deferred.as_ref().is_some_and(|d|
            matches!(&d.kind, DeferredKind::GxtAltered { info, .. }
                if info.format == GxtFormat::Dxt5 && info.layout == 0xa000_0000))
    { return false; }
    let [w, h] = source.physical.map(f64::from);
    let [x0, y0, x1, y1] = source.rect.map(f64::from);
    // Check edges, not just pixel centres, plus two texels for bilinear taps
    // and interpolation rounding on the F32 native vertex path.
    (-l * w / (r - l)) >= x0 + 2.
        && (-t * h / (b - t)) >= y0 + 2.
        && (f64::from(target[0]) - l) * w / (r - l) <= x1 - 2.
        && (f64::from(target[1]) - t) * h / (b - t) <= y1 - 2.
}

fn transparent_is_identity(q: &Quad, fetch: Option<EffectShader>) -> bool {
    let fixed_alpha_add = matches!(q.blend, Blend::Alpha | Blend::Add);
    if q.shade == Shade::Sprite && fixed_alpha_add {
        return true;
    }
    if !optimizations::enabled(COVERAGE_MATERIALS) || !matches!(q.shade, Shade::Sprite | Shade::Fill) {
        return false;
    }
    // Both shaders output premultiplied zero RGB at zero source alpha.
    // The fixed Alpha/Add/Multiply/Screen factors preserve destination RGBA.
    if fixed_alpha_add || matches!(q.blend, Blend::Multiply | Blend::Screen) {
        return true;
    }
    // The native UCHAR4 programs return the incoming destination at zero
    // alpha, before a possible RGB or alpha rewrite. The generic program
    // unpacks and repacks the incoming UNORM8 bytes exactly on that branch.
    // Select only reviewed operations and their actual shader entries.
    use EffectShader::*;
    matches!((q.blend, fetch),
        (Blend::Ps(PsOp::Add), Some(PsFetch | PsFetch1 | PsFull1 | PsAll1 | PsHalf1 | PsMaterial88))
        | (Blend::Ps(PsOp::DodgeAlpha), Some(PsFetch | PsFetch7 | PsPlain7 | PsAll7 | PsHalf7 | PsMaterial92)))
}

/// Source texel rectangles use the original 0..1 UV domain. Clipped, cropped
/// and shader-distorted inputs retain the original quad.
pub(super) fn rectangles(q: &Quad, plan: SparseCoveragePlan, scale: [f32; 2],
    fetch: Option<EffectShader>) -> Option<&'static [[u16; 4]]> {
    if q.tex.native.render_target
        || q.tex.native.content_version.get() != 0
        || q.clip.is_some() || q.rule.is_some() || q.soft.is_some() || q.masked
        || q.haze != [[0.; 4]; 2]
        || q.color[3] <= 0.
        || q.uv != [[0., 0.], [1., 0.], [1., 1.], [0., 1.]]
        || q.pos.iter().flatten().any(|v| !v.is_finite())
    { return None; }

    if let SparseCoveragePlan::Live(LiveAlphaCoverage(id)) = plan {
        let source = &LIVE_PLANS[id];
        let immutable_bc3 = q.tex.native.deferred.as_ref().is_some_and(|deferred| {
            matches!(&deferred.kind, DeferredKind::GxtAltered { info, .. }
                if info.format == GxtFormat::Dxt5 && info.layout == 0xa000_0000)
        });
        if !transparent_is_identity(q, fetch)
            || !immutable_bc3
            || q.tex.native.srgb
            || !q.tex.story_tiles.is_empty()
            || q.tex.physical_size() != source.physical
        {
            return None;
        }
        // Aligned screen edges keep rasterized endpoints collinear even at
        // T-junctions between adjacent rectangles of different lengths.
        let [[l, t], [r, t1], [r1, b], [l1, b1]] = q.pos;
        let wide_enough = |start: f32, end: f32, axis: usize| {
            // Two source texels project to at least one physical pixel. This
            // leaves room for the bilinear footprint and raster edge snapping.
            (f64::from(end) - f64::from(start)).abs() * f64::from(scale[axis]) * 2.
                >= f64::from(source.physical[axis])
        };
        return (l == l1 && r == r1 && t == t1 && b == b1 && wide_enough(l, r, 0) && wide_enough(t, b, 1))
            .then_some(source.rects);
    }

    if q.tex.physical_size() != IMG0436_PHYSICAL || q.blend != Blend::Ps(PsOp::DodgeAlpha)
        || q.shade != Shade::Sprite {
        return None;
    }
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

pub(super) fn axis_aligned_point(q: &Quad, [u, v]: [f32; 2]) -> [f32; 4] {
    fn interval(start: f32, end: f32, fraction: f32) -> f32 {
        if fraction == 0. {
            start
        } else if fraction == 1. {
            end
        } else {
            let (start, end) = (f64::from(start), f64::from(end));
            (start + (end - start) * f64::from(fraction)) as f32
        }
    }
    [interval(q.pos[0][0], q.pos[1][0], u), interval(q.pos[0][1], q.pos[3][1], v), u, v]
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
