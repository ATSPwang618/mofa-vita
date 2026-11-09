//! A script barrier owns a finite image cohort. Prepared images are invisible
//! until every key is ready and the complete source generation still matches.
//! CPU jobs use Assets' existing bounded worker; GPU registration stays on the
//! render owner, at most one completed image per update.

struct Cohort<K, V> {
    generation: u64,
    items: Vec<(K, Option<V>)>,
}
impl<K, V> Cohort<K, V> {
    fn new(generation: u64, keys: Vec<K>) -> Self {
        Self { generation, items: keys.into_iter().map(|key| (key, None)).collect() }
    }
    fn next(&self) -> Option<(usize, &K)> {
        self.items.iter().enumerate().find(|(_, (_, value))| value.is_none()).map(|(i, (key, _))| (i, key))
    }
    fn ready(&mut self, index: usize, value: V) {
        assert!(self.items[index].1.is_none(), "cohort slot registered twice");
        self.items[index].1 = Some(value);
    }
    fn complete(&self) -> bool { self.items.iter().all(|(_, value)| value.is_some()) }
    fn can_commit(&self, generation: u64, valid: impl Fn(&K) -> bool) -> bool {
        self.generation == generation && self.complete() && self.items.iter().all(|(key, _)| valid(key))
    }
    fn into_ready(self) -> impl Iterator<Item = (K, V)> {
        assert!(self.complete(), "partial cohort cannot be published");
        self.items.into_iter().map(|(key, value)| (key, value.unwrap()))
    }
}

/// A prepared timestep is consumed exactly once and remains pinned while a
/// menu pauses the story between preparation and publication.
pub(super) fn resume_once<T>(prepared: &mut Option<T>, paused: bool) -> Option<T> {
    if paused { None } else { prepared.take() }
}

#[cfg(target_os = "vita")]
use super::{Game, Source};
#[cfg(target_os = "vita")]
use crate::{assets::{Image, ImageNeeded}, grade::Bake};
#[cfg(target_os = "vita")]
use std::{sync::Arc, time::{Duration, Instant}};

#[cfg(target_os = "vita")]
struct Key {
    layer: u8, name: String, bake: Bake, source: Arc<Image>,
    // A motion demand prepares a future image without changing the current
    // source, parameters or motion clock. Rebakings have no motion origin.
    motion_origin: Option<(String, i32, u32)>,
}
#[cfg(target_os = "vita")]
pub(super) struct Pending {
    scene: usize,
    pc: usize,
    cohort: Cohort<Key, Arc<Image>>,
    started: Instant,
    motion_dt: Option<f32>,
}
#[cfg(target_os = "vita")]
pub(super) struct PreparedMotion {
    pub(super) dt: f32,
    // Pin even initially cached variants until apply_motion has consumed the
    // complete set; another upload must not evict half of a prepared frame.
    _images: Vec<Arc<Image>>,
}
#[cfg(target_os = "vita")]
impl Key {
    fn valid(&self, game: &Game) -> bool {
        game.stage.layers.get(self.layer as usize).and_then(Option::as_ref).is_some_and(|layer| {
            let source = matches!(&layer.source, Source::Image(image) if Arc::ptr_eq(image, &self.source));
            if let Some((name, id, time)) = &self.motion_origin {
                source && layer.name == *name && layer.bake == self.bake
                    && layer.motion.as_ref().is_some_and(|motion| motion.id == *id && motion.t.to_bits() == *time)
            } else {
                source && layer.pending && !layer.keeps_texture && layer.name == self.name
                    && Bake::new(layer.stec_mode, layer.snx[0], layer.snx[1]) == self.bake
            }
        })
    }
}

#[cfg(target_os = "vita")]
impl Game {
    pub(super) fn begin_rebake_cohort(&mut self, dirty: Vec<u8>) -> bool {
        let mut keys = Vec::with_capacity(dirty.len());
        for layer in dirty {
            let Some(item) = self.stage.get(i32::from(layer)) else { continue; };
            if !item.pending || item.keeps_texture { continue; }
            let bake = Bake::new(item.stec_mode, item.snx[0], item.snx[1]);
            if item.stec_mode == 1 { item.fill = Some(bake.fill(item.stec_color)); }
            if bake == item.bake { continue; }
            if let Source::Image(source) = &item.source {
                keys.push(Key { layer, name: item.name.clone(), bake, source: source.clone(), motion_origin: None });
            } else { item.bake = bake; }
        }
        if keys.is_empty() { return true; }
        // Forty logical layers is the hard upper bound; the Batch deduplicates
        // setters before this point, so no script can grow this owner unbounded.
        assert!(keys.len() <= 40);
        let mut cohort = Cohort::new(self.assets.image_needed_context(), keys);
        for (key, ready) in &mut cohort.items {
            *ready = self.assets.cached_graded_image(&key.name, key.bake);
        }
        let count = cohort.items.len();
        knife_perf::add(knife_perf::Metric::AsyncBakeBatch, 0, count as u64);
        if cohort.complete() {
            // Cache-only cohorts do no upload and need not delay a warm script.
            if cohort.can_commit(self.assets.image_needed_context(), |key| key.valid(self)) {
                self.commit_rebake_cohort(cohort);
                return true;
            }
        }
        let pc = self.pc.saturating_sub(1);
        self.rebake_pending = Some(Pending { scene: self.scene_index, pc, cohort, started: Instant::now(), motion_dt: None });
        knife_perf::gauge_set(knife_perf::Gauge::AsyncBakeLayers, count as u64);
        self.resource_pending = true;
        self.resource_event_at("pending", "rebake cohort; old complete stage retained", self.scene_index, pc);
        false
    }

    /// Inspect only variant fields with the exact MotionTable gates, HOLD,
    /// easing and loop rules. No layer/tween/RNG/VM clock has advanced yet.
    pub(super) fn prepare_motion_variants(&mut self, dt: f32, step: f32) -> bool {
        if !knife_perf::optimizations::enabled(knife_perf::optimizations::ASYNC_REBAKE) { return true; }
        let mut keys = Vec::new();
        for (layer, item) in self.stage.layers.iter().enumerate() {
            let Some(item) = item else { continue; };
            let Some(motion) = &item.motion else { continue; };
            let Source::Image(source) = &item.source else { continue; };
            let (a, b) = self.variant_of(&item.name);
            let variant = self.motions.variant_after(motion, step, [a, b]);
            let Some(name) = self.motion_variant_values(layer, variant) else { continue; };
            keys.push(Key { layer: layer as u8, name, bake: item.bake, source: source.clone(),
                motion_origin: Some((item.name.clone(), motion.id, motion.t.to_bits())) });
        }
        if keys.is_empty() { return true; }
        let mut cohort = Cohort::new(self.assets.image_needed_context(), keys);
        for (key, ready) in &mut cohort.items { *ready = self.assets.cached_graded_image(&key.name, key.bake); }
        if cohort.complete() {
            // Pin warm variants too: prefetch runs between this preflight and
            // apply_motion, and may otherwise reclaim a cache-only image.
            self.motion_resume = Some(PreparedMotion { dt,
                _images: cohort.into_ready().map(|(_, image)| image).collect() });
            return true;
        }
        let count = cohort.items.len();
        knife_perf::add(knife_perf::Metric::MotionVariantBarrier, 0, count as u64);
        knife_perf::gauge_set(knife_perf::Gauge::AsyncBakeLayers, count as u64);
        self.rebake_pending = Some(Pending { scene: self.scene_index, pc: self.pc, cohort,
            started: Instant::now(), motion_dt: Some(dt) });
        self.resource_pending = true;
        self.resource_event_at("pending", "motion variants; complete old pose and clocks retained", self.scene_index, self.pc);
        false
    }

    pub(super) fn poll_rebake_cohort(&mut self, r: &knife_render::Renderer) {
        let Some(mut pending) = self.rebake_pending.take() else { return; };
        if self.resource_error.is_some() { self.rebake_pending = Some(pending); return; }
        // A seek/load/language change normally cancels through reset_resource_command.
        // This catches a context change initiated outside that VM path as well.
        let same_generation = pending.scene == self.scene_index
            && pending.cohort.generation == self.assets.image_needed_context();
        if !same_generation {
            // A language switch may invalidate Assets without changing Game's
            // PC. Retire old prepared owners and rebuild the still-desired set
            // in the new context; stale pixels must never become a fatal load.
            if pending.scene == self.scene_index && pending.motion_dt.is_none() {
                for (key, _) in &pending.cohort.items {
                    self.rebake_batch.defer(i32::from(key.layer));
                }
            }
            knife_perf::add(knife_perf::Metric::AsyncBakeCancel, 0, 1);
            knife_perf::gauge_set(knife_perf::Gauge::AsyncBakeLayers, 0);
            self.resource_pending = true;
            return;
        }
        let result = if let Some((index, key)) = pending.cohort.next() {
            if pending.started.elapsed() >= Duration::from_secs(30) {
                Err("rebake cohort deadline (30s); old complete stage retained".into())
            } else {
                // Optional retained targets must yield before Assets performs
                // its mandatory owner-thread upload, rather than after failure.
                let reserve = knife_perf::memory_budget::GPU_PRESSURE_HEADROOM
                    + 4 * knife_perf::memory_budget::MIB;
                if knife_platform_vita::memory_available().ok().is_some_and(|memory|
                    (memory.kernel_cdram as usize) < reserve) {
                    self.vita_release_optional_render_targets(r);
                }
                let probe = knife_perf::Scope::new(knife_perf::Metric::AsyncBakePoll);
                let result = if pending.motion_dt.is_some() {
                    self.assets.poll_motion_needed(r, &key.name, key.bake)
                } else { self.assets.poll_rebake_needed(r, &key.name, key.bake) };
                probe.finish(0, 1);
                match result {
                    ImageNeeded::Ready(image) => {
                        let [width, height] = image.tex.physical_size();
                        let bytes = u64::from(width) * u64::from(height) * 4;
                        pending.cohort.ready(index, image);
                        knife_perf::add(knife_perf::Metric::AsyncBakeReady, bytes, 1);
                        Ok(())
                    }
                    ImageNeeded::Pending => Ok(()),
                    ImageNeeded::Pressure(error) => {
                        self.vita_reclaim_resource_pressure(r);
                        self.resource_event_at("allocation_retry", &error, pending.scene, pending.pc);
                        Ok(())
                    }
                    ImageNeeded::Failed(error) => Err(error),
                }
            }
        } else { Ok(()) };
        if let Err(error) = result {
            knife_perf::add(knife_perf::Metric::AsyncBakeFailed, 0, 1);
            self.resource_event_at("failed", &error, pending.scene, pending.pc);
            self.resource_error = Some(error);
            // Drop staged GPU owners immediately; do not publish a partial set.
            for (_, ready) in &mut pending.cohort.items { *ready = None; }
            self.rebake_pending = Some(pending);
            return;
        }
        if pending.cohort.complete() && !self.paused {
            if pending.cohort.can_commit(self.assets.image_needed_context(), |key| key.valid(self)) {
                let detail = if let Some(dt) = pending.motion_dt {
                    self.motion_resume = Some(PreparedMotion { dt,
                        _images: pending.cohort.into_ready().map(|(_, image)| image).collect() });
                    "motion variants ready; resume original clock step next update"
                } else {
                    self.commit_rebake_cohort(pending.cohort);
                    "rebake cohort committed; resume exact barrier next update"
                };
                self.resource_event_at("ready", detail, pending.scene, pending.pc);
                // Keep this frame on the previous complete stage. The next
                // normal dispatch executes the barrier/FADS before publication.
                self.resource_pending = true;
                knife_perf::gauge_set(knife_perf::Gauge::AsyncBakeLayers, 0);
                return;
            }
            knife_perf::add(knife_perf::Metric::AsyncBakeFailed, 0, 1);
            let error = "rebake layer changed before atomic commit".to_string();
            self.resource_event_at("failed", &error, pending.scene, pending.pc);
            self.resource_error = Some(error);
            for (_, ready) in &mut pending.cohort.items { *ready = None; }
        }
        self.rebake_pending = Some(pending);
    }

    fn commit_rebake_cohort(&mut self, cohort: Cohort<Key, Arc<Image>>) {
        let count = cohort.items.len();
        // All checks precede this loop. It contains no fallible work, resource
        // callbacks, rendering or yielding: image and truthful Bake move together.
        for (key, image) in cohort.into_ready() {
            let layer = self.stage.layers[key.layer as usize].as_mut().unwrap();
            layer.source = Source::Image(image);
            layer.bake = key.bake;
        }
        knife_perf::add(knife_perf::Metric::AsyncBakeCommit, 0, count as u64);
    }
}
