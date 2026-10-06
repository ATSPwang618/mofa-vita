//! The SDL lzfs provider registers an empty, case-sensitive storage medium.
//! It does not implement compression or expose a filesystem path.
use krkr_engine::{
    assets::{self, ReadPlan, StorageMedium, Vfs},
    plugins::{Context, Plugin},
    storages,
};
use std::sync::Arc;
use tjs_core::{NativeError, NativeResult};

struct Medium;
impl StorageMedium for Medium {
    fn normalize(&self, path: &[u16]) -> Vec<u16> {
        path.to_vec()
    }
    fn plan(&self, _: &mut Vfs, _: &[u16]) -> assets::Result<Option<ReadPlan>> {
        Ok(None)
    }
    fn list(&self, _: &mut Vfs, _: &[u16]) -> assets::Result<Vec<Vec<u16>>> {
        Ok(Vec::new())
    }
}
struct Registration {
    medium: Arc<dyn StorageMedium>,
    vfs: storages::Shared,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.vfs
            .borrow_mut()
            .unregister_medium("lzfs", &self.medium);
    }
}
#[derive(Default, tjs_bind::Trace)]
pub(crate) struct Lzfs {
    #[trace(skip = "RAII registration has no script values")]
    registration: Option<Registration>,
}
krkr_engine::native_plugin! { impl Lzfs { names: ["lzfs.dll", "lzfs.tpm"] } }
impl Plugin for Lzfs {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        let vfs = storages::service_from_heap(cx.heap)?;
        let medium: Arc<dyn StorageMedium> = Arc::new(Medium);
        vfs.borrow_mut()
            .register_medium("lzfs", medium.clone())
            .map_err(|e| NativeError::Detail(e.to_string()))?;
        self.registration = Some(Registration { medium, vfs });
        Ok(())
    }
    fn unlink(&mut self, _: &mut Context<'_>) -> NativeResult<bool> {
        self.registration = None;
        Ok(true)
    }
}
