//! Reuse deterministic image edits while their pixels are still alive.
//! Weak versions never pin an image or force a copy on its next write.
use crate::{Image, image::Plane, scene_damage::Version};
use krkr_protocol::{
    budget::{Budget, Permit},
    filter::{Filter, Kind},
    graphics::{Adjustment, Rect},
};
use std::{
    collections::VecDeque,
    rc::{Rc, Weak},
    sync::Arc,
};

pub(crate) enum Key {
    Gray,
    Gamma(Arc<[[u32; 4]; 256]>, bool),
    Lookup(Arc<krkr_protocol::pixels::Bytes>),
    Colorize(Arc<krkr_protocol::pixels::Bytes>, u16),
    Blur([u32; 2], bool),
}
impl Key {
    pub fn new(operation: &Adjustment) -> Option<Self> {
        Some(match operation {
            Adjustment::GrayScale => Self::Gray,
            Adjustment::Gamma { table, additive } => Self::Gamma(table.clone(), *additive),
            Adjustment::Filter(Filter {
                kind: Kind::Lookup,
                table,
            }) => Self::Lookup(table.clone()),
            Adjustment::Filter(Filter {
                kind: Kind::Colorize { amount },
                table,
            }) => Self::Colorize(table.clone(), *amount),
            Adjustment::BoxBlur { radius, alpha } => Self::Blur(*radius, *alpha),
            _ => return None,
        })
    }
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Gray, Self::Gray) => true,
            (Self::Gamma(a, x), Self::Gamma(b, y)) => x == y && a == b,
            (Self::Lookup(a), Self::Lookup(b)) => a.as_slice() == b.as_slice(),
            (Self::Colorize(a, x), Self::Colorize(b, y)) => x == y && a.as_slice() == b.as_slice(),
            (Self::Blur(a, x), Self::Blur(b, y)) => a == b && x == y,
            _ => false,
        }
    }
}
struct Entry {
    key: Key,
    area: Rect,
    source: Version,
    result: Version,
    plane: Weak<Plane>,
    _permit: Permit,
}
#[derive(Default)]
pub(crate) struct Cache(VecDeque<Entry>);
impl Cache {
    pub fn trim(&mut self) {
        self.0
            .retain(|e| e.source.current() && e.result.current() && e.plane.strong_count() != 0);
    }
    pub fn get(&mut self, source: &Image, area: Rect, key: &Key) -> Option<Rc<Plane>> {
        let Some(index) = self.0.iter().rposition(|e| {
            e.area == area
                && e.key.same(key)
                && e.source.matches(source)
                && e.result.current()
                && e.plane.strong_count() != 0
        }) else {
            self.trim();
            return None;
        };
        let entry = self.0.remove(index)?;
        let result = entry.plane.upgrade();
        self.0.push_back(entry);
        result
    }
    pub fn insert(
        &mut self,
        key: Key,
        area: Rect,
        source: Version,
        image: &Image,
        budget: &Budget,
    ) {
        self.trim();
        if !source.current() {
            return;
        }
        let Ok(result) = Version::capture(image) else {
            return;
        };
        if self.0.len() == 32 {
            self.0.pop_front();
        }
        let Ok(permit) =
            budget.reserve(source.bytes() + result.bytes() + std::mem::size_of::<Entry>())
        else {
            return;
        };
        self.0.push_back(Entry {
            key,
            area,
            source,
            result,
            plane: Rc::downgrade(image.main.as_ref().unwrap()),
            _permit: permit,
        });
    }
}
