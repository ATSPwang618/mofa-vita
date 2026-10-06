//! Fresh character layers already have a known backdrop and CPU glyph masks.
//! Upload their completed pixels once instead of updating a live atlas and
//! resolving a tiny offscreen draw for every new character.
use crate::{
    Error, Gpu, Image, Result,
    image::{Plane, Tile},
    scene_damage::Version,
};
use krkr_protocol::{
    budget::{Budget, Permit},
    graphics::{DrawFace, Rect},
    pixels::Bytes,
    text::{Run, Style},
};
use std::{
    collections::HashMap,
    rc::{Rc, Weak},
};

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct Key {
    size: [u32; 2],
    clip: [i32; 4],
    background: u32,
    count: usize,
    glyphs: [(u64, i32, i32, u32); 2],
}
impl Key {
    fn new(image: &Image, run: &Run, clip: Rect, background: u32) -> Self {
        let mut glyphs = [(0, 0, 0, 0); 2];
        for (slot, placed) in glyphs.iter_mut().zip(&run.glyphs) {
            *slot = (placed.glyph.id, placed.x, placed.y, placed.color);
        }
        Self {
            size: [image.size.width, image.size.height],
            clip: [clip.left, clip.top, clip.width as i32, clip.height as i32],
            background,
            count: run.glyphs.len(),
            glyphs,
        }
    }
}
struct Entry {
    plane: Weak<Plane>,
    version: Version,
    touched: u64,
    _permit: Permit,
}
/// Repeated characters share pixels already owned by live layers. Weak entries
/// never keep an old dialogue page or its textures resident.
#[derive(Default)]
pub(crate) struct Cache {
    entries: HashMap<Key, Entry>,
    clock: u64,
}
impl Cache {
    fn entry_bytes() -> usize {
        2 * (std::mem::size_of::<Key>() + std::mem::size_of::<Entry>())
    }
    pub fn trim(&mut self) {
        self.entries
            .retain(|_, entry| entry.plane.strong_count() != 0 && entry.version.current());
    }
    fn get(&mut self, key: &Key) -> Option<Rc<Plane>> {
        let entry = self.entries.get_mut(key)?;
        if !entry.version.current() {
            self.entries.remove(key);
            return None;
        }
        let plane = entry.plane.upgrade()?;
        self.clock += 1;
        entry.touched = self.clock;
        Some(plane)
    }
    fn insert(&mut self, key: Key, image: &Image, budget: &Budget) {
        if self.entries.len() >= 256 {
            self.trim();
            if self.entries.len() >= 256 {
                let oldest = *self
                    .entries
                    .iter()
                    .min_by_key(|(_, e)| e.touched)
                    .unwrap()
                    .0;
                self.entries.remove(&oldest);
            }
        }
        let Ok(version) = Version::capture(image) else {
            return;
        };
        // Includes amortized hash-table buckets. Retention is optional and
        // uses the existing resident budget, without an additional pixel pool.
        let Ok(permit) = budget.reserve(Self::entry_bytes()) else {
            return;
        };
        self.clock += 1;
        self.entries.insert(
            key,
            Entry {
                plane: Rc::downgrade(image.main.as_ref().unwrap()),
                version,
                touched: self.clock,
                _permit: permit,
            },
        );
    }
}

impl Gpu {
    pub(crate) fn text_tile_background(
        &self,
        image: &Image,
        run: &Run,
        style: Style,
    ) -> Option<u32> {
        if !self.device.streamed_uploads()
            || self.small_canvas_edge == 0
            || !image.canvas
            || style.face != DrawFace::Alpha
            || style.opacity != 255
            || run.glyphs.is_empty()
            || run.glyphs.len() > 2
            || image.size.rgba_bytes()? > 64 * 64 * 4
            || image.size.width > self.tile_edge.min(256)
            || image.size.height > self.tile_edge.min(256)
            || self.fill_main_size(image) != image.size
        {
            return None;
        }
        self.solid_images.borrow().color(image)
    }

    pub(crate) fn text_tile_write_bytes(
        &self,
        image: &Image,
        run: &Run,
        clip: Rect,
        color: u32,
    ) -> usize {
        let key = Key::new(image, run, clip, color);
        if self.text_tiles.borrow_mut().get(&key).is_some() {
            0
        } else {
            image
                .size
                .rgba_bytes()
                .unwrap()
                .saturating_add(Cache::entry_bytes())
        }
    }

    pub(crate) fn draw_text_tile(
        &self,
        image: &mut Image,
        run: &Run,
        clip: Rect,
        color: u32,
    ) -> Result<()> {
        if !run.glyphs.iter().any(|placed| {
            (Rect {
                left: placed.x.saturating_add(placed.glyph.origin[0]),
                top: placed.y.saturating_add(placed.glyph.origin[1]),
                ..placed.glyph.size.rect()
            })
            .intersection(clip)
            .is_some()
        }) {
            return Ok(());
        }
        let key = Key::new(image, run, clip, color);
        if let Some(plane) = self.text_tiles.borrow_mut().get(&key) {
            let _profile = krkr_protocol::profile::span("gpu.text.tile_reuse");
            image.main = Some(plane);
            return Ok(());
        }
        let size = image.size;
        let mut bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &self.staging)?;
        let background = [
            (color >> 16) as u8,
            (color >> 8) as u8,
            color as u8,
            (color >> 24) as u8,
        ];
        bytes.as_mut_slice().as_chunks_mut::<4>().0.fill(background);
        for placed in &run.glyphs {
            let glyph = &placed.glyph;
            let left = placed.x.saturating_add(glyph.origin[0]);
            let top = placed.y.saturating_add(glyph.origin[1]);
            let Some(area) = (Rect {
                left,
                top,
                ..glyph.size.rect()
            })
            .intersection(clip) else {
                continue;
            };
            if !matches!(glyph.levels, 65 | 256)
                || glyph.size.rgba_bytes().map(|n| n / 4) != Some(glyph.mask.as_slice().len())
            {
                return Err(Error::Message("invalid glyph mask"));
            }
            let color = [
                (placed.color >> 16) as u8,
                (placed.color >> 8) as u8,
                placed.color as u8,
            ];
            for y in area.top..area.top + area.height as i32 {
                for x in area.left..area.left + area.width as i32 {
                    let mask = glyph.mask.as_slice()
                        [(y - top) as usize * glyph.size.width as usize + (x - left) as usize];
                    let offset = (y as usize * size.width as usize + x as usize) * 4;
                    let pixel = &mut bytes.as_mut_slice()[offset..offset + 4];
                    let ratio = i32::from(krkr_render::blend::opacity_factor(
                        pixel[3],
                        mask,
                        glyph.levels,
                    ));
                    for channel in 0..3 {
                        let old = i32::from(pixel[channel]);
                        pixel[channel] =
                            (old + (((i32::from(color[channel]) - old) * ratio) >> 8)) as u8;
                    }
                    let alpha = if glyph.levels == 65 {
                        (u32::from(mask) * 4).min(255)
                    } else {
                        u32::from(mask)
                    };
                    pixel[3] = (255 - (255 - u32::from(pixel[3])) * (255 - alpha) / 255) as u8;
                }
            }
        }
        // Always detach: a published scene or another layer can still own the
        // blank plane. Commit only after the complete upload succeeds.
        let budget = image.plane(false)?.budget.clone();
        let texture = self.device.sample_texture(size, &budget)?;
        self.device
            .upload_owned_region(&texture, size.rect(), bytes)?;
        image.main = Some(Rc::new(Plane {
            size,
            budget,
            tiles: vec![Tile {
                rectangle: size.rect(),
                backing: None,
                texture,
            }],
        }));
        self.text_tiles
            .borrow_mut()
            .insert(key, image, &self.resident);
        Ok(())
    }
}
