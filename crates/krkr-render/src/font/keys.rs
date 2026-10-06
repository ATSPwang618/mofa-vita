//! Borrowed glyph queries keep cache hits independent of font-name allocation.
use super::{FaceKey, Font};
use std::{
    borrow::Borrow,
    hash::{Hash, Hasher},
    sync::Arc,
};

type Fields<'a> = (&'a Font, u16, bool, Option<(i32, i32)>);

pub(super) trait Lookup {
    fn fields(&self) -> Fields<'_>;
}
impl Lookup for Fields<'_> {
    fn fields(&self) -> Fields<'_> {
        *self
    }
}
impl PartialEq for dyn Lookup + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.fields() == other.fields()
    }
}
impl Eq for dyn Lookup + '_ {}
impl Hash for dyn Lookup + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.fields().hash(state);
    }
}

#[derive(PartialEq, Eq, Clone)]
pub(super) struct GlyphKey {
    pub font: Arc<Font>,
    pub code: u16,
    pub aa: bool,
    pub shadow: Option<(i32, i32)>,
}
impl Lookup for GlyphKey {
    fn fields(&self) -> Fields<'_> {
        (&self.font, self.code, self.aa, self.shadow)
    }
}
impl Hash for GlyphKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.fields().hash(state);
    }
}
impl<'a> Borrow<dyn Lookup + 'a> for GlyphKey {
    fn borrow(&self) -> &(dyn Lookup + 'a) {
        self
    }
}

pub(super) type FaceFields<'a> = (&'a str, bool, bool, bool);
pub(super) trait FaceLookup {
    fn fields(&self) -> FaceFields<'_>;
}
impl FaceLookup for FaceFields<'_> {
    fn fields(&self) -> FaceFields<'_> {
        *self
    }
}
impl FaceLookup for FaceKey {
    fn fields(&self) -> FaceFields<'_> {
        (&self.name, self.bold, self.italic, self.file)
    }
}
impl PartialEq for dyn FaceLookup + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.fields() == other.fields()
    }
}
impl Eq for dyn FaceLookup + '_ {}
impl Hash for dyn FaceLookup + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.fields().hash(state);
    }
}
impl Hash for FaceKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.fields().hash(state);
    }
}
impl<'a> Borrow<dyn FaceLookup + 'a> for FaceKey {
    fn borrow(&self) -> &(dyn FaceLookup + 'a) {
        self
    }
}
