use super::{Stored, error};
use krkr_engine::{
    assets::{self, ReadPlan, ReadSource, StorageMedium, Stream, Vfs, name},
    protocol::budget::Budget,
    storages,
};
use krkr_image::psd::{BmpStream, Decoder, Document, Image};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};
use tjs_core::NativeResult;

#[derive(Default)]
struct Cache {
    objects: BTreeMap<Vec<u16>, Weak<Stored>>,
    current: Option<(Vec<u16>, Arc<Stored>)>,
}
pub(super) struct Medium {
    cache: Mutex<Cache>,
    budget: Budget,
    directory: Vec<u16>,
}
impl Medium {
    pub fn new(budget: Budget, directory: Vec<u16>) -> Self {
        Self {
            cache: Mutex::new(Cache::default()),
            budget,
            directory,
        }
    }
    pub fn remember(&self, name: &[u16], doc: &Arc<Stored>) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.objects.retain(|_, d| d.strong_count() != 0);
        cache.objects.insert(name.to_vec(), Arc::downgrade(doc));
        if cache.current.as_ref().is_some_and(|(n, _)| n == name) {
            cache.current = None;
        }
    }
    pub fn forget(&self, doc: &Arc<Stored>) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .objects
            .retain(|_, d| d.upgrade().is_some_and(|d| !Arc::ptr_eq(&d, doc)));
        if cache
            .current
            .as_ref()
            .is_some_and(|(_, d)| Arc::ptr_eq(d, doc))
        {
            cache.current = None;
        }
    }
    fn resolve(&self, vfs: &mut Vfs, domain: &[u16]) -> assets::Result<Option<Arc<Stored>>> {
        {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((name, doc)) = &cache.current
                && name == domain
            {
                return Ok(Some(doc.clone()));
            }
            if let Some(doc) = cache.objects.get(domain).and_then(Weak::upgrade) {
                cache.current = Some((domain.to_vec(), doc.clone()));
                return Ok(Some(doc));
            }
        }
        // Always resolve the backing PSD through a file cwd (including auto
        // paths/XP3), so a psd:// cwd cannot recursively reopen its own medium.
        let path = name::normalize(domain, &self.directory)?;
        let plan = match vfs.plan(&path) {
            Ok(plan) => Arc::new(plan),
            Err(assets::Error::Missing(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let doc = Arc::new(Stored::new(Arc::new(
            Document::load(plan, vfs.limits()).map_err(asset_error)?,
        )));
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.current = Some((domain.to_vec(), doc.clone()));
        Ok(Some(doc))
    }
}
fn split(path: &[u16]) -> assets::Result<(&[u16], &[u16])> {
    let rest = path
        .strip_prefix(name::units("psd://").as_slice())
        .ok_or(assets::Error::Name("invalid PSD medium name"))?;
    let at = rest
        .iter()
        .position(|&u| u == 47)
        .ok_or(assets::Error::Name("missing PSD layer path"))?;
    Ok((&rest[..at], &rest[at + 1..]))
}
impl StorageMedium for Medium {
    fn plan(&self, vfs: &mut Vfs, path: &[u16]) -> assets::Result<Option<ReadPlan>> {
        let (domain, file) = split(path)?;
        let Some(stored) = self.resolve(vfs, domain)? else {
            return Ok(None);
        };
        let index = if let Some(id) = file.strip_prefix(name::units("id/").as_slice()) {
            let Some(id) = id.strip_suffix(name::units(".bmp").as_slice()) else {
                return Ok(None);
            };
            if id.is_empty() || !id.iter().all(|c| (48..=57).contains(c)) {
                return Ok(None);
            }
            let Some(id) = id.iter().try_fold(0i32, |n, &c| {
                n.checked_mul(10)?.checked_add(i32::from(c) - 48)
            }) else {
                return Ok(None);
            };
            stored.ids.get(&id).copied()
        } else {
            stored.paths.get(file).copied()
        };
        let Some(index) = index else {
            return Ok(None);
        };
        let bounds = stored.doc.layers[index].bounds;
        let bytes = if bounds.width() <= 0 || bounds.height() <= 0 {
            54
        } else {
            let size = bounds.size().map_err(asset_error)?;
            size.rgba_bytes().ok_or(assets::Error::Limit("PSD image"))? as u64 + 54
        };
        Ok(Some(ReadPlan::custom(
            path.to_vec(),
            bytes,
            vfs.limits().max_read_bytes,
            Arc::new(Source {
                stored,
                index,
                budget: self.budget.clone(),
            }),
        )))
    }
    fn list(&self, vfs: &mut Vfs, path: &[u16]) -> assets::Result<Vec<Vec<u16>>> {
        let (domain, directory) = split(path)?;
        let Some(stored) = self.resolve(vfs, domain)? else {
            return Ok(Vec::new());
        };
        if directory == name::units("id/") {
            return Ok(stored
                .ids
                .keys()
                .map(|id| name::units(&format!("{id}.bmp")))
                .collect());
        }
        Ok(stored
            .paths
            .keys()
            .filter_map(|path| {
                let rest = path.strip_prefix(directory)?;
                (!rest.contains(&47)).then(|| rest.to_vec())
            })
            .collect())
    }
    fn clear_cache(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).current = None;
    }
}
fn asset_error(e: impl std::fmt::Display) -> assets::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()).into()
}
struct Source {
    stored: Arc<Stored>,
    index: usize,
    budget: Budget,
}
impl ReadSource for Source {
    fn open(&self) -> assets::Result<Box<dyn Stream>> {
        self.open_interruptible(&|| false)
    }
    fn open_interruptible(&self, cancelled: &dyn Fn() -> bool) -> assets::Result<Box<dyn Stream>> {
        let mut decoder = Decoder::new(
            self.stored.doc.clone(),
            Image::Layer(self.index),
            self.budget.clone(),
        )
        .map_err(asset_error)?;
        loop {
            if cancelled() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "PSD decode cancelled",
                )
                .into());
            }
            if decoder.advance().map_err(asset_error)? {
                break;
            }
        }
        Ok(Box::new(
            BmpStream::new(decoder.finish()).map_err(asset_error)?,
        ))
    }
}
pub(super) struct Registration {
    pub medium: Arc<Medium>,
    vfs: storages::Shared,
}
impl Registration {
    pub fn new(medium: Arc<Medium>, vfs: storages::Shared) -> NativeResult<Self> {
        vfs.borrow_mut()
            .register_medium("psd", medium.clone())
            .map_err(error)?;
        Ok(Self { medium, vfs })
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        let owner: Arc<dyn StorageMedium> = self.medium.clone();
        self.vfs.borrow_mut().unregister_medium("psd", &owner);
        self.medium.clear_cache();
    }
}
