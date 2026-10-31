//! Exact pure-stage prefix memoization. Destination-dependent suffixes keep
//! their original order and read the cached prefix's RGBA8 result.
use std::sync::{Arc, Weak};
#[cfg(not(test))]
use knife_render::{Blend, CommandEncoder, GpuTexture, Quad, RenderTarget, Renderer, Shade};
#[cfg(test)]
use crate::vita::{Blend, CommandEncoder, GpuTexture, Quad, RenderTarget, Renderer, Shade};
use knife_perf::{Gauge, Metric, memory_budget};

struct Entry {
    key: Vec<Vec<u64>>,
    dependencies: Vec<Weak<GpuTexture>>,
    target: RenderTarget,
    version: (usize, u64),
    clear: [u32; 4],
    bytes: usize,
    draws: usize,
}
#[derive(Default)]
pub(super) struct Cache {
    previous: Vec<Vec<u64>>,
    entry: Option<Entry>,
    clock: u64,
    retry_after: u64,
}

/// Alpha keeps the cleared destination alpha at one. Every PS operation
/// either preserves destination or writes alpha one. Other blend equations
/// are deliberately excluded rather than baking a different alpha contract.
fn preserves_opaque_alpha(q: &Quad) -> bool { matches!(q.blend, Blend::Alpha | Blend::Ps(_)) }
fn common_prefix(previous: &[Vec<u64>], now: &[Vec<u64>]) -> usize {
    if previous.len() != now.len() { return 0; }
    previous.iter().zip(now).take_while(|(a,b)| a == b).count()
}

impl Cache {
    pub(super) fn bytes(&self) -> usize { self.entry.as_ref().map_or(0, |e| e.bytes) }
    pub(super) fn clear(&mut self) {
        self.entry = None;
        self.previous.clear();
        knife_perf::gauge_set(Gauge::StagePrefixBytes, 0);
    }

    /// Returns replacement draws only after the original prefix has been
    /// rendered with its original clear, blends and per-draw quantization.
    pub(super) fn apply(&mut self, r: &mut Renderer, enc: &mut CommandEncoder,
        template: &RenderTarget, clear: [f32; 4], quads: &mut Vec<Quad>) -> bool
    {
        self.clock = self.clock.saturating_add(1);
        let allowed = clear[3] == 1.0 && quads.len() <= 64 && quads.iter().all(|q|
            preserves_opaque_alpha(q) && q.rule.is_none() && !q.masked
            && (q.tex.vita_immutable_stage_source() || Arc::ptr_eq(&q.tex, &r.white)));
        if !allowed {
            self.clear();
            knife_perf::add(Metric::StagePrefixBypass, 0, 1);
            return false;
        }
        let keys: Vec<Vec<u64>> = quads.iter().map(Quad::render_key).collect();
        let count = common_prefix(&self.previous, &keys);
        self.previous = keys.clone();
        // A stable complete stage already has the cheaper existing whole-stage
        // cache. Caching two ordinary sprites would add bandwidth for little gain.
        if count < 2 || count == quads.len()
            || !quads[..count].iter().any(|q| matches!(q.blend, Blend::Ps(_))) {
            // A changed scene must not keep an obsolete optional target while
            // admission loads its new source working set.
            self.entry = None;
            knife_perf::gauge_set(Gauge::StagePrefixBytes, 0);
            knife_perf::add(Metric::StagePrefixBypass, 0, 2);
            return false;
        }
        let key = keys[..count].to_vec();
        let clear_bits = clear.map(f32::to_bits);
        let hit = self.entry.as_ref().is_some_and(|e| e.key == key && e.clear == clear_bits
            && e.version == e.target.tex.render_identity()
            && e.dependencies.iter().all(|source| source.strong_count() != 0)
            && e.target.tex.physical_size() == template.tex.physical_size()
            && e.target.tex.width == template.tex.width && e.target.tex.height == template.tex.height);
        if hit {
            knife_perf::add(Metric::StagePrefixHit, self.bytes() as u64, count as u64);
        } else {
            knife_perf::add(Metric::StagePrefixMiss, 0, count as u64);
            if self.entry.as_ref().is_some_and(|e| e.target.tex.physical_size() != template.tex.physical_size()
                || e.target.tex.width != template.tex.width || e.target.tex.height != template.tex.height) {
                self.entry = None;
            }
            let reusable = self.entry.take().map(|e| e.target);
            let target = match reusable {
                Some(target) => target,
                None => {
                    let Ok(bytes) = r.vita_target_allocation_bytes(template) else { return false; };
                    if bytes > memory_budget::GPU_STAGE_PREFIX_BYTES || self.clock < self.retry_after {
                        knife_perf::add(Metric::StagePrefixBypass, 0, 3);
                        return false;
                    }
                    if !r.vita_prepare_cache_allocation(enc, quads, bytes).unwrap_or(false) {
                        self.retry_after = self.clock.saturating_add(4);
                        knife_perf::add(Metric::StagePrefixBypass, 0, 3);
                        return false;
                    }
                    let Ok(target) = r.try_create_scene_target(template.tex.width,
                        template.tex.height, "stage prefix") else { return false; };
                    if target.tex.physical_size() != template.tex.physical_size() { return false; }
                    target
                }
            };
            let Ok(bytes) = r.vita_target_allocation_bytes(&target) else { return false; };
            // Every original draw still lands in UCHAR4. This only changes the
            // surface holding those intermediate values, never their equations.
            let draws = r.try_draw_stage_prefix(enc, &target, clear, &quads[..count])
                .expect("recording static stage prefix");
            let dependencies = quads[..count].iter().map(|q| Arc::downgrade(&q.tex)).collect();
            self.entry = Some(Entry { key, dependencies, version: target.tex.render_identity(),
                target, clear: clear_bits, bytes, draws });
            knife_perf::add(Metric::StagePrefixBuild, bytes as u64, count as u64);
            knife_perf::gauge_set(Gauge::StagePrefixBytes, bytes as u64);
        }
        let entry = self.entry.as_ref().unwrap();
        let full = [0.0, 0.0, template.tex.width as f32, template.tex.height as f32];
        // Prefix alpha is proven one. NoAlpha + Opaque restores exact RGB and
        // alpha one, with no second coverage multiplication.
        let cached = Quad { blend: Blend::Opaque, shade: Shade::NoAlpha,
            ..Quad::rect(entry.target.tex.clone(), full, [1.0; 4]) };
        let mut result = Vec::with_capacity(quads.len() - count + 1);
        result.push(cached);
        result.extend(quads.drain(count..));
        *quads = result;
        if hit { knife_perf::add(Metric::StagePrefixDrawSaved, 0, entry.draws.saturating_sub(1) as u64); }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::common_prefix;
    #[test]
    fn only_the_unchanged_prefix_before_the_first_moving_input_is_eligible() {
        let previous=vec![vec![1,11],vec![2,22],vec![3,33],vec![4,44]];
        let mut now=previous.clone();now[2][1]=34;
        assert_eq!(common_prefix(&previous,&now),2);
        now[0][1]=12;assert_eq!(common_prefix(&previous,&now),0);
        assert_eq!(common_prefix(&previous,&previous[..3]),0);
    }
}
