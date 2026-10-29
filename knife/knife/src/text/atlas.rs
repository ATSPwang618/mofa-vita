//! Two cached glyph pages. Submitted quads retain their own texture references.

use std::collections::HashMap;

pub(super) const VITA_PAGE_SIZE: u32 = 1024;
pub(super) const PAGE_SIZE: u32 = if cfg!(target_os = "vita") { VITA_PAGE_SIZE } else { 2048 };
const PAD: u32 = 2;

pub(super) struct GlyphAtlas<T, G> {
    pub texture: T,
    pub glyphs: HashMap<char, G>,
    previous: Option<(T, HashMap<char, G>)>,
    size: u32,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
}

impl<T, G> GlyphAtlas<T, G> {
    pub fn new(texture: T, size: u32) -> Self {
        Self { texture, glyphs: HashMap::new(), previous: None, size, shelf_x: 0, shelf_y: 0, shelf_h: 0 }
    }

    pub fn size(&self) -> u32 { self.size }

    pub fn get(&self, c: char) -> Option<(&T, &G)> {
        if let Some(glyph) = self.glyphs.get(&c) {
            return Some((&self.texture, glyph));
        }
        self.previous.as_ref().and_then(|(texture, glyphs)|
            glyphs.get(&c).map(|glyph| (texture, glyph)))
    }

    /// Reserve only a complete glyph. The factory creates a blank texture with
    /// the same dimensions when a new page is required.
    pub fn place(&mut self, width: u32, height: u32, new_page: impl FnOnce(u32) -> T) -> Option<[u32; 2]> {
        let padded_w = width.checked_add(PAD)?;
        let padded_h = height.checked_add(PAD)?;
        if width == 0 || height == 0 || padded_w > self.size || padded_h > self.size { return None; }
        if self.shelf_x + padded_w > self.size {
            self.shelf_x = 0;
            self.shelf_y += self.shelf_h + PAD;
            self.shelf_h = 0;
        }
        if self.shelf_y + padded_h > self.size {
            // Never overwrite a page referenced by an earlier queued glyph.
            // Its draw owns a texture clone until submission. Keep just the
            // current and previous page for reuse across nearby text pages.
            let texture = new_page(self.size);
            let previous = std::mem::replace(&mut self.texture, texture);
            self.previous = Some((previous, std::mem::take(&mut self.glyphs)));
            self.shelf_x = 0;
            self.shelf_y = 0;
            self.shelf_h = 0;
        }
        let origin = [self.shelf_x, self.shelf_y];
        self.shelf_x += padded_w;
        self.shelf_h = self.shelf_h.max(height);
        Some(origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::{Cell, RefCell}, rc::Rc};

    #[test]
    fn vita_page_leaves_room_for_scene_and_menu_resources() {
        // Each full R8 page occupies exactly 1 MiB in 256 KiB native blocks.
        assert_eq!(VITA_PAGE_SIZE * VITA_PAGE_SIZE, 1024 * 1024);
    }

    #[test]
    fn rollover_preserves_queued_glyph_pixels_and_discards_old_coordinates() {
        let first = Rc::new(RefCell::new(vec![0u8; 64]));
        let mut atlas = GlyphAtlas::new(first.clone(), 8);
        assert_eq!(atlas.place(6, 6, |_| panic!("first glyph fits")), Some([0, 0]));
        atlas.texture.borrow_mut()[0] = 17;
        atlas.glyphs.insert('a', [0, 0]);
        let queued_texture = atlas.texture.clone();
        let new_pages = Cell::new(0);
        assert_eq!(atlas.place(6, 6, |size| {
            new_pages.set(new_pages.get() + 1);
            Rc::new(RefCell::new(vec![0; (size * size) as usize]))
        }), Some([0, 0]));
        atlas.texture.borrow_mut()[0] = 93;
        assert_eq!(new_pages.get(), 1, "a full page must allocate new storage");
        assert!(!Rc::ptr_eq(&queued_texture, &atlas.texture));
        assert_eq!(queued_texture.borrow()[0], 17, "queued text must retain its glyph");
        assert!(atlas.glyphs.is_empty(), "old coordinates cannot index the new page");
        atlas.glyphs.insert('b', [0, 0]);
        assert_eq!(atlas.glyphs.len(), 1);
        drop(atlas);
        assert_eq!(Rc::strong_count(&first), 2, "only the caller and queued draw retain the old page");
    }

    #[test]
    fn cached_glyph_keeps_its_own_page_and_history_is_bounded() {
        let mut atlas = GlyphAtlas::<_, [u32; 2]>::new(1, 8);
        atlas.place(6, 6, |_| unreachable!()).unwrap();
        atlas.glyphs.insert('a', [0, 0]);
        atlas.place(6, 6, |_| 2).unwrap();
        atlas.glyphs.insert('b', [0, 0]);
        assert_eq!(atlas.get('a'), Some((&1, &[0, 0])));
        assert_eq!(atlas.get('b'), Some((&2, &[0, 0])));
        atlas.place(6, 6, |_| 3).unwrap();
        atlas.glyphs.insert('c', [0, 0]);
        assert_eq!(atlas.get('a'), None, "two-page cache retires the oldest coordinates");
        assert_eq!(atlas.get('b'), Some((&2, &[0, 0])));
        assert_eq!(atlas.get('c'), Some((&3, &[0, 0])));
    }

    #[test]
    fn rows_fit_without_overlap_and_invalid_glyph_does_not_rotate() {
        let mut atlas = GlyphAtlas::<_, [u32; 2]>::new(7, 16);
        let unexpected = |_| panic!("page must not rotate");
        assert_eq!(atlas.place(6, 4, unexpected), Some([0, 0]));
        assert_eq!(atlas.place(6, 4, unexpected), Some([8, 0]));
        assert_eq!(atlas.place(6, 4, unexpected), Some([0, 6]));
        atlas.glyphs.insert('x', [0, 6]);
        for [w, h] in [[15, 1], [1, 15], [0, 2], [2, 0], [u32::MAX, 1]] {
            assert_eq!(atlas.place(w, h, unexpected), None);
        }
        assert_eq!(atlas.texture, 7);
        assert_eq!(atlas.glyphs.get(&'x'), Some(&[0, 6]));
        assert_eq!(atlas.place(6, 4, unexpected), Some([8, 6]));
    }
}
