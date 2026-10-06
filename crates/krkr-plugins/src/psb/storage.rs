use super::decode::{self, Document};
use krkr_engine::{
    assets::{self, ReadPlan, ReadSource, StorageMedium, Stream, Vfs, name},
    storages,
};
use std::{
    collections::BTreeMap,
    io::Cursor,
    sync::{Arc, Mutex},
};

pub(super) struct Medium {
    docs: Mutex<BTreeMap<Vec<u16>, Arc<Document>>>,
    directory: Vec<u16>,
}
impl Medium {
    pub fn new(directory: Vec<u16>) -> Self {
        Self {
            docs: Mutex::default(),
            directory,
        }
    }
    pub fn remember(&self, name: &[u16], doc: Arc<Document>) {
        // The reference retains the first registration until plugin unlink.
        self.docs
            .lock()
            .unwrap()
            .entry(name::fold(name))
            .or_insert(doc);
    }
}
impl StorageMedium for Medium {
    fn normalize(&self, path: &[u16]) -> Vec<u16> {
        let mut path = path.to_vec();
        let end = path[6..]
            .iter()
            .position(|&u| u == 47)
            .map_or(path.len(), |n| n + 6);
        if let Some(dot) = path[6..end].iter().position(|&u| u == 46) {
            for u in &mut path[6 + dot..end] {
                if (65..=90).contains(u) {
                    *u += 32;
                }
            }
        }
        path
    }
    fn plan(&self, vfs: &mut Vfs, path: &[u16]) -> assets::Result<Option<ReadPlan>> {
        let rest = path
            .get(6..)
            .ok_or(assets::Error::Name("invalid PSB path"))?;
        let Some(slash) = rest.iter().position(|&u| u == 47) else {
            return Ok(None);
        };
        let domain = &rest[..slash];
        let key = String::from_utf16_lossy(&rest[slash + 1..]);
        let cached = self.docs.lock().unwrap().get(domain).cloned();
        let doc = if let Some(doc) = cached {
            doc
        } else {
            let backing = name::normalize(domain, &self.directory)?;
            let plan = match vfs.plan(&backing) {
                Ok(plan) => plan,
                Err(assets::Error::Missing(_)) => return Ok(None),
                Err(e) => return Err(e),
            };
            if plan.bytes > 64 * 1024 * 1024 {
                return Err(assets::Error::Limit("PSB file"));
            }
            let bytes = plan.read(0)?;
            let doc = Arc::new(decode::decode(bytes, &|| false).map_err(asset_error)?);
            self.remember(domain, doc);
            // AddPSBFile lowercases the registration; uppercase domains before
            // the first dot do not become an alias in the reference.
            let Some(doc) = self.docs.lock().unwrap().get(domain).cloned() else {
                return Ok(None);
            };
            doc
        };
        let Some(range) = doc.resources.get(&key).cloned() else {
            return Ok(None);
        };
        Ok(Some(ReadPlan::custom(
            path.to_vec(),
            range.len() as u64,
            vfs.limits().max_read_bytes,
            Arc::new(Source { doc, range }),
        )))
    }
    fn list(&self, _: &mut Vfs, _: &[u16]) -> assets::Result<Vec<Vec<u16>>> {
        Ok(Vec::new())
    }
}
fn asset_error(error: impl std::fmt::Display) -> assets::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()).into()
}
struct Source {
    doc: Arc<Document>,
    range: std::ops::Range<usize>,
}
impl ReadSource for Source {
    fn open(&self) -> assets::Result<Box<dyn Stream>> {
        Ok(Box::new(Cursor::new(
            self.doc.bytes[self.range.clone()].to_vec(),
        )))
    }
}
pub(super) struct Registration {
    pub medium: Arc<Medium>,
    vfs: storages::Shared,
}
impl Registration {
    pub fn new(vfs: storages::Shared) -> tjs_core::NativeResult<Self> {
        let medium = Arc::new(Medium::new(vfs.borrow().file_directory().to_vec()));
        vfs.borrow_mut()
            .register_medium("psb", medium.clone())
            .map_err(super::error)?;
        Ok(Self { medium, vfs })
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        let owner: Arc<dyn StorageMedium> = self.medium.clone();
        self.vfs.borrow_mut().unregister_medium("psb", &owner);
        // Registered class state can outlive the plugin's exports. Release its
        // archive cache now; already-open ReadPlans retain their own documents.
        self.medium.docs.lock().unwrap().clear();
    }
}
