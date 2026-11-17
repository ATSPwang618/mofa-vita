//! Reject a complete fixed-blend stage draw only when all possible bilinear
//! taps lie inside a zero-alpha rectangle of the resident BC3 source.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct Proof(usize);

struct AlphaSource {
    label: &'static str,
    physical: [u32; 2],
    sha256: &'static [u8; 64],
    transparent: &'static [[u32; 4]],
}

// Generated from original R6 BC3 entries. The locator and SHA bind each proof.
// Every source texel inside each half-open rectangle has alpha zero.
const SOURCES: &[AlphaSource] = &[
    AlphaSource { label: "aok_m_04_21_00_02_02", physical: [257, 751], sha256: b"bf04b9a102b97f1da5a0133a6f1eef037056139fa1a01fe7b6cfb0b5e6bf41dc",
        transparent: &[[245, 0, 257, 693]] },
    AlphaSource { label: "aok_m_04_21_00_04_04", physical: [257, 751], sha256: b"5682440fa3022e5210ea1738c895874782bd258922cf268277abc2e4b072d96a",
        transparent: &[[245, 0, 257, 689]] },
    AlphaSource { label: "img1481_02_02/tile6", physical: [512, 508], sha256: b"ff4cdf07611999563e762ea38f514de33333437f5163b8191729321ed4d7e0c2",
        transparent: &[[0, 0, 512, 141]] },
    AlphaSource { label: "img1481_02_02/tile7", physical: [512, 508], sha256: b"2d062cc058677a2105cca101c2db1ec44c6128a9daec5b59084adf60b5ac28f1",
        transparent: &[[0, 0, 512, 6]] },
    AlphaSource { label: "img1481_02_02/tile8", physical: [512, 508], sha256: b"4d1904df3755ce6ae2074ae20aa0e745379882c1cfdd9055898615e4dfce95e1",
        transparent: &[[0, 0, 393, 205]] },
    AlphaSource { label: "img1482_01_00/tile0", physical: [508, 508], sha256: b"19915cded02f3da85190339cb43699a77c2243de6aa4d1c89240eaa7dda2a359",
        transparent: &[[0, 0, 508, 94]] },
    AlphaSource { label: "img1482_01_00/tile1", physical: [512, 508], sha256: b"3f23de33da0fe88a1e8f31650f74b8228a322ce16a003a5f22bab1037440a5d0",
        transparent: &[[0, 0, 512, 129]] },
    AlphaSource { label: "img1482_01_00/tile2", physical: [512, 508], sha256: b"f5ea42ec9442b110fa39e0344d81f0c7d12202da00dff418186bceb55ea1463c",
        transparent: &[[0, 0, 512, 34]] },
    AlphaSource { label: "img1482_01_00/tile3", physical: [512, 508], sha256: b"6a346d64ef63d73b6d7e9435e03f7b1525df7160be747de47dd6e261c088e26a",
        transparent: &[[0, 0, 512, 5]] },
    AlphaSource { label: "img1482_01_00/tile4", physical: [303, 508], sha256: b"6fbfa4f9bf4a574c40f5915a1070ca19d36c589864bb251d563817bafc5df79e",
        transparent: &[[0, 0, 231, 183], [0, 0, 303, 103]] },
    AlphaSource { label: "img1482_02_02/tile0", physical: [508, 508], sha256: b"3fa19a08b0674365deca0bfcd2a0aa5ed0ef05add13b0a428087a3e201347a12",
        transparent: &[[0, 0, 508, 92]] },
    AlphaSource { label: "img1482_02_02/tile1", physical: [512, 508], sha256: b"3f9c142b58bea5f66b3b705f8905bd2664baf8b3003b2819a08bfb1c9894ce59",
        transparent: &[[0, 0, 512, 128]] },
    AlphaSource { label: "img1482_02_02/tile2", physical: [512, 508], sha256: b"18a41cb6895acff6b7c139e1606a6154e2430e4b98c545666039931c163ae87f",
        transparent: &[[0, 0, 512, 34]] },
    AlphaSource { label: "img1482_02_02/tile4", physical: [303, 508], sha256: b"4a9f8af4a7aabb763db6cfa9eb04e0a81ced4436932f3366faf49e19151cd7a0",
        transparent: &[[0, 0, 160, 215], [0, 0, 211, 195]] },
    AlphaSource { label: "img2034_03_03/tile0", physical: [508, 508], sha256: b"b457247f0f3efa02025271260f07f1241c7ba1d5a43417bd89f3aa09e80de172",
        transparent: &[[0, 0, 379, 116]] },
    AlphaSource { label: "img2041_03_03", physical: [559, 1094], sha256: b"68e65cbd08cf703928c0c57ef1612d460a44ea3f91e058cd0dc443d595806ba9",
        transparent: &[[451, 0, 559, 347]] },
    AlphaSource { label: "sou_m_01_02_00_02_02", physical: [259, 817], sha256: b"63f34302789bc90f965cd4184fff2ed0677c1adf5fae43fe9a330b578d0bc74c",
        transparent: &[[0, 0, 44, 734]] },
    AlphaSource { label: "sou_m_01_02_00_04_04", physical: [259, 817], sha256: b"5c57907d2e253a719b54f5f285a5c56c3dad317b9a7ce4c0b8541d43bef7109a",
        transparent: &[[0, 0, 43, 731]] },
];

pub(super) fn verify(label: &str, gxt: &GxtTexture<'_>,
    hash: impl FnOnce() -> Option<[u8; 64]>) -> Option<Proof>
{
    if gxt.format != GxtFormat::Dxt5 || gxt.texture_type != 0xa000_0000 { return None; }
    let physical = [u32::from(gxt.width), u32::from(gxt.height)];
    let id = SOURCES.iter().position(|source|
        label.eq_ignore_ascii_case(source.label) && physical == source.physical)?;
    (hash().as_ref() == Some(SOURCES[id].sha256)).then_some(Proof(id))
}

pub(super) fn for_upload(bytes: &[u8], label: &str, gxt: &GxtTexture<'_>) -> Option<Proof> {
    #[cfg(target_os = "vita")]
    if knife_perf::optimizations::enabled(knife_perf::optimizations::TRANSPARENT_DRAW_REJECT) {
        return verify(label, gxt, || knife_platform_vita::image::sha256_hex(bytes));
    }
    let _ = (bytes, label, gxt);
    None
}

pub(super) fn rejects(q: &Quad, logical: [f32; 2], physical: [u32; 2]) -> bool {
    let texture = &q.tex.native;
    let Some(Proof(id)) = texture.deferred.as_ref().and_then(|source| source.transparent_draw.get())
        else { return false; };
    if texture.content_version.get() != 0 || texture.id.borrow().is_none() || texture.srgb
        || logical != [1920., 1080.] || physical != [960, 540]
        || q.rule.is_some() || q.soft.is_some() || q.masked || q.haze != [[0.; 4]; 2]
        || !q.color.iter().all(|v| (0. ..=1.).contains(v))
        || !matches!(q.shade, Shade::Sprite | Shade::Fill)
        || !matches!(q.blend, Blend::Alpha | Blend::Multiply)
        || !story_tiles::rectangular_uv(q.uv)
    {
        return false;
    }
    let [[l,t], [r,rt], [rb,b], [lb,bb]] = q.pos;
    if l != lb || r != rb || t != rt || b != bb
        || [l,t,r,b].iter().any(|v| v.abs() > 32768.)
    {
        return false;
    }
    let source = &SOURCES[id];
    // Bound the entire viewport intersection even if a hard clip covers less.
    // Allow one physical pixel beyond each viewport edge and two source texels
    // for bilinear taps and vertex/interpolant precision.
    let Some([x0,x1]) = taps([l,r], [q.uv[0][0],q.uv[1][0]], logical[0], source.physical[0]) else { return false; };
    let Some([y0,y1]) = taps([t,b], [q.uv[0][1],q.uv[3][1]], logical[1], source.physical[1]) else { return false; };
    source.transparent.iter().any(|&[l,t,r,b]| l <= x0 && t <= y0 && r >= x1 && b >= y1)
}

fn taps(pos: [f32; 2], uv: [f32; 2], view: f32, size: u32) -> Option<[u32; 2]> {
    let [a, b] = pos.map(f64::from);
    let [u, v] = uv.map(f64::from);
    let lo = a.min(b).max(-2.);
    let hi = a.max(b).min(f64::from(view) + 2.);
    if hi <= lo || (b - a).abs() < 2. { return None; }
    let sample = |x| (u + (v - u) * ((x - a) / (b - a))) * f64::from(size);
    let [first, last] = [sample(lo), sample(hi)];
    Some([
        (first.min(last) - 2.).floor().clamp(0., f64::from(size - 1)) as u32,
        (first.max(last) + 2.).ceil().clamp(1., f64::from(size)) as u32,
    ])
}
