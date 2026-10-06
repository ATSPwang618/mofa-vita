use super::path;
use std::{
    fs::{File, OpenOptions},
    path::Path,
};
use tjs_core::{Heap, NativeCx, NativeResult, ObjId, Value};

#[cfg(windows)]
struct Entry {
    _file: File,
}

#[cfg(unix)]
struct Entry {
    file: File,
    path: std::path::PathBuf,
    directory: bool,
}
#[cfg(not(any(windows, unix)))]
struct Entry;

impl Entry {
    fn open(path: &Path, folder: bool) -> std::io::Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Original OPEN_EXISTING + DELETE_ON_CLOSE: deletion follows this
            // handle even if the pathname is renamed or replaced. Never create
            // or truncate; share read/write/delete with other owners.
            let flags = 0x0400_0000 | if folder { 0x0200_0000 } else { 0x80 };
            OpenOptions::new()
                .access_mode(0)
                .share_mode(7)
                .custom_flags(flags)
                .open(path)
                .map(|file| Self { _file: file })
        }
        #[cfg(unix)]
        {
            let file = OpenOptions::new().read(true).open(path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() && !(folder && metadata.is_dir()) {
                return Err(std::io::Error::other("not a temporary file or directory"));
            }
            // Unix has no Windows delete-on-last-close contract. Retain the
            // identity and registered path; cleanup will preserve replacements.
            Ok(Self {
                file,
                path: path.to_owned(),
                directory: metadata.is_dir(),
            })
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = (path, folder);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "temporary handle cleanup is unavailable",
            ))
        }
    }
}
#[cfg(unix)]
impl Drop for Entry {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        let Ok(registered) = self.file.metadata() else {
            return;
        };
        let Ok(current) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if (registered.dev(), registered.ino()) != (current.dev(), current.ino()) {
            return;
        }
        if self.directory {
            let _ = std::fs::remove_dir(&self.path);
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[tjs_bind::class(name = "TemporaryFiles")]
mod implementation {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        #[trace(skip = "Temporary entries own OS files and paths, without TJS handles")]
        entries: Vec<Entry>,
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method]
        fn entry(&mut self, cx: &mut NativeCx<'_>, filename: Value) -> NativeResult<bool> {
            self.register(cx, filename, false)
        }
        #[tjs::method(name = "entryFolder")]
        fn entry_folder(&mut self, cx: &mut NativeCx<'_>, filename: Value) -> NativeResult<bool> {
            self.register(cx, filename, true)
        }
        fn register(
            &mut self,
            cx: &mut NativeCx<'_>,
            filename: Value,
            folder: bool,
        ) -> NativeResult<bool> {
            let path = path(cx, filename)?;
            if self.entries.len() == 4096 {
                return Ok(false);
            }
            match Entry::open(&path, folder) {
                Ok(entry) => {
                    self.entries.push(entry);
                    Ok(true)
                }
                Err(_) => Ok(false),
            }
        }
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            // Native hook runs after script finalize and is not replaceable.
            // Drop of the facet also closes entries if the Heap itself ends.
            self.entries.clear();
        }
    }
}
pub(super) fn install(heap: &mut Heap) -> NativeResult<ObjId> {
    implementation::install(heap)
}
