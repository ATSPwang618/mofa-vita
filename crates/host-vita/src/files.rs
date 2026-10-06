//! Native Vita file handles: 64-bit XP3 reads and protected save replacement.
#![allow(unsafe_code)]
use krkr_engine::assets::{
    file::{FileHost, ReadFile, WriteFile},
    xp3::Version,
};
use std::{
    ffi::CString,
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::ffi::OsStrExt,
    path::Path,
    time::{Duration, SystemTime},
};
use vitasdk_sys::*;

pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        assert!(
            krkr_engine::assets::file::install(FileHost {
                open: |path| Ok(Box::new(File::open(path)?)),
                version: path_version,
                create_new: |path| Ok(Box::new(File::create_new(path)?)),
                rename_no_replace,
            })
            .is_ok(),
            "Vita file host already installed"
        );
    });
}

// Use newlib's own SCE-to-errno mapping; SCE error codes are not POSIX errno.
unsafe extern "C" {
    fn __vita_sce_errno_to_errno(error: i32, kind: i32) -> i32;
}
fn checked(value: i32) -> io::Result<i32> {
    if value < 0 {
        krkr_protocol::diagnostic!("[VITA][IO] native error={value:#010x}");
        Err(io::Error::from_raw_os_error(unsafe {
            __vita_sce_errno_to_errno(value, 0)
        }))
    } else {
        Ok(value)
    }
}
fn native_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file path contains NUL"))
}
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let from = native_path(from)?;
    let to = native_path(to)?;
    // Unlike newlib _rename_r, the kernel call never removes `to` first.
    let result = unsafe { sceIoRename(from.as_ptr(), to.as_ptr()) };
    if result < 0 {
        // Existing destinations are the normal protected-replacement path.
        Err(io::Error::from_raw_os_error(unsafe {
            __vita_sce_errno_to_errno(result, 0)
        }))
    } else {
        Ok(())
    }
}
fn version(stat: &SceIoStat) -> io::Result<Version> {
    let bytes = u64::try_from(stat.st_size)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative Vita file size"))?;
    let mut ticks = SceRtcTick { tick: 0 };
    let timer = krkr_protocol::diagnostics::Timer::start();
    let result = unsafe { sceRtcGetTick(&stat.st_mtime, &mut ticks) };
    timer.report(|| "stage=native-file-time".into());
    let modified = if result >= 0 {
        // SceRtcTick counts microseconds since 0001-01-01.
        const UNIX_EPOCH: u64 = 62_135_596_800_000_000;
        if ticks.tick >= UNIX_EPOCH {
            SystemTime::UNIX_EPOCH.checked_add(Duration::from_micros(ticks.tick - UNIX_EPOCH))
        } else {
            SystemTime::UNIX_EPOCH.checked_sub(Duration::from_micros(UNIX_EPOCH - ticks.tick))
        }
    } else {
        None
    };
    Ok(Version { bytes, modified })
}
fn path_version(path: &Path) -> io::Result<Version> {
    let path = native_path(path)?;
    let mut stat = std::mem::MaybeUninit::<SceIoStat>::uninit();
    let timer = krkr_protocol::diagnostics::Timer::start();
    let result = unsafe { sceIoGetstat(path.as_ptr(), stat.as_mut_ptr()) };
    timer.report(|| format!("stage=native-stat path={}", path.to_string_lossy()));
    checked(result)?;
    version(&unsafe { stat.assume_init() })
}
struct File {
    fd: SceUID,
}
impl File {
    fn create_new(path: &Path) -> io::Result<Self> {
        let path = native_path(path)?;
        let fd = checked(unsafe {
            sceIoOpen(
                path.as_ptr(),
                (SCE_O_WRONLY | SCE_O_CREAT | SCE_O_EXCL) as i32,
                0o666,
            )
        })?;
        Ok(Self { fd })
    }
    fn open(path: &Path) -> io::Result<Self> {
        let path = native_path(path)?;
        let timer = krkr_protocol::diagnostics::Timer::start();
        let result = unsafe { sceIoOpen(path.as_ptr(), SCE_O_RDONLY as i32, 0) };
        timer.report(|| format!("stage=native-open path={}", path.to_string_lossy()));
        let fd = checked(result)?;
        Ok(Self { fd })
    }
}
impl Drop for File {
    fn drop(&mut self) {
        if self.fd >= 0 {
            let timer = krkr_protocol::diagnostics::Timer::start();
            unsafe {
                sceIoClose(self.fd);
            }
            timer.report(|| format!("stage=native-close fd={}", self.fd));
        }
    }
}
impl WriteFile for File {
    fn close(mut self: Box<Self>) -> io::Result<()> {
        let fd = std::mem::replace(&mut self.fd, -1);
        checked(unsafe { sceIoClose(fd) }).map(|_| ())
    }
}
impl Write for File {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let count = bytes.len().min(i32::MAX as usize) as u32;
        checked(unsafe { sceIoWrite(self.fd, bytes.as_ptr().cast(), count) })
            .map(|written| written as usize)
    }
    fn flush(&mut self) -> io::Result<()> {
        // This handle has no userspace buffering, like std::fs::File::flush.
        Ok(())
    }
}
impl ReadFile for File {
    fn version(&self) -> io::Result<Version> {
        let mut stat = std::mem::MaybeUninit::<SceIoStat>::uninit();
        let timer = krkr_protocol::diagnostics::Timer::start();
        let result = unsafe { sceIoGetstatByFd(self.fd, stat.as_mut_ptr()) };
        timer.report(|| format!("stage=native-fstat fd={}", self.fd));
        checked(result)?;
        version(&unsafe { stat.assume_init() })
    }
}
impl Read for File {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let count = bytes.len().min(i32::MAX as usize) as u32;
        let timer = krkr_protocol::diagnostics::Timer::start();
        let result = unsafe { sceIoRead(self.fd, bytes.as_mut_ptr().cast(), count) };
        timer.report(|| {
            format!(
                "stage=native-read fd={} bytes={count} result={result}",
                self.fd
            )
        });
        checked(result).map(|read| read as usize)
    }
}
impl Seek for File {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let (offset, whence) = match from {
            SeekFrom::Start(offset) => (
                i64::try_from(offset).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "file offset exceeds i64")
                })?,
                SCE_SEEK_SET,
            ),
            SeekFrom::Current(offset) => (offset, SCE_SEEK_CUR),
            SeekFrom::End(offset) => (offset, SCE_SEEK_END),
        };
        let timer = krkr_protocol::diagnostics::Timer::start();
        let position = unsafe { sceIoLseek(self.fd, offset, whence as i32) };
        timer.report(|| {
            format!(
                "stage=native-seek fd={} offset={offset} whence={whence}",
                self.fd
            )
        });
        if position < 0 {
            krkr_protocol::diagnostic!("[VITA][IO] seek FAILED result={position:#x}");
            Err(io::Error::from_raw_os_error(unsafe {
                __vita_sce_errno_to_errno(position as i32, 0)
            }))
        } else {
            Ok(position as u64)
        }
    }
}
