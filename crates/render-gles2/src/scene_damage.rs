//! Display-only retained pixels. Metadata is weak: remembering a frame must
//! never force copy-on-write of the game's next glyph or keep a dead page alive.
use crate::{Error, Gpu, Image, Result, scene::raster::Raster};
use krkr_protocol::{
    budget::Permit,
    graphics::{Blend, DrawFace, ImageId, Rect, Scene, Size},
};
use std::{
    collections::HashMap,
    rc::{Rc, Weak},
};
#[cfg(test)]
#[path = "../tests/scene_damage/internal.rs"]
mod tests;
mod versions;

#[derive(Default)]
pub struct SceneState {
    frame: Option<Stamp>,
    display_blend: bool,
}
pub(crate) struct DisplayBlending<'a> {
    state: &'a std::cell::Cell<bool>,
    previous: bool,
}
impl Drop for DisplayBlending<'_> {
    fn drop(&mut self) {
        self.state.set(self.previous);
    }
}
struct Stamp {
    logical: Size,
    physical: Size,
    nodes: Vec<Node>,
    transitions: Vec<Transition>,
    _permit: Permit,
}
struct Transition {
    destination: usize,
    source: usize,
    with_children: bool,
    frame: krkr_protocol::transition::Frame,
    rule: Option<Version>,
}
impl Transition {
    fn same(&self, old: &Self) -> bool {
        self.destination == old.destination
            && self.source == old.source
            && self.with_children == old.with_children
            && self.frame.effect == old.frame.effect
            && self.frame.face == old.frame.face
            && self.frame.size == old.frame.size
            && self.frame.phase == old.frame.phase
            && same_image(&self.rule, &old.rule)
    }
}
fn same_image(a: &Option<Version>, b: &Option<Version>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.same(b),
        _ => false,
    }
}
#[derive(PartialEq)]
struct Geometry {
    parent: Option<usize>,
    visible: bool,
    size: Size,
    clip: Option<Rect>,
    origin: (i64, i64),
    image_origin: (i64, i64),
    blend: Blend,
    opacity: u8,
    neutral: u32,
}
struct Node {
    geometry: Geometry,
    image: Option<Version>,
    transition_input: bool,
    transition_children: bool,
}
impl Node {
    fn same(&self, old: &Self) -> bool {
        self.geometry == old.geometry
            && self.transition_input == old.transition_input
            && self.transition_children == old.transition_children
            && same_image(&self.image, &old.image)
    }
}
#[derive(Clone)]
pub(crate) struct Version {
    logical: Size,
    stored: Size,
    province: bool,
    effect_canvas: bool,
    // Most Vita images occupy one native tile. Snapshot its weak identity and
    // generation inline rather than allocating separately for every layer.
    tiles: versions::Tiles,
}
impl Version {
    fn same(&self, old: &Self) -> bool {
        self.logical == old.logical
            && self.stored == old.stored
            && self.province == old.province
            && self.effect_canvas == old.effect_canvas
            && self.tiles.len() == old.tiles.len()
            && self
                .tiles
                .iter(self.stored)
                .zip(old.tiles.iter(old.stored))
                .all(|(a, b)| a.0 == b.0 && a.2 == b.2 && a.3 == b.3 && Weak::ptr_eq(a.1, b.1))
    }
    fn layout_damage(&self, old: &Self) -> Option<Rect> {
        let mut changed = None;
        for (rectangle, texture, generation, backing) in self.tiles.iter(self.stored) {
            let mut covered = 0u64;
            for (before, old_texture, old_generation, old_backing) in old.tiles.iter(old.stored) {
                let Some(part) = rectangle.intersection(before) else {
                    continue;
                };
                covered += u64::from(part.width) * u64::from(part.height);
                let same_texture =
                    Weak::ptr_eq(texture, old_texture) && generation == old_generation;
                let same_sampling = backing.unwrap_or(rectangle) == old_backing.unwrap_or(before);
                if same_texture && same_sampling {
                    continue;
                }
                // Constant views can have different logical extents while
                // still sampling the same single known color everywhere.
                let same_solid =
                    texture
                        .upgrade()
                        .zip(old_texture.upgrade())
                        .is_some_and(|(a, b)| {
                            a.generation.get() == generation
                                && b.generation.get() == old_generation
                                && a.solid_color().is_some()
                                && a.solid_color() == b.solid_color()
                        });
                if !same_solid {
                    changed = Some(union(changed, part));
                }
            }
            if covered != u64::from(rectangle.width) * u64::from(rectangle.height) {
                return Some(self.logical.rect());
            }
        }
        changed.map(|r| {
            let axis = |start: i32, length: u32, n: u32, d: u32| {
                let lo = (i64::from(start) * i64::from(n) / i64::from(d) - 1).max(0) as i32;
                let hi = (((i64::from(start) + i64::from(length)) * i64::from(n) + i64::from(d)
                    - 1)
                    / i64::from(d)
                    + 1)
                .min(i64::from(n)) as i32;
                (lo, (hi - lo) as u32)
            };
            let (left, width) = axis(r.left, r.width, self.logical.width, self.stored.width);
            let (top, height) = axis(r.top, r.height, self.logical.height, self.stored.height);
            Rect {
                left,
                top,
                width,
                height,
            }
        })
    }
    pub(crate) fn current(&self) -> bool {
        self.tiles
            .iter(self.stored)
            .all(|(_, texture, generation, _)| {
                texture
                    .upgrade()
                    .is_some_and(|t| t.generation.get() == generation)
            })
    }
    pub(crate) fn matches(&self, image: &Image) -> bool {
        self.matches_plane(image, false)
    }
    /// A gather can be updated in place when only source write generations change.
    pub(crate) fn same_storage(&self, image: &Image) -> bool {
        let Ok(plane) = image.plane(false) else {
            return false;
        };
        !self.province
            && self.effect_canvas == (image.canvas && !image.text)
            && self.logical == image.size
            && self.stored == plane.size
            && self.tiles.len() == plane.tiles.len()
            && self.tiles.iter(self.stored).zip(&plane.tiles).all(
                |((rect, texture, _, backing), tile)| {
                    rect == tile.rectangle
                        && backing == tile.backing
                        && texture.as_ptr() == Rc::as_ptr(&tile.texture)
                },
            )
    }
    pub(crate) fn matches_plane(&self, image: &Image, province: bool) -> bool {
        let Ok(plane) = image.plane(province) else {
            return false;
        };
        self.province == province
            && self.effect_canvas == (!province && image.canvas && !image.text)
            && self.logical == image.size
            && self.stored == plane.size
            && self.tiles.len() == plane.tiles.len()
            && self.tiles.iter(self.stored).zip(&plane.tiles).all(
                |((rect, texture, generation, backing), tile)| {
                    rect == tile.rectangle
                        && backing == tile.backing
                        && generation == tile.texture.generation.get()
                        && texture.as_ptr() == Rc::as_ptr(&tile.texture)
                },
            )
    }
    pub(crate) fn capture(image: &Image) -> Result<Self> {
        Self::capture_plane(image, false)
    }
    pub(crate) fn capture_plane(image: &Image, province: bool) -> Result<Self> {
        let plane = image.plane(province)?;
        Ok(Self {
            logical: image.size,
            stored: plane.size,
            province,
            effect_canvas: !province && image.canvas && !image.text,
            tiles: versions::Tiles::capture(plane),
        })
    }
    pub(crate) fn bytes(&self) -> usize {
        self.tiles.bytes()
    }
    pub(crate) fn damage(&self, old: &Self) -> Option<Rect> {
        if self.logical != old.logical
            || self.stored != old.stored
            || self.province != old.province
            || self.effect_canvas != old.effect_canvas
        {
            // A smaller replacement also exposes pixels covered by the old
            // image. Repaint both extents to remove those retained pixels.
            return Some(union(Some(old.logical.rect()), self.logical.rect()));
        }
        if self.tiles.len() != old.tiles.len()
            || self
                .tiles
                .iter(self.stored)
                .zip(old.tiles.iter(old.stored))
                .any(|(a, b)| a.0 != b.0 || a.3 != b.3)
        {
            return self.layout_damage(old);
        }
        let mut changed = None;
        for (new, old) in self.tiles.iter(self.stored).zip(old.tiles.iter(old.stored)) {
            if new.0 != old.0 || new.3 != old.3 {
                return Some(self.logical.rect());
            }
            if new.2 == old.2 && Weak::ptr_eq(new.1, old.1) {
                continue;
            }
            if new.3.is_some() {
                return Some(self.logical.rect());
            }
            let Some(texture) = new.1.upgrade() else {
                return Some(self.logical.rect());
            };
            if let Some(area) = texture.damage_from(old.1, old.2) {
                let x0 = i64::from(new.0.left) + i64::from(area.left);
                let y0 = i64::from(new.0.top) + i64::from(area.top);
                // Expand outwards across the two nearest-neighbour grids.
                // The guard also covers f32 rounding at compact tile edges.
                let axis = |start: i64, length: u32, n: u32, d: u32| {
                    let lo = (start * i64::from(n) / i64::from(d) - 1).max(0) as i32;
                    let hi = (((start + i64::from(length)) * i64::from(n) + i64::from(d) - 1)
                        / i64::from(d)
                        + 1)
                    .min(i64::from(n)) as i32;
                    (lo, (hi - lo) as u32)
                };
                let (left, width) = axis(x0, area.width, self.logical.width, self.stored.width);
                let (top, height) = axis(y0, area.height, self.logical.height, self.stored.height);
                changed = Some(union(
                    changed,
                    Rect {
                        left,
                        top,
                        width,
                        height,
                    },
                ));
            }
        }
        changed
    }
}
impl Stamp {
    /// Reuse the weak stamp when scene inputs are identical. This avoids
    /// allocating and dropping another node/transition snapshot on idle ticks.
    fn matches(
        &self,
        gpu: &Gpu,
        logical: Size,
        physical: Size,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
    ) -> bool {
        if self.logical != logical
            || self.physical != physical
            || self.nodes.len() != scene.nodes.len()
            || self.transitions.len() != scene.transitions.len()
        {
            return false;
        }
        let image_matches = |version: &Option<Version>,
                             reference: &Option<krkr_protocol::graphics::ImageRef>,
                             rule: bool| {
            match (version, reference) {
                (None, None) => true,
                (Some(version), Some(reference)) => {
                    images.get(&reference.id).is_some_and(|image| {
                        gpu.check_image(image).is_ok()
                            && version.matches_plane(image, rule && image.has_province())
                    })
                }
                _ => false,
            }
        };
        if !self
            .transitions
            .iter()
            .zip(&scene.transitions)
            .all(|(old, new)| {
                new.custom.is_none()
                    && old.destination == new.destination
                    && old.source == new.source
                    && old.with_children == new.with_children
                    && old.frame.effect == new.frame.effect
                    && old.frame.face == new.frame.face
                    && old.frame.size == new.frame.size
                    && old.frame.phase == new.frame.phase
                    && image_matches(&old.rule, &new.rule, true)
            })
        {
            return false;
        }
        self.nodes.iter().zip(&scene.nodes).all(|(old, new)| {
            let g = &old.geometry;
            if g.parent != new.parent
                || g.visible != new.visible
                || g.opacity != new.opacity
                || g.blend != new.blend
                || g.neutral != new.neutral_color
                || g.size.width != new.rectangle.width
                || g.size.height != new.rectangle.height
            {
                return false;
            }
            let parent = g.parent.map_or((0, 0), |i| self.nodes[i].geometry.origin);
            let origin = (
                parent.0 + i64::from(new.rectangle.left),
                parent.1 + i64::from(new.rectangle.top),
            );
            g.origin == origin
                && g.image_origin
                    == (
                        origin.0 + i64::from(new.image_left),
                        origin.1 + i64::from(new.image_top),
                    )
                && if g.clip.is_none() && !old.transition_input {
                    // Capture validates present image planes even for hidden
                    // nodes while estimating its metadata allowance.
                    new.image
                        .as_ref()
                        .and_then(|r| images.get(&r.id))
                        .is_none_or(Image::has_main)
                } else {
                    image_matches(&old.image, &new.image, false)
                }
        })
    }
    fn capture(
        gpu: &Gpu,
        logical: Size,
        physical: Size,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
    ) -> Result<Self> {
        // Bounded metadata is charged before building weak records. A frame
        // has one record per node plus its referenced physical tiles.
        let mut bytes = scene
            .nodes
            .len()
            .checked_mul(std::mem::size_of::<Node>())
            .ok_or(Error::Message("scene metadata overflow"))?;
        for node in &scene.nodes {
            if let Some(image) = node.image.as_ref().and_then(|r| images.get(&r.id)) {
                bytes = bytes
                    .checked_add(versions::Tiles::allocation_bytes(image.plane(false)?))
                    .ok_or(Error::Message("scene metadata overflow"))?;
            }
        }
        let transition_bytes = scene
            .transitions
            .len()
            .checked_mul(std::mem::size_of::<Transition>())
            .ok_or(Error::Message("scene metadata overflow"))?;
        bytes = bytes
            .checked_add(transition_bytes)
            .ok_or(Error::Message("scene metadata overflow"))?;
        for transition in &scene.transitions {
            if let Some(image) = transition.rule.as_ref().and_then(|r| images.get(&r.id)) {
                bytes = bytes
                    .checked_add(versions::Tiles::allocation_bytes(
                        image.plane(image.has_province())?,
                    ))
                    .ok_or(Error::Message("scene metadata overflow"))?;
            }
        }
        let permit = gpu.staging.reserve(bytes)?;
        // Index endpoints once. Scanning every transition for every node made
        // unchanged effect-heavy scenes cost O(nodes * transitions) per tick.
        let endpoint_count = if scene.transitions.is_empty() {
            0
        } else {
            scene.nodes.len()
        };
        let _endpoints = gpu.staging.reserve(endpoint_count)?;
        let mut endpoints = vec![0_u8; endpoint_count];
        for transition in &scene.transitions {
            for index in [transition.destination, transition.source] {
                let flags = endpoints
                    .get_mut(index)
                    .ok_or(Error::Message("transition references a missing scene node"))?;
                *flags |= if transition.with_children { 3 } else { 1 };
            }
        }
        let mut nodes: Vec<Node> = Vec::with_capacity(scene.nodes.len());
        for (index, node) in scene.nodes.iter().enumerate() {
            let (parent_origin, parent_clip) = if let Some(parent) = node.parent {
                if parent >= index {
                    return Err(Error::Message("scene parent must precede its children"));
                }
                (nodes[parent].geometry.origin, nodes[parent].geometry.clip)
            } else {
                ((0, 0), Some(logical.rect()))
            };
            let origin = (
                parent_origin.0 + i64::from(node.rectangle.left),
                parent_origin.1 + i64::from(node.rectangle.top),
            );
            let clip = parent_clip
                .filter(|_| node.visible && node.opacity != 0)
                .and_then(|clip| {
                    crate::scene::intersection(
                        origin.0,
                        origin.1,
                        node.rectangle.width,
                        node.rectangle.height,
                        clip,
                    )
                });
            let transition_children = node.parent.is_some_and(|p| nodes[p].transition_children)
                || endpoints.get(index).is_some_and(|flags| flags & 2 != 0);
            let transition_input =
                transition_children || endpoints.get(index).is_some_and(|flags| flags & 1 != 0);
            // Hidden nodes can be transition inputs. Their pixel versions
            // must invalidate retained output even at an unchanged phase.
            let image = if clip.is_some() || transition_input {
                node.image
                    .as_ref()
                    .map(|r| {
                        let image = images
                            .get(&r.id)
                            .ok_or(Error::Message("scene image is no longer available"))?;
                        gpu.check_image(image)?;
                        Version::capture(image)
                    })
                    .transpose()?
            } else {
                None
            };
            nodes.push(Node {
                geometry: Geometry {
                    parent: node.parent,
                    visible: node.visible,
                    size: Size {
                        width: node.rectangle.width,
                        height: node.rectangle.height,
                    },
                    clip,
                    origin,
                    image_origin: (
                        origin.0 + i64::from(node.image_left),
                        origin.1 + i64::from(node.image_top),
                    ),
                    blend: node.blend,
                    opacity: node.opacity,
                    neutral: node.neutral_color,
                },
                image,
                transition_input,
                transition_children,
            });
        }
        let mut transitions = Vec::with_capacity(scene.transitions.len());
        for t in &scene.transitions {
            let rule = t
                .rule
                .as_ref()
                .map(|reference| {
                    let image = images
                        .get(&reference.id)
                        .ok_or(Error::Message("transition rule is no longer available"))?;
                    gpu.check_image(image)?;
                    // Universal transitions prefer the grayscale province
                    // plane, including rules with no RGBA main plane at all.
                    Version::capture_plane(image, image.has_province())
                })
                .transpose()?;
            transitions.push(Transition {
                destination: t.destination,
                source: t.source,
                with_children: t.with_children,
                frame: t.frame,
                rule,
            });
        }
        Ok(Self {
            logical,
            physical,
            nodes,
            transitions,
            _permit: permit,
        })
    }
    fn damage(&self, old: &Self) -> Option<Rect> {
        if self.logical != old.logical || self.physical != old.physical {
            return Some(self.physical.rect());
        }
        if !self.transitions.is_empty() || !old.transitions.is_empty() {
            // At an unchanged phase, only endpoint changes require rebuilding
            // the whole transition. Unrelated text/overlays keep local damage.
            let same = self.transitions.len() == old.transitions.len()
                && self
                    .transitions
                    .iter()
                    .zip(&old.transitions)
                    .all(|(a, b)| a.same(b))
                && (0..self.nodes.len().max(old.nodes.len())).all(|index| {
                    let a = self.nodes.get(index);
                    let b = old.nodes.get(index);
                    if !a.is_some_and(|n| n.transition_input)
                        && !b.is_some_and(|n| n.transition_input)
                    {
                        return true;
                    }
                    a.zip(b).is_some_and(|(a, b)| {
                        a.geometry == b.geometry
                            && a.transition_input == b.transition_input
                            && same_image(&a.image, &b.image)
                    })
                });
            if !same {
                return Some(self.physical.rect());
            }
        }
        let raster = Raster::new(self.logical, self.physical, (0, 0)).expect("validated raster");
        let mut damage = None;
        let (current, previous) = changed_nodes(&self.nodes, &old.nodes);
        for (new, old) in current.iter().zip(previous) {
            if new.geometry != old.geometry {
                for clip in [new.geometry.clip, old.geometry.clip].into_iter().flatten() {
                    if let Some(area) = raster.rect(clip) {
                        damage = Some(union(damage, area));
                    }
                }
                continue;
            }
            let Some(clip) = new.geometry.clip else {
                continue;
            };
            let local = match (&new.image, &old.image) {
                (Some(new), Some(old)) => new.damage(old),
                (None, None) => None,
                _ => {
                    if let Some(area) = raster.rect(clip) {
                        damage = Some(union(damage, area));
                    }
                    continue;
                }
            };
            if let Some(area) = local
                .and_then(|area| {
                    crate::scene::intersection(
                        new.geometry.image_origin.0 + i64::from(area.left),
                        new.geometry.image_origin.1 + i64::from(area.top),
                        area.width,
                        area.height,
                        clip,
                    )
                })
                .and_then(|r| raster.rect(r))
            {
                damage = Some(union(damage, area));
            }
        }
        // Added/removed nodes contribute their full visible clips, including
        // old pixels exposed by removal. Unchanged later siblings were trimmed
        // above, so inserting a glyph before an overlay cannot dirty it too.
        let shared = current.len().min(previous.len());
        for node in current[shared..].iter().chain(&previous[shared..]) {
            if let Some(area) = node.geometry.clip.and_then(|clip| raster.rect(clip)) {
                damage = Some(union(damage, area));
            }
        }
        damage
    }
}
fn changed_nodes<'a>(new: &'a [Node], old: &'a [Node]) -> (&'a [Node], &'a [Node]) {
    let prefix = new.iter().zip(old).take_while(|(a, b)| a.same(b)).count();
    let new_tail = &new[prefix..];
    let old_tail = &old[prefix..];
    let suffix = new_tail
        .iter()
        .rev()
        .zip(old_tail.iter().rev())
        .take_while(|(a, b)| {
            // Only skip nodes whose ancestry is in the unchanged prefix.
            // Equal numeric parent indices inside the edited range can refer
            // to different groups after insertion or removal. Keep those
            // comparisons conservative, including transition participants.
            a.geometry.parent.is_none_or(|p| p < prefix)
                && !a.transition_input
                && !b.transition_input
                && a.same(b)
        })
        .count();
    (
        &new_tail[..new_tail.len() - suffix],
        &old_tail[..old_tail.len() - suffix],
    )
}
pub(crate) fn union(old: Option<Rect>, area: Rect) -> Rect {
    let Some(old) = old else {
        return area;
    };
    let left = old.left.min(area.left);
    let top = old.top.min(area.top);
    Rect {
        left,
        top,
        width: ((i64::from(old.left) + i64::from(old.width))
            .max(i64::from(area.left) + i64::from(area.width))
            - i64::from(left)) as u32,
        height: ((i64::from(old.top) + i64::from(old.height))
            .max(i64::from(area.top) + i64::from(area.height))
            - i64::from(top)) as u32,
    }
}
impl Gpu {
    pub(crate) fn display_blend_kind(
        &self,
        options: krkr_protocol::graphics::BlendOptions,
    ) -> Option<f32> {
        if !self.display_scene_blend.get() || options.opacity == 0 || options.hold_alpha {
            return None;
        }
        match (options.mode, options.face) {
            (Blend::Alpha, DrawFace::Opaque) if options.opacity == 255 => Some(9.),
            (Blend::AddAlpha, DrawFace::Opaque) => Some(10.),
            (Blend::AddAlpha, DrawFace::AddAlpha) => Some(11.),
            _ => None,
        }
    }
    pub(crate) fn suspend_display_blending(&self) -> DisplayBlending<'_> {
        DisplayBlending {
            state: &self.display_scene_blend,
            previous: self.display_scene_blend.replace(false),
        }
    }
    pub(crate) fn display_scene_scope<'a>(&'a self, scene: &Scene) -> DisplayBlending<'a> {
        // Final display composition does not feed edited pixels back into the
        // script. Opaque groups can use fixed blending without a backdrop copy;
        // isolated groups and script captures keep the byte-domain kernels.
        let enabled = self.device.streamed_uploads() && scene.transitions.is_empty();
        DisplayBlending {
            state: &self.display_scene_blend,
            previous: self.display_scene_blend.replace(enabled),
        }
    }
    /// Update a host-owned display canvas. Script captures continue to use the
    /// atomic full composition API. A failed host update must not be presented.
    /// Returns the physical region redrawn, or None when pixels are unchanged.
    pub fn update_scene_surface(
        &self,
        canvas: &mut Option<Image>,
        state: &mut SceneState,
        logical: Size,
        physical: Size,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
    ) -> Result<Option<Rect>> {
        let _blending = self.display_scene_scope(scene);
        let display_blend = self.display_scene_blend.get();
        let changed_blending = display_blend != state.display_blend;
        Raster::new(logical, physical, (0, 0))?;
        let reusable = canvas
            .as_ref()
            .is_some_and(|image| image.size == logical && image.stored_size() == Some(physical));
        if reusable
            && !changed_blending
            && state
                .frame
                .as_ref()
                .is_some_and(|old| old.matches(self, logical, physical, scene, images))
        {
            return Ok(None);
        }
        let next = if scene.transitions.iter().all(|t| t.custom.is_none()) {
            Some(Stamp::capture(self, logical, physical, scene, images)?)
        } else {
            None
        };
        let damage = if reusable && !changed_blending {
            match (&next, &state.frame) {
                (Some(new), Some(old)) => new.damage(old),
                _ => Some(physical.rect()),
            }
        } else {
            Some(physical.rect())
        };
        // Glyph bearings and fractional scaling otherwise produce a new
        // allocation shape almost every frame. Bucket only the repaint extent;
        // composition still uses the original global pixel grid and clips.
        let damage = damage.map(|area| {
            if self.device.streamed_uploads() {
                reuse_region(area, physical)
            } else {
                area
            }
        });
        if let Some(area) = damage {
            if area == physical.rect() && !(self.device.streamed_uploads() && reusable) {
                let patch = self.compose_patch(
                    logical,
                    physical,
                    scene,
                    images,
                    (0, 0),
                    &self.scratch,
                    area,
                )?;
                *canvas = Some(self.logical_image(patch, logical)?);
            } else if self.device.streamed_uploads() {
                let target = canvas.as_mut().expect("retained display canvas");
                // Streamed GLES can render directly into the retained canvas;
                // the damage region is cleared and rebuilt in place. This
                // removes one temporary surface and one full GL copy command.
                // Full repaints and transition ticks can reuse it too: avoid
                // holding two panel-sized canvases and allocating a new target
                // every frame while its previous GPU commands are still live.
                // The canvas stores physical pixels but reports logical script
                // coordinates, so relabel it as a physical surface: composition
                // works in physical coordinates and must not materialize the
                // compact storage back to the logical size.
                let was_canvas = target.canvas;
                target.canvas = false;
                target.size = physical;
                let result = self.compose_patch_into(
                    target,
                    logical,
                    physical,
                    scene,
                    images,
                    (0, 0),
                    area,
                    (0, 0),
                    true,
                );
                target.size = logical;
                target.canvas = was_canvas;
                if let Err(error) = result {
                    // Direct composition may have cleared part of the
                    // retained canvas before a late GL error. Discard the
                    // damage stamp so the next frame performs a full rebuild;
                    // validation failures occur before the clear below.
                    state.frame = None;
                    return Err(error);
                }
            } else {
                let patch = self.compose_patch(
                    logical,
                    physical,
                    scene,
                    images,
                    (0, 0),
                    &self.scratch,
                    area,
                )?;
                let target = canvas.as_mut().expect("partial display canvas");
                self.check_image(target)?;
                // The display canvas is private and already physically sized;
                // do not materialize its logical-resolution script coordinates.
                let was_canvas = target.canvas;
                target.size = physical;
                target.canvas = false;
                let result = self.copy_rect(
                    target,
                    &patch,
                    patch.size.rect(),
                    area.left,
                    area.top,
                    area,
                    DrawFace::Alpha,
                    false,
                );
                target.size = logical;
                target.canvas = was_canvas;
                if let Err(error) = result {
                    state.frame = None;
                    return Err(error);
                }
            }
            self.resolve()?;
        }
        state.frame = next;
        state.display_blend = display_blend;
        Ok(damage)
    }
}
fn reuse_region(area: Rect, size: Size) -> Rect {
    let width = area.width.div_ceil(16).saturating_mul(16).min(size.width);
    let height = area.height.div_ceil(16).saturating_mul(16).min(size.height);
    Rect {
        left: area.left.min((size.width - width) as i32),
        top: area.top.min((size.height - height) as i32),
        width,
        height,
    }
}
