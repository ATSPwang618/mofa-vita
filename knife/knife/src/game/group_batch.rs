//! Full-clear independent group producers may run before the stage. Their
//! retained RGBA8 outputs are then sampled in the original stage/PS order.
#[cfg(not(test))]
use knife_render::{Blend, CommandEncoder, CoverageBound, Quad, RenderTarget, Renderer};
#[cfg(test)]
use crate::vita::{Blend, CommandEncoder, CoverageBound, Quad, RenderTarget, Renderer};
use super::group_cache;
use knife_perf::Metric;

pub(super) enum Draw {
    Stage(Quad),
    Pixel { quad: Quad, clear: bool },
    Mask { quad: Quad, clear: bool },
    Composite { blend: Blend, alpha: f32 },
}
enum Action { Stage(Quad), Composite(usize, Blend, f32, CoverageBound) }
struct Plan { producers: Vec<[Vec<Quad>; 2]>, actions: Vec<Action>, sources: Vec<Quad> }

enum Reject {
    Structure(u64),
    TooFew,
    TooMany,
}

fn plan(draws: Vec<Draw>) -> Result<Plan, Reject> {
    let mut groups = Vec::new();
    let mut actions = Vec::new();
    let mut pending: [Vec<Quad>; 2] = [Vec::new(), Vec::new()];
    let mut sources = Vec::new();
    for draw in draws {
        match draw {
            Draw::Stage(q) => { sources.push(q.clone()); actions.push(Action::Stage(q)); }
            Draw::Pixel { quad, clear } => {
                // The first write must discard the persistent previous pair.
                // A second clear or a no-clear dependency crosses this exact
                // subset's boundary; the original renderer handles all of it.
                if clear != pending[0].is_empty() { return Err(Reject::Structure(1)); }
                sources.push(quad.clone()); pending[0].push(quad);
            }
            Draw::Mask { quad, clear } => {
                if clear != pending[1].is_empty() { return Err(Reject::Structure(1)); }
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
    if groups.len() < 2 { return Err(Reject::TooFew); }
    if pending.iter().any(|q| !q.is_empty()) { return Err(Reject::Structure(4)); }
    Ok(Plan { producers: groups, actions, sources })
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

#[allow(clippy::too_many_arguments)]
pub(super) fn render(r: &mut Renderer, enc: &mut CommandEncoder,
    stage: &RenderTarget, pair: &mut [RenderTarget; 2], scratch: &RenderTarget,
    cache: &mut group_cache::Cache, clear: [f32; 4], draws: Vec<Draw>,
    mut shape: impl FnMut(u8, u8)) -> bool
{
    let mut plan = match plan(draws) {
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
    if !cache.prepare_retained(r, enc, pair, &plan.producers, &plan.sources) {
        knife_perf::add(Metric::GroupBatchRejectAdmission, 0, 1);
        knife_perf::add(Metric::GroupBatchBypass, 0, 3);
        return false;
    }
    // Everything that can refuse the optional path ran before this point.
    // Producer recording never falls back after a partially completed plan.
    let mut results = Vec::with_capacity(plan.producers.len());
    for group in &plan.producers {
        for k in 0..2 { cache.draw(r, enc, pair, k, true, &group[k], true); }
        cache.retain_pair(pair);
        results.push(pair.each_ref().map(|t| RenderTarget { tex: t.tex.clone() }));
        shape(group[0].len().min(255) as u8, group[1].len().min(255) as u8);
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
