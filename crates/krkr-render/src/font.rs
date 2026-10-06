//! Portable font metrics and cached masks. Called by the engine's IO worker;
//! the host supplies system-font discovery, and renderers consume owned masks.
pub mod bundled;
mod outline;
pub mod prerendered;
mod private;
pub use private::Registration;
mod keys;
mod shadow;
use crate::{
    Error, Rect, Result, Size,
    budget::{Budget, Permit},
};
use ab_glyph::Font as _;
pub use ab_glyph::{FontArc, FontVec};
use keys::{FaceLookup, GlyphKey, Lookup};
use krkr_protocol::{
    pixels::Bytes,
    text::{Font, Glyph, Metrics, PlacedGlyph, Run, Style},
};
use lru::LruCache;
use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

static NEXT_GLYPH: AtomicU64 = AtomicU64::new(1);
pub(super) fn glyph_id() -> u64 {
    NEXT_GLYPH.fetch_add(1, Ordering::Relaxed)
}
pub(super) fn cancelled(flag: &AtomicBool) -> Result<()> {
    if flag.load(Ordering::Relaxed) {
        Err(Error::Message("font operation cancelled"))
    } else {
        Ok(())
    }
}
pub struct Face {
    pub font: FontArc,
    /// Unscaled typographic ascent, matching the native OTM baseline rather
    /// than the larger Windows clipping extent or hhea line metrics.
    pub ascent: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: [f32; 2],
    pub coverage: Coverage,
    pub permit: Permit,
}
/// Portable enumeration metadata. Character coverage approximates the native
/// charset filter; it does not expose Windows charset numbers to the engine.
#[derive(Clone, Copy)]
pub struct Coverage {
    ranges: u128,
    symbol: bool,
    outline: bool,
    monospaced: bool,
}
impl Coverage {
    pub fn from_face(face: &ttf_parser::Face<'_>) -> Self {
        let tables = face.tables();
        Self {
            ranges: tables.os2.map_or(0, |os2| os2.unicode_ranges().0),
            symbol: tables.cmap.is_some_and(|cmap| {
                cmap.subtables
                    .into_iter()
                    .any(|s| s.platform_id == ttf_parser::PlatformId::Windows && s.encoding_id == 0)
            }),
            outline: tables.glyf.is_some() || tables.cff.is_some() || tables.cff2.is_some(),
            monospaced: face.is_monospaced(),
        }
    }
    pub fn matches(self, flags: u32, selected: Option<Self>) -> bool {
        (flags & 1 == 0 || self.monospaced)
            && (flags & 8 == 0 || self.outline)
            && (flags & 16 == 0 || !self.symbol)
            && (flags & 2 == 0
                || selected
                    .is_none_or(|s| self.symbol == s.symbol && self.ranges & s.ranges == s.ranges))
    }
}
impl Face {
    fn ascent(metadata: &ttf_parser::Face<'_>) -> f32 {
        metadata
            .typographic_ascender()
            .filter(|&v| v > 0)
            .unwrap_or_else(|| metadata.ascender())
            .into()
    }
    pub fn from_bytes(data: Vec<u8>, index: u32, permit: Permit) -> Result<Self> {
        let metadata = ttf_parser::Face::parse(&data, index)
            .map_err(|_| Error::Message("invalid OpenType/TrueType font"))?;
        let units = f32::from(metadata.units_per_em());
        let underline = metadata
            .underline_metrics()
            .map_or([-units * 0.1, units * 0.05], |m| {
                [f32::from(m.position), f32::from(m.thickness)]
            });
        let (bold, italic) = (metadata.is_bold(), metadata.is_italic());
        let coverage = Coverage::from_face(&metadata);
        let ascent = Self::ascent(&metadata);
        let font = FontVec::try_from_vec_and_index(data, index)
            .map_err(|_| Error::Message("invalid OpenType/TrueType font"))?;
        Ok(Self {
            font: FontArc::new(font),
            ascent,
            bold,
            italic,
            underline,
            coverage,
            permit,
        })
    }
}
pub trait Provider: Send {
    /// Load one explicit family. None means absent; IO/parse/budget failures
    /// remain errors. Family candidates and fallback belong to the shared service.
    fn load(&mut self, font: &Font, budget: &Budget) -> Result<Option<Face>>;
    fn list(&mut self, flags: u32, selected: Option<Coverage>) -> Result<Vec<String>>;
}
#[derive(PartialEq, Eq, Clone)]
struct FaceKey {
    name: String,
    bold: bool,
    italic: bool,
    file: bool,
}
impl FaceKey {
    fn query(font: &Font) -> keys::FaceFields<'_> {
        (
            &font.face,
            !font.file && font.bold,
            !font.file && font.italic,
            font.file,
        )
    }
}
impl From<&Font> for FaceKey {
    fn from(f: &Font) -> Self {
        Self {
            name: f.face.clone(),
            bold: !f.file && f.bold,
            italic: !f.file && f.italic,
            file: f.file,
        }
    }
}
struct Measurement {
    advance: i32,
    bounds: Option<Rect>,
    _permit: Permit,
}
pub struct System {
    pub budget: Budget,
    provider: Option<Box<dyn Provider>>,
    faces: LruCache<FaceKey, Arc<Face>>,
    glyphs: LruCache<GlyphKey, Arc<Glyph>>,
    measurements: LruCache<GlyphKey, Measurement>,
    mapped: HashMap<Font, Arc<prerendered::Font>>,
    private: HashMap<String, Arc<private::Entry>>,
    rasterized: u64,
}
impl Default for System {
    fn default() -> Self {
        Self::new(Budget::new(24 * 1024 * 1024))
    }
}
impl System {
    pub fn new(budget: Budget) -> Self {
        Self {
            budget,
            provider: None,
            faces: LruCache::new(NonZeroUsize::new(16).unwrap()),
            glyphs: LruCache::new(NonZeroUsize::new(4096).unwrap()),
            measurements: LruCache::new(NonZeroUsize::new(512).unwrap()),
            mapped: HashMap::new(),
            private: HashMap::new(),
            rasterized: 0,
        }
    }
    pub fn set_provider(&mut self, provider: Box<dyn Provider>) {
        self.faces.clear();
        self.glyphs.clear();
        self.measurements.clear();
        self.provider = Some(provider);
    }
    pub fn rasterized(&self) -> u64 {
        self.rasterized
    }
    pub fn has_face(&self, font: &Font) -> bool {
        self.faces
            .contains(&FaceKey::query(font) as &dyn FaceLookup)
    }
    pub fn insert_face(&mut self, font: &Font, face: Face) {
        self.faces.put(FaceKey::from(font), Arc::new(face));
        self.glyphs.clear();
        self.measurements.clear();
    }
    pub fn reserve(&mut self, bytes: usize) -> Result<Permit> {
        loop {
            match self.budget.reserve(bytes) {
                Ok(permit) => return Ok(permit),
                Err(e) => {
                    if self.measurements.pop_lru().is_none()
                        && self.glyphs.pop_lru().is_none()
                        && self.faces.pop_lru().is_none()
                    {
                        return Err(e.into());
                    }
                }
            }
        }
    }
    pub fn face(&mut self, font: &Font) -> Result<Arc<Face>> {
        if let Some(face) = self.faces.get(&FaceKey::query(font) as &dyn FaceLookup) {
            return Ok(face.clone());
        }
        if font.file {
            return Err(Error::Message("font file has not been loaded"));
        }
        let mut requested = font.clone();
        let mut selected = None;
        let mut previous_empty = false;
        for name in font.face.strip_prefix('@').unwrap_or(&font.face).split(',') {
            let name = name.trim();
            if name.is_empty() {
                previous_empty = true;
                continue;
            }
            if let Some(entry) = self.private.get(name) {
                selected = Some(entry.face.clone());
                break;
            }
            if bundled::is_family(name) {
                selected = Some(bundled::face());
                break;
            }
            requested.face = name.to_owned();
            if let Some(face) = self.load_system_face(&requested)? {
                selected = Some(Arc::new(face));
                break;
            }
            // FontSystem::GetBeingFont forces the candidate following an empty
            // entry. If the host cannot resolve it, use the portable default.
            if previous_empty {
                break;
            }
        }
        let face = selected.unwrap_or_else(bundled::face);
        // A family reloaded after eviction may resolve to a different face.
        self.measurements.clear();
        self.faces.put(FaceKey::from(font), face.clone());
        Ok(face)
    }
    fn load_system_face(&mut self, font: &Font) -> Result<Option<Face>> {
        let Some(mut provider) = self.provider.take() else {
            return Ok(None);
        };
        let result = loop {
            match provider.load(font, &self.budget) {
                Err(Error::Budget(_))
                    if !self.measurements.is_empty()
                        || !self.glyphs.is_empty()
                        || !self.faces.is_empty() =>
                {
                    if self.measurements.pop_lru().is_none() && self.glyphs.pop_lru().is_none() {
                        self.faces.pop_lru();
                    }
                }
                result => break result,
            }
        };
        self.provider = Some(provider);
        result
    }
    pub fn list(&mut self, font: &Font, flags: u32) -> Result<Vec<String>> {
        let selected = if flags & 2 != 0 {
            // Enumeration uses the family name, as the native getList does;
            // it never opens a VFS file solely to enumerate system fonts.
            let mut family = font.clone();
            family.file = false;
            Some(self.face(&family)?.coverage)
        } else {
            None
        };
        let mut list = match self.provider.as_mut() {
            Some(provider) => provider.list(flags, selected)?,
            None => Vec::new(),
        };
        list.extend(
            self.private
                .iter()
                .filter(|(_, entry)| entry.face.coverage.matches(flags, selected))
                .map(|(name, _)| name.clone()),
        );
        if bundled::face().coverage.matches(flags, selected) {
            list.push(krkr_protocol::text::DEFAULT_FONT_FACE.to_owned());
        }
        list.sort_unstable();
        list.dedup();
        Ok(list)
    }
    pub fn map(&mut self, font: Font, data: prerendered::Font) {
        self.set_mapping(font, Some(Arc::new(data)));
    }
    pub fn unmap(&mut self, font: &Font) {
        if self.mapped.remove(font).is_some() {
            self.invalidate_mapped_glyphs(font);
        }
    }
    fn invalidate_mapped_glyphs(&mut self, font: &Font) {
        // A mapping changes only this exact font's main and shadow masks.
        // Keep other message/UI fonts hot, including their GPU atlas identities.
        let keys: Vec<_> = self
            .glyphs
            .iter()
            .filter(|(key, _)| key.font.as_ref() == font)
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            self.glyphs.pop(&key);
        }
    }
    pub fn set_mapping(&mut self, font: Font, mapped: Option<Arc<prerendered::Font>>) {
        if self
            .mapped
            .get(&font)
            .zip(mapped.as_ref())
            .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
        {
            return;
        }
        if let Some(mapped) = mapped {
            self.invalidate_mapped_glyphs(&font);
            self.mapped.insert(font, mapped);
        } else {
            self.unmap(&font);
        }
    }
    pub fn ascent(&mut self, font: &Font) -> Result<i32> {
        let face = self.face(font)?;
        Ok((face.ascent * font.height.unsigned_abs() as f32
            / face.font.units_per_em().unwrap_or(1.0))
        .round() as i32)
    }
    /// A bounded metadata-only path for the script thread. Never discovers or
    /// loads a face, reads glyph pixels, or computes outlines. The caller's
    /// mapping is authoritative even before the worker has seen it.
    pub fn measure_cached(
        &mut self,
        font: &Font,
        text: &[u16],
        mapped: Option<&prerendered::Font>,
    ) -> Option<Metrics> {
        // Longer work stays on the cancellable worker; this is a scheduling
        // threshold, not a script text limit.
        if text.len() > 256 {
            return None;
        }
        let face = self.faces.get(&FaceKey::query(font) as &dyn FaceLookup);
        let mut metrics = Metrics {
            height: font.height.saturating_abs(),
            ..Default::default()
        };
        for &code in text.iter().take_while(|&&c| c != 0) {
            let advance = match mapped.and_then(|mapped| mapped.find(code)) {
                Some(item) => item.inc,
                None => {
                    let face = face?;
                    // First use may parse the embedded fallback; keep that on
                    // the font worker just like ordinary family discovery.
                    if !outline::contains(face, code) && !bundled::is_loaded() {
                        return None;
                    }
                    let fallback = outline::fallback(face, code);
                    advance(fallback.as_deref().unwrap_or(face), font, code)
                }
            };
            metrics.width = metrics.width.saturating_add(advance);
        }
        Some(metrics)
    }
    pub fn measure(
        &mut self,
        font: &Font,
        text: &[u16],
        bounds: bool,
        stop: &AtomicBool,
    ) -> Result<Metrics> {
        let mut face = None;
        let mut metrics = Metrics {
            height: font.height.saturating_abs(),
            ..Default::default()
        };
        let mut area: Option<Rect> = None;
        let mut measure_font = None;
        for &code in text.iter().take_while(|&&c| c != 0) {
            cancelled(stop)?;
            let query = (font, code, false, None);
            if let Some(cached) = bounds
                .then(|| self.measurements.get(&query as &dyn Lookup))
                .flatten()
            {
                if let Some(mut rect) = cached.bounds {
                    rect.left += metrics.width;
                    union(&mut area, rect);
                }
                metrics.width = metrics.width.saturating_add(cached.advance);
                continue;
            }
            let item = if bounds {
                None
            } else {
                self.mapped.get(font).and_then(|f| f.find(code))
            };
            let fallback = if item.is_none() {
                if face.is_none() {
                    face = Some(self.face(font)?);
                }
                outline::fallback(face.as_ref().unwrap(), code)
            } else {
                None
            };
            let glyph_face = fallback.as_deref().or(face.as_deref());
            let advance = match item {
                Some(item) => item.inc,
                None => advance(glyph_face.unwrap(), font, code),
            };
            if bounds {
                // The native glyph-rectangle query lays out horizontal advances,
                // independently of escapement angle and prerendered mappings.
                let mut measured = None;
                if let Some(glyph) = outline::prepare(
                    glyph_face.unwrap(),
                    face.as_ref().unwrap(),
                    font,
                    code,
                    false,
                )? {
                    let r = glyph.px_bounds();
                    measured = Some(Rect {
                        left: r.min.x as i32,
                        top: r.min.y as i32,
                        width: r.width() as u32,
                        height: r.height() as u32,
                    });
                    union(
                        &mut area,
                        Rect {
                            left: metrics.width + r.min.x as i32,
                            top: r.min.y as i32,
                            width: r.width() as u32,
                            height: r.height() as u32,
                        },
                    );
                }
                // Metadata only: retain neither the outline nor raster masks.
                // Caching is optional under pressure and charged to the same
                // font budget, including the key and amortized LRU overhead.
                let bytes =
                    2 * (size_of::<GlyphKey>() + size_of::<Measurement>()) + font.face.len();
                if let Ok(permit) = self.budget.reserve(bytes) {
                    self.measurements.put(
                        GlyphKey {
                            font: measure_font
                                .get_or_insert_with(|| Arc::new(font.clone()))
                                .clone(),
                            code,
                            aa: false,
                            shadow: None,
                        },
                        Measurement {
                            advance,
                            bounds: measured,
                            _permit: permit,
                        },
                    );
                }
            }
            metrics.width = metrics.width.saturating_add(advance);
        }
        metrics.bounds = area.unwrap_or_default();
        Ok(metrics)
    }
    fn glyph(
        &mut self,
        face: &Face,
        font: &Arc<Font>,
        code: u16,
        aa: bool,
        stop: &AtomicBool,
    ) -> Result<Arc<Glyph>> {
        let key = GlyphKey {
            font: font.clone(),
            code,
            aa,
            shadow: None,
        };
        if let Some(glyph) = self.glyphs.get(&key) {
            return Ok(glyph.clone());
        }
        cancelled(stop)?;
        let glyph = if let Some(mapped) = self.mapped.get(font.as_ref()).cloned()
            && let Some(item) = mapped.find(code)
        {
            let len = item.size.width as usize * item.size.height as usize;
            let permit = self.reserve(len)?;
            mapped.glyph_interruptible(item, outline::baseline(face, font), permit, stop)?
        } else {
            let fallback = outline::fallback(face, code);
            let glyph_face = fallback.as_deref().unwrap_or(face);
            let outline = outline::prepare(glyph_face, face, font, code, true)?;
            let scale =
                font.height.unsigned_abs() as f32 / glyph_face.font.units_per_em().unwrap_or(1.0);
            let width = (glyph_face
                .font
                .h_advance_unscaled(outline::id(glyph_face, code))
                * scale)
                .round() as i32;
            let advance = if font.angle == 0 {
                [width, 0]
            } else {
                let angle = (font.angle as f64 * std::f64::consts::PI / 1800.0).sin_cos();
                [
                    (angle.1 * width as f64).round() as i32,
                    (-angle.0 * width as f64).round() as i32,
                ]
            };
            let r = outline.as_ref().map(|o| o.px_bounds()).unwrap_or_default();
            let size = Size {
                width: r.width() as u32,
                height: r.height() as u32,
            };
            let len = size.width as usize * size.height as usize;
            // ab_glyph's coverage rasterizer uses a temporary f32 grid.
            let _work = self.reserve((size.width as usize + 1) * (size.height as usize + 1) * 4)?;
            let permit = self.reserve(len)?;
            let mut mask = Bytes::with_permit(vec![0; len], permit);
            if let Some(outline) = outline {
                outline.draw(|x, y, a| {
                    let coverage = if aa {
                        (a.clamp(0.0, 1.0) * 255.0).round() as u8
                    } else {
                        if a >= 0.5 { 255 } else { 0 }
                    };
                    let pixel =
                        &mut mask.as_mut_slice()[y as usize * size.width as usize + x as usize];
                    *pixel = (*pixel).max(coverage);
                });
            }
            let mut glyph = Glyph {
                id: glyph_id(),
                size,
                origin: [r.min.x as i32, r.min.y as i32],
                advance,
                levels: 256,
                mask,
            };
            if font.bold && !glyph_face.bold {
                glyph = shadow::bold(glyph, font.height, self)?;
            }
            glyph
        };
        self.rasterized += 1;
        let glyph = Arc::new(glyph);
        self.glyphs.put(key, glyph.clone());
        Ok(glyph)
    }
    /// Reposition existing glyphs only. Short UI redraws need no worker trip
    /// when every main/shadow mask is cached for the caller's current mapping.
    /// Admission does not evict or rasterize here; misses use the worker path.
    pub fn layout_cached(
        &mut self,
        font: &Font,
        text: &[u16],
        style: Style,
        position: [i32; 2],
        mapped: Option<&Arc<prerendered::Font>>,
    ) -> Option<Run> {
        if text.len() > 256 {
            return None;
        }
        match (self.mapped.get(font), mapped) {
            (None, None) => {}
            (Some(a), Some(b)) if Arc::ptr_eq(a, b) => {}
            _ => return None,
        }
        self.faces.get(&FaceKey::query(font) as &dyn FaceLookup)?;
        let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        let shadowed = style.shadow_level != 0;
        let count = len * if shadowed { 2 } else { 1 };
        let permit = self
            .budget
            .reserve(count * std::mem::size_of::<PlacedGlyph>())
            .ok()?;
        let main_count = if shadowed { len } else { 0 };
        let _work = self
            .budget
            .reserve(if main_count > 16 {
                main_count * std::mem::size_of::<PlacedGlyph>()
            } else {
                0
            })
            .ok()?;
        let mut glyphs = Vec::with_capacity(count);
        let mut main = smallvec::SmallVec::<[PlacedGlyph; 16]>::with_capacity(main_count);
        let mut pen = position;
        for &code in &text[..len] {
            let mut query = (font, code, style.antialias, None);
            let glyph = self.glyphs.get(&query as &dyn Lookup)?.clone();
            let advance = glyph.advance;
            if glyph.size.width != 0 && glyph.size.height != 0 {
                if shadowed {
                    let shadow = if style.shadow_level == 255 && style.shadow_width == 0 {
                        glyph.clone()
                    } else {
                        query.3 = Some((style.shadow_level, style.shadow_width));
                        self.glyphs.get(&query as &dyn Lookup)?.clone()
                    };
                    glyphs.push(PlacedGlyph {
                        x: pen[0].saturating_add(style.shadow_offset[0]),
                        y: pen[1].saturating_add(style.shadow_offset[1]),
                        glyph: shadow,
                        color: style.shadow_color,
                    });
                }
                let placed = PlacedGlyph {
                    x: pen[0],
                    y: pen[1],
                    glyph,
                    color: style.color,
                };
                if shadowed {
                    main.push(placed);
                } else {
                    glyphs.push(placed);
                }
            }
            pen[0] = pen[0].saturating_add(advance[0]);
            pen[1] = pen[1].saturating_add(advance[1]);
        }
        glyphs.extend(main);
        Some(Run { glyphs, permit })
    }
    pub fn layout(
        &mut self,
        font: &Font,
        text: &[u16],
        style: Style,
        x: i32,
        y: i32,
        stop: &AtomicBool,
    ) -> Result<Run> {
        let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        let shadowed = style.shadow_level != 0;
        let count = len * if shadowed { 2 } else { 1 };
        let permit = self.reserve(count * std::mem::size_of::<PlacedGlyph>())?;
        let mut glyphs = Vec::with_capacity(count);
        let main_count = if shadowed { len } else { 0 };
        let _work = self.reserve(if main_count > 16 {
            main_count * std::mem::size_of::<PlacedGlyph>()
        } else {
            0
        })?;
        let mut main = smallvec::SmallVec::<[PlacedGlyph; 16]>::with_capacity(main_count);
        let mut pen = [x, y];
        let face = self.face(font)?;
        let font = Arc::new(font.clone());
        for &code in &text[..len] {
            cancelled(stop)?;
            let glyph = self.glyph(&face, &font, code, style.antialias, stop)?;
            let advance = glyph.advance;
            if glyph.size.width != 0 && glyph.size.height != 0 {
                if style.shadow_level != 0 {
                    let key = GlyphKey {
                        font: font.clone(),
                        code,
                        aa: style.antialias,
                        shadow: Some((style.shadow_level, style.shadow_width)),
                    };
                    let shadow = if style.shadow_level == 255 && style.shadow_width == 0 {
                        glyph.clone()
                    } else if let Some(shadow) = self.glyphs.get(&key) {
                        shadow.clone()
                    } else {
                        let shadow = Arc::new(shadow::blur(
                            &glyph,
                            style.shadow_level,
                            style.shadow_width,
                            self,
                            stop,
                        )?);
                        self.glyphs.put(key, shadow.clone());
                        shadow
                    };
                    glyphs.push(PlacedGlyph {
                        x: pen[0].saturating_add(style.shadow_offset[0]),
                        y: pen[1].saturating_add(style.shadow_offset[1]),
                        glyph: shadow,
                        color: style.shadow_color,
                    });
                }
                let placed = PlacedGlyph {
                    x: pen[0],
                    y: pen[1],
                    glyph,
                    color: style.color,
                };
                if shadowed {
                    main.push(placed);
                } else {
                    glyphs.push(placed);
                }
            }
            pen[0] = pen[0].saturating_add(advance[0]);
            pen[1] = pen[1].saturating_add(advance[1]);
        }
        glyphs.extend(main);
        Ok(Run { glyphs, permit })
    }
}
fn advance(face: &Face, font: &Font, code: u16) -> i32 {
    let scale = font.height.unsigned_abs() as f32 / face.font.units_per_em().unwrap_or(1.0);
    (face.font.h_advance_unscaled(outline::id(face, code)) * scale).round() as i32
}
fn union(area: &mut Option<Rect>, r: Rect) {
    if r.width == 0 || r.height == 0 {
        return;
    }
    *area = Some(match *area {
        None => r,
        Some(a) => {
            let left = a.left.min(r.left);
            let top = a.top.min(r.top);
            Rect {
                left,
                top,
                width: (a.left + a.width as i32)
                    .max(r.left + r.width as i32)
                    .saturating_sub(left) as u32,
                height: (a.top + a.height as i32)
                    .max(r.top + r.height as i32)
                    .saturating_sub(top) as u32,
            }
        }
    });
}
