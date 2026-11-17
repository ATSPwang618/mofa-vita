//! Full-clear independent group producers may run before the stage. Their
//! retained RGBA8 outputs are then sampled in the original stage/PS order.
#[cfg(not(test))]
use knife_render::{Blend, CommandEncoder, CoverageBound, PsOp, Quad, RenderTarget, Renderer, Shade, TexRef};
#[cfg(test)]
use crate::vita::{Blend, CommandEncoder, CoverageBound, PsOp, Quad, RenderTarget, Renderer, Shade, TexRef};
use super::group_cache;
use knife_perf::Metric;

#[path = "group_overwrite.rs"]
mod group_overwrite;

pub(super) enum Draw {
    Stage(Quad),
    Pixel { quad: Quad, clear: bool },
    Mask { quad: Quad, clear: bool },
    Composite { blend: Blend, alpha: f32 },
}
enum Action { Stage(Quad), Composite(usize, Blend, f32, CoverageBound) }
struct Plan {
    producers: Vec<[Vec<Quad>; 2]>,
    actions: Vec<Action>,
    sources: Vec<Quad>,
    overwritten: usize,
}

enum Reject {
    Structure(u64),
    TooFew,
    TooMany,
}

fn plan(draws: Vec<Draw>, merge_clears: bool) -> Result<Plan, Reject> {
    let mut groups = Vec::new();
    let mut actions = Vec::new();
    let mut pending: [Vec<Quad>; 2] = [Vec::new(), Vec::new()];
    let mut sources = Vec::new();
    let mut overwritten = 0;
    for draw in draws {
        match draw {
            Draw::Stage(q) => { sources.push(q.clone()); actions.push(Action::Stage(q)); }
            Draw::Pixel { quad, clear } => {
                if pending[0].is_empty() && !clear { return Err(Reject::Structure(1)); }
                if clear && !pending[0].is_empty() {
                    if !merge_clears { return Err(Reject::Structure(1)); }
                    overwritten += pending[0].len();
                    pending[0].clear();
                }
                sources.push(quad.clone()); pending[0].push(quad);
            }
            Draw::Mask { quad, clear } => {
                if pending[1].is_empty() && !clear { return Err(Reject::Structure(1)); }
                if clear && !pending[1].is_empty() {
                    if !merge_clears { return Err(Reject::Structure(1)); }
                    overwritten += pending[1].len();
                    pending[1].clear();
                }
                sources.push(quad.clone()); pending[1].push(quad);
            }
            Draw::Composite { blend, alpha } => {
                if pending.iter().any(Vec::is_empty) { return Err(Reject::Structure(2)); }
                if !alpha.is_finite() { return Err(Reject::Structure(3)); }
                actions.push(Action::Composite(groups.len(), blend, alpha, CoverageBound::Unknown));
                groups.push(std::mem::take(&mut pending));
                if groups.len() * 2 > knife_perf::memory_budget::GPU_GROUP_ENTRIES { return Err(Reject::TooMany); }
            }
        }
    }
    // No trailing pair mutation is moved or dropped. Every retained result has
    // exactly the same producer clear and per-target draw sequence as before.
    if groups.is_empty() || (groups.len() == 1 && !merge_clears) { return Err(Reject::TooFew); }
    if pending.iter().any(|q| !q.is_empty()) { return Err(Reject::Structure(4)); }
    Ok(Plan { producers: groups, actions, sources, overwritten })
}

fn coverage_bound(pending: &[Vec<Quad>; 2], logical: [u32; 2], physical: [u32; 2]) -> CoverageBound {
    if logical.contains(&0) || physical.contains(&0) { return CoverageBound::Unknown; }
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let mut seen = false;
    for quad in pending.iter().flatten() {
        for point in quad.pos.iter().chain(quad.clip.iter().flatten()) {
            if !point[0].is_finite() || !point[1].is_finite() { return CoverageBound::Unknown; }
            min_x = min_x.min(point[0]); max_x = max_x.max(point[0]);
            min_y = min_y.min(point[1]); max_y = max_y.max(point[1]); seen = true;
        }
    }
    if !seen || min_x >= max_x || min_y >= max_y { return CoverageBound::Unknown; }
    // Map both endpoints independently and outwards. Rounding the width after
    // rounding its origin can lose a physical pixel at odd script coordinates.
    let sx = f64::from(physical[0]) / f64::from(logical[0]);
    let sy = f64::from(physical[1]) / f64::from(logical[1]);
    let x0 = (f64::from(min_x) * sx).floor().clamp(0.0, f64::from(physical[0])) as u32;
    let y0 = (f64::from(min_y) * sy).floor().clamp(0.0, f64::from(physical[1])) as u32;
    let x1 = (f64::from(max_x) * sx).ceil().clamp(0.0, f64::from(physical[0])) as u32;
    let y1 = (f64::from(max_y) * sy).ceil().clamp(0.0, f64::from(physical[1])) as u32;
    if x0 >= x1 || y0 >= y1 { return CoverageBound::Unknown; }
    CoverageBound::rect(x0, y0, x1 - x0, y1 - y0)
}

/// A full-clear mask can only contribute inside its geometry. In an independent
/// intermediate group, a disjoint pixel draw is unobservable after the mask is
/// sampled. Keep two physical texels for filtering and raster precision.
fn reject_masked_producers(plan: &mut Plan, logical: [u32; 2], physical: [u32; 2]) {
    if logical.contains(&0) || physical.contains(&0) { return; }
    let last = plan.producers.len() - 1;
    let padding = std::array::from_fn::<_, 2, _>(|i| 2. * logical[i] as f32 / physical[i] as f32);
    let mut removed = 0;
    for action in &plan.actions {
        // The final pair persists into the next frame, including its off-mask
        // contents. Non-Alpha composites may write even for a zero mask.
        let Action::Composite(index, Blend::Alpha, _, _) = action else { continue; };
        if *index == last { continue; }
        let [pixels, masks] = &mut plan.producers[*index];
        let points = || masks.iter().flat_map(|q| q.pos.iter().copied());
        if !points().flatten().all(f32::is_finite) { continue; }
        let lo = std::array::from_fn::<_, 2, _>(|i|
            points().map(|p| p[i]).fold(f32::INFINITY, f32::min) - padding[i]);
        let hi = std::array::from_fn::<_, 2, _>(|i|
            points().map(|p| p[i]).fold(f32::NEG_INFINITY, f32::max) + padding[i]);
        let count = pixels.len();
        pixels.retain(|q| {
            let plain = q.rule.is_none() && q.soft.is_none() && !q.masked
                && q.haze.iter().flatten().all(|&v| v == 0.)
                && q.pos.iter().flatten().chain(q.uv.iter().flatten()).chain(&q.color)
                    .chain(q.clip.iter().flatten().flatten()).all(|v| v.is_finite());
            !plain || !(0..2).any(|i| q.pos.iter().all(|p| p[i] < lo[i])
                || q.pos.iter().all(|p| p[i] > hi[i]))
        });
        removed += count - pixels.len();
    }
    if removed == 0 { return; }
    let mut remap = Vec::with_capacity(plan.producers.len());
    let mut next = 0;
    plan.producers.retain(|g| {
        let keep = !g[0].is_empty();
        remap.push(keep.then_some(next));
        next += usize::from(keep);
        keep
    });
    plan.actions.retain_mut(|action| {
        let Action::Composite(index, _, _, _) = action else { return true; };
        let Some(next) = remap[*index] else { return false; };
        *index = next;
        true
    });
    knife_perf::add(Metric::GroupMaskReject, removed as u64, (remap.len() - next) as u64);
}

/// A full-clear Alpha mask limits the observable pixels of an intermediate
/// producer. Use an outward physical tile bound, leaving the original vertices
/// and interpolants for all visible tiles. The final persistent pair stays full.
fn quad_bound(q: &Quad) -> Option<[f64; 4]> {
    if !q.pos.iter().flatten().all(|value| value.is_finite()) { return None; }
    let rect = q.clip.filter(|c| c.iter().flatten().all(|v| v.is_finite())
        && c[0][0] == c[3][0] && c[1][0] == c[2][0]
        && c[0][1] == c[1][1] && c[2][1] == c[3][1]
        && c[0][0] < c[2][0] && c[0][1] < c[2][1]);
    let mut bounds = [0.; 4];
    for i in 0..2 {
        bounds[i] = q.pos.iter().map(|p| f64::from(p[i])).fold(f64::INFINITY, f64::min);
        bounds[i + 2] = q.pos.iter().map(|p| f64::from(p[i])).fold(f64::NEG_INFINITY, f64::max);
        if let Some(c) = rect {
            bounds[i] = bounds[i].max(f64::from(c[0][i]));
            bounds[i + 2] = bounds[i + 2].min(f64::from(c[2][i]));
        }
    }
    Some(bounds)
}

fn producer_clip(masks: &[Quad], logical: [u32; 2], physical: [u32; 2]) -> Option<[u32; 4]> {
    if logical.contains(&0) || physical.contains(&0) { return None; }
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for q in masks {
        let bound = quad_bound(q)?;
        for i in 0..2 {
            lo[i] = lo[i].min(bound[i]);
            hi[i] = hi[i].max(bound[i + 2]);
        }
    }
    let mut region = [0; 4];
    for i in 0..2 {
        let scale = f64::from(physical[i]) / f64::from(logical[i]);
        let limit = f64::from(physical[i]);
        // Two physical pixels cover raster snapping and bilinear group sampling.
        let first = (lo[i] * scale - 2.).floor().clamp(0., limit) as u32;
        let end = (hi[i] * scale + 2.).ceil().clamp(0., limit) as u32;
        if first >= end { return None; }
        region[i] = first / 32 * 32;
        region[i + 2] = (end.div_ceil(32) * 32).min(physical[i]);
    }
    (region != [0, 0, physical[0], physical[1]]).then_some(region)
}

fn has_off_mask_draw(pixels: &[Quad], region: [u32; 4], logical: [u32; 2], physical: [u32; 2]) -> bool {
    pixels.iter().filter_map(quad_bound).any(|q| {
        (0..2).any(|axis| {
            let limit = f64::from(physical[axis]);
            let scale = limit / f64::from(logical[axis]);
            (q[axis] * scale).clamp(0., limit) < f64::from(region[axis])
                || (q[axis + 2] * scale).clamp(0., limit) > f64::from(region[axis + 2])
        })
    })
}

fn shared_pixel_producer(producers: &[[Vec<Quad>; 2]], current: usize) -> bool {
    let quads = &producers[current][0];
    producers.iter().enumerate().any(|(index, group)| {
        let other = &group[0];
        index != current && quads.len() == other.len() && quads.iter().zip(other)
            .all(|(a,b)| a.render_key() == b.render_key())
    })
}

/// The final Alpha composite overwrites every RGB pixel and preserves the
/// original stage alpha of one. Both of its producers have full clears (from
/// plan()), so the earlier pair cannot affect that output or its live owners.
fn discard_covered_prefix(plan: &mut Plan, white: &TexRef, clear: [f32; 4],
    logical: [u32; 2], physical: [u32; 2], opaque_masks: &mut Vec<bool>) -> usize
{
    let last = plan.producers.len() - 1;
    let Some((Action::Composite(index, Blend::Alpha, alpha, coverage), earlier)) =
        plan.actions.split_last() else { return 0; };
    if last == 0 || *index != last || clear[3] != 1. || *alpha != 1. || !opaque_masks[last]
        || *coverage != CoverageBound::rect(0, 0, physical[0], physical[1])
        || !earlier.iter().all(|action| match action {
            Action::Stage(q) => group_overwrite::keeps_one_alpha(q),
            Action::Composite(_, Blend::Alpha, alpha, _) => alpha.is_finite(),
            _ => false,
        })
    {
        return 0;
    }
    let [pixels, masks] = &plan.producers[last];
    let [mask] = masks.as_slice() else { return 0; };
    // Restrict admission to the original, hash-verified opaque BC3 mask
    // sources. The general-purpose white-mask reuse can run independently.
    if std::sync::Arc::ptr_eq(&mask.tex, white)
        || !group_overwrite::opaque_pixels(pixels, mask, logical, physical)
    {
        return 0;
    }

    let discarded = earlier.len();
    let final_action = Action::Composite(0, Blend::Alpha, 1., *coverage);
    let final_group = plan.producers.pop().expect("complete final group");
    plan.sources = final_group.iter().flatten().cloned().collect();
    plan.producers.clear();
    plan.producers.push(final_group);
    plan.actions.clear();
    plan.actions.push(final_action);
    opaque_masks.truncate(1);
    opaque_masks[0] = true;
    discarded
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render(r: &mut Renderer, enc: &mut CommandEncoder,
    stage: &RenderTarget, pair: &mut [RenderTarget; 2], scratch: &RenderTarget,
    cache: &mut group_cache::Cache, clear: [f32; 4], draws: Vec<Draw>, live_charge: bool) -> bool
{
    let exact_masks = knife_perf::optimizations::enabled(knife_perf::optimizations::GROUP_OPAQUE_MASK);
    let mut plan = match plan(draws, exact_masks) {
        Ok(plan) => plan,
        Err(Reject::Structure(reason)) => {
            knife_perf::add(Metric::GroupBatchRejectStructure, 0, reason);
            knife_perf::add(Metric::GroupBatchRejectPlan, 0, 1);
            knife_perf::add(Metric::GroupBatchBypass, 0, 1);
            return false;
        }
        Err(Reject::TooFew) => {
            knife_perf::add(Metric::GroupBatchRejectTooFew, 0, 1);
            knife_perf::add(Metric::GroupBatchRejectPlan, 0, 2);
            knife_perf::add(Metric::GroupBatchBypass, 0, 1);
            return false;
        }
        Err(Reject::TooMany) => {
            knife_perf::add(Metric::GroupBatchRejectTooMany, 0, 1);
            knife_perf::add(Metric::GroupBatchRejectPlan, 0, 3);
            knife_perf::add(Metric::GroupBatchBypass, 0, 1);
            return false;
        }
    };
    let logical = [stage.tex.width, stage.tex.height];
    let physical = stage.tex.physical_size();
    let opaque = |group: &[Vec<Quad>; 2]| exact_masks
        && matches!(group[1].as_slice(), [mask] if mask.vita_group_mask_opaque(logical, &r.white));
    if plan.producers.len() == 1 && plan.overwritten == 0 && !opaque(&plan.producers[0]) {
        knife_perf::add(Metric::GroupBatchRejectTooFew, 0, 1);
        knife_perf::add(Metric::GroupBatchBypass, 0, 1);
        return false;
    }
    for action in &mut plan.actions {
        if let Action::Composite(index, _, _, coverage) = action {
            *coverage = coverage_bound(&plan.producers[*index], logical, physical);
        }
    }
    // Alias rejection includes all sampler/rule inputs, not only the primary
    // texture. These values must not observe a stage or shared-pair mutation.
    let stage_id = stage.tex.render_identity().0;
    let pair_ids = pair.each_ref().map(|t| t.tex.render_identity().0);
    if plan.sources.iter().any(|q| std::iter::once(&q.tex)
        .chain(q.rule.as_ref().map(|m| &m.tex)).any(|source| {
            let id = source.render_identity().0;
            id == stage_id || pair_ids.contains(&id) || cache.owns_texture(source)
        })) {
        knife_perf::add(Metric::GroupBatchRejectAlias, 0, 1);
        knife_perf::add(Metric::GroupBatchBypass, 0, 2);
        return false;
    }
    if !plan.actions.iter().all(|a| match a {
        Action::Composite(_, blend, _, _) => r.vita_group_fused_available(stage, *blend).unwrap_or(false),
        Action::Stage(_) => true,
    }) {
        knife_perf::add(Metric::GroupBatchRejectCapability, 0, 1);
        knife_perf::add(Metric::GroupBatchBypass, 0, 3);
        return false;
    }
    if knife_perf::optimizations::enabled(knife_perf::optimizations::GROUP_MASK_REJECT) {
        reject_masked_producers(&mut plan, logical, physical);
    }
    let mut opaque_masks: Vec<_> = plan.producers.iter().map(opaque).collect();
    let discarded = if live_charge && knife_perf::optimizations::enabled(knife_perf::optimizations::GROUP_OPAQUE_OVERWRITE) {
        discard_covered_prefix(&mut plan, &r.white, clear, logical, physical, &mut opaque_masks)
    } else { 0 };
    let mut clips = vec![None; plan.producers.len()];
    if knife_perf::optimizations::enabled(knife_perf::optimizations::GROUP_MASK_CLIP) {
        let last = plan.producers.len() - 1;
        for action in &plan.actions {
            if let Action::Composite(index, Blend::Alpha, _, _) = action
                && *index < last
                && !opaque_masks[*index]
                && let Some(clip) = producer_clip(&plan.producers[*index][1], logical, physical)
                && has_off_mask_draw(&plan.producers[*index][0], clip, logical, physical)
                && !shared_pixel_producer(&plan.producers, *index) {
                clips[*index] = Some(clip);
            }
        }
    }
    let Some(keys) = cache.prepare_retained(r, enc, pair, &plan.producers, &plan.sources, &clips, &opaque_masks) else {
        knife_perf::add(Metric::GroupBatchRejectAdmission, 0, 1);
        knife_perf::add(Metric::GroupBatchBypass, 0, 3);
        return false;
    };
    // Everything that can refuse the optional path ran before this point.
    // Producer recording never falls back after a partially completed plan.
    let mut results = Vec::with_capacity(plan.producers.len());
    for (((group, clip), opaque), [pixel_key, mask_key]) in
        plan.producers.iter().zip(clips).zip(opaque_masks).zip(keys) {
        cache.draw_with_clip(r, enc, pair, 0, true, &group[0], true, clip, false, Some(pixel_key));
        cache.draw_with_clip(r, enc, pair, 1, true, &group[1], true, None, opaque, Some(mask_key));
        cache.retain_pair(pair);
        results.push(pair.each_ref().map(|t| RenderTarget { tex: t.tex.clone() }));
    }
    let before = r.vita_target_passes(enc, stage).expect("counting group stage passes");
    let mut clear = Some(clear);
    let mut run = Vec::new();
    for action in plan.actions {
        match action {
            Action::Stage(q) => run.push(q),
            Action::Composite(index, blend, alpha, coverage) => {
                r.draw(enc, stage, clear.take(), &std::mem::take(&mut run));
                let [pixel, mask] = &results[index];
                r.try_group_composite_via_scratch_region(enc, stage, pixel, mask, scratch, blend, alpha, coverage)
                    .expect("recording retained group composite");
            }
        }
    }
    r.draw(enc, stage, clear, &run);
    let passes = r.vita_target_passes(enc, stage).expect("counting retained stage passes") - before;
    knife_perf::add(Metric::GroupBatchPlan, 0, results.len() as u64);
    knife_perf::add(Metric::GroupBatchStagePasses, 0, passes as u64);
    if plan.overwritten != 0 { knife_perf::add(Metric::GroupClearOverwrite, 0, plan.overwritten as u64); }
    if discarded != 0 { knife_perf::add(Metric::GroupOpaqueOverwrite, 0, discarded as u64); }
    cache.finish_retained();
    true
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    fn producers(rect: [f32; 4]) -> [Vec<Quad>; 2] {
        let renderer = Renderer::new().unwrap();
        let source = renderer.create_target(1, 1, "bound source");
        [vec![Quad::rect(source.tex, rect, [1.0; 4])], Vec::new()]
    }

    #[test]
    fn group_bounds_round_each_endpoint_outwards_at_odd_script_coordinates() {
        let quads = producers([1.0, 1.0, 4.0, 4.0]);
        assert_eq!(coverage_bound(&quads, [1920, 1080], [960, 540]),
            CoverageBound::rect(0, 0, 3, 3));
    }

    #[test]
    fn group_bounds_use_actual_target_ratio_and_clamp_to_its_physical_size() {
        let quads = producers([1.0, 3.0, 4.0, 4.0]);
        assert_eq!(coverage_bound(&quads, [10, 10], [7, 3]),
            CoverageBound::rect(0, 0, 4, 3));
        let quads = producers([-4.0, -2.0, 20.0, 20.0]);
        assert_eq!(coverage_bound(&quads, [10, 10], [7, 3]),
            CoverageBound::rect(0, 0, 7, 3));
    }

    #[test]
    fn group_bounds_keep_unknown_for_invalid_geometry_or_target_layout() {
        let quads = producers([1.0, 1.0, 4.0, 4.0]);
        assert_eq!(coverage_bound(&quads, [0, 10], [7, 3]), CoverageBound::Unknown);
        assert_eq!(coverage_bound(&quads, [10, 10], [7, 0]), CoverageBound::Unknown);
        let mut quads = quads;
        quads[0][0].pos[0][0] = f32::NAN;
        assert_eq!(coverage_bound(&quads, [10, 10], [7, 3]), CoverageBound::Unknown);
    }
}
