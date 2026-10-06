//! Script gradients often read adjacent pixels from an unchanged image.
//! Cache small CPU blocks without retaining their GPU texture storage.
use crate::{Image, ReadPixels, scene_damage::Version};
use krkr_protocol::{
    budget::{Budget, Permit},
    graphics::{Rect, Size},
};
use std::collections::VecDeque;

// A 1024-wide scan needs 64 square blocks. Sixteen entries then evict the
// preceding row before its next pixel can reuse them. Wider blocks keep one
// complete row band resident; the sixteen-entry cache holds at most 64 KiB.
const WIDTH: u32 = 64;
const HEIGHT: u32 = 16;
const ENTRIES: usize = 16;

struct Entry {
    version: Version,
    area: Rect,
    read: ReadPixels,
    _permit: Permit,
}

#[derive(Default)]
pub(crate) struct Cache(VecDeque<Entry>);

pub(crate) fn area(size: Size, x: i32, y: i32) -> Rect {
    let left = x as u32 / WIDTH * WIDTH;
    let top = y as u32 / HEIGHT * HEIGHT;
    Rect {
        left: left as i32,
        top: top as i32,
        width: WIDTH.min(size.width - left),
        height: HEIGHT.min(size.height - top),
    }
}

pub(crate) fn value(read: &ReadPixels, area: Rect, x: i32, y: i32) -> u32 {
    let index = ((y - area.top) as usize * read.size.width as usize + (x - area.left) as usize)
        * read.channels;
    let bytes = &read.data.as_slice()[index..];
    if read.channels == 1 {
        u32::from(bytes[0])
    } else {
        u32::from_be_bytes([bytes[3], bytes[0], bytes[1], bytes[2]])
    }
}

impl Cache {
    pub fn clear(&mut self) {
        self.0.clear();
    }
    pub fn get(&mut self, image: &Image, x: i32, y: i32, province: bool) -> Option<u32> {
        let point = Rect {
            left: x,
            top: y,
            width: 1,
            height: 1,
        };
        let index = self.0.iter().rposition(|e| {
            e.area.intersection(point) == Some(point) && e.version.matches_plane(image, province)
        })?;
        let entry = self.0.remove(index)?;
        let pixel = value(&entry.read, entry.area, x, y);
        self.0.push_back(entry);
        Some(pixel)
    }
    pub fn insert(
        &mut self,
        image: &Image,
        area: Rect,
        province: bool,
        read: ReadPixels,
        budget: &Budget,
    ) {
        self.0.retain(|e| e.version.current());
        if self.0.len() == ENTRIES {
            self.0.pop_front();
        }
        let Ok(version) = Version::capture_plane(image, province) else {
            return;
        };
        let Ok(permit) = budget.reserve(version.bytes() + std::mem::size_of::<Entry>()) else {
            return;
        };
        self.0.push_back(Entry {
            version,
            area,
            read,
            _permit: permit,
        });
    }
}
