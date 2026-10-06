//! Evictable aliases of renderer-owned images. This LRU pins existing GPU (or
//! software-renderer) allocations; it never retains decoded CPU upload pixels.
use crate::graphics::{ImageRef, Size};
use lru::LruCache;
use std::sync::{Arc, Mutex};

pub const MAX_ENTRIES: usize = 1024;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Key {
    /// Main, mask, province and optional logical-size metadata.
    pub names: [Option<Vec<u16>>; 4],
    pub color_key: u32,
    /// Luminance transition rules are tiled to this logical size. They must
    /// not alias ordinary color images or rules of another size.
    pub rule_size: Option<(u32, u32)>,
}
#[derive(Clone)]
pub struct Entry {
    pub image: ImageRef,
    pub size: Size,
    pub tags: Arc<Vec<(String, String)>>,
    pub bytes: usize,
}
#[derive(Clone)]
pub struct Cache(Arc<Mutex<State>>);
struct State {
    maximum: usize,
    limit: usize,
    bytes: usize,
    generation: u64,
    entries: LruCache<Key, (Entry, usize)>,
}
impl Cache {
    pub fn new(maximum: usize) -> Self {
        Self(Arc::new(Mutex::new(State {
            maximum,
            limit: maximum,
            bytes: 0,
            generation: 0,
            entries: LruCache::unbounded(),
        })))
    }
    pub fn limit(&self) -> usize {
        self.0.lock().unwrap().limit
    }
    pub fn generation(&self) -> u64 {
        self.0.lock().unwrap().generation
    }
    pub fn set_limit(&self, bytes: i32) {
        let mut s = self.0.lock().unwrap();
        s.limit = (bytes as u64).min(s.maximum as u64) as usize;
        s.generation = s.generation.wrapping_add(1);
        while s.bytes > s.limit {
            s.evict();
        }
    }
    pub fn clear(&self) {
        let mut s = self.0.lock().unwrap();
        s.entries.clear();
        s.bytes = 0;
        s.generation = s.generation.wrapping_add(1);
    }
    pub fn get(&self, key: &Key) -> Option<Entry> {
        let mut s = self.0.lock().unwrap();
        s.entries.get(key).map(|(entry, _)| entry.clone())
    }
    pub fn insert(&self, key: Key, entry: Entry, generation: u64) {
        // Retained metadata is bounded along with pixels. Tags can otherwise
        // dominate the cache when an image is small but carries large strings.
        let mut charge = entry
            .bytes
            .saturating_add(std::mem::size_of::<(Key, Entry, usize)>() + 64);
        for name in key.names.iter().flatten() {
            charge = charge.saturating_add(name.capacity().saturating_mul(2));
        }
        charge = charge.saturating_add(
            entry
                .tags
                .capacity()
                .saturating_mul(std::mem::size_of::<(String, String)>()),
        );
        for (name, value) in entry.tags.iter() {
            charge = charge
                .saturating_add(name.capacity())
                .saturating_add(value.capacity());
        }
        let mut s = self.0.lock().unwrap();
        if generation != s.generation || charge > s.limit || s.limit == 0 {
            return;
        }
        if let Some((_, bytes)) = s.entries.pop(&key) {
            s.bytes -= bytes;
        }
        while charge > s.limit - s.bytes || s.entries.len() >= MAX_ENTRIES {
            s.evict();
        }
        s.bytes += charge;
        s.entries.put(key, (entry, charge));
    }
    /// Hosts may evict optional aliases before allocating authoritative images.
    pub fn evict(&self) -> bool {
        self.0.lock().unwrap().evict()
    }
    /// Evict the oldest matching entry without touching the other LRU ages.
    pub fn evict_where(&self, mut eligible: impl FnMut(&Entry) -> bool) -> bool {
        let mut s = self.0.lock().unwrap();
        let key = s
            .entries
            .iter()
            .rev()
            .find_map(|(key, (entry, _))| eligible(entry).then(|| key.clone()));
        if let Some(key) = key {
            let (_, charge) = s.entries.pop(&key).unwrap();
            s.bytes -= charge;
            true
        } else {
            false
        }
    }
}
impl State {
    fn evict(&mut self) -> bool {
        if let Some((_, (_, charge))) = self.entries.pop_lru() {
            self.bytes -= charge;
            true
        } else {
            false
        }
    }
}
