//! Content versions belong to producer outputs, not animation phase flags.
//! Only complete/discardable outputs may rotate; inherited history never does.
use std::sync::{Arc, Weak};
#[cfg(not(test))]
use knife_render::{CommandEncoder, GpuTexture, Quad, RenderTarget, Renderer};
#[cfg(test)]
use crate::vita::{CommandEncoder, GpuTexture, Quad, RenderTarget, Renderer};
use knife_perf::{Gauge, Metric, memory_budget};

#[derive(Default)]
pub(super) struct Memo {
    key: Vec<u64>,
    inputs: Vec<Weak<GpuTexture>>,
    output: Option<(Weak<GpuTexture>, (usize, u64))>,
}
impl Memo {
    pub fn finish(&mut self, target: &RenderTarget) {
        self.output = Some((Arc::downgrade(&target.tex), target.tex.render_identity()));
    }
    pub fn matches(&self, key: &[u64], target: &RenderTarget) -> bool {
        knife_perf::optimizations::enabled(knife_perf::optimizations::PRODUCER_REUSE)
            && self.key == key && self.output.as_ref().is_some_and(|(owner, version)|
                *version == target.tex.render_identity()
                && owner.upgrade().is_some_and(|old| Arc::ptr_eq(&old, &target.tex)))
            && self.inputs.iter().all(|input| input.strong_count() != 0)
    }
    pub fn commit(&mut self, key: Vec<u64>, inputs: &[&Quad], target: &RenderTarget) {
        self.key = key;
        self.inputs.clear();
        for q in inputs {
            for tex in std::iter::once(&q.tex).chain(q.rule.iter().map(|m| &m.tex)) {
                let weak = Arc::downgrade(tex);
                if !self.inputs.iter().any(|old| old.ptr_eq(&weak)) { self.inputs.push(weak); }
            }
        }
        self.finish(target);
    }
}

/// One additional target, allocated lazily within measured native/JPEG limits.
/// The caller must issue a complete clear/copy/blit before reading its new output.
#[derive(Default)]
pub(super) struct Ring {
    spare: Option<RenderTarget>,
    bytes: usize,
    clock: u64,
    retry_after: u64,
}
impl Ring {
    pub fn bytes(&self) -> usize { self.bytes }
    /// Give a spare output to a required capture before allocating more CDRAM.
    /// A surface still read/written by the frame must never become its capture.
    pub fn take_for_capture(&mut self, logical: [u32; 2], physical: [u32; 2],
        excluded: &[usize], role: u64) -> Option<RenderTarget>
    {
        let spare = self.spare.as_ref()?;
        if [spare.tex.width, spare.tex.height] != logical || spare.tex.physical_size() != physical
            || excluded.contains(&spare.tex.render_identity().0) { return None; }
        let target = self.spare.take();
        let bytes = std::mem::take(&mut self.bytes);
        self.retry_after = self.clock.saturating_add(8);
        knife_perf::add(Metric::TargetRingRelease, bytes as u64, role);
        tracing::info!(role, reused_bytes = bytes, "Reusing target ring spare for required capture");
        target
    }
    pub fn release(&mut self, role: u64) -> usize {
        let released = std::mem::take(&mut self.bytes);
        self.spare = None;
        self.retry_after = self.clock.saturating_add(8);
        if released != 0 { knife_perf::add(Metric::TargetRingRelease, released as u64, role); }
        released
    }
    pub fn rotate(&mut self, r: &Renderer, enc: &CommandEncoder, active: &mut RenderTarget,
        upcoming: &[Quad], role: u64)
    {
        self.clock = self.clock.saturating_add(1);
        let id = active.tex.render_identity().0;
        let spare_id = self.spare.as_ref().map(|t| t.tex.render_identity().0);
        // An explicit feedback read names a particular surface. Renaming its
        // destination would change semantics, even if a later clear covers it.
        let alias = upcoming.iter().any(|q| std::iter::once(&q.tex)
            .chain(q.rule.iter().map(|m| &m.tex)).any(|t| {
                let source = t.render_identity().0; source == id || Some(source) == spare_id
            }));
        if !knife_perf::optimizations::enabled(knife_perf::optimizations::TARGET_RING) || alias {
            knife_perf::add(Metric::TargetRingBypass, 0, (role << 8) | if alias { 2 } else { 1 });
            return;
        }
        if self.spare.is_none() {
            if self.clock < self.retry_after { return; }
            let Ok(bytes) = r.vita_target_allocation_bytes(active) else { return; };
            if bytes > memory_budget::GPU_TARGET_RING_BYTES / 2
                || !r.vita_prepare_cache_allocation(enc, upcoming, bytes).unwrap_or(false)
            {
                self.retry_after = self.clock.saturating_add(8);
                knife_perf::add(Metric::TargetRingBypass, bytes as u64, (role << 8) | 3);
                return;
            }
            let target = if role == 2 {
                r.try_create_composite_target(active.tex.width, active.tex.height, "canvas ring")
            } else { r.try_create_scene_target(active.tex.width, active.tex.height, "stage ring") };
            let Ok(target) = target else {
                self.retry_after = self.clock.saturating_add(8);
                knife_perf::add(Metric::TargetRingBypass, bytes as u64, (role << 8) | 4);
                return;
            };
            if target.tex.physical_size() != active.tex.physical_size() {
                knife_perf::add(Metric::TargetRingBypass, bytes as u64, (role << 8) | 5);
                return;
            }
            self.bytes = bytes;
            self.spare = Some(target);
            knife_perf::add(Metric::TargetRingEnable, bytes as u64, role);
        }
        std::mem::swap(active, self.spare.as_mut().unwrap());
        // Rotation changes ownership only; no image bytes are copied.
        knife_perf::add(Metric::TargetRingRotate, 0, role);
    }
}

/// Diagnostic hashes are split by cause; cache correctness uses exact keys above.
#[derive(Default)]
pub(super) struct Changes { previous: Vec<[u64; 4]> }
impl Changes {
    pub fn observe(&mut self, quads: &[&Quad]) {
        if !knife_perf::enabled() { self.previous.clear(); return; }
        use std::hash::{Hash, Hasher};
        fn hash(values: &[u32]) -> u64 {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            values.hash(&mut h); h.finish()
        }
        let now: Vec<[u64; 4]> = quads.iter().map(|q| {
            let mut geometry = std::collections::hash_map::DefaultHasher::new();
            for p in q.pos.into_iter().chain(q.uv) { p.map(f32::to_bits).hash(&mut geometry); }
            q.clip.is_some().hash(&mut geometry);
            if let Some(clip) = q.clip { for p in clip { p.map(f32::to_bits).hash(&mut geometry); } }
            let mut texture = std::collections::hash_map::DefaultHasher::new();
            q.tex.render_identity().hash(&mut texture);
            if let Some(rule) = &q.rule { rule.tex.render_identity().hash(&mut texture); }
            let mut h = std::collections::hash_map::DefaultHasher::new();
            q.blend.hash(&mut h); q.shade.hash(&mut h); q.masked.hash(&mut h);
            for p in q.haze { p.map(f32::to_bits).hash(&mut h); }
            q.soft.is_some().hash(&mut h);
            if let Some(m) = q.soft {
                m.inner.map(f32::to_bits).hash(&mut h); m.pivot.map(f32::to_bits).hash(&mut h);
                [m.rot,m.radius,m.fill].map(f32::to_bits).hash(&mut h);
                m.row.hash(&mut h); m.texels.hash(&mut h);
            }
            q.rule.is_some().hash(&mut h);
            if let Some(m) = &q.rule {
                [m.progress,m.soft].map(f32::to_bits).hash(&mut h);
                m.flip.hash(&mut h); m.invert.hash(&mut h);
            }
            [geometry.finish(), hash(&q.color.map(f32::to_bits)), h.finish(), texture.finish()]
        }).collect();
        if self.previous.len() == now.len() {
            let metrics = [Metric::StageInputGeometryChanged, Metric::StageInputColorChanged,
                Metric::StageInputMaterialChanged, Metric::StageInputTextureChanged];
            for (i, metric) in metrics.into_iter().enumerate() {
                let count = self.previous.iter().zip(&now).filter(|(a,b)| a[i] != b[i]).count();
                if count != 0 { knife_perf::add(metric, 0, count as u64); }
            }
        }
        self.previous = now;
    }
}

pub(super) fn ring_gauge(stage: &Ring, canvas: &Ring) {
    knife_perf::gauge_set(Gauge::TargetRingBytes, (stage.bytes() + canvas.bytes()) as u64);
}
