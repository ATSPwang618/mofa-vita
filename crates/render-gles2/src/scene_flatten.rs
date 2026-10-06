//! Retain small display gathers without retaining their source pixels.
use crate::{Image, image::Plane, scene_damage::Version};
use krkr_protocol::budget::{Budget, Permit};
use krkr_protocol::graphics::Rect;
use std::collections::VecDeque;

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 4;

pub(crate) struct Entry {
    pub source: Version,
    pub plane: Plane,
    _permit: Permit,
}
impl Entry {
    fn bytes(&self) -> usize {
        self.plane.tiles[0].texture.allocation_bytes()
    }
    pub fn new(source: &Image, plane: Plane, budget: &Budget) -> Option<Self> {
        let source = Version::capture(source).ok()?;
        let permit = budget
            .reserve(
                source.bytes()
                    + std::mem::size_of::<Self>()
                    + plane.tiles.capacity() * std::mem::size_of::<crate::image::Tile>(),
            )
            .ok()?;
        Some(Self {
            source,
            plane,
            _permit: permit,
        })
    }
}

#[derive(Default)]
pub(crate) struct Cache(VecDeque<Entry>);
impl Cache {
    pub fn clear(&mut self) {
        self.0.clear();
    }
    pub fn trim(&mut self) {
        self.0.retain(|entry| entry.source.current());
    }
    pub fn take(&mut self, source: &Image, area: Rect) -> Option<Entry> {
        let index = self.0.iter().rposition(|entry| {
            entry.plane.tiles[0].rectangle.intersection(area) == Some(area)
                && entry.source.same_storage(source)
        })?;
        self.0.remove(index)
    }
    pub fn covers(&self, source: &Image, area: Rect) -> bool {
        self.0.iter().any(|entry| {
            entry.plane.tiles[0].rectangle.intersection(area) == Some(area)
                && entry.source.same_storage(source)
        })
    }
    pub fn make_room(&mut self, bytes: usize) {
        self.trim();
        while !self.0.is_empty()
            && (self.0.len() >= MAX_ENTRIES
                || self
                    .0
                    .iter()
                    .map(Entry::bytes)
                    .sum::<usize>()
                    .saturating_add(bytes)
                    > MAX_BYTES)
        {
            self.0.pop_front();
        }
    }
    pub fn put(&mut self, entry: Entry) {
        let bytes = entry.bytes();
        self.make_room(bytes);
        if bytes <= MAX_BYTES {
            self.0.push_back(entry);
        }
    }
}
