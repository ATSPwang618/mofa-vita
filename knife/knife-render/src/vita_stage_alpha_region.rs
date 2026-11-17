//! Conservative per-draw stage tile bounds for authenticated zero-alpha support.
//! Each physical pixel executes its original primitive order at most once.
use super::{EffectBlend, EffectShader, Vertex, executed_work::DrawWork};
use knife_perf::{Metric, Scope};

const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;
const TILE: u32 = 32;
const COLS: usize = (WIDTH / TILE) as usize;
const ROWS: usize = HEIGHT.div_ceil(TILE) as usize;
const MAX_VERTICES: usize = 24;

type Rect = [u32; 4];
#[derive(Clone, Copy, Debug)]
pub(super) struct RegionPlan {
    pub rectangles: [Rect; 2],
    pub count: usize,
    pub saved_pixels: u64,
}

fn bounds(rows: &[u32; ROWS]) -> Option<Rect> {
    let top = rows.iter().position(|row| *row != 0)?;
    let bottom = rows.iter().rposition(|row| *row != 0).unwrap();
    let columns = rows.iter().fold(0, |acc, &row| acc | row);
    Some([columns.trailing_zeros() * TILE, top as u32 * TILE,
        (32 - columns.leading_zeros()) * TILE, ((bottom as u32 + 1) * TILE).min(HEIGHT)])
}

fn coverage(work: DrawWork, vertices: &[Vertex], rect: Rect) -> u64 {
    let mut piece = work;
    piece.clip_region(vertices, rect);
    piece.pixels
}

fn mark_box(rows: &mut [u32; ROWS], box_px: Rect) {
    let [l, t, r, b] = box_px;
    let set = (1u32 << r) - (1u32 << l);
    for row in &mut rows[t as usize..b as usize] { *row |= set; }
}

fn mark_triangle(rows: &mut [u32; ROWS], triangle: &[Vertex], input: [u32; 2], support: &[[u16; 4]]) -> Option<()> {
    let uvs = std::array::from_fn::<_, 3, _>(|i| triangle[i].uv.map(f64::from));
    let [s, t, z] = uvs;
    let x = [t[0]-s[0], t[1]-s[1]];
    let y = [z[0]-s[0], z[1]-s[1]];
    let det = x[0]*y[1]-x[1]*y[0];
    if det.abs() < 1e-8 { return None; }
    let screen = std::array::from_fn::<_, 3, _>(|i| [(f64::from(triangle[i].pos[0])+1.)*f64::from(WIDTH)*0.5,
        (1.-f64::from(triangle[i].pos[1]))*f64::from(HEIGHT)*0.5]);
    let gradient = std::array::from_fn::<_, 2, _>(|axis| {
        let u=screen[1][axis]-screen[0][axis];
        let v=screen[2][axis]-screen[0][axis];
        [(u*y[1]-v*x[1])/det, (v*x[0]-u*y[0])/det]
    });
    let uv_low = std::array::from_fn::<_,2,_>(|i| s[i].min(t[i]).min(z[i]));
    let uv_high = std::array::from_fn::<_,2,_>(|i| s[i].max(t[i]).max(z[i]));
    let to_tiles = |lo: f64, hi: f64, limit: usize| -> [u32;2] {
        // Preserve a physical pixel on both sides of each projected padded
        // source cell for interpolation and tile-border rounding.
        [( (lo-1.) / f64::from(TILE)).floor().clamp(0., limit as f64) as u32,
         (((hi+1.) / f64::from(TILE)).floor()+1.).clamp(0., limit as f64) as u32]
    };
    for &[l,t,r,b] in support {
        // The catalog already expands the bilinear BC3 support by two texels.
        let u0=(f64::from(l)/f64::from(input[0])).max(uv_low[0]);
        let v0=(f64::from(t)/f64::from(input[1])).max(uv_low[1]);
        let u1=(f64::from(r)/f64::from(input[0])).min(uv_high[0]);
        let v1=(f64::from(b)/f64::from(input[1])).min(uv_high[1]);
        if u0>u1 || v0>v1 { continue; }
        let ext = |axis:usize| {
            let [a,b]=gradient[axis];
            let u=[a*(u0-s[0]),a*(u1-s[0])];
            let v=[b*(v0-s[1]),b*(v1-s[1])];
            [screen[0][axis]+u[0].min(u[1])+v[0].min(v[1]),screen[0][axis]+u[0].max(u[1])+v[0].max(v[1])]
        };
        let [x0,x1]=ext(0);let [y0,y1]=ext(1);
        let [l,r]=to_tiles(x0,x1,COLS);let [t,b]=to_tiles(y0,y1,ROWS);
        if l<r && t<b { mark_box(rows,[l,t,r,b]); }
    }
    Some(())
}

pub(super) fn plan(vertices: &[Vertex], work: DrawWork, input: [u32;2], support: &[[u16;4]],
    split: bool) -> Option<RegionPlan>
{
    if work.target_size != [WIDTH,HEIGHT] || work.pixels < 45_000 || input.contains(&0)
        || vertices.len() > MAX_VERTICES || vertices.len() < 6 || vertices.len()%3 != 0
        || vertices.iter().any(|v| v.pos.iter().any(|p| !p.is_finite())
            || v.uv.iter().any(|p| !(0. ..=1.).contains(p))) { return None; }
    let mut rows=[0;ROWS];
    for triangle in vertices.chunks_exact(3) { mark_triangle(&mut rows, triangle,input,support)?; }
    let best=bounds(&rows)?;
    let single=coverage(work,vertices,best);
    let mut chosen=RegionPlan{rectangles:[best,[0;4]],count:1,saved_pixels:work.pixels.saturating_sub(single)};
    if split {
        for (axis, limit) in [COLS,ROWS].into_iter().enumerate() {
            for seam in 1..limit {
                let bit_prefix=(1u32<<seam)-1;
                let earlier=std::array::from_fn(|i|if axis==0 { rows[i] & bit_prefix } else if i<seam { rows[i] } else { 0 });
                let later=std::array::from_fn(|i|rows[i] & !earlier[i]);
                let (Some(a),Some(b))=(bounds(&earlier),bounds(&later)) else {continue;};
                let cost=coverage(work,vertices,a)+coverage(work,vertices,b);
                let save=work.pixels.saturating_sub(cost);
                if save>chosen.saved_pixels {
                    chosen=RegionPlan{rectangles:[a,b],count:2,saved_pixels:save};
                }
            }
        }
    }
    let min_saved=if chosen.count==1 { 20_000 } else { 45_000 };
    (chosen.saved_pixels>=min_saved && chosen.saved_pixels.saturating_mul(5)>=work.pixels)
        .then_some(chosen)
}


pub(super) struct Source {
    pub index: usize,
    pub size: [u32; 2],
    pub support: &'static [[u16; 4]],
}

pub(super) fn for_draw(source: Source, vertices: &[Vertex], work: DrawWork,
    shader: EffectShader, blend: EffectBlend, params: &[f32; 8]) -> Option<RegionPlan>
{
    use EffectShader::*;
    let split = match (source.index, shader, blend) {
        // The reviewed PS7 fetch implementations return the original target
        // register when source alpha is zero. Source material here is Sprite.
        (96272, PsFetch | PsFetch7 | PsPlain7 | PsAll7 | PsHalf7 | PsMaterial92, EffectBlend::Opaque)
            if params[0] == 7.0 && params[1] == 0.0 && params[4] < 0.0
                && params[6] == 0.0 && params[7] == 0.0 => true,
        // fs_fill premultiplies tint by coverage, with destination alpha
        // unchanged by fixed Alpha blending. IMG0617 only needs one region.
        (96328, Fill, EffectBlend::Alpha) => true,
        (98107, Fill, EffectBlend::Alpha) => false,
        _ => return None,
    };
    if vertices.iter().any(|vertex| vertex.haze != [[0.; 4]; 2]
        || vertex.soft != [[0.; 4]; 3]
        || vertex.color.iter().any(|factor| !(0.0..=1.0).contains(factor)))
    {
        return None;
    }
    let _plan = Scope::new(Metric::StageAlphaRegionPlan);
    plan(vertices, work, source.size, source.support, split)
}
