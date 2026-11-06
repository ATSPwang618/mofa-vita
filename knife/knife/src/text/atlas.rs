//! Desktop rollover atlas and Vita's bounded immutable glyph-page pool.

use std::{cell::Cell, collections::{HashMap, HashSet}, rc::Rc};

pub(super) const VITA_PAGE_SIZE: u32 = 256;
pub(super) const VITA_POOL_BYTES: usize = 2 * 1024 * 1024;
pub(super) const VITA_POOL_PAGES: usize = 128;
pub(super) const PAGE_SIZE: u32 = if cfg!(target_os = "vita") { VITA_PAGE_SIZE } else { 2048 };
const PAD: u32 = 2;

#[derive(Clone)]
pub(super) struct GlyphAtlas<T, G> {
    pub texture: T,
    pub glyphs: HashMap<char, G>,
    previous: Option<(T, HashMap<char, G>)>,
    size: u32,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
}

/// CPU-only glyph data. Positioning and publication are separate so a sampled
/// texture is never changed while a message page is being prepared.
pub(super) struct R8Glyph<G> {
    pub glyph: G,
    pub size: [u32; 2],
    pub pixels: Vec<u8>,
}

pub(super) struct PreparedR8<T, G> {
    pages: Vec<PendingPage<T, G>>,
    blanks: Vec<char>,
    victims: Vec<usize>,
    pub rasterized: usize,
}

struct PendingPage<T, G> { texture: T, glyphs: HashMap<char, G>, bytes: usize }
struct CachedGlyph<T, G> { texture: T, glyph: G, used: Rc<Cell<u64>> }
struct CachedPage { chars: Vec<char>, bytes: usize, used: Rc<Cell<u64>> }

/// A page is uploaded once and never appended to, renamed or overwritten.
/// Glyph quads keep their own owners when the bounded lookup pool evicts a page.
pub(super) struct ImmutableAtlas<T, G> {
    glyphs: HashMap<char, CachedGlyph<T, G>>,
    blanks: HashSet<char>,
    pages: Vec<CachedPage>,
    clock: Cell<u64>,
    size: u32,
    max_bytes: usize,
    max_pages: usize,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct Evicted { pub pages: usize, pub bytes: usize }

#[derive(Debug)]
pub(super) enum PrepareError<E> {
    Allocation,
    Capacity,
    Raster(E),
    Upload(E),
}

impl<T: Clone, G> ImmutableAtlas<T, G> {
    pub fn new(size: u32, max_bytes: usize, max_pages: usize) -> Self {
        Self { glyphs: HashMap::new(), blanks: HashSet::new(), pages: Vec::new(),
            clock: Cell::new(0), size, max_bytes, max_pages, bytes: 0 }
    }
    pub fn bytes(&self) -> usize { self.bytes }
    pub fn pages(&self) -> usize { self.pages.len() }
    pub fn contains(&self, c: char) -> bool { self.blanks.contains(&c) || self.get(c).is_some() }
    pub fn get(&self, c: char) -> Option<(&T, &G)> {
        let glyph = self.glyphs.get(&c)?;
        let next = self.clock.get().wrapping_add(1);
        self.clock.set(next);
        glyph.used.set(next);
        Some((&glyph.texture, &glyph.glyph))
    }
    fn protected(&self, chars: &[char]) -> HashSet<usize> {
        chars.iter().filter_map(|c| self.glyphs.get(c))
            .map(|g| Rc::as_ptr(&g.used) as usize).collect()
    }
    fn admission(&self, chars: &[char], new_bytes: usize, new_pages: usize) -> Option<Vec<usize>> {
        let protected = self.protected(chars);
        let (mut bytes, mut pages) = (self.bytes.checked_add(new_bytes)?, self.pages.len().checked_add(new_pages)?);
        let mut candidates: Vec<usize> = (0..self.pages.len())
            .filter(|&i| !protected.contains(&(Rc::as_ptr(&self.pages[i].used) as usize))).collect();
        candidates.sort_by_key(|&i| self.pages[i].used.get());
        let mut victims = Vec::new();
        for i in candidates {
            if bytes <= self.max_bytes && pages <= self.max_pages { break; }
            bytes -= self.pages[i].bytes;
            pages -= 1;
            victims.push(i);
        }
        (bytes <= self.max_bytes && pages <= self.max_pages).then_some(victims)
    }
    fn evict(&mut self, mut victims: Vec<usize>) -> Evicted {
        victims.sort_unstable_by(|a, b| b.cmp(a));
        let mut evicted = Evicted::default();
        for i in victims {
            let page = self.pages.remove(i);
            for c in page.chars { self.glyphs.remove(&c); }
            self.bytes -= page.bytes;
            evicted.pages += 1;
            evicted.bytes += page.bytes;
        }
        evicted
    }
    /// One bounded pressure retry may release unrequested cached owners. Queued
    /// quads still own their immutable textures; no native Finish is requested.
    pub fn trim_unrequested(&mut self, chars: &[char]) -> Evicted {
        let protected = self.protected(chars);
        let victims = (0..self.pages.len())
            .filter(|&i| !protected.contains(&(Rc::as_ptr(&self.pages[i].used) as usize))).collect();
        self.evict(victims)
    }
    pub fn commit(&mut self, prepared: PreparedR8<T, G>) -> (usize, Evicted) {
        let evicted = self.evict(prepared.victims);
        if self.blanks.len() + prepared.blanks.len() > 1024 { self.blanks.clear(); }
        self.blanks.extend(prepared.blanks);
        for page in prepared.pages {
            let used = Rc::new(Cell::new(self.clock.get()));
            let mut chars = Vec::with_capacity(page.glyphs.len());
            for (c, glyph) in page.glyphs {
                chars.push(c);
                self.glyphs.insert(c, CachedGlyph { texture: page.texture.clone(), glyph, used: used.clone() });
            }
            self.bytes += page.bytes;
            self.pages.push(CachedPage { chars, bytes: page.bytes, used });
        }
        (prepared.rasterized, evicted)
    }
    /// Oversized base glyphs and pressure fallback get an independent immutable
    /// texture; they must never index the normal page with oversized UVs.
    pub fn insert_page(&mut self, c: char, texture: T, glyph: G, bytes: usize) -> Result<Evicted, ()> {
        let victims = self.admission(&[c], bytes, 1).ok_or(())?;
        let mut glyphs = HashMap::new();
        glyphs.insert(c, glyph);
        let (_, evicted) = self.commit(PreparedR8 { pages: vec![PendingPage { texture, glyphs, bytes }],
            blanks: Vec::new(), victims, rasterized: 1 });
        Ok(evicted)
    }
    /// Prepare only missing glyphs into zeroed private scratch. Upload just the
    /// used rows (with transparent guard rows), never copy an old page. Failed
    /// upload/admission leaves old lookup maps and all sampled pixels intact.
    pub fn prepare_r8<E>(
        &self,
        chars: &[char],
        mut raster: impl FnMut(char) -> Result<Option<R8Glyph<G>>, E>,
        mut position: impl FnMut(G, [u32; 2]) -> G,
        mut upload: impl FnMut([u32; 2], &[u8]) -> Result<(T, usize), E>,
    ) -> Result<Option<PreparedR8<T, G>>, PrepareError<E>> {
        if chars.iter().all(|&c| self.contains(c)) { return Ok(None); }
        let bytes = (self.size as usize).checked_mul(self.size as usize).ok_or(PrepareError::Capacity)?;
        if chars.len() > 1024 || self.size < 8 { return Err(PrepareError::Capacity); }
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(bytes).map_err(|_| PrepareError::Allocation)?;
        pixels.resize(bytes, 0);
        let mut shelf = GlyphAtlas::<_, ()>::new((), self.size);
        let (mut pages, mut glyphs, mut blanks) = (Vec::new(), HashMap::new(), Vec::new());
        let mut seen = HashSet::new();
        let (mut rasterized, mut new_bytes) = (0, 0usize);
        let mut publish = |shelf: &GlyphAtlas<(), ()>, pixels: &[u8], glyphs: &mut HashMap<char, G>| {
            let height = (shelf.shelf_y + shelf.shelf_h + PAD + 7) & !7;
            let height = height.min(self.size);
            let (texture, charge) = upload([self.size, height], &pixels[..(self.size * height) as usize])
                .map_err(PrepareError::Upload)?;
            new_bytes = new_bytes.checked_add(charge).ok_or(PrepareError::Capacity)?;
            if self.admission(chars, new_bytes, pages.len() + 1).is_none() { return Err(PrepareError::Capacity); }
            pages.push(PendingPage { texture, glyphs: std::mem::take(glyphs), bytes: charge });
            Ok(())
        };
        for &c in chars {
            if self.contains(c) || !seen.insert(c) { continue; }
            let Some(bitmap) = raster(c).map_err(PrepareError::Raster)? else {
                blanks.push(c);
                continue;
            };
            rasterized += 1;
            let [w, h] = bitmap.size;
            if w == 0 || h == 0 || w.checked_add(PAD).is_none_or(|v| v > self.size)
                || h.checked_add(PAD).is_none_or(|v| v > self.size)
                || (w as usize).checked_mul(h as usize) != Some(bitmap.pixels.len()) {
                return Err(PrepareError::Capacity);
            }
            if !shelf.fits(w, h) {
                publish(&shelf, &pixels, &mut glyphs)?;
                shelf = GlyphAtlas::new((), self.size);
                pixels.fill(0);
            }
            let origin = shelf.place(w, h, |_| unreachable!("fresh immutable scratch page"))
                .ok_or(PrepareError::Capacity)?;
            for y in 0..h as usize {
                let start = (origin[1] as usize + y) * self.size as usize + origin[0] as usize;
                pixels[start..start + w as usize]
                    .copy_from_slice(&bitmap.pixels[y * w as usize..(y + 1) * w as usize]);
            }
            glyphs.insert(c, position(bitmap.glyph, origin));
        }
        if !glyphs.is_empty() { publish(&shelf, &pixels, &mut glyphs)?; }
        drop(publish);
        let victims = self.admission(chars, new_bytes, pages.len()).ok_or(PrepareError::Capacity)?;
        Ok(Some(PreparedR8 { pages, blanks, victims, rasterized }))
    }
}

impl<T, G> GlyphAtlas<T, G> {
    pub fn new(texture: T, size: u32) -> Self {
        Self { texture, glyphs: HashMap::new(), previous: None, size, shelf_x: 0, shelf_y: 0, shelf_h: 0 }
    }

    /// Same shelf admission as place(), without changing coordinates or owners.
    pub fn fits(&self, width: u32, height: u32) -> bool {
        let Some(w) = width.checked_add(PAD) else { return false };
        let Some(h) = height.checked_add(PAD) else { return false };
        if width == 0 || height == 0 || w > self.size || h > self.size { return false; }
        let y = if self.shelf_x + w > self.size { self.shelf_y + self.shelf_h + PAD }
            else { self.shelf_y };
        y + h <= self.size
    }

    pub fn rotate(&mut self, texture: T) {
        let previous = std::mem::replace(&mut self.texture, texture);
        self.previous = Some((previous, std::mem::take(&mut self.glyphs)));
        self.shelf_x = 0;
        self.shelf_y = 0;
        self.shelf_h = 0;
    }

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
            self.rotate(new_page(self.size));
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
        assert_eq!(VITA_PAGE_SIZE * VITA_PAGE_SIZE, 64 * 1024);
        assert_eq!(VITA_POOL_BYTES, 2 * 1024 * 1024);
        assert_eq!(VITA_POOL_PAGES, 128);
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

    fn bitmap(c: char, size: u32) -> Result<Option<R8Glyph<[u32; 2]>>, &'static str> {
        Ok(Some(R8Glyph { glyph: [0, 0], size: [size, size], pixels: vec![c as u8; (size * size) as usize] }))
    }

    #[derive(Debug)]
    struct Texture { size: [u32; 2], pixels: Vec<u8> }
    fn upload(size: [u32; 2], pixels: &[u8]) -> Result<(Rc<Texture>, usize), &'static str> {
        Ok((Rc::new(Texture { size, pixels: pixels.to_vec() }), pixels.len()))
    }
    fn prepare(atlas: &ImmutableAtlas<Rc<Texture>, [u32; 2]>, chars: &[char], px: u32)
        -> PreparedR8<Rc<Texture>, [u32; 2]>
    {
        atlas.prepare_r8(chars, |c| bitmap(c, px), |_, p| p, upload).unwrap().unwrap()
    }

    #[test]
    fn new_batch_uploads_only_used_rows_and_reveal_reuses_immutable_pages() {
        let mut atlas = ImmutableAtlas::new(256, VITA_POOL_BYTES, VITA_POOL_PAGES);
        let page = prepare(&atlas, &['a', 'b', 'c', 'd', 'e'], 40);
        assert_eq!(page.rasterized, 5);
        assert_eq!(page.pages.len(), 1);
        assert_eq!(page.pages[0].texture.size, [256, 48]);
        assert_eq!(page.pages[0].texture.pixels.len(), 12_288, "not a1MiB upload");
        atlas.commit(page);
        for prefix in [&['a'][..], &['a', 'b'][..], &['a', 'b', 'c', 'd', 'e'][..]] {
            let hit = atlas.prepare_r8::<&str>(prefix, |_| panic!("reveal cannot raster"),
                |_, _| unreachable!(), |_, _| panic!("reveal cannot upload")).unwrap();
            assert!(hit.is_none());
        }
        for c in ['a', 'b', 'c', 'd', 'e'] {
            let (texture, &[x, y]) = atlas.get(c).unwrap();
            assert_eq!(texture.pixels[(y * texture.size[0] + x) as usize], c as u8);
            assert!(texture.pixels[40 * 256..].iter().all(|&v| v == 0), "cropped guard rows remain transparent");
        }
    }

    #[test]
    fn new_message_never_reuploads_or_changes_existing_glyph_owners() {
        let mut atlas = ImmutableAtlas::new(256, VITA_POOL_BYTES, VITA_POOL_PAGES);
        atlas.commit(prepare(&atlas, &['a'], 40));
        let queued = atlas.get('a').unwrap().0.clone();
        let old_position = *atlas.get('a').unwrap().1;
        let next = prepare(&atlas, &['a', 'b', 'c'], 40);
        assert_eq!(next.rasterized, 2);
        atlas.commit(next);
        assert!(Rc::ptr_eq(&queued, atlas.get('a').unwrap().0), "cached text RT source identity stays stable");
        assert_eq!(*atlas.get('a').unwrap().1, old_position);
        assert!(!Rc::ptr_eq(&queued, atlas.get('b').unwrap().0));
        assert_eq!(queued.pixels[0], b'a');
        assert_eq!(queued.pixels[42], 0, "no append writes into an already published page");
    }

    #[test]
    fn eviction_keeps_requested_pages_and_queued_owners_alive() {
        let mut atlas = ImmutableAtlas::new(8, 128, 2);
        atlas.commit(prepare(&atlas, &['a'], 6));
        let queued_a = atlas.get('a').unwrap().0.clone();
        atlas.commit(prepare(&atlas, &['b'], 6));
        let (count, evicted) = atlas.commit(prepare(&atlas, &['b', 'c'], 6));
        assert_eq!((count, evicted.pages, evicted.bytes), (1, 1, 64));
        assert!(!atlas.contains('a'));
        assert!(atlas.contains('b') && atlas.contains('c'));
        assert_eq!(queued_a.pixels[0], b'a', "eviction removes lookup only; queued quad still owns pixels");
        assert_eq!(atlas.pages(), 2);
        assert_eq!(atlas.bytes(), 128);
    }

    #[test]
    fn partial_upload_and_raster_failures_do_not_publish_new_coordinates() {
        let mut atlas = ImmutableAtlas::new(8, 256, 4);
        atlas.commit(prepare(&atlas, &['a'], 6));
        let owner = atlas.get('a').unwrap().0.clone();
        let calls = Cell::new(0);
        let failed = atlas.prepare_r8(&['b', 'c'], |c| bitmap(c, 6), |_, p| p,
            |size, pixels| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 { Err("allocation rejected") } else { upload(size, pixels) }
            });
        assert!(matches!(failed, Err(PrepareError::Upload("allocation rejected"))));
        let raster_failed = atlas.prepare_r8(&['z'], |_| Err("bad outline"), |_, p| p, upload);
        assert!(matches!(raster_failed, Err(PrepareError::Raster("bad outline"))));
        assert!(Rc::ptr_eq(&owner, atlas.get('a').unwrap().0));
        assert!(!atlas.contains('b') && !atlas.contains('c'));
        assert_eq!(atlas.bytes(), 64);
    }

    #[test]
    fn admission_uses_native_charge_and_blank_glyphs_need_no_texture() {
        let mut atlas = ImmutableAtlas::new(8, 256, 128);
        let charged = |size, pixels: &[u8]| upload(size, pixels).map(|(texture, _)| (texture, 256));
        let page = atlas.prepare_r8(&['a'], |c| bitmap(c, 6), |_, p| p, charged).unwrap().unwrap();
        atlas.commit(page);
        let blocked = atlas.prepare_r8(&['a', 'b'], |c| bitmap(c, 6), |_, p| p, charged);
        assert!(matches!(blocked, Err(PrepareError::Capacity)), "two64B payloads still need512B native charge");
        assert_eq!(atlas.bytes(), 256);
        let replacement = atlas.prepare_r8(&['b'], |c| bitmap(c, 6), |_, p| p, charged).unwrap().unwrap();
        atlas.commit(replacement);
        assert!(!atlas.contains('a') && atlas.contains('b'));
        let blank = atlas.prepare_r8::<&str>(&[' '], |_| Ok(None), |_, p| p,
            |_, _| panic!("space must not upload")).unwrap().unwrap();
        atlas.commit(blank);
        assert!(atlas.contains(' '));
        assert!(atlas.get(' ').is_none());
    }

    #[test]
    fn oversized_glyph_fallback_has_independent_texture_geometry() {
        let mut atlas = ImmutableAtlas::<_, [u32; 2]>::new(256, VITA_POOL_BYTES, VITA_POOL_PAGES);
        let pixels = vec![93; 304 * 48];
        let (texture, charge) = upload([304, 48], &pixels).unwrap();
        atlas.insert_page('W', texture.clone(), [0, 0], charge).unwrap();
        let (actual, pos) = atlas.get('W').unwrap();
        assert!(Rc::ptr_eq(actual, &texture));
        assert_eq!(actual.size, [304, 48], "UV extent must use this real width and height, not256");
        assert_eq!(*pos, [0, 0]);
    }

    #[test]
    fn synthetic_200_short_messages_have_bounded_pool_and_small_payloads() {
        let mut atlas = ImmutableAtlas::new(256, VITA_POOL_BYTES, VITA_POOL_PAGES);
        let mut uploaded = 0;
        let mut queued = Vec::new();
        for batch in 0..200u32 {
            let chars: Vec<char> = (0..5).map(|i| char::from_u32(0x4e00 + batch * 5 + i).unwrap()).collect();
            let page = prepare(&atlas, &chars, 40);
            uploaded += page.pages.iter().map(|page| page.texture.pixels.len()).sum::<usize>();
            atlas.commit(page);
            queued.push(atlas.get(chars[0]).unwrap().0.clone());
            assert!(atlas.pages() <= VITA_POOL_PAGES);
            assert!(atlas.bytes() <= VITA_POOL_BYTES);
        }
        assert_eq!(uploaded, 2_457_600);
        assert_eq!(200 * 1024 * 1024 - uploaded, 207_257_600);
        assert_eq!(queued[0].pixels[0], 0x4e00u32 as u8, "even oldest queued owner survives eviction");
    }
}
