use crate::Result;
#[cfg(any(target_os = "vita", test))]
mod vita;
use std::{
    io::{self, Seek, SeekFrom, Write},
    path::PathBuf,
};

/// A replacement file, independent of the VFS and its current directory.
pub struct WritePlan {
    pub(crate) path: PathBuf,
    pub(crate) limit: u64,
}
pub struct WriteFile {
    #[cfg(not(target_os = "vita"))]
    file: tempfile::NamedTempFile,
    #[cfg(target_os = "vita")]
    file: vita::Staged,
    path: PathBuf,
    limit: u64,
    position: u64,
}
impl WritePlan {
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    /// Capture a host filename for APIs whose reference accepts local paths
    /// rather than storage names. Resolve cwd before entering an IO worker.
    pub fn local(path: impl AsRef<std::path::Path>, limit: u64) -> Result<Self> {
        let path = crate::local::absolute(path.as_ref())?;
        if path.file_name().is_none() {
            return Err(crate::Error::Name("destination must be a filename"));
        }
        Ok(Self { path, limit })
    }
    pub fn create(self) -> Result<WriteFile> {
        #[cfg(not(target_os = "vita"))]
        let file = tempfile::Builder::new()
            .prefix(".krkr-")
            .tempfile_in(self.path.parent().expect("resolved storage parent"))?;
        #[cfg(target_os = "vita")]
        let file = vita::Staged::new_in(self.path.parent().expect("resolved storage parent"))?;
        Ok(WriteFile {
            file,
            path: self.path,
            limit: self.limit,
            position: 0,
        })
    }
}
impl WriteFile {
    /// Publish only after the encoder and its buffered writer have succeeded.
    /// Dropping an unfinished write removes its temporary file.
    pub fn finish(mut self) -> Result<()> {
        self.file.flush()?;
        #[cfg(not(target_os = "vita"))]
        self.file.persist(&self.path).map_err(|error| error.error)?;
        #[cfg(target_os = "vita")]
        self.file.publish(&self.path)?;
        Ok(())
    }
}
impl Write for WriteFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.position) {
            return Err(io::Error::other("asset exceeds host write bytes budget"));
        }
        let written = self.file.write(bytes)?;
        self.position += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Seek for WriteFile {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.position = self.file.seek(from)?;
        Ok(self.position)
    }
}
