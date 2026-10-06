//! Reuse copies onto equal solid canvases or the same immutable image version.
//! Both input/output versions are weak: this cache cannot retain a texture or
//! cause copy-on-write. Every lookup verifies the exact source generation.
use crate::{Image, image::Plane, scene_damage::Version};
use krkr_protocol::{
    budget::{Budget, Permit},
    graphics::{Rect, Size},
};
use std::{
    collections::VecDeque,
    rc::{Rc, Weak},
};

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Key {
    pub color: Option<u32>,
    pub logical: Size,
    pub stored: Size,
    pub source: Rect,
    pub destination: Rect,
}
struct Entry {
    key: Key,
    source: Version,
    target: Option<Version>,
    result: Version,
    plane: Weak<Plane>,
    _permit: Permit,
}
#[derive(Default)]
pub(crate) struct Cache(VecDeque<Entry>);
impl Cache {
    pub fn trim(&mut self) {
        self.0.retain(|e| {
            e.source.current()
                && e.target.as_ref().is_none_or(Version::current)
                && e.result.current()
                && e.plane.strong_count() != 0
        });
    }
    pub fn get(&mut self, source: &Image, target: &Image, key: Key) -> Option<Rc<Plane>> {
        let Some(index) = self.0.iter().rposition(|e| {
            e.key == key
                && e.source.matches(source)
                && e.target
                    .as_ref()
                    .is_none_or(|version| version.matches(target))
                && e.result.current()
                && e.plane.strong_count() != 0
        }) else {
            self.trim();
            return None;
        };
        let entry = self.0.remove(index)?;
        let plane = entry.plane.upgrade();
        self.0.push_back(entry);
        plane
    }
    pub fn insert(
        &mut self,
        source: Version,
        target: Option<Version>,
        key: Key,
        image: &Image,
        budget: &Budget,
    ) {
        self.trim();
        if !source.current() || target.as_ref().is_some_and(|version| !version.current()) {
            return;
        }
        let Ok(result) = Version::capture(image) else {
            return;
        };
        let Ok(permit) = budget.reserve(
            source.bytes()
                + target.as_ref().map_or(0, Version::bytes)
                + result.bytes()
                + std::mem::size_of::<Entry>(),
        ) else {
            return;
        };
        if self.0.len() == 32 {
            self.0.pop_front();
        }
        self.0.push_back(Entry {
            key,
            source,
            target,
            result,
            plane: Rc::downgrade(image.main.as_ref().unwrap()),
            _permit: permit,
        });
    }
}
