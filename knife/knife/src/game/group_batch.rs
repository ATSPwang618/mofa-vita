//! Full-clear independent group producers may run before the stage. Their
//! retained RGBA8 outputs are then sampled in the original stage/PS order.
#[cfg(not(test))]
use knife_render::{Blend, CommandEncoder, Quad, RenderTarget, Renderer};
#[cfg(test)]
use crate::vita::{Blend, CommandEncoder, Quad, RenderTarget, Renderer};
use super::group_cache;
use knife_perf::Metric;

pub(super) enum Draw {
    Stage(Quad),
    Pixel { quad: Quad, clear: bool },
    Mask { quad: Quad, clear: bool },
    Composite { blend: Blend, alpha: f32 },
}
enum Action { Stage(Quad), Composite(usize, Blend, f32) }
struct Plan { producers: Vec<[Vec<Quad>; 2]>, actions: Vec<Action>, sources: Vec<Quad> }

fn plan(draws: Vec<Draw>) -> Option<Plan> {
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
                if clear != pending[0].is_empty() { return None; }
                sources.push(quad.clone()); pending[0].push(quad);
            }
            Draw::Mask { quad, clear } => {
                if clear != pending[1].is_empty() { return None; }
                sources.push(quad.clone()); pending[1].push(quad);
            }
            Draw::Composite { blend, alpha } => {
                if pending.iter().any(Vec::is_empty) || !alpha.is_finite() { return None; }
                actions.push(Action::Composite(groups.len(), blend, alpha));
                groups.push(std::mem::take(&mut pending));
                if groups.len() * 2 > knife_perf::memory_budget::GPU_GROUP_ENTRIES { return None; }
            }
        }
    }
    // No trailing pair mutation is moved or dropped. Every retained result has
    // exactly the same producer clear and per-target draw sequence as before.
    if groups.len() < 2 || pending.iter().any(|q| !q.is_empty()) { return None; }
    Some(Plan { producers: groups, actions, sources })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render(r: &mut Renderer, enc: &mut CommandEncoder,
    stage: &RenderTarget, pair: &mut [RenderTarget; 2], scratch: &RenderTarget,
    cache: &mut group_cache::Cache, clear: [f32; 4], draws: Vec<Draw>,
    mut shape: impl FnMut(u8, u8)) -> bool
{
    let Some(plan) = plan(draws) else {
        knife_perf::add(Metric::GroupBatchBypass, 0, 1); return false;
    };
    // Alias rejection includes all sampler/rule inputs, not only the primary
    // texture. These values must not observe a stage or shared-pair mutation.
    let stage_id = stage.tex.render_identity().0;
    let pair_ids = pair.each_ref().map(|t| t.tex.render_identity().0);
    if plan.sources.iter().any(|q| std::iter::once(&q.tex)
        .chain(q.rule.as_ref().map(|m| &m.tex)).any(|source| {
            let id = source.render_identity().0;
            id == stage_id || pair_ids.contains(&id) || cache.owns_texture(source)
        })) {
        knife_perf::add(Metric::GroupBatchBypass, 0, 2); return false;
    }
    if !plan.actions.iter().all(|a| match a {
        Action::Composite(_, blend, _) => r.vita_group_fused_available(stage, *blend).unwrap_or(false),
        Action::Stage(_) => true,
    }) || !cache.prepare_retained(r, enc, pair, &plan.producers, &plan.sources) {
        knife_perf::add(Metric::GroupBatchBypass, 0, 3); return false;
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
            Action::Composite(index, blend, alpha) => {
                r.draw(enc, stage, clear.take(), &std::mem::take(&mut run));
                let [pixel, mask] = &results[index];
                r.try_group_composite_via_scratch(enc, stage, pixel, mask, scratch, blend, alpha)
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
