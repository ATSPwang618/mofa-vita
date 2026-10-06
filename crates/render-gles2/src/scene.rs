//! Tree composition keeps a group's own bitmap and children together until its
//! blend/opacity is applied once. Ordinary trees use bounded reusable bands.
pub(crate) mod raster;
use crate::{
    Error, Gpu, Image, Result,
    drawing::{Draw, face, rgba},
};
use krkr_protocol::graphics::{Blend, BlendOptions, DrawFace, Fill, ImageId, Rect, Scene, Size};
use krkr_render::scene::Children;
use raster::Raster;
use std::collections::HashMap;

struct Frame<'a> {
    gpu: &'a Gpu,
    scene: &'a Scene,
    images: &'a HashMap<ImageId, Image>,
    children: Children,
    transitions: Vec<Option<usize>>,
    active: Vec<usize>,
    groups: Vec<Image>,
    depth: usize,
    raster: Raster,
    band: Rect,
    cache_entries: Vec<crate::scene_cache::Entry>,
    cache_allowance: usize,
    cache_pending: usize,
    cache_count: usize,
}
impl Frame<'_> {
    fn transition(&self, node: usize) -> Option<usize> {
        self.transitions.get(node).copied().flatten()
    }

    fn endpoint_view(&self, image: &Image, offset: (i32, i32), size: Size) -> Option<Image> {
        let plane = image.main.as_ref()?;
        let physical = self.raster.extent(size);
        if offset == (0, 0)
            && image.size == size
            && plane.size == physical
            && plane
                .tiles
                .iter()
                .all(|tile| tile.rectangle.intersection(physical.rect()).is_some())
        {
            let mut view = image.shared_main();
            view.canvas = false;
            return Some(view);
        }
        if offset.0 > 0
            || offset.1 > 0
            || i64::from(offset.0) + i64::from(image.size.width) < i64::from(size.width)
            || i64::from(offset.1) + i64::from(image.size.height) < i64::from(size.height)
            || u64::from(plane.size.width) * u64::from(size.width)
                != u64::from(physical.width) * u64::from(image.size.width)
            || u64::from(plane.size.height) * u64::from(size.height)
                != u64::from(physical.height) * u64::from(image.size.height)
        {
            return None;
        }
        let x = i64::from(offset.0) * i64::from(physical.width);
        let y = i64::from(offset.1) * i64::from(physical.height);
        if x % i64::from(size.width) != 0 || y % i64::from(size.height) != 0 {
            return None;
        }
        let (x, y) = (
            (x / i64::from(size.width)) as i32,
            (y / i64::from(size.height)) as i32,
        );
        let tiles = plane
            .tiles
            .iter()
            .filter_map(|tile| {
                let rectangle = Rect {
                    left: tile.rectangle.left.checked_add(x)?,
                    top: tile.rectangle.top.checked_add(y)?,
                    ..tile.rectangle
                };
                rectangle
                    .intersection(physical.rect())
                    .map(|_| crate::image::Tile {
                        backing: tile.backing.map(|r| Rect {
                            left: r.left + x,
                            top: r.top + y,
                            ..r
                        }),
                        rectangle,
                        texture: tile.texture.clone(),
                    })
            })
            .collect();
        // Read-only transition input: shifted tile rectangles crop the existing
        // storage without changing the exact nearest-neighbor sampling grid.
        Some(Image {
            size,
            device: image.device.clone(),
            canvas: false,
            text: false,
            province: None,
            main: Some(std::rc::Rc::new(crate::image::Plane {
                size: physical,
                budget: plane.budget.clone(),
                tiles,
            })),
        })
    }
    fn leaf_batch(
        &self,
        siblings: &[usize],
        target: &mut Image,
        origin: (i64, i64),
        parent: (i64, i64),
        clip: Rect,
        face: DrawFace,
    ) -> Result<usize> {
        // Opaque display groups use fixed alpha blending. Keep that rounding
        // independent of how many consecutive siblings fit a shader batch,
        // otherwise inserting a glyph changes pixels outside its damage.
        if self.gpu.display_scene_blend.get() && face == DrawFace::Opaque {
            return Ok(0);
        }
        if siblings.len() < 2 || self.depth + self.active.len() >= 127 {
            return Ok(0);
        }
        // Most nodes are groups, effects or isolated leaves. Reject them
        // before allocating batch metadata or cloning any image ownership.
        if siblings[..2].iter().any(|&index| {
            let node = &self.scene.nodes[index];
            !node.visible
                || node.opacity == 0
                || node.blend != Blend::Alpha
                || !self.children[index].is_empty()
                || self.transition(index).is_some()
        }) {
            return Ok(0);
        }
        let mut layers = smallvec::SmallVec::<[crate::scene_batch::Layer; 4]>::new();
        let mut coverage = None;
        let mut sampled = 0u128;
        for &index in siblings.iter().take(4) {
            let node = &self.scene.nodes[index];
            if !node.visible
                || node.opacity == 0
                || node.blend != Blend::Alpha
                || !self.children[index].is_empty()
                || self.transition(index).is_some()
            {
                break;
            }
            let Some(image) = node.image.as_ref().and_then(|r| self.images.get(&r.id)) else {
                break;
            };
            let left = parent.0 + i64::from(node.rectangle.left);
            let top = parent.1 + i64::from(node.rectangle.top);
            let source = (
                left + i64::from(node.image_left),
                top + i64::from(node.image_top),
            );
            let pixels = intersection(left, top, node.rectangle.width, node.rectangle.height, clip)
                .and_then(|clip| {
                    intersection(
                        source.0,
                        source.1,
                        image.size.width,
                        image.size.height,
                        clip,
                    )
                })
                .and_then(|area| self.raster.rect(area))
                .and_then(|area| area.intersection(self.band));
            let Some(pixels) = pixels else { break };
            let joined = crate::scene_damage::union(coverage, pixels);
            let next_sampled = sampled + u128::from(pixels.width) * u128::from(pixels.height);
            let fused_samples =
                u128::from(joined.width) * u128::from(joined.height) * (layers.len() as u128 + 1);
            // Every sampler runs over the union. Limit additional texture
            // samples to 25% for large layers. Small glyph runs and dense menu
            // button strips can afford more samples to avoid separate work
            // surface resolves. Still reject sparse runs with large gaps.
            let joined_area = u128::from(joined.width) * u128::from(joined.height);
            let small_run =
                (layers.len() <= 2 && joined_area <= 4096 && fused_samples <= next_sampled * 3)
                    || (self.gpu.device.streamed_uploads()
                        && joined_area <= 16 * 1024
                        && joined_area * 4 <= next_sampled * 5);
            if fused_samples * 4 > next_sampled * 5 && !small_run {
                break;
            }
            sampled = next_sampled;
            coverage = Some(joined);
            layers.push(crate::scene_batch::Layer {
                image: image.shared(),
                origin: source,
                opacity: node.opacity,
                coverage: local(pixels, origin),
            });
        }
        if layers.len() < 2 {
            return Ok(0);
        }
        while layers.len() >= 2 {
            let area = layers
                .iter()
                .fold(None, |area, layer| {
                    Some(crate::scene_damage::union(area, layer.coverage))
                })
                .unwrap();
            if self
                .gpu
                .scene_alpha_batch(target, &layers, area, self.raster, origin, face)?
            {
                return Ok(layers.len());
            }
            // Offset tile grids can split four layers into too many regions.
            // A shorter consecutive prefix may still reduce drawing work.
            layers.pop();
        }
        Ok(0)
    }
    /// A later opaque child can replace its parent's bitmap and all earlier
    /// siblings in this band. Prove stored-pixel coverage, including compact
    /// source offsets; geometry alone would incorrectly hide bitmap gaps.
    fn covering_child(
        &self,
        index: usize,
        parent: (i64, i64),
        clip: Rect,
        pixels: Rect,
    ) -> Result<Option<usize>> {
        if !self.scene.transitions.is_empty() {
            return Ok(None);
        }
        for (position, &child) in self.children[index].iter().enumerate().rev() {
            let node = &self.scene.nodes[child];
            if !node.visible || node.opacity != 255 || node.blend != Blend::Opaque {
                continue;
            }
            let left = parent.0 + i64::from(node.rectangle.left);
            let top = parent.1 + i64::from(node.rectangle.top);
            if intersection(left, top, node.rectangle.width, node.rectangle.height, clip)
                .and_then(|area| self.raster.rect(area))
                .and_then(|area| area.intersection(pixels))
                != Some(pixels)
            {
                continue;
            }
            let covers = match &node.image {
                None => true, // An opaque solid fills its entire clipped node.
                Some(reference) => self
                    .images
                    .get(&reference.id)
                    .map(|image| {
                        self.gpu.scene_bitmap_overwrites(
                            image,
                            pixels,
                            self.raster,
                            (0, 0),
                            (
                                left + i64::from(node.image_left),
                                top + i64::from(node.image_top),
                            ),
                        )
                    })
                    .transpose()?
                    .unwrap_or(false),
            };
            if covers {
                return Ok(Some(position));
            }
        }
        Ok(None)
    }
    fn can_cache(&self, bytes: usize) -> bool {
        self.cache_count < 32
            && bytes <= self.cache_allowance
            && bytes <= self.gpu.resident.available()
            && self
                .gpu
                .scene_cache
                .borrow_mut()
                .make_room(self.cache_pending.saturating_add(bytes))
    }
    fn release_completed_cache_for(&mut self, size: Size, budget: &krkr_protocol::budget::Budget) {
        if size.rgba_bytes().unwrap_or(usize::MAX) <= self.gpu.capacity_after_collect(budget) {
            return;
        }
        // Newly completed siblings are not in Gpu::scene_cache until the end
        // of this frame, so the allocator's usual eviction cannot reach them.
        // Their draws have already been submitted. Active endpoint aliases
        // retain ownership; retired storage still obeys GPU completion fences.
        for entry in self.cache_entries.drain(..) {
            self.cache_pending -= entry.bytes();
            self.cache_count -= 1;
        }
        self.cache_allowance = 0;
    }
    fn group(&mut self, size: Size) -> Result<Image> {
        // The node's clipped pixels already fit the current band. A small
        // button/text group must not inherit the entire screen's allocation.
        let found = self
            .groups
            .iter()
            .enumerate()
            .filter(|(_, image)| image.size.width >= size.width && image.size.height >= size.height)
            .min_by_key(|(_, image)| image.size.rgba_bytes())
            .map(|(i, _)| i);
        if let Some(index) = found {
            return Ok(self.groups.swap_remove(index));
        }
        // None of the idle surfaces can serve this group. Keeping every
        // smaller sibling alive makes scratch grow with sibling count,
        // although admission only reserves the nesting depth. Retire them
        // before allocating; active ancestors stay with the recursive caller.
        self.groups.clear();
        self.release_completed_cache_for(size, &self.gpu.scratch);
        Ok(Image {
            size,
            device: self.gpu.device.clone(),
            canvas: false,
            text: false,
            main: Some(
                self.gpu
                    .overwrite_plane(size, &self.gpu.scratch)
                    .map_err(|error| {
                        Error::Backend(format!(
                            "scene group {size:?}, depth={}: {error}",
                            self.depth
                        ))
                    })?,
            ),
            province: None,
        })
    }
    fn node(
        &mut self,
        index: usize,
        target: &mut Image,
        origin: (i64, i64),
        parent: (i64, i64),
        clip: Rect,
        face: DrawFace,
    ) -> Result<()> {
        if self.depth + self.active.len() >= 128 {
            return Err(Error::Message(
                "scene composition nesting exceeds renderer limit",
            ));
        }
        self.depth += 1;
        let band = self.band;
        let result = self.node_inner(index, target, origin, parent, clip, face);
        self.band = band;
        self.depth -= 1;
        result.map_err(|error| {
            Error::Backend(format!(
                "scene node {index} {:?}: {error}",
                self.scene.nodes[index].blend
            ))
        })
    }
    /// Find the final opaque bitmap of a subtree when it covers every pixel
    /// that earlier bitmaps and siblings could have contributed to this band.
    /// Such a subtree is equivalent to a single opaque draw at its destination.
    fn opaque_covering_bitmap(
        &self,
        index: usize,
        parent: (i64, i64),
        clip: Rect,
        pixels: Rect,
        output_origin: (i64, i64),
    ) -> Result<Option<(usize, (i64, i64))>> {
        let node = &self.scene.nodes[index];
        if !node.visible
            || node.opacity != 255
            || !matches!(node.blend, Blend::Opaque | Blend::Alpha)
            || self.transition(index).is_some()
        {
            return Ok(None);
        }
        let left = parent.0 + i64::from(node.rectangle.left);
        let top = parent.1 + i64::from(node.rectangle.top);
        let Some(clip) = intersection(left, top, node.rectangle.width, node.rectangle.height, clip)
        else {
            return Ok(None);
        };
        if self
            .raster
            .rect(clip)
            .and_then(|area| area.intersection(self.band))
            .and_then(|area| area.intersection(pixels))
            != Some(pixels)
        {
            return Ok(None);
        }
        for &child_index in self.children[index].iter().rev() {
            let child = &self.scene.nodes[child_index];
            if !child.visible || child.opacity == 0 {
                continue;
            }
            let child_left = left + i64::from(child.rectangle.left);
            let child_top = top + i64::from(child.rectangle.top);
            if intersection(
                child_left,
                child_top,
                child.rectangle.width,
                child.rectangle.height,
                clip,
            )
            .and_then(|area| self.raster.rect(area))
            .and_then(|area| area.intersection(pixels))
            .is_none()
            {
                continue;
            }
            // An empty alpha leaf cannot alter the pixels left by its sibling.
            if child.blend == Blend::Alpha
                && child.image.is_none()
                && self.children[child_index].is_empty()
                && self.transition(child_index).is_none()
            {
                continue;
            }
            return self.opaque_covering_bitmap(
                child_index,
                (left, top),
                clip,
                pixels,
                output_origin,
            );
        }
        if node.blend != Blend::Opaque {
            return Ok(None);
        }
        let Some(image) = node.image.as_ref().and_then(|r| self.images.get(&r.id)) else {
            return Ok(None);
        };
        let source = (
            left + i64::from(node.image_left),
            top + i64::from(node.image_top),
        );
        if intersection(
            source.0,
            source.1,
            image.size.width,
            image.size.height,
            clip,
        )
        .and_then(|area| self.raster.rect(area))
        .and_then(|area| area.intersection(pixels))
            != Some(pixels)
            || !self.gpu.scene_bitmap_overwrites(
                image,
                local(pixels, output_origin),
                self.raster,
                output_origin,
                source,
            )?
        {
            return Ok(None);
        }
        Ok(Some((index, source)))
    }
    fn node_inner(
        &mut self,
        index: usize,
        target: &mut Image,
        origin: (i64, i64),
        parent: (i64, i64),
        clip: Rect,
        face: DrawFace,
    ) -> Result<()> {
        let node = self.scene.nodes[index].clone();
        if !node.visible || node.opacity == 0 {
            return Ok(());
        }
        let left = parent.0 + i64::from(node.rectangle.left);
        let top = parent.1 + i64::from(node.rectangle.top);
        let Some(clip) = intersection(left, top, node.rectangle.width, node.rectangle.height, clip)
        else {
            return Ok(());
        };
        let Some(pixels) = self
            .raster
            .rect(clip)
            .and_then(|r| r.intersection(self.band))
        else {
            return Ok(());
        };
        let transition = self
            .transition(index)
            .filter(|_| !self.active.contains(&index));
        let with_children = transition.is_some_and(|i| self.scene.transitions[i].with_children);
        if let Some(transition) = transition
            && self.active.is_empty()
            && self.scene.transitions[transition].custom.is_none()
            && node.blend == Blend::Opaque
            && node.opacity == 255
            && (with_children || self.children[index].is_empty())
            && (with_children || (node.image_left == 0 && node.image_top == 0))
            && (left, top) == (0, 0)
            && origin == (0, 0)
            && pixels == target.size.rect()
            && self.raster
                == Raster::new(
                    self.scene.transitions[transition].frame.size,
                    target.size,
                    (0, 0),
                )?
        {
            // A full opaque node replaces every target pixel, including inside
            // an ancestor group. Compose its
            // endpoints and run the transition into the retained display,
            // instead of allocating/copying another panel-sized intermediate.
            return self.transition_direct(transition, target, face);
        }
        if transition.is_none()
            && node.blend == Blend::Alpha
            && node.opacity == 255
            && !self.children[index].is_empty()
            && matches!(face, DrawFace::Opaque | DrawFace::Alpha)
            && let Some((leaf, source)) =
                self.opaque_covering_bitmap(index, parent, clip, pixels, origin)?
        {
            let image = self.scene.nodes[leaf]
                .image
                .as_ref()
                .and_then(|reference| self.images.get(&reference.id))
                .ok_or(Error::Message("scene image is no longer available"))?;
            return self.gpu.scene_bitmap(
                target,
                image,
                local(pixels, origin),
                self.raster,
                origin,
                source,
                BlendOptions::for_composition(Blend::Opaque, face, 255),
                false,
            );
        }
        let transitioned = transition.map(|i| self.transition_bitmap(i)).transpose()?;
        // When the bitmap covers the node, a band without visible children is
        // just a leaf draw. Preserve the group otherwise: its neutral pixels
        // outside the bitmap can matter for opaque and arithmetic blends.
        let covered = transitioned
            .as_ref()
            .or_else(|| {
                node.image
                    .as_ref()
                    .and_then(|reference| self.images.get(&reference.id))
            })
            .is_some_and(|image| {
                node.image_left <= 0
                    && node.image_top <= 0
                    && i64::from(node.image_left) + i64::from(image.size.width)
                        >= i64::from(node.rectangle.width)
                    && i64::from(node.image_top) + i64::from(image.size.height)
                        >= i64::from(node.rectangle.height)
            });
        let grouped = !with_children
            && !self.children[index].is_empty()
            && (node.opacity != 255 || node.blend != Blend::Opaque)
            && (!covered
                || self.children[index].iter().any(|&child| {
                    let child = &self.scene.nodes[child];
                    child.visible
                        && child.opacity != 0
                        && intersection(
                            left + i64::from(child.rectangle.left),
                            top + i64::from(child.rectangle.top),
                            child.rectangle.width,
                            child.rectangle.height,
                            clip,
                        )
                        .and_then(|area| self.raster.rect(area))
                        .and_then(|area| area.intersection(self.band))
                        .is_some()
                }));
        let mut cached = None;
        let mut group = None;
        let requested = pixels;
        let mut storage = pixels;
        let mut pixels = pixels;
        let signature = if grouped
            && transitioned.is_none()
            // Transition endpoints already retain their nested groups.
            && self.active.is_empty()
            && (node.cache.is_some() || self.gpu.device.streamed_uploads())
        {
            let query = crate::scene_cache::Query {
                scene: self.scene,
                images: self.images,
                children: &self.children,
                root: index,
                raster: self.raster,
                origin: (left, top),
                region: pixels,
                content: false,
            };
            let lookup = self
                .gpu
                .scene_cache
                .borrow_mut()
                .lookup(node.cache.as_ref(), &query);
            match lookup {
                crate::scene_cache::Lookup::Hit(image, rectangle) => {
                    let destination = local(pixels, origin);
                    return self.gpu.operate(
                        target,
                        &image,
                        rectangle,
                        destination.left,
                        destination.top,
                        destination,
                        BlendOptions::for_composition(node.blend, face, node.opacity),
                    );
                }
                crate::scene_cache::Lookup::Miss(signature) => signature,
            }
        } else {
            None
        };
        if let Some(mut signature) = signature {
            // A glyph or a small bitmap edit need not discard the surrounding
            // completed group. Keep its original coordinate grid and rebuild
            // just its dirty rectangle with the same blend order/rounding.
            let repair = if self.cache_count < 32 {
                self.gpu.scene_cache.borrow_mut().take_dirty(
                    node.cache.as_ref(),
                    &mut signature,
                    self.raster.rect(clip).unwrap(),
                    self.band,
                    crate::scene_cache::LIMIT.saturating_sub(self.cache_pending),
                    &self.gpu.resident,
                )
            } else {
                None
            };
            if let Some((entry, damage, region)) = repair {
                self.cache_pending += entry.image.resident_bytes() + signature.bytes();
                self.cache_count += 1;
                self.gpu
                    .scene_cache
                    .borrow_mut()
                    .make_room(self.cache_pending);
                pixels = damage;
                storage = region;
                self.band = damage;
                cached = Some((entry.owner, signature, entry._metadata));
                group = Some(entry.image);
            } else {
                let size = Size {
                    width: pixels.width,
                    height: pixels.height,
                };
                let bytes = size.rgba_bytes().unwrap().saturating_add(signature.bytes());
                if self.can_cache(bytes) {
                    let metadata = self.gpu.resident.reserve(signature.bytes())?;
                    let image = Image {
                        size,
                        device: self.gpu.device.clone(),
                        canvas: false,
                        text: false,
                        main: Some(self.gpu.overwrite_plane(size, &self.gpu.resident)?),
                        province: None,
                    };
                    self.cache_allowance -= bytes;
                    self.cache_pending += bytes;
                    self.cache_count += 1;
                    cached = Some((
                        node.cache.as_ref().map(std::sync::Arc::downgrade),
                        signature,
                        metadata,
                    ));
                    group = Some(image);
                }
            }
        }
        if group.is_none() && grouped {
            group = Some(self.group(Size {
                width: pixels.width,
                height: pixels.height,
            })?);
        }
        let output_origin = if grouped {
            (i64::from(storage.left), i64::from(storage.top))
        } else {
            origin
        };
        let covering_child = if with_children {
            None
        } else {
            self.covering_child(index, (left, top), clip, pixels)?
        };
        let bitmap = if covering_child.is_some() {
            None
        } else if let Some(image) = transitioned {
            Some(image)
        } else {
            node.image
                .as_ref()
                .map(|reference| {
                    self.images
                        .get(&reference.id)
                        .map(Image::shared_main)
                        .ok_or(Error::Message("scene image is no longer available"))
                })
                .transpose()?
        };
        let source_origin = (
            left + i64::from(if with_children { 0 } else { node.image_left }),
            top + i64::from(if with_children { 0 } else { node.image_top }),
        );
        let bitmap_area = bitmap.as_ref().and_then(|image| {
            intersection(
                source_origin.0,
                source_origin.1,
                image.size.width,
                image.size.height,
                clip,
            )
            .and_then(|r| self.raster.rect(r))
            .and_then(|r| r.intersection(pixels))
        });
        let initialized = if !grouped {
            false
        } else if covering_child.is_some() {
            true
        } else if let Some(image) = bitmap.as_ref() {
            bitmap_area == Some(pixels)
                && self.gpu.scene_bitmap_overwrites(
                    image,
                    local(pixels, output_origin),
                    self.raster,
                    output_origin,
                    source_origin,
                )?
        } else {
            node.blend == Blend::Opaque
        };
        if let Some(group) = &mut group
            && !initialized
        {
            self.gpu.fill(
                group,
                &[Fill {
                    rectangle: local(pixels, output_origin),
                    color: node.blend.neutral(),
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )?;
        }
        // Cached/intermediate groups can later become script capture inputs.
        // Only their final display draw uses normalized hardware blending.
        let group_blending = grouped.then(|| self.gpu.suspend_display_blending());
        let output = group.as_mut().unwrap_or(target);
        if let Some(image) = bitmap {
            if let Some(area) = bitmap_area {
                self.gpu.scene_bitmap(
                    output,
                    &image,
                    local(area, output_origin),
                    self.raster,
                    output_origin,
                    source_origin,
                    BlendOptions::for_composition(node.blend, face, node.opacity),
                    grouped,
                )?;
            }
        } else if node.blend == Blend::Opaque && covering_child.is_none() {
            self.gpu.solid(
                output,
                local(pixels, output_origin),
                node.neutral_color | 0xff000000,
                BlendOptions::for_composition(node.blend, face, node.opacity),
                grouped,
            )?;
        }
        if !with_children {
            let mut child = covering_child.unwrap_or(0);
            while child < self.children[index].len() {
                let batched = self.leaf_batch(
                    &self.children[index][child..],
                    output,
                    output_origin,
                    (left, top),
                    clip,
                    node.blend.face(),
                )?;
                if batched != 0 {
                    child += batched;
                    continue;
                }
                self.node(
                    self.children[index][child],
                    output,
                    output_origin,
                    (left, top),
                    clip,
                    node.blend.face(),
                )?;
                child += 1;
            }
        }
        drop(group_blending);
        if let Some(group) = group {
            let part = local(requested, output_origin);
            let destination = local(requested, origin);
            self.gpu.operate(
                target,
                &group,
                part,
                destination.left,
                destination.top,
                destination,
                BlendOptions::for_composition(node.blend, face, node.opacity),
            )?;
            // All reads precede reuse in the GL command stream. Recycling this
            // private group avoids a new GPU allocation for every sibling.
            if let Some((owner, signature, metadata)) = cached {
                self.cache_entries.push(crate::scene_cache::Entry {
                    owner,
                    signature,
                    image: group,
                    _metadata: metadata,
                    used: true,
                });
            } else {
                self.groups.push(group);
            }
        }
        Ok(())
    }
    fn content(&mut self, index: usize, with_children: bool, size: Size) -> Result<Image> {
        // Transition sources use a local grid at the display's density. Their
        // virtual dimensions remain logical for spatial sampling and tables.
        let raster = self.raster;
        let band = self.band;
        let physical = raster.extent(size);
        self.raster = Raster::new(size, physical, (0, 0))?;
        self.band = physical.rect();
        let result = self.content_inner(index, with_children, size);
        self.raster = raster;
        self.band = band;
        result
    }
    fn content_inner(&mut self, index: usize, with_children: bool, size: Size) -> Result<Image> {
        let node = self.scene.nodes[index].clone();
        let nested = with_children
            .then_some(self.transition(index))
            .flatten()
            .filter(|_| !self.active.contains(&index));
        let nested_children = nested.is_some_and(|i| self.scene.transitions[i].with_children);
        let bitmap = if let Some(nested) = nested {
            Some(self.transition_bitmap(nested)?)
        } else {
            node.image
                .as_ref()
                .map(|reference| {
                    self.images
                        .get(&reference.id)
                        .map(Image::shared)
                        .ok_or(Error::Message("transition image is no longer available"))
                })
                .transpose()?
        };
        let offset = if with_children && !nested_children {
            (node.image_left, node.image_top)
        } else {
            (0, 0)
        };
        let children = with_children && !nested_children;
        if children && node.blend == Blend::Opaque {
            // The last visible full opaque leaf replaces the endpoint's own
            // bitmap and every earlier child. Its crop can be sampled directly.
            for &child in self.children[index].iter().rev() {
                let child_node = &self.scene.nodes[child];
                if !child_node.visible
                    || child_node.opacity == 0
                    || child_node.rectangle.intersection(size.rect()).is_none()
                {
                    continue;
                }
                if child_node.rectangle == size.rect()
                    && child_node.blend == Blend::Opaque
                    && child_node.opacity == 255
                    && self.children[child].is_empty()
                    && self.transition(child).is_none()
                    && let Some(image) = child_node
                        .image
                        .as_ref()
                        .and_then(|r| self.images.get(&r.id))
                    && let Some(view) = self.endpoint_view(
                        image,
                        (child_node.image_left, child_node.image_top),
                        size,
                    )
                {
                    return Ok(view);
                }
                break;
            }
        }
        if (!children || self.children[index].is_empty())
            && offset == (0, 0)
            && bitmap.as_ref().is_some_and(|image| image.size == size)
        {
            return Ok(bitmap.unwrap());
        }
        if (!children
            || self.children[index].iter().all(|&child| {
                let child = &self.scene.nodes[child];
                !child.visible
                    || child.opacity == 0
                    || child.rectangle.intersection(size.rect()).is_none()
            }))
            && let Some(view) = bitmap
                .as_ref()
                .and_then(|image| self.endpoint_view(image, offset, size))
        {
            return Ok(view);
        }
        let physical = self.raster.extent(size);
        // Animation progress affects the transition shader, not unchanged
        // input subtrees. Keep completed inputs across frames under the same
        // cache/memory allowance as ordinary grouped composition.
        let mut cached = None;
        let signature = if children && nested.is_none() && self.gpu.device.streamed_uploads() {
            let query = crate::scene_cache::Query {
                scene: self.scene,
                images: self.images,
                children: &self.children,
                root: index,
                raster: self.raster,
                origin: (0, 0),
                region: physical.rect(),
                content: true,
            };
            let lookup = self.gpu.scene_cache.borrow_mut().lookup(None, &query);
            match lookup {
                crate::scene_cache::Lookup::Hit(image, _) => return Ok(image),
                crate::scene_cache::Lookup::Miss(signature) => signature,
            }
        } else {
            None
        };
        if let Some(signature) = signature {
            let bytes = physical
                .rgba_bytes()
                .unwrap()
                .saturating_add(signature.bytes());
            if self.can_cache(bytes) {
                let metadata = self.gpu.resident.reserve(signature.bytes())?;
                self.cache_allowance -= bytes;
                self.cache_pending += bytes;
                self.cache_count += 1;
                cached = Some((signature, metadata));
            }
        }
        let budget = if cached.is_some() {
            &self.gpu.resident
        } else {
            &self.gpu.scratch
        };
        let recycled = if cached.is_none() {
            self.groups
                .iter()
                .position(|image| image.size == physical)
                .map(|at| self.groups.swap_remove(at))
        } else {
            None
        };
        let mut output = if let Some(image) = recycled {
            image
        } else {
            // Idle child groups must not block the next endpoint's durable
            // raster. Active ancestors live with their recursive callers.
            self.groups.clear();
            self.release_completed_cache_for(physical, budget);
            Image {
                size: physical,
                device: self.gpu.device.clone(),
                canvas: false,
                text: false,
                main: Some(
                    self.gpu
                        .overwrite_plane(physical, budget)
                        .map_err(|error| {
                            Error::Backend(format!(
                                "transition endpoint node={index}, size={physical:?}: {error}"
                            ))
                        })?,
                ),
                province: None,
            }
        };
        let initialized = bitmap
            .as_ref()
            .map(|image| {
                self.gpu.scene_bitmap_overwrites(
                    image,
                    physical.rect(),
                    self.raster,
                    (0, 0),
                    (i64::from(offset.0), i64::from(offset.1)),
                )
            })
            .transpose()?
            .unwrap_or(false);
        if !initialized {
            self.gpu.fill(
                &mut output,
                &[Fill {
                    rectangle: physical.rect(),
                    color: if bitmap.is_none() && node.blend == Blend::Opaque {
                        node.neutral_color | 0xff000000
                    } else {
                        node.blend.neutral()
                    },
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )?;
        }
        if let Some(image) = bitmap {
            self.gpu.scene_bitmap(
                &mut output,
                &image,
                physical.rect(),
                self.raster,
                (0, 0),
                (i64::from(offset.0), i64::from(offset.1)),
                BlendOptions::for_composition(Blend::Opaque, DrawFace::Alpha, 255),
                true,
            )?;
        }
        if children {
            // Endpoint rasters are full-sized, but their nested groups need
            // only a band. Holding both endpoints must not disable the same
            // bounded composition policy used for ordinary frames.
            let mut depths = vec![None; self.scene.nodes.len()];
            depths[index] = Some(0usize);
            let mut maximum = 0usize;
            let mut backdrop = false;
            for (at, child) in self.scene.nodes.iter().enumerate().skip(index + 1) {
                if child.visible
                    && child.opacity != 0
                    && let Some(depth) = child.parent.and_then(|p| depths[p])
                {
                    let grouped = !self.children[at].is_empty()
                        && (child.opacity != 255 || child.blend != Blend::Opaque);
                    depths[at] = Some(depth + usize::from(grouped));
                    maximum = maximum.max(depth + usize::from(grouped));
                    backdrop |= child.blend != Blend::Opaque || child.opacity != 255;
                }
            }
            let row = (physical.width as usize * 4).saturating_mul(
                maximum + usize::from(backdrop && !self.gpu.device.streamed_uploads()),
            );
            if row.saturating_mul(physical.height as usize) > self.gpu.scratch_capacity() {
                self.groups.clear();
            }
            let height = if let Some(rows) = self.gpu.scratch_capacity().checked_div(row) {
                if rows == 0 {
                    return Err(Error::Message(
                        "transition endpoint cannot fit one group band row",
                    ));
                }
                physical.height.min(rows as u32)
            } else {
                physical.height
            };
            let previous_band = self.band;
            for top in (0..physical.height).step_by(height as usize) {
                self.band = Rect {
                    top: top as i32,
                    height: height.min(physical.height - top),
                    ..physical.rect()
                };
                for child in 0..self.children[index].len() {
                    self.node(
                        self.children[index][child],
                        &mut output,
                        (0, 0),
                        (0, 0),
                        size.rect(),
                        node.blend.face(),
                    )?;
                }
            }
            self.band = previous_band;
        }
        let output = self.gpu.logical_image(output, size)?;
        if let Some((signature, metadata)) = cached {
            self.cache_entries.push(crate::scene_cache::Entry {
                owner: None,
                signature,
                image: output.shared(),
                _metadata: metadata,
                used: true,
            });
        }
        Ok(output)
    }
    fn transition_direct(
        &mut self,
        index: usize,
        output: &mut Image,
        face: DrawFace,
    ) -> Result<()> {
        let transition = self.scene.transitions[index].clone();
        self.active.push(transition.destination);
        let result = (|| {
            let frame = transition.frame;
            if (frame.phase == 0 && transition.custom.is_none())
                || frame.phase >= frame.effect.phases(frame.size)
            {
                let image = self.content(
                    if frame.phase == 0 {
                        transition.destination
                    } else {
                        transition.source
                    },
                    transition.with_children,
                    frame.size,
                )?;
                return self.gpu.scene_bitmap(
                    output,
                    &image,
                    output.size.rect(),
                    self.raster,
                    (0, 0),
                    (0, 0),
                    BlendOptions::for_composition(Blend::Opaque, face, 255),
                    false,
                );
            }
            let first =
                self.content(transition.destination, transition.with_children, frame.size)?;
            let second = self.content(transition.source, transition.with_children, frame.size)?;
            let rule = transition
                .rule
                .as_ref()
                .map(|reference| {
                    self.images
                        .get(&reference.id)
                        .ok_or(Error::Message("transition rule is no longer available"))
                })
                .transpose()?;
            self.gpu.transition_raster(
                output,
                &first,
                &second,
                rule,
                frame,
                transition.custom.as_ref(),
            )?;
            if face != DrawFace::Opaque {
                // Opaque tree composition replaces alpha with 255 even when
                // the transition itself produced straight/premultiplied alpha.
                self.gpu.fill(
                    output,
                    &[Fill {
                        rectangle: output.size.rect(),
                        color: 255,
                        face: DrawFace::Mask,
                        hold_alpha: false,
                    }],
                )?;
            }
            Ok(())
        })();
        self.active.pop();
        result
    }
    fn transition_bitmap(&mut self, index: usize) -> Result<Image> {
        if self.active.len() + self.depth >= 128 {
            return Err(Error::Message(
                "transition composition nesting exceeds renderer limit",
            ));
        }
        let transition = self.scene.transitions[index].clone();
        self.active.push(transition.destination);
        let result = (|| {
            let frame = transition.frame;
            if (frame.phase == 0 && transition.custom.is_none())
                || frame.phase >= frame.effect.phases(frame.size)
            {
                return self.content(
                    if frame.phase == 0 {
                        transition.destination
                    } else {
                        transition.source
                    },
                    transition.with_children,
                    frame.size,
                );
            }
            let first =
                self.content(transition.destination, transition.with_children, frame.size)?;
            let second = self.content(transition.source, transition.with_children, frame.size)?;
            let rule = transition
                .rule
                .as_ref()
                .map(|reference| {
                    self.images
                        .get(&reference.id)
                        .ok_or(Error::Message("transition rule is no longer available"))
                })
                .transpose()?;
            let physical = self.raster.extent(frame.size);
            self.groups.clear();
            self.release_completed_cache_for(physical, &self.gpu.scratch);
            let mut output = self.gpu.create_surface_image(physical).map_err(|error| {
                Error::Backend(format!(
                    "transition output node={}, size={:?}: {error}",
                    transition.destination, frame.size
                ))
            })?;
            self.gpu.transition_raster(
                &mut output,
                &first,
                &second,
                rule,
                frame,
                transition.custom.as_ref(),
            )?;
            self.gpu.logical_image(output, frame.size)
        })();
        self.active.pop();
        result
    }
}
impl Gpu {
    /// Compose one isolated transition endpoint before admitting the other.
    /// The first node is the endpoint root; its own visibility and opacity
    /// are ignored, as in ordinary transition composition.
    pub fn scene_endpoint(
        &self,
        logical: Size,
        physical: Size,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
    ) -> Result<Image> {
        if scene.nodes.is_empty() || !scene.transitions.is_empty() {
            return Err(Error::Message("expected an isolated transition endpoint"));
        }
        let _draw_state = self.device.draw_state.scope();
        for image in images.values() {
            self.check_image(image)?;
        }
        let _metadata = self
            .staging
            .reserve(scene.nodes.len().saturating_mul(128))?;
        let mut frame = Frame {
            gpu: self,
            scene,
            images,
            children: Children::new(&scene.nodes, 127)?,
            transitions: Vec::new(),
            active: Vec::new(),
            groups: Vec::new(),
            depth: 0,
            raster: Raster::new(logical, physical, (0, 0))?,
            band: physical.rect(),
            cache_entries: Vec::new(),
            cache_allowance: 0,
            cache_pending: 0,
            cache_count: 0,
        };
        let root = scene.nodes[0].rectangle;
        let image = frame.content(
            0,
            true,
            Size {
                width: root.width,
                height: root.height,
            },
        )?;
        self.resolve()?;
        Ok(image)
    }

    pub fn create_surface_image(&self, size: Size) -> Result<Image> {
        Ok(Image {
            size,
            device: self.device.clone(),
            canvas: false,
            text: false,
            main: Some(self.plane(size, &self.scratch)?),
            province: None,
        })
    }
    pub fn compose(
        &self,
        target: &mut Image,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
    ) -> Result<()> {
        self.compose_region(target, scene, images, (0, 0))
    }
    pub fn compose_region(
        &self,
        target: &mut Image,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
        origin: (i32, i32),
    ) -> Result<()> {
        self.check_image(target)?;
        let physical = if target.canvas {
            self.canvas_storage(target.size, Some(target))
        } else {
            target.plane(false)?.size
        };
        let mut next = self.compose_in(
            target.size,
            physical,
            scene,
            images,
            origin,
            &target.plane(false)?.budget,
        )?;
        next.size = target.size;
        next.canvas = target.canvas;
        next.province = target.province.clone();
        *target = next;
        Ok(())
    }
    /// Compose a durable script image without allocating an empty precursor.
    pub fn scene_image(
        &self,
        size: Size,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
        origin: (i32, i32),
    ) -> Result<Image> {
        let stored = self.canvas_storage(size, None);
        let output = self.compose_in(size, stored, scene, images, origin, &self.resident)?;
        self.logical_image(output, size)
    }
    /// A full piled copy can replace a script canvas's main plane directly.
    /// Compose into resident storage so the capture does not need a second
    /// full-canvas copy from the temporary scratch pool. Keep its hit plane.
    pub fn scene_canvas_replacement(
        &self,
        target: &Image,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
    ) -> Result<Option<Image>> {
        self.check_image(target)?;
        if !target.canvas || !target.has_main() {
            return Ok(None);
        }
        // Leave the scratch path available when resident storage is tight.
        let bytes = self.canvas_allocation_bytes(target.size, None);
        if bytes > self.resident.available() {
            return Ok(None);
        }
        let mut next = match self.scene_image(target.size, scene, images, (0, 0)) {
            Ok(next) => next,
            Err(Error::Budget(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
        next.province = target.province.clone();
        Ok(Some(next))
    }
    /// Compose a script capture using the same density as writable canvases.
    /// Script coordinates and crop origin are retained; a piled copy must not
    /// expand converted assets into a full logical-resolution scratch image.
    pub fn scene_surface(
        &self,
        size: Size,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
        origin: (i32, i32),
    ) -> Result<Image> {
        let stored = self.canvas_storage(size, None);
        let output = self.compose_in(size, stored, scene, images, origin, &self.scratch)?;
        self.logical_image(output, size)
    }
    /// Render script coordinates directly into a physical display canvas.
    /// The returned image has `physical` dimensions. Source images retain
    /// their logical coordinates and compact storage; they are never expanded.
    pub fn scene_surface_scaled(
        &self,
        logical: Size,
        physical: Size,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
    ) -> Result<Image> {
        let _blending = self.display_scene_scope(scene);
        self.compose_in(logical, physical, scene, images, (0, 0), &self.scratch)
    }
    fn compose_in(
        &self,
        size: Size,
        physical: Size,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
        origin: (i32, i32),
        budget: &krkr_protocol::budget::Budget,
    ) -> Result<Image> {
        self.compose_patch(
            size,
            physical,
            scene,
            images,
            origin,
            budget,
            physical.rect(),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compose_patch(
        &self,
        size: Size,
        physical: Size,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
        origin: (i32, i32),
        budget: &krkr_protocol::budget::Budget,
        region: Rect,
    ) -> Result<Image> {
        let mut output = Image {
            size: Size {
                width: region.width,
                height: region.height,
            },
            device: self.device.clone(),
            canvas: false,
            text: false,
            main: Some(self.overwrite_plane(
                Size {
                    width: region.width,
                    height: region.height,
                },
                budget,
            )?),
            province: None,
        };
        self.compose_patch_into(
            &mut output,
            size,
            physical,
            scene,
            images,
            origin,
            region,
            (i64::from(region.left), i64::from(region.top)),
            false,
        )?;
        Ok(output)
    }

    /// Compose a damage region directly into an existing display canvas.
    /// On streamed GLES devices this removes the temporary patch surface and
    /// the copy back into the canvas. The caller supplies a target whose plane
    /// already has `physical` storage; all scene coordinates remain logical.
    /// `retained` marks that target as a display canvas holding earlier pixels:
    /// its storage spans the whole physical canvas and the damage region must be
    /// cleared before it is rebuilt. A patch surface is region sized instead.
    /// The flag is explicit because a retained canvas keeps logical script
    /// coordinates in `Image::size` and compact physical storage in its plane;
    /// direct composition relabels it as a physical surface for the duration.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compose_patch_into(
        &self,
        output: &mut Image,
        size: Size,
        physical: Size,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
        origin: (i32, i32),
        region: Rect,
        target_origin: (i64, i64),
        retained: bool,
    ) -> Result<()> {
        let raster = Raster::new(size, physical, origin)?;
        let _draw_state = self.device.draw_state.scope();
        if physical.rect().intersection(region) != Some(region) {
            return Err(Error::Message("composition damage outside canvas"));
        }
        self.check_image(output)?;
        let expected_storage = if retained {
            physical
        } else {
            Size {
                width: region.width,
                height: region.height,
            }
        };
        if output.plane(false)?.size != expected_storage {
            return Err(Error::Message(
                "composition target storage differs from canvas",
            ));
        }
        for image in images.values() {
            self.check_image(image)?;
        }
        if i64::from(origin.0) + i64::from(size.width) > i64::from(i32::MAX)
            || i64::from(origin.1) + i64::from(size.height) > i64::from(i32::MAX)
        {
            return Err(Error::Message("scene region exceeds coordinate range"));
        }
        let count = scene.nodes.len();
        let _metadata = self.staging.reserve(
            count
                .checked_mul(128)
                .ok_or(Error::Message("scene metadata overflow"))?,
        )?;
        let children = Children::new(&scene.nodes, 127)?;
        let mut transitions = if scene.transitions.is_empty() {
            Vec::new()
        } else {
            vec![None; count]
        };
        for (index, transition) in scene.transitions.iter().enumerate() {
            if transition.destination >= count || transition.source >= count {
                return Err(Error::Message("transition references a missing scene node"));
            }
            if transitions[transition.destination].replace(index).is_some() {
                return Err(Error::Message("scene node has multiple transitions"));
            }
        }
        // A full-cover opaque root replaces every earlier root, including
        // expensive alpha groups behind fullscreen movies. Use the same
        // sampling proof as the bitmap draw, not just rectangle overlap.
        let mut first_root = 0;
        let mut covered = false;
        if scene.transitions.is_empty() {
            for (index, node) in scene.nodes.iter().enumerate().rev() {
                if node.parent.is_some()
                    || !node.visible
                    || node.opacity != 255
                    || node.blend != Blend::Opaque
                    || !raster
                        .rect(node.rectangle)
                        .is_some_and(|r| r.intersection(region) == Some(region))
                {
                    continue;
                }
                let source_origin = (
                    i64::from(node.rectangle.left) + i64::from(node.image_left),
                    i64::from(node.rectangle.top) + i64::from(node.image_top),
                );
                let overwrites = match &node.image {
                    None => true,
                    Some(reference) => images
                        .get(&reference.id)
                        .map(|image| {
                            self.scene_bitmap_overwrites(
                                image,
                                local(region, target_origin),
                                raster,
                                target_origin,
                                source_origin,
                            )
                        })
                        .transpose()?
                        .unwrap_or(false),
                };
                if overwrites {
                    first_root = index;
                    covered = true;
                    break;
                }
            }
        }
        let mut height = region.height;
        if scene.transitions.is_empty() {
            let mut groups = vec![None; count];
            let mut maximum = 0usize;
            let mut backdrop = false;
            for (index, node) in scene.nodes.iter().enumerate().skip(first_root) {
                if node.visible && node.opacity != 0 {
                    groups[index] =
                        node.parent
                            .map_or(Some(0), |parent| groups[parent])
                            .map(|depth| {
                                depth
                                    + usize::from(
                                        !children[index].is_empty()
                                            && (node.opacity != 255 || node.blend != Blend::Opaque),
                                    )
                            });
                    maximum = maximum.max(groups[index].unwrap_or(0));
                    if groups[index].is_some() {
                        backdrop |= node.blend != Blend::Opaque || node.opacity != 255;
                    }
                }
            }
            // Work-surface blends sample the existing backing; they do not
            // allocate another full-screen destination snapshot.
            let row = (region.width as usize * 4)
                .saturating_mul(maximum + usize::from(backdrop && !self.device.streamed_uploads()));
            let available = self.scratch_capacity();
            if let Some(rows) = available.checked_div(row) {
                if rows == 0 {
                    return Err(Error::Message("composition cannot fit one band row"));
                }
                height = height.min(rows as u32);
            }
        }
        if !covered {
            // Both new patch storage and retained canvases need initialization
            // unless a root provably overwrites the whole damage region.
            // Keep this after scene validation and band admission so rejected
            // updates leave the previously presented canvas untouched.
            self.solid(
                output,
                local(region, target_origin),
                0,
                BlendOptions::for_composition(Blend::Opaque, DrawFace::Opaque, 255),
                true,
            )?;
        }
        self.scene_cache.borrow_mut().begin_frame();
        let mut frame = Frame {
            gpu: self,
            scene,
            images,
            children,
            transitions,
            active: Vec::new(),
            groups: Vec::new(),
            depth: 0,
            raster,
            band: physical.rect(),
            cache_entries: Vec::new(),
            cache_pending: 0,
            cache_count: 0,
            cache_allowance: crate::scene_cache::LIMIT.min(
                self.resident
                    .available()
                    .saturating_sub(physical.rgba_bytes().unwrap().saturating_mul(4)),
            ),
        };
        let clip = Rect {
            left: origin.0,
            top: origin.1,
            ..size.rect()
        };
        let roots: smallvec::SmallVec<[_; 4]> = scene
            .nodes
            .iter()
            .enumerate()
            .skip(first_root)
            .filter_map(|(index, node)| node.parent.is_none().then_some(index))
            .collect();
        for top in (0..region.height).step_by(height as usize) {
            frame.band = Rect {
                left: region.left,
                top: region.top + top as i32,
                width: region.width,
                height: height.min(region.height - top),
            };
            let mut root = 0;
            while root < roots.len() {
                let batched = frame.leaf_batch(
                    &roots[root..],
                    output,
                    target_origin,
                    (0, 0),
                    clip,
                    DrawFace::AddAlpha,
                )?;
                if batched == 0 {
                    frame.node(
                        roots[root],
                        output,
                        target_origin,
                        (0, 0),
                        clip,
                        DrawFace::AddAlpha,
                    )?;
                }
                root += batched.max(1);
            }
        }
        self.resolve()?;
        for entry in frame.cache_entries {
            self.scene_cache.borrow_mut().insert(entry);
        }
        Ok(())
    }
    fn solid(
        &self,
        target: &mut Image,
        area: Rect,
        color: u32,
        options: BlendOptions,
        raw: bool,
    ) -> Result<()> {
        self.writable(target, area, false)?;
        self.draw(
            target.plane(false)?,
            None,
            area,
            &Draw {
                kind: if raw || (options.mode == Blend::Opaque && options.opacity == 255) {
                    3.
                } else {
                    5.
                },
                color: rgba(color),
                operation: [
                    options.mode as i32 as f32,
                    face(options.face),
                    f32::from(options.opacity),
                    f32::from(options.hold_alpha),
                ],
                ..Draw::copy([0.; 6], [true; 4])
            },
        )
    }
}
pub(crate) fn intersection(
    left: i64,
    top: i64,
    width: u32,
    height: u32,
    clip: Rect,
) -> Option<Rect> {
    let x = left.max(i64::from(clip.left));
    let y = top.max(i64::from(clip.top));
    let right = (left + i64::from(width)).min(i64::from(clip.left) + i64::from(clip.width));
    let bottom = (top + i64::from(height)).min(i64::from(clip.top) + i64::from(clip.height));
    (x < right && y < bottom).then_some(Rect {
        left: x as i32,
        top: y as i32,
        width: (right - x) as u32,
        height: (bottom - y) as u32,
    })
}
fn local(rect: Rect, origin: (i64, i64)) -> Rect {
    Rect {
        left: (i64::from(rect.left) - origin.0) as i32,
        top: (i64::from(rect.top) - origin.1) as i32,
        ..rect
    }
}
