//! Optional subtree rasters. Weak source identities and GPU write generations
//! invalidate results without retaining source images or forcing their COW.
use crate::gpu::{Allocation, Image};
use krkr_protocol::graphics::{Blend, ImageId, Rect, Scene, Size};
use krkr_render::{budget::Permit, scene::Children};
use std::{
    collections::HashMap,
    sync::{Arc, Weak, atomic::Ordering},
};

// One paragraph can contain hundreds of character layers. Metadata remains
// bounded and charged against the resident budget alongside its cached raster.
// Larger trees use normal composition.
const MAX_PARTS: usize = 1024;
const MAX_ENTRIES: usize = 64;

pub(crate) struct Signature {
    region: Rect,
    parts: Vec<Part>,
}
struct Part {
    depth: usize,
    rectangle: Rect,
    offset: (i32, i32),
    blend: Blend,
    opacity: u8,
    neutral: u32,
    image: Option<ImageVersion>,
}
struct ImageVersion {
    size: Size,
    main: Option<Weak<Allocation>>,
    generation: u64,
    deferred: Option<Weak<crate::deferred::Deferred>>,
}
impl ImageVersion {
    fn capture(image: &Image) -> Self {
        Self {
            size: image.size,
            main: image.main.as_ref().map(Arc::downgrade),
            generation: image
                .main
                .as_ref()
                .map_or(0, |p| p.generation.load(Ordering::Relaxed)),
            deferred: image.deferred.as_ref().map(Arc::downgrade),
        }
    }
}
fn same_weak<T>(a: &Option<Weak<T>>, b: &Option<Weak<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Weak::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}
impl PartialEq for Part {
    fn eq(&self, other: &Self) -> bool {
        self.depth == other.depth
            && self.rectangle == other.rectangle
            && self.offset == other.offset
            && self.blend == other.blend
            && self.opacity == other.opacity
            && self.neutral == other.neutral
            && match (&self.image, &other.image) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.size == b.size
                        && a.generation == b.generation
                        && same_weak(&a.main, &b.main)
                        && same_weak(&a.deferred, &b.deferred)
                }
                _ => false,
            }
    }
}
impl Signature {
    pub fn capture(
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
        children: &Children,
        root: usize,
        region: Rect,
    ) -> Option<Self> {
        let mut parts = Vec::new();
        let mut stack = vec![(root, 0)];
        while let Some((index, depth)) = stack.pop() {
            let node = &scene.nodes[index];
            if !node.visible || node.opacity == 0 {
                continue;
            }
            if parts.len() == MAX_PARTS || stack.len() + children[index].len() > MAX_PARTS {
                return None;
            }
            let image = match &node.image {
                Some(reference) => Some(ImageVersion::capture(images.get(&reference.id)?)),
                None => None,
            };
            let mut rectangle = node.rectangle;
            if index == root {
                rectangle.left = 0;
                rectangle.top = 0;
            }
            parts.push(Part {
                depth,
                rectangle,
                offset: (node.image_left, node.image_top),
                blend: node.blend,
                opacity: if index == root { 255 } else { node.opacity },
                neutral: node.neutral_color,
                image,
            });
            stack.extend(
                children[index]
                    .iter()
                    .rev()
                    .map(|&child| (child, depth + 1)),
            );
        }
        Some(Self { region, parts })
    }
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Entry>() + self.parts.capacity() * std::mem::size_of::<Part>()
    }
}
pub(crate) struct Entry {
    pub owner: Weak<()>,
    pub signature: Signature,
    pub image: Arc<Allocation>,
    pub _metadata: Permit,
}
#[derive(Default)]
pub(crate) struct Cache {
    entries: Vec<Entry>,
    pub hits: u64,
}
impl Cache {
    pub fn evict_oldest(&mut self) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        self.entries.remove(0);
        true
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn trim(&mut self) {
        self.entries.retain(|entry| entry.owner.strong_count() != 0);
    }
    pub fn get(
        &mut self,
        owner: &Arc<()>,
        signature: &Signature,
    ) -> Option<(Arc<Allocation>, Rect)> {
        self.trim();
        let weak = Arc::downgrade(owner);
        let index = self.entries.iter().position(|entry| {
            Weak::ptr_eq(&entry.owner, &weak)
                && entry.signature.parts == signature.parts
                && entry.signature.region.intersection(signature.region) == Some(signature.region)
        })?;
        let entry = self.entries.remove(index);
        // A clipped redraw can sample a larger cached group. Coordinates are
        // relative to the group's old clip, not the new destination clip.
        let region = Rect {
            left: signature.region.left - entry.signature.region.left,
            top: signature.region.top - entry.signature.region.top,
            ..signature.region
        };
        let image = entry.image.clone();
        self.entries.push(entry);
        self.hits += 1;
        Some((image, region))
    }
    pub fn insert(&mut self, entry: Entry) {
        self.entries
            .retain(|old| old.owner.strong_count() != 0 && !Weak::ptr_eq(&old.owner, &entry.owner));
        if entry.owner.strong_count() == 0 {
            return;
        }
        if self.entries.len() == MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }
}
