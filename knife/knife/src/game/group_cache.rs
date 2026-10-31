//! Exact memoization of fully cleared group producer surfaces. This does not
//! flatten blends or change the saved group opacity: the original composite
//! samples the same RGBA8 pixels, including their intermediate rounding.

use std::{collections::HashSet, sync::{Arc, Weak}};
#[cfg(not(test))]
use knife_render::{CommandEncoder, GpuTexture, Quad, RenderTarget, Renderer, TexRef};
#[cfg(test)]
use crate::vita::{CommandEncoder, GpuTexture, Quad, RenderTarget, Renderer, TexRef};
use knife_perf::{Gauge, Metric, memory_budget};

const MAX_IDLE_FRAMES: u64 = 180;
const ALLOCATION_SPARE: usize = 1024 * 1024;

struct Entry {
    key: Vec<u64>,
    dependencies: Vec<Weak<GpuTexture>>,
    target: RenderTarget,
    version: (usize, u64),
    bytes: usize,
    last_use: u64,
    last_frame: u64,
    effect_draws: usize,
}

#[derive(Default)]
pub(super) struct Cache {
    entries: Vec<Entry>,
    clock: u64,
    frame: u64,
    retained: HashSet<usize>,
    spares: Vec<(usize, RenderTarget, usize)>,
}

fn clone_target(t: &RenderTarget) -> RenderTarget { RenderTarget { tex: t.tex.clone() } }
fn identity(t: &RenderTarget) -> usize { t.tex.render_identity().0 }

impl Cache {
    pub(super) fn bytes(&self) -> usize {
        self.entries.iter().map(|e| e.bytes).sum::<usize>()
            + self.spares.iter().map(|s| s.2).sum::<usize>()
    }

    fn publish_bytes(&self) {
        knife_perf::gauge_set(Gauge::GroupCacheBytes, self.bytes() as u64);
    }

    fn remove(&mut self, index: usize) -> Entry {
        let e = self.entries.swap_remove(index);
        knife_perf::add(Metric::GroupCacheEvict, e.bytes as u64, 0);
        e
    }

    /// Dropping cache owners does not invalidate an already recorded command:
    /// CommandEncoder and the current persistent pair retain their own owners.
    /// Native retirement fences the final GPU reader once the final owner drops.
    pub(super) fn trim(&mut self, keep_bytes: usize) {
        while self.bytes() > keep_bytes {
            let Some(index) = self.entries.iter().enumerate()
                .filter(|(_, e)| !self.retained.contains(&identity(&e.target)))
                .min_by_key(|(_, e)| e.last_use)
                .map(|(i, _)| i) else { break };
            self.remove(index);
        }
        self.publish_bytes();
    }

    pub(super) fn clear(&mut self) { self.trim(0); }

    pub(super) fn begin_frame(&mut self) {
        self.finish_retained();
        self.frame = self.frame.saturating_add(1);
        for i in (0..self.entries.len()).rev() {
            let e = &self.entries[i];
            if self.frame.saturating_sub(e.last_frame) > MAX_IDLE_FRAMES
                || e.version != e.target.tex.render_identity()
                || e.dependencies.iter().any(|d| d.upgrade().is_none())
            {
                self.remove(i);
            }
        }
        self.publish_bytes();
    }

    pub(super) fn owns_texture(&self, tex: &TexRef) -> bool {
        let id = tex.render_identity().0;
        self.entries.iter().any(|e| identity(&e.target) == id)
    }

    fn key(pair: &[RenderTarget; 2], kind: usize, quads: &[Quad]) -> Vec<u64> {
        let physical = pair[kind].tex.physical_size();
        let mut key = vec![kind as u64, u64::from(pair[kind].tex.width),
            u64::from(pair[kind].tex.height), u64::from(physical[0]),
            u64::from(physical[1]), quads.len() as u64];
        for q in quads {
            let input = q.render_key(); key.push(input.len() as u64); key.extend(input);
        }
        key
    }

    /// Reserve every needed owner before recording a producer. A refused
    /// optional allocation leaves the original group stream recordable.
    /// Retained output surfaces use the same six-entry/12-MiB cache allowance.
    pub(super) fn prepare_retained(&mut self, r: &Renderer, enc: &CommandEncoder,
        pair: &[RenderTarget; 2], producers: &[[Vec<Quad>; 2]], upcoming: &[Quad]) -> bool
    {
        self.finish_retained();
        if pair[0].tex.physical_size() != pair[1].tex.physical_size()
            || pair[0].tex.width != pair[1].tex.width || pair[0].tex.height != pair[1].tex.height {
            return false;
        }
        let mut keys: [Vec<Vec<u64>>; 2] = [Vec::new(), Vec::new()];
        for group in producers { for k in 0..2 {
            let key = Self::key(pair, k, &group[k]);
            if !keys[k].contains(&key) { keys[k].push(key); }
        } }
        let needed = keys.iter().map(Vec::len).sum::<usize>();
        let Ok(bytes) = r.vita_target_allocation_bytes(&pair[0]) else { return false; };
        if needed > memory_budget::GPU_GROUP_ENTRIES
            || needed.checked_mul(bytes).is_none_or(|n| n > memory_budget::GPU_GROUP_BYTES) {
            return false;
        }
        let available = |entries: &[Entry], k: usize| {
            let mut ids = HashSet::new(); ids.insert(identity(&pair[k]));
            for e in entries.iter().filter(|e| e.key[0] == k as u64) {
                ids.insert(identity(&e.target));
            }
            ids.len()
        };
        // Make room in the existing budget, preferring obsolete result keys.
        loop {
            let missing = (0..2).map(|k| keys[k].len().saturating_sub(available(&self.entries, k)))
                .sum::<usize>();
            if self.entries.len() + missing <= memory_budget::GPU_GROUP_ENTRIES { break; }
            let victim = self.entries.iter().enumerate().filter(|(_, e)| {
                let k = e.key[0] as usize;
                identity(&e.target) != identity(&pair[k])
                    && available(&self.entries, k) > keys[k].len()
            }).min_by_key(|(_, e)| (keys[e.key[0] as usize].contains(&e.key), e.last_use))
                .map(|(i, _)| i);
            let Some(victim) = victim else { return false; };
            self.remove(victim);
        }
        for k in 0..2 {
            let missing = keys[k].len().saturating_sub(available(&self.entries, k));
            for _ in 0..missing {
                if !r.vita_prepare_cache_allocation(enc, upcoming, bytes).unwrap_or(false) {
                    self.finish_retained(); return false;
                }
                let Ok(target) = r.try_create_scene_target(pair[k].tex.width, pair[k].tex.height,
                    if k == 0 { "group pixel" } else { "group mask" }) else {
                    self.finish_retained(); return false;
                };
                if target.tex.physical_size() != pair[k].tex.physical_size() {
                    self.finish_retained(); return false;
                }
                self.spares.push((k, target, bytes));
            }
        }
        self.publish_bytes();
        true
    }

    pub(super) fn retain_pair(&mut self, pair: &[RenderTarget; 2]) {
        for target in pair { self.retained.insert(identity(target)); }
    }

    pub(super) fn finish_retained(&mut self) {
        self.retained.clear(); self.spares.clear(); self.publish_bytes();
    }

    fn invalidate(&mut self, id: usize) {
        for i in (0..self.entries.len()).rev() {
            if identity(&self.entries[i].target) == id { self.remove(i); }
        }
    }

    /// A no-clear write inherits the persistent pair and must execute against
    /// that exact surface. It invalidates an immutable cached result before
    /// mutation. Only full clears can replace a surface without copying it.
    pub(super) fn draw(&mut self, r: &mut Renderer, enc: &mut CommandEncoder,
        pair: &mut [RenderTarget; 2], kind: usize, fresh: bool, quads: &[Quad], allowed: bool)
    {
        self.clock = self.clock.saturating_add(1);
        if !fresh || !allowed {
            knife_perf::add(Metric::GroupCacheBypass, 0, quads.len() as u64);
            self.invalidate(identity(&pair[kind]));
            r.draw(enc, &pair[kind], fresh.then_some([0.0; 4]), quads);
            self.publish_bytes();
            return;
        }
        let key = Self::key(pair, kind, quads);
        if let Some(e) = self.entries.iter_mut().find(|e| e.key == key
            && e.version == e.target.tex.render_identity()
            && e.dependencies.iter().all(|d| d.upgrade().is_some()))
        {
            e.last_use = self.clock;
            e.last_frame = self.frame;
            pair[kind] = clone_target(&e.target);
            knife_perf::add(Metric::GroupCacheHit, e.bytes as u64, kind as u64);
            knife_perf::add(Metric::GroupCacheDrawSaved, e.bytes as u64, e.effect_draws as u64);
            return;
        }
        knife_perf::add(Metric::GroupCacheMiss, 0, kind as u64);
        // Query the native charge: non-pool A/B profiles round to 256 KiB,
        // whereas the default pool rounds to 64 bytes. Never budget payload only.
        let Ok(bytes) = r.vita_target_allocation_bytes(&pair[kind]) else {
            self.invalidate(identity(&pair[kind]));
            r.draw(enc, &pair[kind], Some([0.0; 4]), quads);
            self.publish_bytes();
            return;
        };
        let current = identity(&pair[kind]);
        let other = identity(&pair[1 - kind]);
        let mut replacement = None;
        if self.retained.contains(&current) || self.entries.iter().any(|e| identity(&e.target) == current) {
            // Preserve a previous result only within the shared bounded cache.
            // Under pressure recycle an existing owner rather than upload/copy.
            let room = self.entries.len() < memory_budget::GPU_GROUP_ENTRIES
                && self.bytes().saturating_add(bytes) <= memory_budget::GPU_GROUP_BYTES;
            if let Some(index) = self.spares.iter().position(|s| s.0 == kind) {
                replacement = Some(self.spares.swap_remove(index).1);
            } else if self.retained.is_empty() && room
                && r.vita_cache_headroom().is_ok_and(|n| n >= bytes + ALLOCATION_SPARE) {
                replacement = r.try_create_scene_target(pair[kind].tex.width,
                    pair[kind].tex.height, if kind == 0 { "group pixel" } else { "group mask" }).ok();
            }
            if replacement.is_none() {
                if let Some(index) = self.entries.iter().enumerate()
                    .filter(|(_, e)| identity(&e.target) != other
                        && !self.retained.contains(&identity(&e.target))
                        && (self.retained.is_empty() || e.key[0] == kind as u64))
                    .min_by_key(|(_, e)| e.last_use).map(|(i, _)| i)
                {
                    replacement = Some(self.remove(index).target);
                }
            }
        }
        assert!(!self.retained.contains(&current) || replacement.is_some(),
            "preflight reserved every retained group owner");
        if let Some(target) = replacement { pair[kind] = target; }
        self.invalidate(identity(&pair[kind]));
        let effect_draws = r.try_draw_group_cached_producer(enc, &pair[kind], quads)
            .expect("recording cached group producer");
        // No extra copy: this exact freshly rendered surface becomes the entry.
        // Weak sources reject pointer reuse without pinning obsolete animations.
        let dependencies = quads.iter().flat_map(|q| std::iter::once(Arc::downgrade(&q.tex))
            .chain(q.rule.as_ref().map(|m| Arc::downgrade(&m.tex)))).collect();
        self.entries.push(Entry { key, dependencies, target: clone_target(&pair[kind]),
            version: pair[kind].tex.render_identity(), bytes, last_use: self.clock,
            last_frame: self.frame, effect_draws });
        self.trim(memory_budget::GPU_GROUP_BYTES);
        while self.entries.len() > memory_budget::GPU_GROUP_ENTRIES {
            let index = self.entries.iter().enumerate()
                .filter(|(_, e)| !self.retained.contains(&identity(&e.target)))
                .min_by_key(|(_, e)| e.last_use)
                .map(|(i, _)| i).unwrap();
            self.remove(index);
        }
        self.publish_bytes();
    }
}
