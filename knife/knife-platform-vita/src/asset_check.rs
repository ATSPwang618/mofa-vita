//! Bounded startup checks against the asset profile embedded in the engine.
use std::{fs::File, io::{Read, Seek, SeekFrom}, path::Path};

pub const BUFFER_BYTES: usize = 8192;
pub const PROBE_BYTES: usize = 4096;
const MAX_FULL_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub enum ContentCheck {
    Full(u32),
    Edges { first: u32, last: u32 },
}

#[derive(Clone, Copy, Debug)]
pub struct FileSpec {
    pub path: &'static str,
    pub bytes: u64,
    pub content: ContentCheck,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Missing,
    Size,
    Content,
    Read,
    Profile,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetError {
    pub path: &'static str,
    pub failure: Failure,
}

impl AssetError {
    pub fn description(&self) -> &'static str {
        match self.failure {
            Failure::Missing => "MISSING FILE OR FOLDER",
            Failure::Size => "WRONG FILE SIZE",
            Failure::Content => "WRONG ASSET VERSION",
            Failure::Read => "FILE READ FAILED",
            Failure::Profile => "ENGINE PROFILE ERROR",
        }
    }
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "assets: {}: {}", self.description(), self.path)
    }
}
impl std::error::Error for AssetError {}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CheckReport {
    pub files: usize,
    pub read_bytes: u64,
}

#[cfg(not(feature = "assets-540p"))]
include!("asset_profile_270p.rs");
#[cfg(feature = "assets-540p")]
include!("asset_profile_540p.rs");

/// This identifies the compatible pack and catches incomplete/mixed key files.
/// Large payloads use size + first/last 4 KiB, NOT a whole-file integrity claim.
/// tools/vita-270p-assets.mjs performs the full offline SHA-256 inventory check.
pub fn check_assets(root: &Path) -> Result<CheckReport, AssetError> {
    check_files(root, PROFILE)
}

fn check_files(root: &Path, specs: &[FileSpec]) -> Result<CheckReport, AssetError> {
    if !root.is_dir() {
        return Err(AssetError { path: "assets/", failure: Failure::Missing });
    }
    // One reused buffer, no manifest deserialization or retained per-file data.
    let mut buffer = [0; BUFFER_BYTES];
    let mut report = CheckReport::default();
    for spec in specs {
        let error = |failure| AssetError { path: spec.path, failure };
        if spec.path.contains(['\\', ':', '\0'])
            || spec.path.split('/').any(|part| part.is_empty() || part == "." || part == "..")
            || spec.bytes == 0
            || matches!(spec.content, ContentCheck::Full(_)) && spec.bytes > MAX_FULL_BYTES
            || matches!(spec.content, ContentCheck::Edges { .. }) && spec.bytes < (PROBE_BYTES * 2) as u64
        { return Err(error(Failure::Profile)); }
        let mut file = File::open(root.join(spec.path)).map_err(|e| error(
            if e.kind() == std::io::ErrorKind::NotFound { Failure::Missing } else { Failure::Read }
        ))?;
        let metadata = file.metadata().map_err(|_| error(Failure::Read))?;
        if !metadata.is_file() { return Err(error(Failure::Read)); }
        if metadata.len() != spec.bytes { return Err(error(Failure::Size)); }
        let valid = match spec.content {
            ContentCheck::Full(crc) => {
                let actual = read_crc(&mut file, spec.bytes, &mut buffer).map_err(|_| error(Failure::Read))?;
                report.read_bytes += spec.bytes;
                actual == crc
            }
            ContentCheck::Edges { first, last } => {
                let a = read_crc(&mut file, PROBE_BYTES as u64, &mut buffer).map_err(|_| error(Failure::Read))?;
                file.seek(SeekFrom::Start(spec.bytes - PROBE_BYTES as u64)).map_err(|_| error(Failure::Read))?;
                let b = read_crc(&mut file, PROBE_BYTES as u64, &mut buffer).map_err(|_| error(Failure::Read))?;
                report.read_bytes += (PROBE_BYTES * 2) as u64;
                a == first && b == last
            }
        };
        if !valid { return Err(error(Failure::Content)); }
        report.files += 1;
    }
    Ok(report)
}

fn read_crc(reader: &mut impl Read, mut bytes: u64, buffer: &mut [u8; BUFFER_BYTES]) -> std::io::Result<u32> {
    let mut crc = crc32fast::Hasher::new();
    while bytes != 0 {
        let n = bytes.min(BUFFER_BYTES as u64) as usize;
        reader.read_exact(&mut buffer[..n])?;
        crc.update(&buffer[..n]);
        bytes -= n as u64;
    }
    Ok(crc.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!("knife-assets-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
        fn write(&self, name: &str, data: &[u8]) {
            let p = self.0.join(name);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, data).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); }
    }

    const SMALL: FileSpec = FileSpec { path: "language-profile.txt", bytes: 9, content: ContentCheck::Full(0xcbf43926) };

    #[test]
    fn complete_content_and_nested_files_are_checked() {
        let f = Fixture::new();
        f.write(SMALL.path, b"123456789"); // Published CRC-32/ISO-HDLC check vector.
        f.write("movie/logo.mp4", b"123456789");
        let other = FileSpec { path: "movie/logo.mp4", ..SMALL };
        assert_eq!(check_files(&f.0, &[SMALL, other]).unwrap(), CheckReport { files: 2, read_bytes: 18 });
    }

    #[test]
    fn missing_file_reports_its_exact_name() {
        let f = Fixture::new();
        assert_eq!(check_files(&f.0, &[SMALL]).unwrap_err(), AssetError { path: SMALL.path, failure: Failure::Missing });
    }

    #[test]
    fn old_or_incomplete_archive_fails_before_content_is_loaded() {
        let f = Fixture::new();
        for content in [b"12345678".as_slice(), b"1234567890".as_slice()] {
            f.write(SMALL.path, content);
            assert_eq!(check_files(&f.0, &[SMALL]).unwrap_err().failure, Failure::Size);
        }
    }

    #[test]
    fn same_length_mixed_index_is_rejected() {
        let f = Fixture::new();
        f.write(SMALL.path, b"123056789");
        assert_eq!(check_files(&f.0, &[SMALL]).unwrap_err().failure, Failure::Content);
    }

    #[test]
    fn directory_is_not_accepted_as_an_asset() {
        let f = Fixture::new();
        fs::create_dir(f.0.join(SMALL.path)).unwrap();
        assert_eq!(check_files(&f.0, &[SMALL]).unwrap_err().failure, Failure::Read);
    }

    #[test]
    fn profile_cannot_escape_the_asset_root() {
        let f = Fixture::new();
        for path in ["../escape", "/absolute", "ux0:data/x", "a\\b", "a//b", "a/./b", ""] {
            let spec = FileSpec { path, ..SMALL };
            assert_eq!(check_files(&f.0, &[spec]).unwrap_err().failure, Failure::Profile, "{path}");
        }
    }

    #[test]
    fn large_archive_checks_only_bounded_edges_and_rejects_a_changed_tail() {
        let f = Fixture::new();
        // CRC32 of 4096 zero bytes, independently calculated by zlib.
        let spec = FileSpec { path: "jallpac.mrg", bytes: 3 * 1024 * 1024,
            content: ContentCheck::Edges { first: 0xc71c0011, last: 0xc71c0011 } };
        let mut data = vec![0; spec.bytes as usize];
        // The middle deliberately differs; edge checks do not claim to hash it.
        data[8192] = 127;
        f.write(spec.path, &data);
        assert_eq!(check_files(&f.0, &[spec]).unwrap().read_bytes, 8192);
        *data.last_mut().unwrap() = 1;
        f.write(spec.path, &data);
        assert_eq!(check_files(&f.0, &[spec]).unwrap_err().failure, Failure::Content);
    }

    #[test]
    fn short_read_and_oversized_full_hash_cannot_pass() {
        assert!(read_crc(&mut b"123".as_slice(), 9, &mut [0; BUFFER_BYTES]).is_err());
        let f = Fixture::new();
        let spec = FileSpec { bytes: MAX_FULL_BYTES + 1, ..SMALL };
        assert_eq!(check_files(&f.0, &[spec]).unwrap_err().failure, Failure::Profile);
    }

    #[test]
    fn checksum_covers_multiple_buffers() {
        // 3 repetitions of the 9-byte check vector, crossing tiny reader chunks.
        struct Chunked<'a>(&'a [u8]);
        impl Read for Chunked<'_> {
            fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
                let n = dst.len().min(2).min(self.0.len());
                dst[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let mut reader = Chunked(b"123456789");
        assert_eq!(read_crc(&mut reader, 9, &mut [0; BUFFER_BYTES]).unwrap(), 0xcbf43926);
    }
}
