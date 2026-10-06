use super::{
    archive::{Archive, Source},
    error,
};
use krkr_engine::{
    assets::{self, Limits, ReadPlan, StorageMedium, Vfs},
    storages,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tjs_core::NativeResult;

#[derive(Default)]
struct Mounts {
    active: bool,
    serial: u64,
    entries: BTreeMap<String, (u64, Option<Arc<Archive>>)>,
}
#[derive(Default)]
pub(super) struct Medium {
    mounts: Mutex<Mounts>,
}
impl Medium {
    pub fn begin(&self, name: &str, limits: Limits) -> Option<u64> {
        let mut m = self.mounts.lock().unwrap_or_else(|e| e.into_inner());
        if !m.active {
            return None;
        }
        m.entries.remove(name);
        if m.entries.len() >= limits.max_cached_archives {
            return None;
        }
        m.serial = m.serial.wrapping_add(1);
        let serial = m.serial;
        m.entries.insert(name.into(), (serial, None));
        Some(serial)
    }
    pub fn cancel(&self, name: &str, serial: u64) {
        let mut m = self.mounts.lock().unwrap_or_else(|e| e.into_inner());
        if m.entries
            .get(name)
            .is_some_and(|(n, archive)| *n == serial && archive.is_none())
        {
            m.entries.remove(name);
        }
    }
    pub fn finish(
        &self,
        name: String,
        serial: u64,
        archive: Option<Arc<Archive>>,
        limits: Limits,
    ) -> bool {
        let mut m = self.mounts.lock().unwrap_or_else(|e| e.into_inner());
        if !m.active || !m.entries.get(&name).is_some_and(|(n, _)| *n == serial) {
            return false;
        }
        m.entries.remove(&name);
        let Some(archive) = archive else {
            return false;
        };
        let total: usize = m
            .entries
            .values()
            .filter_map(|(_, a)| a.as_ref())
            .map(|a| a.index_bytes)
            .sum();
        if m.entries.len() >= limits.max_cached_archives
            || archive.index_bytes > limits.max_cached_index_bytes.saturating_sub(total)
        {
            return false;
        }
        m.entries.insert(name, (serial, Some(archive)));
        true
    }
    pub fn unmount(&self, name: &str) -> bool {
        self.mounts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .remove(name)
            .is_some_and(|(_, a)| a.is_some())
    }
    fn find(&self, path: &[u16]) -> assets::Result<(Option<Arc<Archive>>, String)> {
        let text = String::from_utf16_lossy(path);
        let (domain, file) = text
            .strip_prefix("zip://")
            .and_then(|s| s.split_once('/'))
            .ok_or(assets::Error::Name("invalid ZIP medium path"))?;
        let archive = self
            .mounts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .get(domain)
            .and_then(|(_, a)| a.clone());
        Ok((archive, file.into()))
    }
}
impl StorageMedium for Medium {
    fn normalize(&self, path: &[u16]) -> Vec<u16> {
        path.to_vec()
    }
    fn plan(&self, vfs: &mut Vfs, path: &[u16]) -> assets::Result<Option<ReadPlan>> {
        let (Some(archive), file) = self.find(path)? else {
            return Ok(None);
        };
        let Some(index) = archive.find(&file) else {
            return Ok(None);
        };
        Ok(Some(ReadPlan::custom(
            path.to_vec(),
            archive.entries[index].size,
            vfs.limits().max_read_bytes,
            Arc::new(Source { archive, index }),
        )))
    }
    fn list(&self, _: &mut Vfs, path: &[u16]) -> assets::Result<Vec<Vec<u16>>> {
        let (Some(archive), directory) = self.find(path)? else {
            return Ok(Vec::new());
        };
        Ok(archive
            .entries
            .iter()
            .filter_map(|entry| {
                let name = entry.name.strip_prefix(&directory)?;
                (!name.contains('/')).then(|| name.encode_utf16().collect())
            })
            .collect())
    }
}
pub(super) struct Registration {
    pub medium: Arc<Medium>,
    vfs: storages::Shared,
}
impl Registration {
    pub fn new(vfs: storages::Shared) -> NativeResult<Self> {
        let medium = Arc::new(Medium::default());
        vfs.borrow_mut()
            .register_medium("zip", medium.clone())
            .map_err(error)?;
        medium
            .mounts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active = true;
        Ok(Self { medium, vfs })
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut m = self.medium.mounts.lock().unwrap_or_else(|e| e.into_inner());
        m.active = false;
        m.entries.clear();
        let owner: Arc<dyn StorageMedium> = self.medium.clone();
        self.vfs.borrow_mut().unregister_medium("zip", &owner);
    }
}
