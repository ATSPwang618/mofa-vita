//! Same-directory temporary files, with rollback when Vita cannot replace a
//! destination by rename. Never use newlib rename: it deletes the target first.
use std::{
    fs,
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

#[cfg(target_os = "vita")]
type File = Box<dyn crate::file::WriteFile>;
#[cfg(not(target_os = "vita"))]
type File = fs::File;

fn unique_path(parent: &Path, kind: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    parent.join(format!(
        ".krkr-{kind}-{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

pub(super) struct Staged {
    file: Option<File>,
    temporary: Option<PathBuf>,
}
impl Staged {
    pub fn new_in(parent: &Path) -> io::Result<Self> {
        for _ in 0..1024 {
            let temporary = unique_path(parent, "write");
            #[cfg(target_os = "vita")]
            let file = crate::file::create_new(&temporary);
            #[cfg(not(target_os = "vita"))]
            let file = File::create_new(&temporary);
            match file {
                Ok(file) => {
                    return Ok(Self {
                        file: Some(file),
                        temporary: Some(temporary),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "too many unfinished save writes",
        ))
    }
    fn close(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.take() {
            #[cfg(target_os = "vita")]
            file.close()?;
            #[cfg(not(target_os = "vita"))]
            drop(file);
        }
        Ok(())
    }
    pub fn publish(self, destination: &Path) -> io::Result<()> {
        self.publish_with(destination, rename_no_replace)
    }
    fn publish_with(
        mut self,
        destination: &Path,
        mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
    ) -> io::Result<()> {
        self.flush()?;
        self.close()?;
        let temporary = self.temporary.as_ref().expect("unpublished save");
        match rename(temporary, destination) {
            Ok(()) => {
                self.temporary = None;
                return Ok(());
            }
            Err(error) => {
                if !fs::symlink_metadata(destination).is_ok_and(|m| m.file_type().is_file()) {
                    return Err(error);
                }
            }
        }
        // Claim a backup by an atomic no-replace move. A previous failed
        // rollback (or another writer) can never have its backup overwritten.
        let parent = temporary.parent().expect("temporary parent");
        let mut backup = None;
        for _ in 0..1024 {
            let candidate = unique_path(parent, "backup");
            match rename(destination, &candidate) {
                Ok(()) => {
                    backup = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        let backup = backup.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "too many retained save backups",
            )
        })?;
        if let Err(error) = rename(temporary, destination) {
            if let Err(restore) = rename(&backup, destination) {
                return Err(io::Error::other(format!(
                    "save publish failed: {error}; rollback failed: {restore}; original retained at {}",
                    backup.display()
                )));
            }
            return Err(error);
        }
        self.temporary = None;
        let _ = fs::remove_file(backup);
        Ok(())
    }
}
#[cfg(target_os = "vita")]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    crate::file::rename_no_replace(from, to)
}
// This module is only compiled on other hosts for its filesystem regressions.
// The native kernel supplies atomic no-replace semantics on Vita.
#[cfg(not(target_os = "vita"))]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    match fs::symlink_metadata(to) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "destination exists",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::rename(from, to),
        Err(error) => Err(error),
    }
}
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = self.close();
        if let Some(temporary) = self.temporary.take() {
            let _ = fs::remove_file(temporary);
        }
        // Backups are never owned by this cleanup: preserve failed rollbacks.
    }
}
impl Write for Staged {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file.as_mut().expect("open staging file").write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().expect("open staging file").flush()
    }
}
impl Seek for Staged {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.file.as_mut().expect("open staging file").seek(from)
    }
}

#[cfg(test)]
#[path = "../../tests/writing_vita/internal.rs"]
mod tests;
