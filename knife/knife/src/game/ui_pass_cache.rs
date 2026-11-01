//! Memoize fully cleared UI producer passes in their existing targets. Only
//! bounded keys and weak handles are retained: no additional CDRAM allocation.
//! Final UI alpha, cursor animation and presentation remain on the normal path.
use std::sync::{Arc, Weak};

use anyhow::Result;
use knife_perf::{Gauge, Metric, Scope, memory_budget};
#[cfg(not(test))]
use knife_render::{CommandEncoder, GpuTexture, Quad, RenderTarget, Renderer};
#[cfg(test)]
use crate::vita::{CommandEncoder, GpuTexture, Quad, RenderTarget, Renderer};

const IDLE_FRAMES: u64 = 180;
const MAX_QUADS: usize = 256;
const MAX_ENTRY_WORDS: usize = 8192;

struct Entry {
    target: Weak<GpuTexture>,
    version: (usize, u64),
    inputs: Vec<Weak<GpuTexture>>,
    key: Vec<u64>,
    last_used: u64,
}

#[derive(Default)]
pub(super) struct Cache {
    entries: Vec<Entry>,
    frame: u64,
}

impl Cache {
    pub fn bytes(&self) -> usize {
        self.entries.iter().map(|e| e.key.capacity() * 8
            + e.inputs.capacity() * std::mem::size_of::<Weak<GpuTexture>>()).sum()
    }

    fn gauges(&self) {
        knife_perf::gauge_set(Gauge::UiPassCacheEntries, self.entries.len() as u64);
        knife_perf::gauge_set(Gauge::UiPassCacheBytes, self.bytes() as u64);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.gauges();
    }

    pub fn begin_frame(&mut self) {
        self.frame = self.frame.saturating_add(1);
        self.entries.retain(|e| e.target.strong_count() != 0
            && self.frame.saturating_sub(e.last_used) <= IDLE_FRAMES
            && e.inputs.iter().all(|input| input.strong_count() != 0));
        self.gauges();
    }

    fn forget(&mut self, id: usize) {
        self.entries.retain(|e| e.version.0 != id);
    }

    /// Called for Game::ui_passes in one encoder, in their original order. An
    /// encoder error is fatal to that frame/game, as on the ordinary draw path.
    /// Earlier producers mutate source versions when recorded, so a dependent
    /// pass invalidates in the same frame, before native submission.
    pub fn draw(&mut self, r: &mut Renderer, enc: &mut CommandEncoder,
        target: &RenderTarget, clear: Option<[f32; 4]>, quads: &[Quad]) -> Result<bool>
    {
        let _scope = Scope::new(Metric::UiPassCacheRecord);
        let version = target.tex.render_identity();
        let enabled = knife_perf::optimizations::enabled(knife_perf::optimizations::UI_PASS_CACHE);
        if !enabled || clear.is_none() || quads.len() > MAX_QUADS
            || quads.iter().any(|q| q.tex.render_identity().0 == version.0
                || q.rule.as_ref().is_some_and(|m| m.tex.render_identity().0 == version.0))
        {
            self.forget(version.0);
            knife_perf::add(Metric::UiPassCacheBypass, 0, quads.len() as u64);
            r.try_draw(enc, target, clear, quads)?;
            self.gauges();
            return Ok(false);
        }
        let physical = target.tex.physical_size();
        let mut key = vec![u64::from(knife_perf::optimizations::mask()),
            u64::from(target.tex.width), u64::from(target.tex.height),
            u64::from(physical[0]), u64::from(physical[1]), quads.len() as u64];
        key.extend(clear.unwrap().map(|v| u64::from(v.to_bits())));
        let mut inputs: Vec<Weak<GpuTexture>> = Vec::new();
        for q in quads {
            let values = q.render_key();
            key.push(values.len() as u64);
            key.extend(values);
            for input in std::iter::once(&q.tex).chain(q.rule.iter().map(|m| &m.tex)) {
                let weak = Arc::downgrade(input);
                if !inputs.iter().any(|old| old.ptr_eq(&weak)) { inputs.push(weak); }
            }
        }
        if key.len() > MAX_ENTRY_WORDS {
            self.forget(version.0);
            knife_perf::add(Metric::UiPassCacheBypass, 0, quads.len() as u64);
            r.try_draw(enc, target, clear, quads)?;
            self.gauges();
            return Ok(false);
        }
        if let Some(entry) = self.entries.iter_mut().find(|e| e.version == version
            && e.target.upgrade().is_some_and(|t| Arc::ptr_eq(&t, &target.tex))
            && e.inputs.iter().all(|input| input.strong_count() != 0) && e.key == key)
        {
            entry.last_used = self.frame;
            knife_perf::add(Metric::UiPassCacheHit, 0, 1);
            knife_perf::add(Metric::UiPassCacheQuadsSaved, 0, quads.len() as u64);
            return Ok(true);
        }
        if self.entries.iter().any(|e| e.version.0 == version.0) {
            knife_perf::add(Metric::UiPassCacheInvalidate, 0, 1);
        }
        self.forget(version.0);
        r.try_draw(enc, target, clear, quads)?;
        knife_perf::add(Metric::UiPassCacheBuild, 0, quads.len() as u64);
        key.shrink_to_fit();
        inputs.shrink_to_fit();
        let bytes = key.capacity() * 8 + inputs.capacity() * std::mem::size_of::<Weak<GpuTexture>>();
        while !self.entries.is_empty() && (self.entries.len() >= memory_budget::CPU_UI_PASS_CACHE_ENTRIES
            || self.bytes() + bytes > memory_budget::CPU_UI_PASS_CACHE_BYTES)
        {
            let index = self.entries.iter().enumerate().min_by_key(|(_, e)| e.last_used).unwrap().0;
            self.entries.swap_remove(index);
        }
        if bytes <= memory_budget::CPU_UI_PASS_CACHE_BYTES {
            self.entries.push(Entry { target: Arc::downgrade(&target.tex),
                version: target.tex.render_identity(), inputs, key, last_used: self.frame });
        }
        self.gauges();
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vita::{Blend, RuleMask, Shade, TexRef, Upload,
        test_backend::{Control, Event, Gxm}};
    use super::super::OptimizationScope;

    fn setup() -> (Renderer, Control, RenderTarget, TexRef) {
        let gxm = Gxm::new().unwrap();
        let control = gxm.control.clone();
        let r = Renderer::new_with_gxm(gxm).unwrap();
        let canvas = r.create_target(32, 32, "canvas-test");
        let source = r.upload(4, 4, Upload::Rgba8(&[180; 64]), "ui-source");
        (r, control, canvas, source)
    }

    fn quad(source: TexRef) -> Quad {
        Quad::rect(source, [0.0, 0.0, 32.0, 32.0], [1.0; 4])
    }

    fn finish(r: &mut Renderer, enc: &mut CommandEncoder, canvas: &RenderTarget,
        source: TexRef, alpha: f32)
    {
        let q = Quad::rect(source, [0.0, 0.0, 32.0, 32.0], [1.0, 1.0, 1.0, alpha]);
        r.try_draw(enc, canvas, Some([0.0; 4]), &[q]).unwrap();
    }

    #[test]
    fn static_two_pass_backdrop_reuses_targets_and_preserves_final_fade() {
        let _options = OptimizationScope::set(knife_perf::optimizations::IMPLEMENTED);
        let (mut r, control, canvas, source) = setup();
        let pair = [r.create_target(32, 32, "save-0"), r.create_target(32, 32, "save-1")];
        let ids = pair.each_ref().map(|t| t.tex.native.resident_id().unwrap().0);
        let mut cache = Cache::default();
        let input = [quad(source)];
        let dependency = [quad(pair[0].tex.clone())];
        for frame in 0..120 {
            control.borrow_mut().events.clear();
            cache.begin_frame();
            let mut enc = r.encoder();
            assert_eq!(cache.draw(&mut r, &mut enc, &pair[0], Some([0.0; 4]), &input).unwrap(), frame != 0);
            assert_eq!(cache.draw(&mut r, &mut enc, &pair[1], Some([0.0; 4]), &dependency).unwrap(), frame != 0);
            let alpha = (frame % 60) as f32 / 60.0;
            finish(&mut r, &mut enc, &canvas, pair[1].tex.clone(), alpha);
            r.try_present(enc, &canvas).unwrap();
            let trace = control.borrow();
            assert_eq!(trace.events.iter().filter(|e| matches!(e, Event::Begin { id, .. }
                if ids.contains(id))).count(), if frame == 0 { 2 } else { 0 });
            assert!(!trace.events.iter().any(|e| matches!(e, Event::Create { .. } | Event::Destroy(_))));
            if alpha > 0.0 {
                assert!(trace.events.iter().any(|e| matches!(e, Event::Draw { vertices, .. }
                    if vertices.iter().any(|v| v.color[3] == alpha))), "composite fade frame={frame} alpha={alpha}");
            }
        }
    }

    #[test]
    fn source_update_invalidates_both_producers_in_the_same_encoder() {
        let _options = OptimizationScope::set(knife_perf::optimizations::IMPLEMENTED);
        let (mut r, _, canvas, source) = setup();
        let a = r.create_target(32, 32, "mask");
        let b = r.create_target(32, 32, "light");
        let mut cache = Cache::default();
        let inputs = [quad(source.clone())];
        let consumer = [quad(a.tex.clone())];
        for frame in 0..3 {
            if frame == 2 { r.try_write_region(&source, [0, 0, 1, 1], 4, &[255; 4]).unwrap(); }
            cache.begin_frame();
            let mut enc = r.encoder();
            assert_eq!(cache.draw(&mut r, &mut enc, &a, Some([0.0; 4]), &inputs).unwrap(), frame == 1);
            assert_eq!(cache.draw(&mut r, &mut enc, &b, Some([0.0; 4]), &consumer).unwrap(), frame == 1);
            finish(&mut r, &mut enc, &canvas, b.tex.clone(), 1.0);
            r.try_present(enc, &canvas).unwrap();
        }
    }

    #[test]
    fn animated_light_redraws_while_its_unchanged_mask_is_reused() {
        let _options = OptimizationScope::set(knife_perf::optimizations::IMPLEMENTED);
        let (mut r, _, canvas, source) = setup();
        let mask = r.create_target(32, 32, "music mask");
        let light = r.create_target(32, 32, "music light");
        let mut cache = Cache::default();
        for frame in 0..4 {
            cache.begin_frame();
            let mut enc = r.encoder();
            assert_eq!(cache.draw(&mut r, &mut enc, &mask, Some([0.0; 4]), &[quad(source.clone())]).unwrap(), frame != 0);
            let mut moving = quad(source.clone());
            for p in &mut moving.pos { p[0] += frame as f32; }
            let mut clip = quad(mask.tex.clone());
            clip.blend = Blend::MaskAlpha;
            assert!(!cache.draw(&mut r, &mut enc, &light, Some([0.0; 4]), &[moving, clip]).unwrap());
            finish(&mut r, &mut enc, &canvas, light.tex.clone(), 1.0);
            r.try_present(enc, &canvas).unwrap();
        }
    }

    #[test]
    fn rule_material_clear_and_external_target_writes_cannot_hit_stale_pixels() {
        let _options = OptimizationScope::set(knife_perf::optimizations::IMPLEMENTED);
        let (mut r, _, canvas, source) = setup();
        let target = r.create_target(32, 32, "ui");
        let rule = r.upload(4, 4, Upload::R8(&[255; 16]), "rule");
        let mut q = quad(source);
        q.rule = Some(RuleMask::new(rule.clone(), 0.25));
        let mut cache = Cache::default();
        for frame in 0..7 {
            cache.begin_frame();
            let mut enc = r.encoder();
            match frame {
                2 => q.rule.as_mut().unwrap().progress = 0.5,
                3 => q.color[3] = 0.7,
                4 => { r.try_write_region(&rule, [0, 0, 1, 1], 1, &[127]).unwrap(); },
                5 => { r.try_draw(&mut enc, &target, Some([1.0; 4]), &[]).unwrap(); },
                _ => {},
            }
            let clear = if frame == 6 { [0.1; 4] } else { [0.0; 4] };
            assert_eq!(cache.draw(&mut r, &mut enc, &target, Some(clear), &[q.clone()]).unwrap(), frame == 1);
            finish(&mut r, &mut enc, &canvas, target.tex.clone(), 1.0);
            r.try_present(enc, &canvas).unwrap();
        }
    }

    #[test]
    fn preserve_content_passes_and_disabled_profile_always_take_the_original_path() {
        let _options = OptimizationScope::set(knife_perf::optimizations::IMPLEMENTED);
        let (mut r, _, canvas, source) = setup();
        let target = r.create_target(32, 32, "ui");
        let mut cache = Cache::default();
        for frame in 0..4 {
            cache.begin_frame();
            let mut enc = r.encoder();
            if frame >= 2 { knife_perf::optimizations::set(0); }
            let clear = if frame < 2 { None } else { Some([0.0; 4]) };
            assert!(!cache.draw(&mut r, &mut enc, &target, clear, &[quad(source.clone())]).unwrap());
            finish(&mut r, &mut enc, &canvas, target.tex.clone(), 1.0);
            r.try_present(enc, &canvas).unwrap();
        }
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn cache_owns_no_gpu_targets_and_bounds_its_metadata() {
        let _options = OptimizationScope::set(knife_perf::optimizations::IMPLEMENTED);
        let (mut r, control, canvas, source) = setup();
        let mut cache = Cache::default();
        let mut targets = Vec::new();
        let mut enc = r.encoder();
        for _ in 0..(memory_budget::CPU_UI_PASS_CACHE_ENTRIES + 4) {
            let t = r.create_target(8, 8, "temporary-ui");
            assert!(!cache.draw(&mut r, &mut enc, &t, Some([0.0; 4]), &[quad(source.clone())]).unwrap());
            targets.push(t);
        }
        finish(&mut r, &mut enc, &canvas, targets[0].tex.clone(), 1.0);
        r.try_present(enc, &canvas).unwrap();
        assert!(cache.entries.len() <= memory_budget::CPU_UI_PASS_CACHE_ENTRIES);
        assert!(cache.bytes() <= memory_budget::CPU_UI_PASS_CACHE_BYTES);
        let ids = targets.iter().map(|t| t.tex.native.resident_id().unwrap().0).collect::<Vec<_>>();
        control.borrow_mut().events.clear();
        drop(targets);
        cache.begin_frame();
        assert!(cache.entries.is_empty());
        let trace = control.borrow();
        assert_eq!(trace.events.iter().filter(|e| matches!(e, Event::Destroy(id) if ids.contains(id))).count(), ids.len());
    }

    #[test]
    fn failed_recording_and_target_aliases_do_not_publish_cache_entries() {
        let _options = OptimizationScope::set(knife_perf::optimizations::IMPLEMENTED);
        let (mut r, _, _, source) = setup();
        let target = r.create_target(32, 32, "ui");
        let mut cache = Cache::default();
        let mut invalid = quad(source);
        invalid.shade = Shade::Fill;
        invalid.pos[0][0] = f32::NAN;
        let mut enc = r.encoder();
        assert!(cache.draw(&mut r, &mut enc, &target, Some([0.0; 4]), &[invalid]).is_err());
        assert!(cache.entries.is_empty());
        let mut enc = r.encoder();
        assert!(cache.draw(&mut r, &mut enc, &target, Some([0.0; 4]), &[quad(target.tex.clone())]).is_err());
        assert!(cache.entries.is_empty());
    }
}
