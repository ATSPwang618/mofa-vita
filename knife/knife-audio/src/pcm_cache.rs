//! Byte-bounded PCM owners shared by named and item-id sound effects.
//! Active mixer voices retain their own Arc and are never evicted for a miss.
use std::{collections::VecDeque, sync::Arc};
use crate::codec::Pcm;
use knife_perf::{Gauge, gauge_set};

struct Entry { key: String, pcm: Option<Arc<Pcm>>, charge: usize }
impl Entry {
    fn idle(&self) -> bool { self.pcm.as_ref().is_none_or(|pcm| Arc::strong_count(pcm) == 1) }
}
pub struct PcmCache { entries: VecDeque<Entry>, bytes: usize, limit: usize, max_entries: usize }
impl Default for PcmCache {
    fn default() -> Self {
        Self::new(if cfg!(target_os = "vita") { knife_perf::memory_budget::CPU_SOUND_BYTES }
            else { usize::MAX }, 256)
    }
}
impl PcmCache {
    pub fn new(limit: usize, max_entries: usize) -> Self {
        Self { entries: VecDeque::new(), bytes: 0, limit, max_entries }
    }
    pub fn bytes(&self) -> usize { self.bytes }
    pub fn len(&self) -> usize { self.entries.len() }
    pub fn get(&mut self, key: &str) -> Option<Option<Arc<Pcm>>> {
        let at = self.entries.iter().position(|entry| entry.key == key)?;
        let entry = self.entries.remove(at).unwrap();
        let result = entry.pcm.clone();
        self.entries.push_back(entry);
        Some(result)
    }
    pub fn insert(&mut self, key: String, pcm: Option<Arc<Pcm>>) -> bool {
        let charge = pcm.as_ref().map_or(0, |pcm| pcm.frames.capacity().saturating_mul(size_of::<[f32; 2]>()));
        if charge > self.limit || self.max_entries == 0 || self.entries.try_reserve(1).is_err() { return false; }
        if let Some(at) = self.entries.iter().position(|entry| entry.key == key) {
            if !self.entries[at].idle() { return false; }
            self.remove(at);
        }
        while self.bytes > self.limit - charge || self.entries.len() >= self.max_entries {
            let Some(at) = self.entries.iter().position(Entry::idle) else { return false; };
            self.remove(at);
        }
        self.bytes += charge;
        self.entries.push_back(Entry { key, pcm, charge });
        self.gauges();
        true
    }
    /// Return only bytes actually freed; an active external Arc is not reclaimable.
    pub fn trim_idle(&mut self, need: usize) -> usize {
        let mut freed = 0usize;
        while freed < need {
            let Some(at) = self.entries.iter().position(Entry::idle) else { break; };
            freed = freed.saturating_add(self.remove(at));
        }
        freed
    }
    fn remove(&mut self, at: usize) -> usize {
        let entry = self.entries.remove(at).unwrap();
        self.bytes -= entry.charge;
        self.gauges();
        entry.charge
    }
    fn gauges(&self) {
        gauge_set(Gauge::SoundCacheBytes, self.bytes as u64);
        gauge_set(Gauge::SoundCacheEntries, self.entries.len() as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sound(capacity: usize) -> Arc<Pcm> {
        let mut frames = Vec::with_capacity(capacity);
        frames.push([0.25, -0.25]);
        Arc::new(Pcm { sample_rate: 48_000, frames, loop_range: None, mono: false })
    }
    #[test]
    fn sound_cache_bounds_allocated_capacity_and_evicts_cold_effects_before_256_names() {
        let mut cache = PcmCache::new(96, 256);
        for key in ["a", "b", "c"] { assert!(cache.insert(key.into(), Some(sound(4)))); }
        assert_eq!(cache.bytes(), 96, "charge capacity, not the one initialized frame");
        drop(cache.get("a").unwrap());
        assert!(cache.insert("d".into(), Some(sound(4))));
        assert_eq!(cache.bytes(), 96, "new effects cannot grow the retained PCM beyond its byte budget");
        assert_eq!(cache.len(), 3);
        assert!(cache.get("b").is_none(), "the least recently used unpinned effect is first");
        assert!(cache.get("a").is_some());
    }
    #[test]
    fn sound_cache_pressure_frees_only_unpinned_owners_and_never_stops_playback() {
        let mut cache = PcmCache::new(64, 256);
        let playing = sound(4);
        cache.insert("playing".into(), Some(playing.clone()));
        cache.insert("idle".into(), Some(sound(4)));
        assert_eq!(cache.trim_idle(usize::MAX), 32);
        assert_eq!(cache.bytes(), 32);
        assert_eq!(playing.frames, vec![[0.25, -0.25]]);
        drop(playing);
        assert_eq!(cache.trim_idle(usize::MAX), 32);
        assert_eq!(cache.bytes(), 0);
    }
    #[test]
    fn sound_cache_bypasses_large_or_pinned_full_entries_without_dropping_the_callers_sound() {
        let mut cache = PcmCache::new(32, 4);
        let playing = sound(4);
        cache.insert("playing".into(), Some(playing.clone()));
        let next = sound(4);
        assert!(!cache.insert("next".into(), Some(next.clone())));
        assert!(cache.get("next").is_none());
        assert_eq!(next.frames.len(), 1);
        assert_eq!(cache.bytes(), 32);
        assert!(!cache.insert("long".into(), Some(sound(5))));
        assert!(cache.get("playing").is_some());
    }
    #[test]
    fn sound_cache_entry_limit_also_bounds_negative_lookups() {
        let mut cache = PcmCache::new(32, 2);
        for key in ["a", "b", "c"] { cache.insert(key.into(), None); }
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.bytes(), 0);
        assert!(cache.get("a").is_none());
        assert!(cache.get("c").unwrap().is_none());
    }
}
