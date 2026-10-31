//! Vita-only, bounded caches of contiguous text draws. Backgrounds and animated
//! cursors remain in the original UI order. Keys compare every source quad bit;
//! a reveal, fade, wobble, font atlas mutation or colour change cannot hit stale pixels.
use std::sync::{Arc, Weak};

use knife_perf::{Gauge, Metric, Scope, memory_budget};
#[cfg(not(test))]
use knife_render::{Blend, CANVAS_H, CANVAS_W, CommandEncoder, GpuTexture, Quad,
    RenderTarget, Renderer, Shade};
#[cfg(test)]
use crate::vita::{Blend, CANVAS_H, CANVAS_W, CommandEncoder, GpuTexture, Quad,
    RenderTarget, Renderer, Shade};

const MIN_GLYPHS: usize = 8;
const MAX_GLYPHS: usize = 1024;
const IDLE_FRAMES: u64 = 600;

#[derive(Clone, PartialEq, Eq)]
struct Key(Vec<u64>);

struct Entry {
    key: Key,
    // Weak references prevent a freed atlas' native pointer being recycled into
    // a false key hit, without retaining old font pages in GPU memory.
    sources: Vec<Weak<GpuTexture>>,
    target: RenderTarget,
    rect: [f32; 4],
    bytes: usize,
    source_draws: usize,
    last_used: u64,
}

#[derive(Default)]
pub(super) struct Pool {
    entries: Vec<Entry>,
    previous: Vec<Key>,
    observed: Vec<Key>,
    epoch: u64,
    bytes: usize,
}

impl Pool {
    pub fn begin_frame(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
        self.previous = std::mem::take(&mut self.observed);
        let epoch = self.epoch;
        self.remove_where(|entry| epoch.saturating_sub(entry.last_used) > IDLE_FRAMES
            || entry.sources.iter().any(|source| source.strong_count() == 0));
    }

    pub fn bytes(&self) -> usize { self.bytes }

    /// Current-frame readers stay pinned. Native texture retirement supplies the
    /// GPU completion fence after the last recorded command releases its handle.
    pub fn trim(&mut self, keep_bytes: usize) {
        while self.bytes > keep_bytes {
            let victim = self.entries.iter().enumerate()
                .filter(|(_, entry)| entry.last_used != self.epoch)
                .min_by_key(|(_, entry)| entry.last_used).map(|(index, _)| index);
            let Some(victim) = victim else { break };
            self.remove(victim);
        }
        if keep_bytes == 0 {
            self.previous.clear();
            self.observed.clear();
        }
    }

    fn remove_where(&mut self, mut matches: impl FnMut(&Entry) -> bool) {
        let mut index = 0;
        while index < self.entries.len() {
            if matches(&self.entries[index]) && self.entries[index].last_used != self.epoch {
                self.remove(index);
            } else { index += 1; }
        }
    }

    fn remove(&mut self, index: usize) {
        let entry = self.entries.swap_remove(index);
        self.bytes -= entry.bytes;
        knife_perf::add(Metric::TextCacheEvict, entry.bytes as u64, 1);
        self.gauges();
    }

    fn gauges(&self) {
        knife_perf::gauge_set(Gauge::TextCacheBytes, self.bytes as u64);
        knife_perf::gauge_set(Gauge::TextCacheEntries, self.entries.len() as u64);
    }

    /// Draw unchanged prefixes/suffixes immediately around each cached run.
    /// A cache build can close the current canvas pass, but cannot reorder UI.
    pub fn draw_ui(&mut self, r: &mut Renderer, enc: &mut CommandEncoder,
        canvas: &RenderTarget, quads: &[Quad]) {
        let mut begin = 0;
        while begin < quads.len() {
            let text = eligible(&quads[begin]);
            let mut end = begin + 1;
            while end < quads.len() && eligible(&quads[end]) == text { end += 1; }
            let run = &quads[begin..end];
            if !text || run.len() < MIN_GLYPHS || !self.draw_run(r, enc, canvas, run) {
                r.draw(enc, canvas, None, run);
            }
            begin = end;
        }
    }

    fn draw_run(&mut self, r: &mut Renderer, enc: &mut CommandEncoder,
        canvas: &RenderTarget, quads: &[Quad]) -> bool {
        if quads.len() > MAX_GLYPHS || self.observed.len() >= memory_budget::GPU_TEXT_ENTRIES {
            return false;
        }
        let Some(rect) = bounds(quads) else { return false };
        let mut key = Vec::new();
        for quad in quads {
            let values = quad.render_key();
            key.push(values.len() as u64);
            key.extend(values);
        }
        let key = Key(key);
        if self.previous.get(self.observed.len()).is_some_and(|previous| previous != &key) {
            knife_perf::add(Metric::TextCacheInvalidate, 0, quads.len() as u64);
        }
        self.observed.push(key.clone());
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key
            && entry.sources.iter().all(|source| source.strong_count() != 0)) {
            entry.last_used = self.epoch;
            if r.try_composite_text_cache(enc, canvas, &entry.target, entry.rect).is_ok() {
                knife_perf::add(Metric::TextCacheHit, entry.bytes as u64, quads.len() as u64);
                knife_perf::add(Metric::TextCacheSavedDraws, 0, entry.source_draws.saturating_sub(1) as u64);
                return true;
            }
            knife_perf::add(Metric::TextCacheFallback, 0, 1);
            return false;
        }
        knife_perf::add(Metric::TextCacheMiss, 0, quads.len() as u64);
        // During typing/fades/wobbles retain the original direct path. Building
        // only after a repeated signature avoids a render target for every glyph.
        if !self.previous.contains(&key) { return false }
        if !r.text_cache_available().unwrap_or(false) {
            knife_perf::add(Metric::TextCacheFallback, 0, 6);
            return false;
        }
        let logical = [rect[2] as u32, rect[3] as u32];
        let physical = [logical[0] / 2, logical[1] / 2];
        let Some(bytes) = charge(physical) else { return false };
        if bytes > memory_budget::GPU_TEXT_BYTES { return false }
        self.trim(memory_budget::GPU_TEXT_BYTES.saturating_sub(bytes));
        while self.entries.len() >= memory_budget::GPU_TEXT_ENTRIES {
            let victim = self.entries.iter().enumerate()
                .filter(|(_, entry)| entry.last_used != self.epoch)
                .min_by_key(|(_, entry)| entry.last_used).map(|(index, _)| index);
            let Some(victim) = victim else {
                knife_perf::add(Metric::TextCacheFallback, 0, 2);
                return false;
            };
            self.remove(victim);
        }
        if self.bytes + bytes > memory_budget::GPU_TEXT_BYTES {
            knife_perf::add(Metric::TextCacheFallback, 0, 3);
            return false;
        }
        let build = Scope::new(Metric::TextCacheBuild);
        let target = match r.try_create_scene_target(logical[0], logical[1], "text cache") {
            Ok(target) => target,
            Err(_) => { knife_perf::add(Metric::TextCacheFallback, 0, 4); return false }
        };
        let mut shifted = quads.to_vec();
        for quad in &mut shifted {
            for position in &mut quad.pos {
                position[0] -= rect[0];
                position[1] -= rect[1];
            }
        }
        // Dedicated coverage-over alpha: the game's Alpha keeps destination
        // alpha and its Over adds alphas. Neither can create this coverage mask.
        let source_draws = match r.try_draw_text_cache(enc, &target, &shifted) {
            Ok(draws) => draws,
            Err(_) => { knife_perf::add(Metric::TextCacheFallback, 0, 5); return false }
        };
        if r.try_composite_text_cache(enc, canvas, &target, rect).is_err() {
            knife_perf::add(Metric::TextCacheFallback, 0, 5);
            return false;
        }
        let mut sources: Vec<Weak<GpuTexture>> = Vec::new();
        for quad in quads {
            let source = Arc::downgrade(&quad.tex);
            if !sources.iter().any(|existing| existing.ptr_eq(&source)) { sources.push(source); }
        }
        self.entries.push(Entry { key, sources, target, rect, bytes, source_draws, last_used: self.epoch });
        self.bytes += bytes;
        self.gauges();
        build.finish(bytes as u64, quads.len() as u64);
        true
    }
}

fn eligible(quad: &Quad) -> bool {
    matches!(quad.shade, Shade::Text | Shade::RimText) && quad.blend == Blend::Alpha
        && quad.clip.is_none() && quad.rule.is_none() && quad.soft.is_none() && !quad.masked
}

/// Integer physical-pixel origin preserves the original raster phase. One pixel
/// of transparent margin keeps bilinear filtering inside the cached image.
fn bounds(quads: &[Quad]) -> Option<[f32; 4]> {
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for quad in quads {
        for position in quad.pos {
            for axis in 0..2 {
                if !position[axis].is_finite() { return None }
                min[axis] = min[axis].min(position[axis]);
                max[axis] = max[axis].max(position[axis]);
            }
        }
    }
    let x = ((min[0] * 0.5).floor() - 1.0).max(0.0) * 2.0;
    let y = ((min[1] * 0.5).floor() - 1.0).max(0.0) * 2.0;
    let right = ((max[0] * 0.5).ceil() + 1.0).min(CANVAS_W as f32 * 0.5) * 2.0;
    let bottom = ((max[1] * 0.5).ceil() + 1.0).min(CANVAS_H as f32 * 0.5) * 2.0;
    (right > x && bottom > y).then_some([x, y, right - x, bottom - y])
}

fn charge([width, height]: [u32; 2]) -> Option<usize> {
    let pitch = width.checked_add(7)? & !7;
    let bytes = pitch.checked_mul(height)?.checked_mul(4)?;
    #[cfg(not(test))]
    let charged = knife_platform_vita::gxm::Gxm::texture_charge(bytes).ok()?;
    #[cfg(test)]
    let charged = crate::vita::test_backend::Gxm::texture_charge(bytes).ok()?;
    Some(charged as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vita::{Upload, test_backend::{Control, EffectBlend, EffectShader, Event, Gxm}};

    fn setup() -> (Renderer, Control, RenderTarget, Vec<Quad>) {
        let gxm = Gxm::new().unwrap();
        let control = gxm.control.clone();
        control.borrow_mut().shader_capabilities |= (1 << 11) | (1 << 24);
        let renderer = Renderer::new_with_gxm(gxm).unwrap();
        let canvas = renderer.create_composite_target(CANVAS_W, CANVAS_H, "canvas");
        let atlas = renderer.upload(32, 32, Upload::R8(&[255; 1024]), "text test");
        let glyphs = (0..8).map(|i| {
            let mut q = Quad::rect(atlas.clone(),
                [20.25 + i as f32 * 13.0, 40.5, 14.0, 20.0], [0.7, 0.6, 0.5, 0.8]);
            q.shade = Shade::RimText;
            q.haze = [[i as f32 / 32.0, 0.0, (i + 1) as f32 / 32.0, 1.0],
                [0.01, 0.01, 0.4, 0x112233 as f32]];
            q
        }).collect();
        (renderer, control, canvas, glyphs)
    }

    fn frame(pool: &mut Pool, r: &mut Renderer, canvas: &RenderTarget, glyphs: &[Quad]) {
        pool.begin_frame();
        let mut encoder = r.encoder();
        pool.draw_ui(r, &mut encoder, canvas, glyphs);
        r.try_present(encoder, canvas).unwrap();
    }

    #[test]
    fn text_cache_source_draw_counts_rim_page_replaces_glyph_draws_with_one_premultiplied_sample() {
        let (mut r, control, canvas, glyphs) = setup();
        let mut pool = Pool::default();
        frame(&mut pool, &mut r, &canvas, &glyphs);
        assert_eq!(pool.entries.len(), 0, "first/dynamic frame remains direct");
        control.borrow_mut().events.clear();
        frame(&mut pool, &mut r, &canvas, &glyphs);
        assert_eq!(pool.entries.len(), 1);
        assert_eq!(pool.entries[0].source_draws, 8);
        assert!(control.borrow().events.iter().any(|event| matches!(event,
            Event::Draw { shader: EffectShader::RimText, blend: EffectBlend::CoverageOver, .. })));
        control.borrow_mut().events.clear();
        frame(&mut pool, &mut r, &canvas, &glyphs);
        let trace = control.borrow();
        assert_eq!(trace.events.iter().filter(|event| matches!(event,
            Event::Draw { shader: EffectShader::Alter, blend: EffectBlend::Alpha, .. })).count(), 1);
        assert!(!trace.events.iter().any(|event| matches!(event,
            Event::Draw { shader: EffectShader::Text | EffectShader::RimText, .. })));
        assert_eq!(pool.entries[0].target.tex.physical_size(),
            [(pool.entries[0].rect[2] / 2.0) as u32, (pool.entries[0].rect[3] / 2.0) as u32]);
    }

    #[test]
    fn text_cache_source_draw_counts_already_merged_plain_text_does_not_overstate_savings() {
        let (mut r, _control, canvas, mut glyphs) = setup();
        for quad in &mut glyphs { quad.shade = Shade::Text; quad.haze = [[0.0; 4]; 2]; }
        let mut pool = Pool::default();
        frame(&mut pool, &mut r, &canvas, &glyphs);
        frame(&mut pool, &mut r, &canvas, &glyphs);
        assert_eq!(pool.entries.len(), 1);
        assert_eq!(pool.entries[0].source_draws, 1, "the eight plain glyphs already batch into one draw");
        frame(&mut pool, &mut r, &canvas, &glyphs);
        assert_eq!(pool.entries[0].source_draws.saturating_sub(1), 0);
    }

    #[test]
    fn text_cache_reveal_fade_wobble_edge_and_atlas_mutations_invalidate_exact_key() {
        let (mut r, _control, canvas, original) = setup();
        let mut pool = Pool::default();
        frame(&mut pool, &mut r, &canvas, &original);
        frame(&mut pool, &mut r, &canvas, &original);
        assert_eq!(pool.entries.len(), 1);
        let original_key = pool.entries[0].key.clone();
        for kind in 0..5 {
            let mut changed = original.clone();
            match kind {
                0 => changed[0].color[3] = 0.5,
                1 => changed[0].pos[0][0] += 0.125,
                2 => changed[0].haze[1][3] += 1.0,
                3 => changed[0].uv[0][0] += 0.001,
                _ => changed[0].tex.native.changed(),
            }
            frame(&mut pool, &mut r, &canvas, &changed);
            assert!(pool.observed.iter().all(|key| *key != original_key));
            assert_eq!(pool.entries.len(), 1, "changing text must not allocate a cache every frame");
        }
    }

    #[test]
    fn text_cache_trim_preserves_current_frame_readers_and_releases_next_frame() {
        let (mut r, _control, canvas, glyphs) = setup();
        let mut pool = Pool::default();
        frame(&mut pool, &mut r, &canvas, &glyphs);
        frame(&mut pool, &mut r, &canvas, &glyphs);
        let bytes = pool.bytes();
        pool.trim(0);
        assert_eq!(pool.bytes(), bytes);
        pool.begin_frame();
        pool.trim(0);
        assert_eq!(pool.bytes(), 0);
        assert!(pool.entries.is_empty());
    }

    #[test]
    fn text_cache_missing_passthrough_shader_keeps_original_glyphs() {
        let (mut r, control, canvas, glyphs) = setup();
        control.borrow_mut().shader_capabilities &= !(1 << 24);
        let mut pool = Pool::default();
        frame(&mut pool, &mut r, &canvas, &glyphs);
        control.borrow_mut().events.clear();
        frame(&mut pool, &mut r, &canvas, &glyphs);
        assert!(pool.entries.is_empty());
        let trace = control.borrow();
        assert!(!trace.events.iter().any(|event| matches!(event, Event::Create { .. })));
        assert_eq!(trace.events.iter().filter(|event| matches!(event,
            Event::Draw { shader: EffectShader::RimText, .. })).count(), 8);
    }

    #[test]
    fn text_cache_bounds_preserve_raster_phase_and_clip_only_outside_canvas() {
        let (_r, _control, _canvas, mut glyphs) = setup();
        let rect = bounds(&glyphs).unwrap();
        assert_eq!(rect, [18.0, 38.0, 110.0, 26.0]);
        for q in &glyphs {
            for position in q.pos {
                for axis in 0..2 {
                    let original = position[axis] * 0.5;
                    let shifted = (position[axis] - rect[axis]) * 0.5;
                    assert_eq!(original.fract(), shifted.fract());
                }
            }
        }
        glyphs[0].pos[0] = [-15.0, -17.0];
        let rect = bounds(&glyphs).unwrap();
        assert_eq!(rect[0..2], [0.0, 0.0]);
    }

    #[test]
    fn text_cache_preserves_window_text_cursor_draw_order() {
        let (mut r, control, canvas, glyphs) = setup();
        let mut ui = vec![Quad::rect(r.white.clone(), [0.0, 0.0, 200.0, 80.0], [0.2; 4])];
        ui.extend(glyphs);
        ui.push(Quad::rect(r.white.clone(), [150.0, 50.0, 8.0, 8.0], [1.0; 4]));
        let mut pool = Pool::default();
        frame(&mut pool, &mut r, &canvas, &ui);
        frame(&mut pool, &mut r, &canvas, &ui);
        control.borrow_mut().events.clear();
        frame(&mut pool, &mut r, &canvas, &ui);
        let shaders: Vec<_> = control.borrow().events.iter().filter_map(|event| match event {
            Event::Draw { shader, .. } => Some(*shader), _ => None,
        }).collect();
        assert_eq!(shaders, [EffectShader::Sprite, EffectShader::Alter, EffectShader::Sprite]);
    }

    #[test]
    fn text_cache_coverage_over_then_passthrough_matches_direct_rgb_and_keeps_canvas_alpha() {
        let glyphs: [([f32; 3], f32); 2] = [([0.14, 0.07, 0.21], 0.35), ([0.3, 0.2, 0.1], 0.5)];
        let background: [f32; 3] = [0.2, 0.4, 0.6];
        let mut direct = background;
        let (mut cached, mut alpha) = ([0.0; 3], 0.0);
        for (rgb, a) in glyphs {
            for channel in 0..3 {
                direct[channel] = rgb[channel] + direct[channel] * (1.0 - a);
                cached[channel] = rgb[channel] + cached[channel] * (1.0 - a);
            }
            alpha = a + alpha * (1.0 - a);
        }
        for channel in 0..3 {
            let composite = cached[channel] + background[channel] * (1.0 - alpha);
            assert!((composite - direct[channel]).abs() < 1e-6_f32);
            let wrongly_multiplied_twice = cached[channel] * alpha
                + background[channel] * (1.0 - alpha);
            assert!((wrongly_multiplied_twice - direct[channel]).abs() > 0.01);
        }
        assert!((alpha - 0.675).abs() < 1e-6);
        assert!((alpha - (glyphs[0].1 + glyphs[1].1)).abs() > 0.1,
            "the game's additive-alpha Over cannot build this cache");
    }
}
