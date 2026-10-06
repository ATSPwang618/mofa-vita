//! Local file handles. Vita supplies native 64-bit reads and protected save
//! writes; desktop builds keep std::fs::File and tempfile directly.
use crate::xp3::Version;
use std::{io, path::Path};

#[cfg(not(target_os = "vita"))]
pub type File = std::fs::File;
#[cfg(target_os = "vita")]
pub type File = Box<dyn ReadFile>;

#[cfg(target_os = "vita")]
pub trait ReadFile: crate::Stream {
    fn version(&self) -> io::Result<Version>;
}
#[cfg(target_os = "vita")]
pub trait WriteFile: io::Write + io::Seek + Send {
    /// Report close failures before publishing a replacement file.
    fn close(self: Box<Self>) -> io::Result<()>;
}
#[cfg(target_os = "vita")]
pub struct FileHost {
    pub open: fn(&Path) -> io::Result<File>,
    pub version: fn(&Path) -> io::Result<Version>,
    pub create_new: fn(&Path) -> io::Result<Box<dyn WriteFile>>,
    /// Must fail without changing either file if the destination exists.
    pub rename_no_replace: fn(&Path, &Path) -> io::Result<()>,
}
#[cfg(target_os = "vita")]
static HOST: std::sync::OnceLock<FileHost> = std::sync::OnceLock::new();

/// Install before archive browsing or constructing a VFS. Handles retain their
/// own cursor; no global seek position or IO mutex is introduced.
#[cfg(target_os = "vita")]
pub fn install(host: FileHost) -> std::result::Result<(), FileHost> {
    HOST.set(host)
}
#[cfg(target_os = "vita")]
fn host() -> io::Result<&'static FileHost> {
    HOST.get().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Vita 64-bit file host is not installed",
        )
    })
}

#[cfg(target_os = "vita")]
pub(crate) fn create_new(path: &Path) -> io::Result<Box<dyn WriteFile>> {
    (host()?.create_new)(path)
}

#[cfg(target_os = "vita")]
pub(crate) fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    (host()?.rename_no_replace)(from, to)
}

pub(crate) fn open(path: &Path) -> io::Result<File> {
    #[cfg(target_os = "vita")]
    {
        (host()?.open)(path)
    }
    #[cfg(not(target_os = "vita"))]
    {
        File::open(path)
    }
}

pub(crate) fn version(path: &Path, metadata: &std::fs::Metadata) -> io::Result<Version> {
    #[cfg(target_os = "vita")]
    {
        let _ = metadata;
        (host()?.version)(path)
    }
    #[cfg(not(target_os = "vita"))]
    {
        let _ = path;
        Ok(Version::from_metadata(metadata))
    }
}
