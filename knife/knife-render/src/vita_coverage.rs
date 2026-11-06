//! Conservative nonzero RGBA coverage follows the native owner and generation.
//! Predictions are private to their recording encoder until submission succeeds.
use super::{Command, CoverageBound, NativeTexture, Vertex};
use std::{rc::Rc, sync::atomic::{AtomicU64, Ordering}};

pub(super) struct GroupCut {
    sources: [Rc<NativeTexture>; 2],
    versions: [u64; 2],
    color: [f32; 4],
}
impl GroupCut {
    pub(super) fn new(sources: [Rc<NativeTexture>; 2], color: [f32; 4]) -> Self {
        let versions = sources.each_ref().map(|source| source.content_version.get());
        Self { sources, versions, color }
    }
    fn valid(&self) -> bool {
        self.sources.iter().zip(self.versions).all(|(source, version)| source.content_version.get() == version)
    }
}

static NEXT_ENCODER: AtomicU64 = AtomicU64::new(1);
pub(super) fn encoder_id() -> u64 { NEXT_ENCODER.fetch_add(1, Ordering::Relaxed) }

#[derive(Clone, Copy)]
pub(super) struct Stamp {
    pub version: u64,
    pub pending: u64,
    pub bound: CoverageBound,
}
impl Stamp {
    pub const fn unknown(version: u64) -> Self {
        Self { version, pending: 0, bound: CoverageBound::Unknown }
    }
}

pub(super) fn union(a: CoverageBound, b: CoverageBound) -> CoverageBound {
    use CoverageBound::*;
    match (a, b) {
        (Unknown, _) | (_, Unknown) => Unknown,
        (Full, _) | (_, Full) => Full,
        (Empty, b) => b,
        (a, Empty) => a,
        (Rect { x: ax, y: ay, width: aw, height: ah }, Rect { x: bx, y: by, width: bw, height: bh }) => {
            let (Some(ar), Some(ab), Some(br), Some(bb)) = (ax.checked_add(aw), ay.checked_add(ah), bx.checked_add(bw), by.checked_add(bh)) else { return Unknown; };
            let (x, y) = (ax.min(bx), ay.min(by));
            CoverageBound::rect(x, y, ar.max(br) - x, ab.max(bb) - y)
        }
    }
}

pub(super) fn intersection(a: CoverageBound, b: CoverageBound) -> CoverageBound {
    use CoverageBound::*;
    match (a, b) {
        (Unknown, _) | (_, Unknown) => Unknown,
        (Empty, _) | (_, Empty) => Empty,
        (Full, b) => b,
        (a, Full) => a,
        (Rect { x: ax, y: ay, width: aw, height: ah }, Rect { x: bx, y: by, width: bw, height: bh }) => {
            let (Some(ar), Some(ab), Some(br), Some(bb)) = (ax.checked_add(aw), ay.checked_add(ah), bx.checked_add(bw), by.checked_add(bh)) else { return Unknown; };
            let (x, y, r, bottom) = (ax.max(bx), ay.max(by), ar.min(br), ab.min(bb));
            if x >= r || y >= bottom { Empty } else { CoverageBound::rect(x, y, r - x, bottom - y) }
        }
    }
}

fn geometry(vertices: &[Vertex], size: [u32; 2]) -> CoverageBound {
    if size.contains(&0) || vertices.len() % 3 != 0 { return CoverageBound::Unknown; }
    if vertices.is_empty() { return CoverageBound::Empty; }
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for vertex in vertices {
        if !vertex.pos.iter().all(|v| v.is_finite()) { return CoverageBound::Unknown; }
        let pos = [(f64::from(vertex.pos[0]) + 1.0) * 0.5 * f64::from(size[0]),
            (1.0 - f64::from(vertex.pos[1])) * 0.5 * f64::from(size[1])];
        for axis in 0..2 { lo[axis] = lo[axis].min(pos[axis]); hi[axis] = hi[axis].max(pos[axis]); }
    }
    let lo = std::array::from_fn::<_, 2, _>(|axis| lo[axis].floor().clamp(0.0, f64::from(size[axis])) as u32);
    let hi = std::array::from_fn::<_, 2, _>(|axis| hi[axis].ceil().clamp(0.0, f64::from(size[axis])) as u32);
    if lo[0] >= hi[0] || lo[1] >= hi[1] { CoverageBound::Empty }
    else if lo == [0, 0] && hi == size { CoverageBound::Full }
    else { CoverageBound::rect(lo[0], lo[1], hi[0] - lo[0], hi[1] - lo[1]) }
}

pub(super) fn record(commands: &[Command], id: u64, mut active: Option<Rc<NativeTexture>>) -> Option<Rc<NativeTexture>> {
    let enabled = knife_perf::optimizations::enabled(knife_perf::optimizations::GROUP_BOUNDS);
    for command in commands {
        match command {
            Command::Begin { target, clear } => {
                if !enabled { target.changed(); active = Some(target.clone()); continue; }
                let bound = match clear {
                    Some(color) if color.iter().all(|v| *v == 0.0) => CoverageBound::Empty,
                    Some(_) => CoverageBound::Full,
                    None => target.coverage_for(id),
                };
                target.predict_coverage(id, bound);
                active = Some(target.clone());
            }
            Command::End => active = None,
            Command::GroupCut(_) => {},
            Command::Draw { vertices, .. } => {
                if let Some(target) = &active {
                    if !enabled { target.changed(); continue; }
                    target.predict_coverage(id, union(target.coverage_for(id), geometry(vertices, target.size)));
                }
            }
            Command::Copy { src, dst, .. } => {
                if !enabled { dst.changed(); continue; }
                let bound = if src.size == dst.size { src.coverage_for(id) } else { CoverageBound::Unknown };
                dst.predict_coverage(id, bound);
            }
        }
    }
    active
}

fn destinations(commands: &[Command], mut f: impl FnMut(&NativeTexture)) {
    for command in commands {
        match command {
            Command::Begin { target, .. } => f(target),
            Command::Copy { dst, .. } => f(dst),
            _ => {},
        }
    }
}

pub(super) fn invalidate(commands: &[Command]) { destinations(commands, NativeTexture::changed); }

pub(super) fn before_submit(commands: &mut [Command], id: u64) {
    let enabled = knife_perf::optimizations::enabled(knife_perf::optimizations::GROUP_BOUNDS);
    let cuts_invalid = commands.iter().any(|command| match command {
        Command::GroupCut(cut) => !enabled || !cut.valid(), _ => false,
    });
    if cuts_invalid {
        // Restoring every cut also covers dependencies between group outputs in
        // this command stream. Producers, clear/copy order and draw count stay
        // unchanged; only final composite geometry/UVs regain the original six.
        for i in 0..commands.len() {
            let (before, after) = commands.split_at_mut(i + 1);
            if let Command::GroupCut(cut) = &before[i] {
                if let Some(Command::Draw { vertices, work, .. }) = after.iter_mut().find(|command|
                    matches!(command, Command::Draw { .. })) {
                    vertices.clear();
                    vertices.extend_from_slice(&super::fullscreen_verts(cut.color));
                    work.refresh_geometry(vertices);
                }
            }
        }
        invalidate(commands);
    }
    if !enabled { return; }
    destinations(commands, |target| {
        // A different encoder or an external write may have superseded this
        // prediction. Executing older commands cannot preserve the newer bound.
        if target.coverage.get().pending != id { target.changed(); }
    });
}

pub(super) fn finish(commands: &[Command], id: u64, success: bool) {
    destinations(commands, |target| {
        let mut stamp = target.coverage.get();
        if stamp.pending == id {
            if success { stamp.pending = 0; target.coverage.set(stamp); }
            else { target.changed(); }
        }
    });
}
