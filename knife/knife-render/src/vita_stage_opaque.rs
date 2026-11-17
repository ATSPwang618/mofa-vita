//! Drop consecutive stage paints only when the next native Sprite write
//! overwrites the whole viewport and all fixed blends preserve destination alpha.
use super::*;

const WIDTH: u32 = 512;
const HEIGHT: u32 = 508;

/// The exact KIMG contents of IMG1202 tile 1, grades 0 and 33. Every BC3
/// alpha texel at least four texels inside either tile is 255. Compute this
/// identity from the bytes actually passed to GXM on each residency load.
pub(super) fn verifies_opaque_interior(bytes: &[u8], label: &str, gxt: &GxtTexture<'_>) -> bool {
    #[cfg(target_os = "vita")]
    {
        const HASHES: [[u8; 64]; 2] = [
            *b"5cde6d56c9993ecd112754701a4dd9b1b0d21f7c5c346c8209b7f72ce999805d",
            *b"8710af3e80ace01eb39de936bbafadbd22adac1eafd65f0e59ac5d086f6fda7f",
        ];
        [u32::from(gxt.width), u32::from(gxt.height)] == [WIDTH, HEIGHT]
            && gxt.format == GxtFormat::Dxt5 && gxt.texture_type == 0xa000_0000
            && label.eq_ignore_ascii_case("IMG1202/tile1")
            && knife_platform_vita::image::sha256_hex(bytes).is_some_and(|digest| HASHES.contains(&digest))
    }
    #[cfg(not(target_os = "vita"))]
    { let _ = (bytes, label, gxt); false }
}

pub(super) fn hidden_paints(commands: &[Command], begin: usize, target: [u32; 2]) -> Option<std::ops::Range<usize>> {
    let mut start = None;
    for (index, command) in commands.iter().enumerate().skip(begin + 1) {
        match command {
            Command::Draw { blend, .. } if preserves_alpha(*blend) => {
                if let Some(first) = start && is_opaque_cover(command, target) {
                    return Some(first..index);
                }
                start.get_or_insert(index);
            }
            Command::Draw { .. } | Command::GroupCut(_) => start = None,
            _ => break,
        }
    }
    None
}

fn preserves_alpha(blend: EffectBlend) -> bool {
    // The fixed factors for these modes are zero source alpha and unit
    // destination alpha. Opaque, Over, mask and programmable PS can write it.
    matches!(blend, EffectBlend::Alpha | EffectBlend::Add | EffectBlend::Sub
        | EffectBlend::Multiply | EffectBlend::Screen)
}

fn is_opaque_cover(command: &Command, [w, h]: [u32; 2]) -> bool {
    let Command::Draw { shader: EffectShader::PlainSprite, blend: EffectBlend::Alpha,
        textures, params, vertices, .. } = command else { return false; };
    let Some(texture) = &textures[0] else { return false; };
    if *params != [0.; 8] || texture.size != [WIDTH, HEIGHT] || texture.srgb
        || texture.content_version.get() != 0
        || !texture.deferred.as_ref().is_some_and(|source| source.opaque_interior.get()) {
        return false;
    }
    let [tl, tr, br, tl2, br2, bl] = vertices.as_slice() else { return false; };
    if vertices.iter().any(|v| v.color[3] != 1.
        || !v.pos.iter().all(|value| value.is_finite())
        || !v.uv.iter().all(|value| (0. ..=1.).contains(value)))
        || (tl.pos, tl.uv) != (tl2.pos, tl2.uv) || (br.pos, br.uv) != (br2.pos, br2.uv)
        || !rectangular(tl.pos, tr.pos, br.pos, bl.pos)
        || !rectangular(tl.uv, tr.uv, br.uv, bl.uv) {
        return false;
    }
    let [left, top] = tl.pos.map(f64::from);
    let [right, bottom] = br.pos.map(f64::from);
    let x = 1. + 2. / f64::from(w);
    let y = 1. + 2. / f64::from(h);
    left < -x && right > x && bottom < -y && top > y
        && opaque_axis(left, right, tl.uv[0], br.uv[0], x, WIDTH)
        && opaque_axis(top, bottom, tl.uv[1], br.uv[1], y, HEIGHT)
}

fn rectangular(a: [f32; 2], b: [f32; 2], c: [f32; 2], d: [f32; 2]) -> bool {
    a[0] == d[0] && a[1] == b[1] && b[0] == c[0] && c[1] == d[1]
}

fn opaque_axis(start: f64, end: f64, first_uv: f32, last_uv: f32, limit: f64, size: u32) -> bool {
    let size = f64::from(size);
    let u = f64::from(first_uv);
    let span = f64::from(last_uv) - u;
    let texel = |position| (u + span * ((position - start) / (end - start))) * size;
    // Source texels within the four-texel border are certified. Reserve two
    // more texels for bilinear taps and interpolation at raster boundaries.
    (6. ..=size - 6.).contains(&texel(-limit)) && (6. ..=size - 6.).contains(&texel(limit))
}
