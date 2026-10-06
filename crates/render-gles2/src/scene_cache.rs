//! Completed subtree rasters, following the protocol cache token used by WGPU.
//! Weak texture identities plus write generations never keep source images alive.
use crate::{Image, scene::raster::Raster, scene_damage::Version};
use krkr_protocol::{
    budget::{Budget, Permit},
    graphics::{Blend, ImageId, Rect, Scene},
};
use krkr_render::scene::Children;
use std::{
    collections::HashMap,
    sync::{Arc, Weak as Owner},
};
#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[path = "../tests/scene_cache/internal.rs"]
mod tests;

pub(crate) const LIMIT: usize = 8 * 1024 * 1024;
// Character-per-layer message systems can exceed 128 nodes/tiles in one
// paragraph. Keep their immutable raster reusable during a parent fade.
// Metadata is still bounded here and charged with the raster against LIMIT;
// increasing this traversal cap does not increase the 8 MiB cache budget.
const MAX_PARTS: usize = 1024;

pub(crate) struct Signature {
    root: usize,
    content: bool,
    raster: Raster,
    origin: (i64, i64),
    region: Rect,
    parts: Vec<Part>,
}
struct Change {
    signature: Signature,
    unchanged_prefix: usize,
}
#[derive(Clone, Copy, PartialEq)]
struct Geometry {
    depth: usize,
    children: bool,
    rectangle: Rect,
    offset: (i32, i32),
    blend: Blend,
    opacity: u8,
    neutral: u32,
    origin: (i64, i64),
    clip: Option<Rect>,
}
#[derive(Clone)]
struct Part {
    geometry: Geometry,
    image: Option<Version>,
}
impl Part {
    fn same_geometry(&self, other: &Self) -> bool {
        self.geometry == other.geometry && self.image.is_some() == other.image.is_some()
    }
}
/// Borrow the current scene until a changed subtree actually needs a new stamp.
pub(crate) struct Query<'a> {
    pub scene: &'a Scene,
    pub images: &'a HashMap<ImageId, Image>,
    pub children: &'a Children,
    pub root: usize,
    pub raster: Raster,
    pub origin: (i64, i64),
    pub region: Rect,
    pub content: bool,
}
impl Query<'_> {
    fn visit(&self, mut visit: impl FnMut(Geometry, Option<&Image>) -> Option<()>) -> Option<()> {
        struct Level<'a> {
            remaining: &'a [usize],
            origin: (i64, i64),
            clip: Rect,
        }
        let roots = [self.root];
        let mut stack = smallvec::SmallVec::<[Level<'_>; 8]>::new();
        stack.push(Level {
            remaining: &roots,
            origin: self.origin,
            clip: self.raster.clip(),
        });
        let mut parts = 0;
        let mut tiles = 0;
        while !stack.is_empty() {
            let depth = stack.len() - 1;
            let level = stack.last_mut().unwrap();
            let Some((&index, remaining)) = level.remaining.split_first() else {
                stack.pop();
                continue;
            };
            level.remaining = remaining;
            let node = &self.scene.nodes[index];
            if !(self.content && index == self.root) && (!node.visible || node.opacity == 0) {
                continue;
            }
            let position = if depth == 0 {
                self.origin
            } else {
                (
                    level.origin.0 + i64::from(node.rectangle.left),
                    level.origin.1 + i64::from(node.rectangle.top),
                )
            };
            let Some(clip) = crate::scene::intersection(
                position.0,
                position.1,
                node.rectangle.width,
                node.rectangle.height,
                level.clip,
            ) else {
                continue;
            };
            // The endpoint's own transition is handled by Frame::content;
            // nested transitions cannot be cached just by image versions.
            if (!self.content || index != self.root)
                && self
                    .scene
                    .transitions
                    .iter()
                    .any(|t| t.destination == index)
            {
                return None;
            }
            if parts == MAX_PARTS {
                return None;
            }
            parts += 1;
            let image = match &node.image {
                Some(reference) => {
                    let image = self.images.get(&reference.id)?;
                    tiles += image.main.as_ref()?.tiles.len();
                    if tiles > MAX_PARTS {
                        return None;
                    }
                    Some(image)
                }
                None => None,
            };
            visit(
                Geometry {
                    depth,
                    children: !self.children[index].is_empty(),
                    rectangle: node.rectangle,
                    offset: (node.image_left, node.image_top),
                    blend: node.blend,
                    opacity: if index == self.root {
                        255
                    } else {
                        node.opacity
                    },
                    neutral: node.neutral_color,
                    origin: position,
                    clip: Some(clip),
                },
                image,
            )?;
            // Store sibling cursors, not a pending record per child. A wide
            // paragraph uses two inline levels regardless of its glyph count.
            // Traverse the full logical clip so edits outside a damage crop
            // are still represented when repairing the retained raster.
            if !self.children[index].is_empty() {
                stack.push(Level {
                    remaining: &self.children[index],
                    origin: position,
                    clip,
                });
            }
        }
        Some(())
    }
    fn capture(&self) -> Option<Signature> {
        let mut parts = Vec::new();
        self.visit(|geometry, image| {
            parts.push(Part {
                geometry,
                image: image.map(Version::capture).transpose().ok()?,
            });
            Some(())
        })?;
        Some(Signature {
            root: self.root,
            content: self.content,
            raster: self.raster,
            origin: self.origin,
            region: self.region,
            parts,
        })
    }
}
impl Signature {
    #[cfg(all(
        test,
        any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
    ))]
    #[allow(clippy::too_many_arguments)]
    pub fn capture(
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
        children: &Children,
        root: usize,
        raster: Raster,
        origin: (i64, i64),
        region: Rect,
        content: bool,
    ) -> Option<Self> {
        Query {
            scene,
            images,
            children,
            root,
            raster,
            origin,
            region,
            content,
        }
        .capture()
    }
    /// Compare and capture in one traversal. An unchanged prefix stays borrowed;
    /// only the first mismatch allocates the next stamp and copies that prefix.
    /// This avoids walking a growing paragraph twice on every new character.
    fn compare(&self, query: &Query<'_>) -> Option<Option<Change>> {
        let mut seen = 0;
        let mut unchanged_prefix = 0;
        let mut changed: Option<Vec<Part>> = None;
        query.visit(|geometry, image| {
            if changed.is_none()
                && self.parts.get(seen).is_some_and(|previous| {
                    previous.geometry == geometry
                        && match (&previous.image, image) {
                            (None, None) => true,
                            (Some(version), Some(image)) => version.matches(image),
                            _ => false,
                        }
                })
            {
                seen += 1;
                return Some(());
            }
            let parts = changed.get_or_insert_with(|| {
                unchanged_prefix = seen;
                let mut parts = Vec::with_capacity(self.parts.len().max(seen + 1));
                parts.extend_from_slice(&self.parts[..seen]);
                parts
            });
            parts.push(Part {
                geometry,
                image: image.map(Version::capture).transpose().ok()?,
            });
            seen += 1;
            Some(())
        })?;
        if changed.is_none() && seen != self.parts.len() {
            unchanged_prefix = seen;
            changed = Some(self.parts[..seen].to_vec());
        }
        Some(changed.map(|parts| Change {
            signature: Self {
                root: query.root,
                content: query.content,
                raster: query.raster,
                origin: query.origin,
                region: query.region,
                parts,
            },
            unchanged_prefix,
        }))
    }
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Entry>()
            + self.parts.capacity() * std::mem::size_of::<Part>()
            + self
                .parts
                .iter()
                .filter_map(|p| p.image.as_ref())
                .map(Version::bytes)
                .sum::<usize>()
    }
    fn same_space(&self, old: &Self) -> bool {
        self.content == old.content
            && self.raster == old.raster
            && self.origin == old.origin
            && self
                .parts
                .first()
                .zip(old.parts.first())
                .is_some_and(|(a, b)| a.same_geometry(b))
    }
    fn damage(&self, old: &Self, region: Rect) -> Option<Rect> {
        self.damage_after(old, region, 0)
    }
    fn damage_after(&self, old: &Self, region: Rect, unchanged_prefix: usize) -> Option<Rect> {
        let mut damage = None;
        for index in unchanged_prefix..self.parts.len().max(old.parts.len()) {
            let pair = self.parts.get(index).zip(old.parts.get(index));
            let Some((new, old)) = pair.filter(|(a, b)| a.same_geometry(b)) else {
                // Descendant moves, opacity, order and visibility changes
                // affect their clipped extents, not the entire parent raster.
                // Comparing the DFS sequence also covers changed ancestry.
                for part in [self.parts.get(index), old.parts.get(index)]
                    .into_iter()
                    .flatten()
                {
                    if let Some(area) = part
                        .geometry
                        .clip
                        .and_then(|area| self.raster.rect(area))
                        .and_then(|area| area.intersection(region))
                    {
                        damage = Some(crate::scene_damage::union(damage, area));
                    }
                }
                continue;
            };
            let (Some(image), Some(previous), Some(clip)) =
                (&new.image, &old.image, new.geometry.clip)
            else {
                continue;
            };
            let Some(area) = image.damage(previous) else {
                continue;
            };
            if let Some(area) = crate::scene::intersection(
                new.geometry.origin.0 + i64::from(new.geometry.offset.0) + i64::from(area.left),
                new.geometry.origin.1 + i64::from(new.geometry.offset.1) + i64::from(area.top),
                area.width,
                area.height,
                clip,
            )
            .and_then(|r| self.raster.rect(r))
            .and_then(|r| r.intersection(region))
            {
                damage = Some(crate::scene_damage::union(damage, area));
            }
        }
        damage
    }
}
pub(crate) struct Entry {
    pub owner: Option<Owner<()>>,
    pub signature: Signature,
    pub image: Image,
    pub _metadata: Permit,
    pub used: bool,
}
impl Entry {
    pub fn bytes(&self) -> usize {
        self.image.resident_bytes() + self.signature.bytes()
    }
    fn matches(&self, owner: Option<&Arc<()>>, signature: &Signature) -> bool {
        self.matches_key(owner, signature.root, signature.content)
    }
    fn matches_key(&self, owner: Option<&Arc<()>>, root: usize, content: bool) -> bool {
        self.signature.content == content
            && match (&self.owner, owner) {
                (Some(a), Some(b)) => a.as_ptr() == Arc::as_ptr(b),
                (None, None) => self.signature.root == root,
                _ => false,
            }
    }
}
#[derive(Default)]
pub(crate) struct Cache {
    entries: Vec<Entry>,
}
pub(crate) enum Lookup {
    Hit(Image, Rect),
    Miss(Option<Signature>),
}
impl Cache {
    pub fn lookup(&mut self, owner: Option<&Arc<()>>, query: &Query<'_>) -> Lookup {
        self.trim();
        let previous = self.entries.iter().rposition(|entry| {
            let old = &entry.signature;
            entry.matches_key(owner, query.root, query.content)
                && !old.parts.is_empty()
                && old.raster == query.raster
                && old.origin == query.origin
                && old.region.intersection(query.region) == Some(query.region)
        });
        let (signature, proof) = if let Some(index) = previous {
            match self.entries[index].signature.compare(query) {
                Some(None) => {
                    let (image, area) = self.touch(index, query.region);
                    return Lookup::Hit(image, area);
                }
                Some(Some(change)) => (change.signature, Some((index, change.unchanged_prefix))),
                None => return Lookup::Miss(None),
            }
        } else {
            let Some(signature) = query.capture() else {
                return Lookup::Miss(None);
            };
            (signature, None)
        };
        // A changed image can still leave this crop intact. Keep the existing
        // write-lineage comparison and dirty-raster repair on this path.
        if let Some((image, area)) = self.get_compared(owner, &signature, proof) {
            Lookup::Hit(image, area)
        } else {
            Lookup::Miss(Some(signature))
        }
    }
    pub fn begin_frame(&mut self) {
        for entry in &mut self.entries {
            entry.used = false;
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn trim(&mut self) {
        // A changed (or expired COW source) texture can still describe valid
        // cached pixels elsewhere. Compare bounded write lineage at lookup;
        // identities remain weak and LRU storage remains capped by LIMIT.
        self.entries
            .retain(|e| e.owner.as_ref().is_none_or(|o| o.strong_count() != 0));
    }
    #[cfg(all(
        test,
        any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
    ))]
    fn get(&mut self, owner: Option<&Arc<()>>, signature: &Signature) -> Option<(Image, Rect)> {
        self.trim();
        self.get_compared(owner, signature, None)
    }
    fn get_compared(
        &mut self,
        owner: Option<&Arc<()>>,
        signature: &Signature,
        proof: Option<(usize, usize)>,
    ) -> Option<(Image, Rect)> {
        // lookup already trimmed the list before comparing. Keep its original
        // indices while removing entries so proof never applies to another raster.
        let mut index = 0;
        let mut original_index = 0;
        while index < self.entries.len() {
            let unchanged_prefix = proof
                .filter(|&(entry, _)| entry == original_index)
                .map_or(0, |(_, prefix)| prefix);
            original_index += 1;
            let e = &self.entries[index];
            let old = &e.signature;
            if e.matches(owner, signature) {
                if !signature.same_space(old) {
                    self.entries.remove(index);
                    continue;
                }
                if old.region.intersection(signature.region) == Some(signature.region)
                    && signature
                        .damage_after(old, signature.region, unchanged_prefix)
                        .is_none()
                {
                    break;
                }
            }
            index += 1;
        }
        if index == self.entries.len() {
            return None;
        }
        Some(self.touch(index, signature.region))
    }
    fn touch(&mut self, index: usize, region: Rect) -> (Image, Rect) {
        let mut entry = self.entries.remove(index);
        entry.used = true;
        // A small repaint can sample a crop of a previously completed group.
        // Including exact damage dimensions in the key made every moving glyph
        // rebuild even completely unchanged full-screen foreground groups.
        let rectangle = Rect {
            left: region.left - entry.signature.region.left,
            top: region.top - entry.signature.region.top,
            ..region
        };
        let image = entry.image.shared();
        self.entries.push(entry);
        (image, rectangle)
    }
    /// Remove a private raster for repair before sampling it again. Repair all
    /// its stale pixels, including those outside this frame's requested crop;
    /// only then can its signature safely advance to the current versions.
    pub fn take_dirty(
        &mut self,
        owner: Option<&Arc<()>>,
        signature: &mut Signature,
        coverage: Rect,
        band: Rect,
        allowance: usize,
        metadata_budget: &Budget,
    ) -> Option<(Entry, Rect, Rect)> {
        let (index, damage) = self.entries.iter().enumerate().find_map(|(index, entry)| {
            let old = &entry.signature;
            (entry.matches(owner, signature)
                && signature.same_space(old)
                && old.region.intersection(signature.region) == Some(signature.region)
                && old.region.intersection(coverage) == Some(old.region)
                && entry.image.resident_bytes().saturating_add(signature.bytes()) <= allowance
                // Shared transition inputs and any in-flight aliases must not
                // trigger a full-image COW allocation merely to repair a crop.
                && entry.image.write_bytes(false) == 0)
                .then(|| {
                    signature
                        .damage(old, old.region)
                        // Preserve the scratch-row admission used by Frame.
                        // A retained cache cannot expand a bounded band into
                        // an arbitrarily large temporary subtree composition.
                        .filter(|damage| damage.width <= band.width && damage.height <= band.height)
                        .map(|damage| (index, damage))
                })
                .flatten()
        })?;
        // Cropping a shared source changes its tile layout without changing
        // most cached pixels. Keep repair available across those layouts,
        // while accounting for the replacement signature before publishing it.
        let metadata = if signature.bytes() != self.entries[index].signature.bytes() {
            Some(metadata_budget.reserve(signature.bytes()).ok()?)
        } else {
            None
        };
        let mut entry = self.entries.remove(index);
        if let Some(metadata) = metadata {
            entry._metadata = metadata;
        }
        signature.region = entry.signature.region;
        Some((entry, damage, signature.region))
    }
    pub fn available(&self) -> usize {
        LIMIT.saturating_sub(self.entries.iter().map(Entry::bytes).sum())
    }
    /// Admit pending rasters together with retained ones. Waiting until insert
    /// to evict lets a full cache prevent every replacement from being built;
    /// omitting pending ancestors can instead temporarily double the limit.
    pub fn make_room(&mut self, pending: usize) -> bool {
        if pending > LIMIT {
            return false;
        }
        self.trim();
        // Rebuilding a later group must not evict a raster already reused in
        // this composition. Otherwise a working set just above LIMIT thrashes
        // every frame, even though retaining a subset would eliminate work.
        let cold: usize = self
            .entries
            .iter()
            .filter(|e| !e.used)
            .map(Entry::bytes)
            .sum();
        if pending > self.available().saturating_add(cold) {
            return false;
        }
        while pending > self.available() {
            let index = self.entries.iter().position(|e| !e.used).unwrap();
            self.entries.remove(index);
        }
        true
    }
    pub fn insert(&mut self, entry: Entry) {
        self.trim();
        self.entries.retain(|old| match (&old.owner, &entry.owner) {
            (Some(a), Some(b)) => !Owner::ptr_eq(a, b),
            (None, None) => {
                old.signature.content != entry.signature.content
                    || old.signature.root != entry.signature.root
                    || old.signature.origin != entry.signature.origin
                    || old.signature.region != entry.signature.region
            }
            _ => true,
        });
        if entry.owner.as_ref().is_some_and(|o| o.strong_count() == 0) || entry.bytes() > LIMIT {
            return;
        }
        while entry.bytes() > self.available() || self.entries.len() >= 32 {
            let Some(index) = self.entries.iter().position(|e| !e.used) else {
                return;
            };
            self.entries.remove(index);
        }
        self.entries.push(entry);
    }
}
