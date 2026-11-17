//! Geometry and alpha contract for discarding an earlier stage composite.
//! The caller verifies the original mask payload, aliasing and full clears.
use super::{Blend, PsOp, Quad, Shade};

pub(super) fn keeps_one_alpha(q: &Quad) -> bool {
    matches!(q.blend, Blend::Alpha | Blend::Add | Blend::Sub | Blend::Multiply | Blend::Screen
        | Blend::Ps(PsOp::Add | PsOp::Dodge | PsOp::DodgeAlpha | PsOp::Normal | PsOp::Overlay))
        && matches!(q.shade, Shade::Sprite | Shade::Fill | Shade::NoAlpha)
        && q.rule.is_none() && q.soft.is_none() && !q.masked && q.haze == [[0.; 4]; 2]
        && q.pos.iter().flatten().chain(q.uv.iter().flatten()).chain(&q.color)
            .chain(q.clip.iter().flatten().flatten()).all(|value| value.is_finite())
}

pub(super) fn opaque_pixels(pixels: &[Quad], mask: &Quad,
    logical: [u32; 2], physical: [u32; 2]) -> bool
{
    let Some((seed, tail)) = pixels.split_first() else { return false; };
    if logical.contains(&0) || physical.contains(&0) || !std::sync::Arc::ptr_eq(&seed.tex, &mask.tex)
        || seed.shade != Shade::NoAlpha || seed.blend != Blend::Ps(PsOp::Normal)
        || seed.color != [1.; 4] || seed.uv != [[0., 0.], [1., 0.], [1., 1.], [0., 1.]]
        || !keeps_one_alpha(seed) || !tail.iter().all(keeps_one_alpha)
    {
        return false;
    }

    // NoAlpha + PS Normal initializes every covered alpha to one. Require a
    // whole native pixel of overscan, and a clip that leaves the viewport whole.
    let [[l, t], [r, rt], [rb, b], [lb, bb]] = seed.pos;
    let [w, h] = logical.map(|v| v as f32);
    let [sx, sy] = [w / physical[0] as f32, h / physical[1] as f32];
    l == lb && r == rb && t == rt && b == bb && l <= -sx && t <= -sy && r >= w + sx && b >= h + sy
        && seed.clip.is_none_or(|clip| clip == [[0., 0.], [w, 0.], [w, h], [0., h]])
}
