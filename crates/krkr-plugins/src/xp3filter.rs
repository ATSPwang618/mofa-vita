//! Script XP3 filters execute in isolated decoder VMs, as in the SDL plugin.
//! Only owned metadata and byte buffers cross the worker boundary.
pub(crate) mod buffer;
mod worker;
use krkr_engine::{
    assets::{
        self, Vfs,
        xp3::{Entry, Filter, FilterFactory},
    },
    plugins::{Context, Plugin},
    storages,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread::{self, ThreadId},
};
use tjs_core::{NativeError, NativeResult};

#[derive(Clone)]
struct Spec {
    source: Arc<Vec<u16>>,
    source_name: String,
    compiled: Arc<OnceLock<Result<tjs_core::Module, String>>>,
    directory: std::path::PathBuf,
    limits: assets::Limits,
    require_extraction: bool,
}
struct Factory {
    spec: Spec,
    workers: Mutex<HashMap<ThreadId, mpsc::SyncSender<worker::Request>>>,
}

/// Build a script-backed extraction provider without installing it into a game VM.
/// Launchers and offline conversion hosts share the same VFS filter contract.
/// The decoder owns isolated persistent script state on each IO worker.
pub fn script_filter(
    path: &std::path::Path,
    directory: &std::path::Path,
    encoding: &[u16],
    limits: assets::Limits,
) -> assets::Result<Arc<dyn FilterFactory>> {
    script_filter_impl(path, directory, encoding, limits, false)
}

/// Offline decryption must not silently succeed if a script installs no decoder.
/// Registration is checked when the first stream initializes its decoder VM.
pub fn script_filter_checked(
    path: &std::path::Path,
    directory: &std::path::Path,
    encoding: &[u16],
    limits: assets::Limits,
) -> assets::Result<Arc<dyn FilterFactory>> {
    script_filter_impl(path, directory, encoding, limits, true)
}

fn script_filter_impl(
    path: &std::path::Path,
    directory: &std::path::Path,
    encoding: &[u16],
    limits: assets::Limits,
    require_extraction: bool,
) -> assets::Result<Arc<dyn FilterFactory>> {
    let mut vfs = Vfs::new(directory, limits)?;
    let bytes = vfs.plan(&assets::local::units(path)?)?.read(0)?;
    let source = assets::text::decode(&bytes, encoding, limits.max_read_bytes)?;
    Ok(Arc::new(Factory {
        spec: Spec {
            source: Arc::new(source),
            source_name: path.display().to_string(),
            compiled: Arc::default(),
            directory: directory.into(),
            limits,
            require_extraction,
        },
        workers: Mutex::default(),
    }))
}
impl Factory {
    fn channel(&self) -> std::io::Result<mpsc::SyncSender<worker::Request>> {
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| std::io::Error::other("filter worker registry poisoned"))?;
        let id = thread::current().id();
        if let Some(sender) = workers.get(&id) {
            return Ok(sender.clone());
        }
        if workers.len() >= 64 {
            return Err(std::io::Error::other("XP3 filter worker limit"));
        }
        let (send, receive) = mpsc::sync_channel(1);
        let spec = self.spec.clone();
        thread::Builder::new()
            .name("krkr-xp3-filter".into())
            .spawn(move || worker::run(spec, receive))?;
        workers.insert(id, send.clone());
        Ok(send)
    }
}
impl FilterFactory for Factory {
    fn create(&self, storage: &[u16], entry: &Entry) -> assets::Result<Box<dyn Filter>> {
        let sender = self.channel()?;
        let archive = assets::name::split_archive(storage).0.to_vec();
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .send(worker::Request::Open {
                file: entry.name.clone(),
                archive,
                size: entry.size,
                hash: entry.hash,
                reply,
            })
            .map_err(|_| std::io::Error::other("XP3 filter worker stopped"))?;
        let (id, full) = receive
            .recv()
            .map_err(|_| std::io::Error::other("XP3 filter worker stopped"))?
            .map_err(std::io::Error::other)?;
        let (reply, receive) = mpsc::sync_channel(1);
        Ok(Box::new(Stream {
            sender,
            id,
            full,
            reply,
            receive,
            buffer: Vec::new(),
        }))
    }
}
struct Stream {
    sender: mpsc::SyncSender<worker::Request>,
    id: u64,
    full: bool,
    reply: mpsc::SyncSender<Result<Vec<u8>, String>>,
    receive: mpsc::Receiver<Result<Vec<u8>, String>>,
    buffer: Vec<u8>,
}
impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.sender.send(worker::Request::Close(self.id));
    }
}
impl Filter for Stream {
    fn fetch_full_data(&self) -> bool {
        self.full
    }
    fn apply(&mut self, offset: u64, bytes: &mut [u8]) -> std::io::Result<()> {
        let mut buffer = std::mem::take(&mut self.buffer);
        buffer.clear();
        buffer.extend_from_slice(bytes);
        self.sender
            .send(worker::Request::Apply {
                id: self.id,
                offset,
                bytes: buffer,
                reply: self.reply.clone(),
            })
            .map_err(|_| std::io::Error::other("XP3 filter worker stopped"))?;
        let output = self
            .receive
            .recv()
            .map_err(|_| std::io::Error::other("XP3 filter worker stopped"))?
            .map_err(std::io::Error::other)?;
        if output.len() != bytes.len() {
            return Err(std::io::Error::other("filter changed buffer length"));
        }
        bytes.copy_from_slice(&output);
        // Keep the ordinary IO chunk, not a whole-entry fetch allocation.
        if output.capacity() <= 64 * 1024 {
            self.buffer = output;
        }
        Ok(())
    }
}
struct Registration {
    vfs: storages::Shared,
    installed: Arc<dyn FilterFactory>,
    previous: Option<Arc<dyn FilterFactory>>,
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut vfs = self.vfs.borrow_mut();
        if vfs
            .filter()
            .is_some_and(|f| Arc::ptr_eq(&f, &self.installed))
        {
            vfs.set_filter(self.previous.take());
        }
    }
}
#[derive(Default, tjs_bind::Trace)]
pub(crate) struct Xp3Filter {
    #[trace(skip = "Filter registration contains channels and portable VFS metadata")]
    registration: Option<Registration>,
}
krkr_engine::native_plugin! {impl Xp3Filter {names:["xp3filter.dll","xp3filter.tpm"]}}
impl Plugin for Xp3Filter {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        let vfs = storages::service_from_heap(cx.heap)?;
        let (plan, directory, limits) = {
            let mut vfs = vfs.borrow_mut();
            let directory = vfs.file_directory().to_vec();
            let path = [directory.clone(), assets::name::units("xp3filter.tjs")].concat();
            if !vfs.exists_no_search_no_normalize(&path).map_err(error)? {
                return Ok(());
            }
            (
                vfs.plan(&path).map_err(error)?,
                assets::local::from_storage(&directory).map_err(error)?,
                vfs.limits(),
            )
        };
        let encoding = krkr_engine::scripts::text_encoding(cx.heap)?;
        let source = assets::text::decode(
            &plan.read(0).map_err(error)?,
            &encoding,
            limits.max_read_bytes,
        )
        .map_err(error)?;
        let installed: Arc<dyn FilterFactory> = Arc::new(Factory {
            spec: Spec {
                source: Arc::new(source),
                source_name: "xp3filter.tjs".into(),
                compiled: Arc::default(),
                directory,
                limits,
                require_extraction: false,
            },
            workers: Mutex::default(),
        });
        let previous = vfs.borrow().filter();
        vfs.borrow_mut().set_filter(Some(installed.clone()));
        self.registration = Some(Registration {
            vfs,
            installed,
            previous,
        });
        Ok(())
    }
    fn unlink(&mut self, _: &mut Context<'_>) -> NativeResult<bool> {
        self.registration = None;
        Ok(true)
    }
}
fn error(error: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(error.to_string())
}
