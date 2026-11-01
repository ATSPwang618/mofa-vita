//! Generic two-layer pixel-local composition. No extra render target or copy.
//!
//! Split convex footprints into disjoint regions, retaining each original draw
//! outside the intersection. This avoids shader coverage predicates and samples
//! each source with its original affine mapping. The intersection shader performs
//! both operations in order, with an encoded UNORM8 store/load between them.
use super::*;

type Point = [f64; 2];
type Polygon = Vec<Point>;

fn cross(a: Point, b: Point, p: Point) -> f64 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}
fn area(p: &[Point]) -> f64 {
    p.iter()
        .zip(p.iter().cycle().skip(1))
        .take(p.len())
        .map(|(a, b)| a[0] * b[1] - a[1] * b[0])
        .sum::<f64>()
        * 0.5
}
fn convex(p: &[Point]) -> bool {
    if p.len() < 3 || area(p).abs() < 1.0e-8 {
        return false;
    }
    let sign = area(p).signum();
    (0..p.len()).all(|i| cross(p[i], p[(i + 1) % p.len()], p[(i + 2) % p.len()]) * sign >= 0.0)
}
fn cut(p: &[Point], a: Point, b: Point, sign: f64) -> (Polygon, Polygon) {
    let mut inside = Vec::new();
    let mut outside = Vec::new();
    if p.is_empty() {
        return (inside, outside);
    }
    for k in 0..p.len() {
        let prev = p[(k + p.len() - 1) % p.len()];
        let cur = p[k];
        let dp = cross(a, b, prev) * sign;
        let dc = cross(a, b, cur) * sign;
        if (dp < 0.0 && dc > 0.0) || (dp > 0.0 && dc < 0.0) {
            let t = dp / (dp - dc);
            let point = [
                prev[0] + (cur[0] - prev[0]) * t,
                prev[1] + (cur[1] - prev[1]) * t,
            ];
            inside.push(point);
            outside.push(point);
        }
        if dc >= 0.0 {
            inside.push(cur);
        }
        if dc <= 0.0 {
            outside.push(cur);
        }
    }
    (inside, outside)
}
fn partition(subject: &[Point], clip: &[Point]) -> (Polygon, Vec<Polygon>) {
    let mut inside = subject.to_vec();
    let mut outside = Vec::new();
    let sign = area(clip).signum();
    for k in 0..clip.len() {
        let (next, rest) = cut(&inside, clip[k], clip[(k + 1) % clip.len()], sign);
        if rest.len() >= 3 && area(&rest).abs() > 1.0e-8 {
            outside.push(rest);
        }
        inside = next;
        if inside.len() < 3 {
            break;
        }
    }
    (inside, outside)
}
fn footprint(q: &Quad, canvas: [f32; 2]) -> Option<Polygon> {
    let p = q.pos.map(|p| p.map(f64::from));
    if !convex(&p) {
        return None;
    }
    // The facade represents arbitrary quadrilaterals, but only an affine source
    // map is independent of the original triangle diagonal. Reject warped maps.
    for j in 0..2 {
        if (p[0][j] + p[2][j] - p[1][j] - p[3][j]).abs() > 1.0 / 4096.0
            || (q.uv[0][j] + q.uv[2][j] - q.uv[1][j] - q.uv[3][j]).abs() > 1.0e-6
        {
            return None;
        }
    }
    let mut p = p.to_vec();
    if let Some(clip) = q.clip {
        let clip = clip.map(|p| p.map(f64::from));
        if !convex(&clip) {
            return None;
        }
        p = partition(&p, &clip).0;
    }
    let [w, h] = canvas.map(f64::from);
    Some(partition(&p, &[[0., 0.], [w, 0.], [w, h], [0., h]]).0)
}
fn uv(q: &Quad, p: Point) -> [f32; 2] {
    let x = [
        f64::from(q.pos[1][0] - q.pos[0][0]),
        f64::from(q.pos[1][1] - q.pos[0][1]),
    ];
    let y = [
        f64::from(q.pos[3][0] - q.pos[0][0]),
        f64::from(q.pos[3][1] - q.pos[0][1]),
    ];
    let p = [p[0] - f64::from(q.pos[0][0]), p[1] - f64::from(q.pos[0][1])];
    let d = x[0] * y[1] - x[1] * y[0];
    let u = (p[0] * y[1] - p[1] * y[0]) / d;
    let v = (x[0] * p[1] - x[1] * p[0]) / d;
    std::array::from_fn(|j| {
        (f64::from(q.uv[0][j])
            + u * f64::from(q.uv[1][j] - q.uv[0][j])
            + v * f64::from(q.uv[3][j] - q.uv[0][j])) as f32
    })
}
fn vertices(polys: &[Polygon], q: &Quad, second: Option<&Quad>, canvas: [f32; 2]) -> Vec<Vertex> {
    let mut vertices = Vec::new();
    let make = |p: Point| Vertex {
        pos: [
            (p[0] / f64::from(canvas[0]) * 2. - 1.) as f32,
            (1. - p[1] / f64::from(canvas[1]) * 2.) as f32,
        ],
        uv: uv(q, p),
        color: q.color,
        haze: second.map_or([[0.; 4]; 2], |s| {
            let v = uv(s, p);
            [[v[0], v[1], 0., 0.], s.color]
        }),
        soft: [[0.; 4]; 3],
    };
    for p in polys {
        for k in 1..p.len().saturating_sub(1) {
            vertices.extend([make(p[0]), make(p[k]), make(p[k + 1])]);
        }
    }
    vertices
}
fn op(q: &Quad) -> Option<PsOp> {
    match q.blend {
        Blend::Ps(op @ (PsOp::Darken | PsOp::HardLight | PsOp::Overlay | PsOp::Normal))
            if plain_ps_source(q) && !q.tex.native.srgb =>
        {
            Some(op)
        }
        _ => None,
    }
}

/// Only metadata/CPU geometry is inspected. All source owners remain in recorded
/// Draw commands, so deferred admission, scene read plans and retirement fences
/// continue to observe both sources. No GPU pixels are read by the CPU.
pub(super) fn pair(
    a: &Quad,
    b: &Quad,
    shaders: [EffectShader; 2],
    canvas: [f32; 2],
    physical: [u32; 2],
    white: &Rc<NativeTexture>,
    lut: &Rc<NativeTexture>,
) -> Option<Vec<Command>> {
    let (Some(oa), Some(ob)) = (op(a), op(b)) else {
        return None;
    };
    let Some(pa) = footprint(a, canvas) else {
        knife_perf::add(Metric::BlendChainFallback, 0, 1);
        return None;
    };
    let Some(pb) = footprint(b, canvas) else {
        knife_perf::add(Metric::BlendChainFallback, 0, 1);
        return None;
    };
    if pa.len() < 3 || pb.len() < 3 {
        return None;
    }
    let (intersection, outside_a) = partition(&pa, &pb);
    let (_, outside_b) = partition(&pb, &pa);
    let coverage = area(&intersection).abs();
    let density = f64::from(physical[0]) * f64::from(physical[1])
        / (f64::from(canvas[0]) * f64::from(canvas[1]));
    // Region splitting can turn two draw calls into three. Do it only for a
    // substantial overlap: the measured problem is large pixel-bound effects.
    // Small UI and slivers retain their original, cheaper command sequence.
    if coverage * density < 16_384. || coverage < area(&pa).abs().min(area(&pb).abs()) * 0.5 {
        knife_perf::add(Metric::BlendChainFallback, 0, 2);
        return None;
    }
    let single = |q: &Quad, shader, operation: PsOp, polys: &[Polygon]| Command::Draw {
        vertices: vertices(polys, q, None, canvas),
        shader,
        blend: EffectBlend::Opaque,
        textures: [
            Some(q.tex.native.clone()),
            Some(white.clone()),
            Some(white.clone()),
            Some(lut.clone()),
        ],
        params: [
            operation as u32 as f32,
            0.,
            physical[0] as f32,
            physical[1] as f32,
            -1.,
            0.,
            0.,
            0.,
        ],
    };
    let mut commands = Vec::with_capacity(3);
    if !outside_a.is_empty() {
        commands.push(single(a, shaders[0], oa, &outside_a));
    }
    commands.push(Command::Draw {
        vertices: vertices(&[intersection], a, Some(b), canvas),
        shader: EffectShader::PsChain2,
        blend: EffectBlend::Opaque,
        textures: [
            Some(a.tex.native.clone()),
            Some(white.clone()),
            Some(b.tex.native.clone()),
            Some(lut.clone()),
        ],
        params: [
            oa as u32 as f32,
            ob as u32 as f32,
            physical[0] as f32,
            physical[1] as f32,
            -1.,
            0.,
            0.,
            0.,
        ],
    });
    if !outside_b.is_empty() {
        commands.push(single(b, shaders[1], ob, &outside_b));
    }
    if commands.iter().any(|c| matches!(c,Command::Draw{vertices,..} if vertices.len()>96
        || vertices.iter().any(|v| v.pos.iter().chain(v.uv.iter()).chain(v.haze.iter().flatten()).any(|x|!x.is_finite())))) {
        knife_perf::add(Metric::BlendChainFallback,0,3);return None;
    }
    knife_perf::add(Metric::BlendChain, 0, 1);
    knife_perf::add(Metric::BlendChainCoverage, 0, (coverage * density) as u64);
    Some(commands)
}
