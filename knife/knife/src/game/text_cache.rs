//! Bounded immutable text segments. Stable prefixes survive typewriter reveals;
//! changing tails, backgrounds and cursors retain their original UI order.
//! Keys retain every shader input, including per-glyph alpha and atlas versions.
use std::sync::{Arc, Weak};

use knife_perf::{Gauge, Metric, Scope, memory_budget};
use knife_render::{Blend, CANVAS_H, CANVAS_W, CommandEncoder, GpuTexture, Quad,
    RenderTarget, Renderer, Shade};

const MIN_GLYPHS: usize = 8;
const MAX_GLYPHS: usize = 1024;
const MAX_OBSERVATIONS: usize = MAX_GLYPHS / MIN_GLYPHS;
const MAX_BUILDS_PER_FRAME: usize = 2;
const IDLE_FRAMES: u64 = 600;

#[derive(Clone, PartialEq, Eq)]
struct Key(Arc<[u64]>);

struct Entry {
    key: Key,
    // Weak references prevent a freed atlas' native pointer being recycled into
    // a false key hit, without retaining old font pages in GPU memory.
    sources: Vec<Weak<GpuTexture>>,
    target: RenderTarget,
    bytes: usize,
    source_draws: usize,
    last_used: u64,
    promoted: bool,
}

#[derive(Default)]
pub(super) struct Pool {
    entries: Vec<Entry>,
    previous: Vec<Key>,
    observed: Vec<Key>,
    epoch: u64,
    bytes: usize,
    key_scratch: Vec<u64>,
    builds: usize,
}

impl Pool {
    pub fn begin_frame(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
        // Reuse both observation vectors. Repeated/cache-hit keys share their
        // immutable words instead of allocating/copying every glyph's key.
        std::mem::swap(&mut self.previous, &mut self.observed);
        self.observed.clear();
        self.builds = 0;
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

    fn take(&mut self, index: usize) -> Entry {
        let entry = self.entries.swap_remove(index);
        self.bytes -= entry.bytes;
        knife_perf::add(Metric::TextCacheEvict, entry.bytes as u64, 1);
        self.gauges();
        entry
    }
    fn remove(&mut self, index: usize) { drop(self.take(index)); }

    fn gauges(&self) {
        knife_perf::gauge_set(Gauge::TextCacheBytes, self.bytes as u64);
        knife_perf::gauge_set(Gauge::TextCacheEntries, self.entries.len() as u64);
    }

    /// Typing retains immutable eight-glyph prefixes. Once the caller confirms
    /// completion, a repeated exact whole-run key promotes the run into one
    /// surface, including its short tail. Per-glyph alpha remains baked exactly.
    pub fn draw_ui(&mut self, r: &mut Renderer, enc: &mut CommandEncoder,
        canvas: &RenderTarget, quads: &[Quad], finished: bool) {
        let mut begin = 0;
        while begin < quads.len() {
            let text = eligible(&quads[begin]);
            let mut end = begin + 1;
            while end < quads.len() && eligible(&quads[end]) == text { end += 1; }
            let run = &quads[begin..end];
            if !text || run.len() > MAX_GLYPHS {
                if text { knife_perf::add(Metric::TextCacheDirectGlyphs, 0, run.len() as u64); }
                r.draw(enc, canvas, None, run);
            } else if finished && self.draw_run(r, enc, canvas, run, true) {
                // Keep non-text boundaries intact even when the full finished
                // paragraph/page becomes one composite.
            } else {
                // Canonical boundaries never move when more glyphs appear.
                // An incomplete tail is direct: brief pauses during typing do
                // not create one target for every newly revealed character.
                let mut line_begin = 0;
                while line_begin < run.len() {
                    let mut line_end = line_begin + 1;
                    while line_end < run.len() && same_line(&run[line_end - 1], &run[line_end]) {
                        line_end += 1;
                    }
                    for segment in run[line_begin..line_end].chunks(MIN_GLYPHS) {
                        if segment.len() < MIN_GLYPHS || !self.draw_run(r, enc, canvas, segment, false) {
                            knife_perf::add(Metric::TextCacheDirectGlyphs, 0, segment.len() as u64);
                            r.draw(enc, canvas, None, segment);
                        }
                    }
                    line_begin = line_end;
                }
            }
            begin = end;
        }
    }

    fn draw_run(&mut self, r: &mut Renderer, enc: &mut CommandEncoder,
        canvas: &RenderTarget, quads: &[Quad], promoted: bool) -> bool {
        if self.observed.len() >= MAX_OBSERVATIONS {
            return false;
        }
        let Some(rect) = bounds(quads) else { return false };
        self.key_scratch.clear();
        self.key_scratch.extend([quads.len() as u64, rect[2].to_bits() as u64, rect[3].to_bits() as u64]);
        for quad in quads {
            append_key(&mut self.key_scratch, quad, rect);
        }
        // Lookup compares exact words. A hash alone cannot establish a hit.
        let key = self.previous.iter().chain(self.entries.iter().map(|entry| &entry.key))
            .find(|key| key.0.as_ref() == self.key_scratch.as_slice()).cloned()
            .unwrap_or_else(|| Key(Arc::from(self.key_scratch.as_slice())));
        if self.previous.get(self.observed.len()).is_some_and(|previous| previous != &key) {
            knife_perf::add(Metric::TextCacheInvalidate, 0, quads.len() as u64);
        }
        self.observed.push(key.clone());
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key
            && entry.sources.iter().all(|source| source.strong_count() != 0)) {
            entry.last_used = self.epoch;
            entry.promoted |= promoted;
            // The key normalizes only an integer physical-pixel translation.
            // Scroll the cached result using this frame's rect, with unchanged
            // subpixel raster phase and unchanged per-glyph coverage.
            if r.try_composite_text_cache(enc, canvas, &entry.target, rect).is_ok() {
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
        if self.builds >= MAX_BUILDS_PER_FRAME {
            knife_perf::add(Metric::TextCacheFallback, 0, 7);
            return false;
        }
        self.builds += 1;
        if !r.text_cache_available().unwrap_or(false) {
            knife_perf::add(Metric::TextCacheFallback, 0, 6);
            return false;
        }
        let logical = [rect[2] as u32, rect[3] as u32];
        let physical = [logical[0] / 2, logical[1] / 2];
        let Some(bytes) = charge(physical) else { return false };
        if bytes > memory_budget::GPU_TEXT_BYTES { return false }
        // Recycle a cold matching surface when admission/capacity would require
        // eviction anyway. Recorded readers retain owners and native WAR fences.
        let pressured = self.bytes + bytes > memory_budget::GPU_TEXT_BYTES
            || self.entries.len() >= memory_budget::GPU_TEXT_ENTRIES
            || r.vita_cache_headroom().unwrap_or(0) < bytes + memory_budget::GPU_CACHE_SPARE_BYTES;
        let recycled = if pressured {
            self.entries.iter().enumerate().filter(|(_, entry)| entry.last_used != self.epoch
                && entry.target.tex.width == logical[0] && entry.target.tex.height == logical[1]
                && entry.target.tex.physical_size() == physical)
                .min_by_key(|(_, entry)| entry.last_used).map(|(index, _)| index)
                .map(|index| self.take(index).target)
        } else { None };
        // A failed promotion must keep its useful small-prefix fallback. Check
        // native room before retiring any old representation for the large run.
        let promotion_admitted = promoted && recycled.is_none();
        if promotion_admitted && !r.vita_prepare_cache_allocation(enc, quads, bytes).unwrap_or(false) {
            knife_perf::add(Metric::TextCacheFallback, 0, 8); return false;
        }
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
        let target = match recycled {
            Some(target) => {
                knife_perf::add(Metric::TextCacheTargetReuse, bytes as u64, 1);
                target
            }
            None => {
                if !promotion_admitted && !r.vita_prepare_cache_allocation(enc, quads, bytes).unwrap_or(false) {
                    knife_perf::add(Metric::TextCacheFallback, 0, 8); return false;
                }
                match r.try_create_scene_target(logical[0], logical[1], "text cache") {
                    Ok(target) => target,
                    Err(_) => { knife_perf::add(Metric::TextCacheFallback, 0, 4); return false }
                }
            }
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
        self.entries.push(Entry { key: key.clone(), sources, target, bytes, source_draws,
            last_used: self.epoch, promoted });
        self.bytes += bytes;
        if promoted {
            self.retire_covered_segments(quads, &key);
            knife_perf::add(Metric::TextCachePromote, bytes as u64, quads.len() as u64);
        }
        self.gauges();
        build.finish(bytes as u64, quads.len() as u64);
        true
    }

    /// The promoted surface replaces precisely these old immutable segments.
    /// Do not retain both representations until a later pressure sweep. Any
    /// segment already sampled by another draw_ui call this frame stays pinned.
    fn retire_covered_segments(&mut self, quads: &[Quad], promoted: &Key) {
        let mut begin = 0;
        while begin < quads.len() {
            let mut end = begin + 1;
            while end < quads.len() && same_line(&quads[end - 1], &quads[end]) { end += 1; }
            for segment in quads[begin..end].chunks(MIN_GLYPHS) {
                if segment.len() != MIN_GLYPHS { continue; }
                let Some(rect) = bounds(segment) else { continue; };
                self.key_scratch.clear();
                self.key_scratch.extend([segment.len() as u64, rect[2].to_bits() as u64, rect[3].to_bits() as u64]);
                for q in segment { append_key(&mut self.key_scratch, q, rect); }
                if let Some(index) = self.entries.iter().position(|entry| !entry.promoted
                    && entry.last_used != self.epoch && entry.key != *promoted
                    && entry.key.0.as_ref() == self.key_scratch.as_slice()) {
                    self.remove(index);
                }
            }
            begin = end;
        }
    }
}

/// This fixed layout covers every field admitted by eligible(): Alpha blend,
/// no rule/clip/soft/mask. The origin is an exact physical-pixel boundary; all
/// glyph positions and their mutual overlaps keep their original raster phase.
fn append_key(words: &mut Vec<u64>, q: &Quad, rect: [f32; 4]) {
    let (id, generation) = q.tex.render_identity();
    words.extend([id as u64, generation, q.tex.width as u64, q.tex.height as u64]);
    words.extend(q.tex.physical_size().map(u64::from));
    words.push(q.shade as u64);
    for p in q.pos {
        words.extend([(p[0] - rect[0]).to_bits() as u64, (p[1] - rect[1]).to_bits() as u64]);
    }
    for p in q.uv { words.extend(p.map(|v| v.to_bits() as u64)); }
    words.extend(q.color.map(|v| v.to_bits() as u64));
    for p in q.haze { words.extend(p.map(|v| v.to_bits() as u64)); }
}

fn eligible(quad: &Quad) -> bool {
    matches!(quad.shade, Shade::Text | Shade::RimText) && quad.blend == Blend::Alpha
        && quad.clip.is_none() && quad.rule.is_none() && quad.soft.is_none() && !quad.masked
}

/// A clear vertical jump starts another segment row. This is a scheduling
/// boundary only: superscripts/large wobble may split a row conservatively, but
/// no glyph is moved or reordered. Removing a scrolled-off row cannot shift the
/// canonical eight-glyph boundaries of every following visible row.
fn same_line(a: &Quad, b: &Quad) -> bool {
    let span = |q: &Quad| {
        let min = q.pos.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        let max = q.pos.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
        ((min + max) * 0.5, max - min)
    };
    let ((ay, ah), (by, bh)) = (span(a), span(b));
    (by - ay).abs() <= ah.max(bh) * 0.5
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
    let charged = knife_platform_vita::gxm::Gxm::texture_charge(bytes).ok()?;
    Some(charged as usize)
}
