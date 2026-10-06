//! Process-local scalable font registration, independent of OS font installation.
//! Behavior adapted from krkrsdl3/core/media/font/TVPFont.cpp (modified Rust).
//! Upstream notices are retained in private-LICENSE.txt.
use super::*;

pub struct Registration(Vec<Arc<Entry>>);
impl Registration {
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = self
            .0
            .iter()
            .flat_map(|entry| entry.names.clone())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}
pub(super) struct Entry {
    names: Vec<String>,
    pub face: Arc<Face>,
    _metadata: Permit,
}
impl System {
    /// Parse on the font worker; publish only after successful request delivery.
    /// Each collection face owns its ab_glyph buffer and corresponding permit.
    pub fn prepare_registration(
        &mut self,
        bytes: Vec<u8>,
        permit: Permit,
        stop: &AtomicBool,
    ) -> Result<Registration> {
        let count = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1);
        let mut buffer = Some((bytes, permit));
        let mut entries = Vec::new();
        for index in 0..count {
            cancelled(stop)?;
            let bytes = &buffer.as_ref().unwrap().0;
            let Ok(face) = ttf_parser::Face::parse(bytes, index) else {
                continue;
            };
            let tables = face.tables();
            if tables.glyf.is_none() && tables.cff.is_none() && tables.cff2.is_none() {
                continue;
            }
            let mut names = Vec::new();
            // FreeType's family name plus the reference's localized CJK names.
            for name in face.names() {
                if name.name_id != ttf_parser::name_id::FAMILY {
                    continue;
                }
                let cjk = name.platform_id == ttf_parser::PlatformId::Windows
                    && name.encoding_id == 1
                    && matches!(
                        name.language_id,
                        0x0411 | 0x0004 | 0x0404 | 0x0804 | 0x0c04 | 0x1004 | 0x0412 | 0x0812
                    );
                let primary = name.language_id == 0x0409 || name.language_id == 0;
                if (cjk || primary)
                    && let Some(name) = name.to_string()
                    && !name.is_empty()
                {
                    names.push(name);
                }
            }
            if names.is_empty() {
                if let Some(name) = face
                    .names()
                    .into_iter()
                    .filter(|n| n.name_id == ttf_parser::name_id::FAMILY)
                    .find_map(|n| n.to_string())
                {
                    names.push(name);
                } else {
                    continue;
                }
            }
            names.sort_unstable();
            names.dedup();
            let metadata = self.reserve(names.iter().map(|n| n.capacity() + 128).sum())?;
            let (data, face_permit) = if index + 1 == count {
                buffer.take().unwrap()
            } else {
                let permit = self.reserve(bytes.len())?;
                (bytes.clone(), permit)
            };
            // A malformed/unsupported face does not become an advertised family.
            if let Ok(face) = Face::from_bytes(data, index, face_permit) {
                entries.push(Arc::new(Entry {
                    names,
                    face: Arc::new(face),
                    _metadata: metadata,
                }));
            }
        }
        Ok(Registration(entries))
    }
    pub fn register(&mut self, registration: Registration) -> usize {
        let count = registration.0.len();
        for entry in registration.0 {
            for name in &entry.names {
                self.private.insert(name.clone(), entry.clone());
            }
        }
        if count != 0 {
            // A previously cached fallback must not hide the newly loaded font.
            self.faces.clear();
            self.glyphs.clear();
            self.measurements.clear();
        }
        count
    }
}
