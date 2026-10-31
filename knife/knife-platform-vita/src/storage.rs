//! Bounded save persistence. The byte layout belongs to `savedata.rs`.
//!
//! One process owns a save and its `.tmp`, `.bak`, and `.corrupt` sidecars. A complete
//! temporary file is flushed, synced, closed, and verified before any committed name
//! moves. A same-directory backup remains after commit. An interrupted replacement
//! restores that backup; an uncommitted first save is never promoted automatically.
//! Length validation cannot detect same-sized corruption in the original format.
//! Vita filesystem power-loss guarantees still require hardware verification.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MAX_SAVE_BYTES: usize = 8 * 1024 * 1024;
pub const DEV_SAVE_ROOT: &str = "ux0:data/knife-woh-dev/saves";
#[cfg(not(feature = "assets-540p"))]
pub const PRODUCTION_SAVE_ROOT: &str = "ux0:data/knife-woh/saves";
#[cfg(feature = "assets-540p")]
pub const PRODUCTION_SAVE_ROOT: &str = "ux0:data/knife-woh-540p/saves";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Namespace {
    Dev,
    Production,
}

impl Namespace {
    pub fn from_setting(setting: Option<&str>) -> io::Result<Self> {
        match setting {
            None | Some("dev") => Ok(Self::Dev),
            Some("production") => Ok(Self::Production),
            Some(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid KNIFE_VITA_SAVE_NAMESPACE",
            )),
        }
    }

    pub fn root(self) -> &'static str {
        match self {
            Self::Dev => DEV_SAVE_ROOT,
            Self::Production => PRODUCTION_SAVE_ROOT,
        }
    }

    pub fn path(self, relative: &str) -> io::Result<PathBuf> {
        if relative.contains(['\\', ':', '\0'])
            || relative
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a normalized relative save path",
            ));
        }
        Ok(PathBuf::from(format!("{}/{relative}", self.root())))
    }

    /// Absolute Vita overrides must remain in the selected save namespace.
    pub fn validate(self, path: &Path) -> io::Result<()> {
        let value = path
            .to_str()
            .ok_or_else(|| invalid_input("save path is not UTF-8"))?;
        let prefix = format!("{}/", self.root());
        let relative = value
            .strip_prefix(&prefix)
            .ok_or_else(|| invalid_input("save path is outside the selected Vita save root"))?;
        if self.path(relative)?.as_os_str() != path.as_os_str() {
            return Err(invalid_input("save path is not normalized"));
        }
        Ok(())
    }
}

pub fn configured_namespace() -> io::Result<Namespace> {
    Namespace::from_setting(option_env!("KNIFE_VITA_SAVE_NAMESPACE"))
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

static TRANSACTION: Mutex<()> = Mutex::new(());

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn validate_size(expected: usize) -> io::Result<()> {
    if expected == 0 || expected > MAX_SAVE_BYTES {
        return Err(invalid_input("save length must be 1..=8 MiB"));
    }
    Ok(())
}

struct Files<'a> {
    main: &'a Path,
    temporary: PathBuf,
    backup: PathBuf,
    corrupt: PathBuf,
}

impl<'a> Files<'a> {
    fn new(path: &'a Path) -> io::Result<Self> {
        if path.file_name().is_none()
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(invalid_input(
                "expected a file path without parent traversal",
            ));
        }
        #[cfg(target_os = "vita")]
        configured_namespace()?.validate(path)?;
        // symlink_metadata distinguishes an absent path from an unreadable one and
        // prevents an existing link from redirecting writes outside the save root.
        for parent in path
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            match fs::symlink_metadata(parent) {
                Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                    return Err(invalid_data(format!(
                        "{} is not a real directory",
                        parent.display()
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(Self {
            main: path,
            temporary: sidecar(path, ".tmp"),
            backup: sidecar(path, ".bak"),
            corrupt: sidecar(path, ".corrupt"),
        })
    }

    fn directory(&self) -> &Path {
        self.main
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Absent,
    Complete,
    WrongLength(u64),
}

fn state(path: &Path, expected: usize) -> io::Result<State> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => Err(invalid_data(format!(
            "{} is not a regular save file",
            path.display()
        ))),
        Ok(meta) if meta.len() == expected as u64 => Ok(State::Complete),
        Ok(meta) => Ok(State::WrongLength(meta.len())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(State::Absent),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    WriteTemporary,
    FlushTemporary,
    SyncTemporary,
    VerifyTemporary,
    RemoveBackup,
    BackupRename,
    SyncBackup,
    CommitRename,
    SyncCommit,
    RollbackRename,
    SyncRollback,
    QuarantineRename,
    SyncQuarantine,
    RecoveryRename,
    SyncRecovery,
}

trait Operations {
    fn before(&mut self, _step: Step) -> io::Result<()> {
        Ok(())
    }

    fn write_all(&mut self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        file.write_all(bytes)
    }

    fn sync_names(&mut self, directory: &Path) -> io::Result<()> {
        sync_directory(directory)
    }
}

struct NativeOperations;
impl Operations for NativeOperations {}

fn context(step: Step, path: &Path, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{step:?} {}: {error}", path.display()),
    )
}

fn fence(ops: &mut impl Operations, files: &Files<'_>, step: Step) -> io::Result<()> {
    let _sync = knife_perf::Scope::new(knife_perf::Metric::SaveSync);
    ops.before(step)
        .and_then(|()| ops.sync_names(files.directory()))
        .map_err(|error| context(step, files.main, error))
}

fn rename(ops: &mut impl Operations, from: &Path, to: &Path, step: Step) -> io::Result<()> {
    (|| {
        ops.before(step)?;
        match fs::symlink_metadata(to) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("destination {} already exists", to.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        rename_to_absent(from, to)
    })()
    .map_err(|error| context(step, from, error))
}

#[cfg(target_os = "vita")]
#[link(name = "SceIofilemgr_stub", kind = "static")]
unsafe extern "C" {
    fn sceIoRename(old: *const std::ffi::c_char, new: *const std::ffi::c_char) -> i32;
    fn sceIoSync(device: *const std::ffi::c_char, flags: u32) -> i32;
}

#[cfg(target_os = "vita")]
fn sce_result(operation: &str, status: i32) -> io::Result<()> {
    if status < 0 {
        Err(io::Error::other(format!(
            "{operation} returned 0x{:08X}",
            status as u32
        )))
    } else {
        Ok(())
    }
}

#[cfg(target_os = "vita")]
fn rename_to_absent(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    let from = CString::new(
        from.to_str()
            .ok_or_else(|| invalid_input("non-UTF-8 save path"))?,
    )
    .map_err(|_| invalid_input("NUL in save path"))?;
    let to = CString::new(
        to.to_str()
            .ok_or_else(|| invalid_input("non-UTF-8 save path"))?,
    )
    .map_err(|_| invalid_input("NUL in save path"))?;
    // VitaSDK's newlib rename first calls sceIoRemove(new). Bypass that wrapper:
    // every destination here was checked absent, and no replacement is requested.
    sce_result("sceIoRename", unsafe {
        sceIoRename(from.as_ptr(), to.as_ptr())
    })
}

#[cfg(not(target_os = "vita"))]
fn rename_to_absent(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(target_os = "vita")]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    // newlib fsync checks sceIoSyncByFd(fd, 0) for the temporary's contents;
    // additionally request and check the ux0 device barrier after name changes.
    sce_result("sceIoSync(ux0:, 0)", unsafe {
        sceIoSync(c"ux0:".as_ptr(), 0)
    })
}

#[cfg(all(unix, not(target_os = "vita")))]
fn sync_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(any(unix, target_os = "vita")))]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "save name durability barrier is unavailable on this host",
    ))
}

fn recover(files: &Files<'_>, expected: usize, ops: &mut impl Operations) -> io::Result<bool> {
    let main = state(files.main, expected)?;
    let backup = state(&files.backup, expected)?;
    let temporary = state(&files.temporary, expected)?;
    let corrupt = state(&files.corrupt, expected)?;
    if main == State::Complete {
        return Ok(true);
    }
    if backup != State::Complete {
        if main == State::Absent
            && backup == State::Absent
            && temporary == State::Absent
            && corrupt == State::Absent
        {
            return Ok(false);
        }
        return Err(invalid_data(format!(
            "{} has no complete committed save or backup (main={main:?}, backup={backup:?}, temporary={temporary:?}, corrupt={corrupt:?}); files retained",
            files.main.display()
        )));
    }
    if main != State::Absent {
        if corrupt != State::Absent {
            return Err(invalid_data(format!(
                "{} already exists; refusing to overwrite corruption evidence",
                files.corrupt.display()
            )));
        }
        rename(ops, files.main, &files.corrupt, Step::QuarantineRename)?;
        fence(ops, files, Step::SyncQuarantine)?;
    }
    rename(ops, &files.backup, files.main, Step::RecoveryRename)?;
    fence(ops, files, Step::SyncRecovery)?;
    Ok(true)
}

fn read_complete(path: &Path, expected: usize) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() != expected as u64 {
        return Err(invalid_data(format!(
            "{} changed length before reading",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected)
        .map_err(|_| io::Error::other("cannot allocate bounded save buffer"))?;
    bytes.resize(expected, 0);
    file.read_exact(&mut bytes)?;
    if file.read(&mut [0u8; 1])? != 0 {
        return Err(invalid_data("save grew during reading"));
    }
    Ok(bytes)
}

fn verify_temporary(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let _verify = knife_perf::Scope::new(knife_perf::Metric::SaveVerify);
    let mut file = File::open(path)?;
    if file.metadata()?.len() != bytes.len() as u64 {
        return Err(invalid_data("temporary save length changed"));
    }
    let mut scratch = [0u8; 8192];
    for expected in bytes.chunks(scratch.len()) {
        let actual = &mut scratch[..expected.len()];
        file.read_exact(actual)?;
        if actual != expected {
            return Err(invalid_data(
                "temporary save readback differs from requested bytes",
            ));
        }
    }
    if file.read(&mut [0u8; 1])? != 0 {
        return Err(invalid_data("temporary save grew during verification"));
    }
    Ok(())
}

fn load_with(
    path: &Path,
    expected: usize,
    ops: &mut impl Operations,
) -> io::Result<Option<Vec<u8>>> {
    validate_size(expected)?;
    let _transaction = TRANSACTION
        .lock()
        .map_err(|_| io::Error::other("save transaction lock was poisoned"))?;
    let files = Files::new(path)?;
    if !recover(&files, expected, ops)? {
        return Ok(None);
    }
    read_complete(path, expected).map(Some)
}

/// Only the absence of both the committed file and all sidecars means a new game.
pub fn load(path: &Path, expected: usize) -> io::Result<Option<Vec<u8>>> {
    load_with(path, expected, &mut NativeOperations)
}

fn restore_after_error(
    files: &Files<'_>,
    ops: &mut impl Operations,
    original: io::Error,
) -> io::Error {
    let restore = rename(ops, &files.backup, files.main, Step::RollbackRename)
        .and_then(|()| fence(ops, files, Step::SyncRollback));
    match restore {
        Ok(()) => io::Error::new(
            original.kind(),
            format!("{original}; previous save restored, temporary retained"),
        ),
        Err(error) => io::Error::new(
            original.kind(),
            format!("{original}; restoring previous save also failed: {error}; files retained"),
        ),
    }
}

fn store_with(
    path: &Path,
    bytes: &[u8],
    expected: usize,
    ops: &mut impl Operations,
) -> io::Result<()> {
    validate_size(expected)?;
    if bytes.len() != expected {
        return Err(invalid_input(
            "save bytes do not match the selected layout length",
        ));
    }
    let _transaction = TRANSACTION
        .lock()
        .map_err(|_| io::Error::other("save transaction lock was poisoned"))?;
    let files = Files::new(path)?;
    fs::create_dir_all(files.directory())?;
    let had_previous = recover(&files, expected, ops)?;
    // A failed or interrupted attempt may contain useful new data. Loading a valid
    // main is allowed, but a later save must not silently destroy the pending file.
    if state(&files.temporary, expected)? != State::Absent {
        return Err(invalid_data(format!(
            "{} is an uncommitted save; retained for explicit recovery before another store",
            files.temporary.display()
        )));
    }
    if matches!(state(&files.backup, expected)?, State::WrongLength(_)) {
        return Err(invalid_data(format!(
            "{} has an invalid length; backup retained",
            files.backup.display()
        )));
    }
    let mut temporary = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&files.temporary)?;
    let ready = (|| {
        ops.before(Step::WriteTemporary)?;
        { let _write = knife_perf::Scope::new(knife_perf::Metric::SaveWrite);
            ops.write_all(&mut temporary, bytes)?; }
        ops.before(Step::FlushTemporary)?;
        temporary.flush()?;
        ops.before(Step::SyncTemporary)?;
        { let _sync = knife_perf::Scope::new(knife_perf::Metric::SaveSync);
            temporary.sync_all()?; }
        Ok::<(), io::Error>(())
    })();
    drop(temporary);
    ready.map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "preparing {}: {error}; committed save untouched, temporary retained",
                files.temporary.display()
            ),
        )
    })?;
    ops.before(Step::VerifyTemporary)
        .and_then(|()| verify_temporary(&files.temporary, bytes))
        .map_err(|error| context(Step::VerifyTemporary, &files.temporary, error))?;
    if had_previous {
        if state(&files.backup, expected)? != State::Absent {
            ops.before(Step::RemoveBackup)
                .and_then(|()| fs::remove_file(&files.backup))
                .map_err(|error| context(Step::RemoveBackup, &files.backup, error))?;
        }
        rename(ops, files.main, &files.backup, Step::BackupRename)?;
        if let Err(error) = fence(ops, &files, Step::SyncBackup) {
            return Err(restore_after_error(&files, ops, error));
        }
    }
    if let Err(error) = rename(ops, &files.temporary, files.main, Step::CommitRename) {
        return Err(if had_previous {
            restore_after_error(&files, ops, error)
        } else {
            error
        });
    }
    // A failure here is an error even though the new name is already visible. Keep
    // both generations: the caller cannot claim durable success from a readback.
    fence(ops, &files, Step::SyncCommit)
}

/// Store exactly one original-format save. Every sync/rename failure is returned.
pub fn store(path: &Path, bytes: &[u8], expected: usize) -> io::Result<()> {
    let _store = knife_perf::Scope::new(knife_perf::Metric::SaveStore);
    store_with(path, bytes, expected, &mut NativeOperations)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "knife-vita-storage-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        root.join("savedata000")
    }

    #[test]
    fn namespaces_default_to_dev_and_require_an_explicit_production_setting() {
        assert_eq!(Namespace::from_setting(None).unwrap(), Namespace::Dev);
        assert_eq!(
            Namespace::from_setting(Some("production")).unwrap(),
            Namespace::Production
        );
        assert_eq!(
            Namespace::Dev
                .path("witch/savedata000")
                .unwrap()
                .to_str()
                .unwrap(),
            "ux0:data/knife-woh-dev/saves/witch/savedata000"
        );
        assert!(Namespace::from_setting(Some("prod")).is_err());
        for input in [
            "", "../x", "a/../b", "/a", "a//b", "./a", "ux0:x", "a\\b", "a\0b",
        ] {
            assert!(Namespace::Dev.path(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn second_store_keeps_the_previous_complete_bytes_as_backup() {
        let path = fixture();
        store(&path, b"old-save", 8).unwrap();
        store(&path, b"new-save", 8).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new-save");
        assert_eq!(fs::read(sidecar(&path, ".bak")).unwrap(), b"old-save");
    }

    #[test]
    fn interrupted_rename_recovers_the_complete_backup_before_loading() {
        let path = fixture();
        fs::write(sidecar(&path, ".bak"), b"old-save").unwrap();
        fs::write(sidecar(&path, ".tmp"), b"new-save").unwrap();
        assert_eq!(load(&path, 8).unwrap().unwrap(), b"old-save");
        assert_eq!(fs::read(&path).unwrap(), b"old-save");
    }

    #[test]
    fn orphan_temporary_is_an_error_and_cannot_be_overwritten() {
        let path = fixture();
        fs::write(sidecar(&path, ".tmp"), b"partial").unwrap();
        assert!(load(&path, 8).is_err());
        assert!(store(&path, b"new-save", 8).is_err());
        assert_eq!(fs::read(sidecar(&path, ".tmp")).unwrap(), b"partial");
        assert!(!path.exists());
    }

    #[test]
    fn corrupt_main_is_preserved_and_blocks_store_without_a_valid_backup() {
        let path = fixture();
        fs::write(&path, b"bad").unwrap();
        assert!(load(&path, 8).is_err());
        assert!(store(&path, b"new-save", 8).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"bad");
    }

    #[derive(Default)]
    struct Faults {
        failures: Vec<Step>,
        events: Vec<Step>,
        partial_write: bool,
        damage_before_verify: Option<PathBuf>,
    }

    impl Operations for Faults {
        fn before(&mut self, step: Step) -> io::Result<()> {
            self.events.push(step);
            if step == Step::VerifyTemporary {
                if let Some(path) = self.damage_before_verify.take() {
                    fs::write(path, b"bad-data")?;
                }
            }
            if self.failures.contains(&step) {
                Err(io::Error::other(format!("injected {step:?} failure")))
            } else {
                Ok(())
            }
        }

        fn write_all(&mut self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
            if self.partial_write {
                file.write_all(&bytes[..3])?;
                Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "injected partial write",
                ))
            } else {
                file.write_all(bytes)
            }
        }
    }

    #[test]
    fn an_absent_file_is_the_only_fresh_save_case() {
        let path = fixture();
        assert!(load(&path, 8).unwrap().is_none());
        for suffix in [".tmp", ".bak", ".corrupt"] {
            let path = fixture();
            fs::write(sidecar(&path, suffix), b"bad").unwrap();
            assert!(load(&path, 8).is_err(), "treated {suffix} as a new save");
        }
    }

    #[test]
    fn corrupt_main_is_quarantined_before_backup_recovery() {
        let path = fixture();
        fs::write(&path, b"bad").unwrap();
        fs::write(sidecar(&path, ".bak"), b"old-save").unwrap();
        assert_eq!(load(&path, 8).unwrap().unwrap(), b"old-save");
        assert_eq!(fs::read(sidecar(&path, ".corrupt")).unwrap(), b"bad");
        assert!(!sidecar(&path, ".bak").exists());
    }

    #[test]
    fn a_second_corrupt_file_does_not_replace_prior_corruption_evidence() {
        let path = fixture();
        fs::write(&path, b"bad").unwrap();
        fs::write(sidecar(&path, ".bak"), b"old-save").unwrap();
        fs::write(sidecar(&path, ".corrupt"), b"earlier").unwrap();
        assert!(load(&path, 8).is_err());
        assert!(store(&path, b"new-save", 8).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"bad");
        assert_eq!(fs::read(sidecar(&path, ".corrupt")).unwrap(), b"earlier");
    }

    #[test]
    fn a_valid_main_loads_but_does_not_overwrite_a_retained_temporary() {
        let path = fixture();
        fs::write(&path, b"old-save").unwrap();
        fs::write(sidecar(&path, ".tmp"), b"partial").unwrap();
        assert_eq!(load(&path, 8).unwrap().unwrap(), b"old-save");
        assert!(store(&path, b"new-save", 8).is_err());
        assert_eq!(fs::read(sidecar(&path, ".tmp")).unwrap(), b"partial");
    }

    #[test]
    fn a_valid_main_does_not_allow_overwriting_a_corrupt_backup() {
        let path = fixture();
        fs::write(&path, b"old-save").unwrap();
        fs::write(sidecar(&path, ".bak"), b"bad").unwrap();
        assert_eq!(load(&path, 8).unwrap().unwrap(), b"old-save");
        assert!(store(&path, b"new-save", 8).is_err());
        assert_eq!(fs::read(sidecar(&path, ".bak")).unwrap(), b"bad");
        assert!(!sidecar(&path, ".tmp").exists());
    }

    #[test]
    fn partial_write_flush_and_sync_errors_leave_the_old_save_untouched() {
        for failure in [None, Some(Step::FlushTemporary), Some(Step::SyncTemporary)] {
            let path = fixture();
            fs::write(&path, b"old-save").unwrap();
            let mut faults = Faults {
                partial_write: failure.is_none(),
                failures: failure.into_iter().collect(),
                ..Default::default()
            };
            let error = store_with(&path, b"new-save", 8, &mut faults).unwrap_err();
            assert!(error.to_string().contains("temporary retained"));
            assert_eq!(fs::read(&path).unwrap(), b"old-save");
            assert!(sidecar(&path, ".tmp").exists());
            assert!(!sidecar(&path, ".bak").exists());
            assert!(!faults.events.contains(&Step::BackupRename));
            assert!(store(&path, b"try-save", 8).is_err());
        }
    }

    #[test]
    fn temporary_readback_rejects_same_sized_changed_content_before_names_move() {
        let path = fixture();
        fs::write(&path, b"old-save").unwrap();
        let mut faults = Faults {
            damage_before_verify: Some(sidecar(&path, ".tmp")),
            ..Default::default()
        };
        let error = store_with(&path, b"new-save", 8, &mut faults).unwrap_err();
        assert!(error.to_string().contains("readback differs"));
        assert_eq!(fs::read(&path).unwrap(), b"old-save");
        assert_eq!(fs::read(sidecar(&path, ".tmp")).unwrap(), b"bad-data");
        assert!(!faults.events.contains(&Step::BackupRename));
    }

    #[test]
    fn failed_backup_rename_keeps_main_and_verified_temporary() {
        let path = fixture();
        fs::write(&path, b"old-save").unwrap();
        let mut faults = Faults {
            failures: vec![Step::BackupRename],
            ..Default::default()
        };
        assert!(store_with(&path, b"new-save", 8, &mut faults).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old-save");
        assert_eq!(fs::read(sidecar(&path, ".tmp")).unwrap(), b"new-save");
        assert!(!faults.events.contains(&Step::CommitRename));
    }

    #[test]
    fn failed_previous_backup_removal_does_not_touch_either_committed_generation() {
        let path = fixture();
        fs::write(&path, b"old-save").unwrap();
        fs::write(sidecar(&path, ".bak"), b"old-back").unwrap();
        let mut faults = Faults {
            failures: vec![Step::RemoveBackup],
            ..Default::default()
        };
        assert!(store_with(&path, b"new-save", 8, &mut faults).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old-save");
        assert_eq!(fs::read(sidecar(&path, ".bak")).unwrap(), b"old-back");
        assert!(!faults.events.contains(&Step::BackupRename));
    }

    #[test]
    fn backup_sync_or_commit_rename_failure_restores_previous_save_and_returns_error() {
        for step in [Step::SyncBackup, Step::CommitRename] {
            let path = fixture();
            fs::write(&path, b"old-save").unwrap();
            let mut faults = Faults {
                failures: vec![step],
                ..Default::default()
            };
            let error = store_with(&path, b"new-save", 8, &mut faults).unwrap_err();
            assert!(
                error.to_string().contains("previous save restored"),
                "{error}"
            );
            assert_eq!(fs::read(&path).unwrap(), b"old-save");
            assert_eq!(fs::read(sidecar(&path, ".tmp")).unwrap(), b"new-save");
            assert!(faults.events.contains(&Step::SyncRollback));
        }
    }

    #[test]
    fn failed_rollback_retains_backup_for_the_next_load_to_recover() {
        let path = fixture();
        fs::write(&path, b"old-save").unwrap();
        let mut faults = Faults {
            failures: vec![Step::CommitRename, Step::RollbackRename],
            ..Default::default()
        };
        let error = store_with(&path, b"new-save", 8, &mut faults).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("restoring previous save also failed")
        );
        assert!(!path.exists());
        assert_eq!(fs::read(sidecar(&path, ".bak")).unwrap(), b"old-save");
        assert_eq!(load(&path, 8).unwrap().unwrap(), b"old-save");
        assert_eq!(fs::read(sidecar(&path, ".tmp")).unwrap(), b"new-save");
    }

    #[test]
    fn rollback_sync_failure_is_an_error_even_when_old_name_is_readable() {
        let path = fixture();
        fs::write(&path, b"old-save").unwrap();
        let mut faults = Faults {
            failures: vec![Step::CommitRename, Step::SyncRollback],
            ..Default::default()
        };
        let error = store_with(&path, b"new-save", 8, &mut faults).unwrap_err();
        assert!(error.to_string().contains("SyncRollback"));
        assert_eq!(fs::read(&path).unwrap(), b"old-save");
    }

    #[test]
    fn post_commit_sync_failure_returns_error_and_preserves_both_generations() {
        let path = fixture();
        fs::write(&path, b"old-save").unwrap();
        let mut faults = Faults {
            failures: vec![Step::SyncCommit],
            ..Default::default()
        };
        let error = store_with(&path, b"new-save", 8, &mut faults).unwrap_err();
        assert!(error.to_string().contains("SyncCommit"));
        assert_eq!(fs::read(&path).unwrap(), b"new-save");
        assert_eq!(fs::read(sidecar(&path, ".bak")).unwrap(), b"old-save");
    }

    #[test]
    fn failed_first_commit_does_not_turn_the_temporary_into_a_fresh_profile() {
        let path = fixture();
        let mut faults = Faults {
            failures: vec![Step::CommitRename],
            ..Default::default()
        };
        assert!(store_with(&path, b"new-save", 8, &mut faults).is_err());
        assert!(load(&path, 8).is_err());
        assert_eq!(fs::read(sidecar(&path, ".tmp")).unwrap(), b"new-save");
        assert!(!faults.events.contains(&Step::RollbackRename));
    }

    #[test]
    fn recovery_rename_and_sync_failures_are_never_reported_as_success_or_new_save() {
        for step in [Step::RecoveryRename, Step::SyncRecovery] {
            let path = fixture();
            fs::write(sidecar(&path, ".bak"), b"old-save").unwrap();
            let mut faults = Faults {
                failures: vec![step],
                ..Default::default()
            };
            assert!(load_with(&path, 8, &mut faults).is_err());
            assert_eq!(load(&path, 8).unwrap().unwrap(), b"old-save");
        }
    }

    #[test]
    fn quarantine_sync_failure_leaves_the_backup_available_for_recovery() {
        let path = fixture();
        fs::write(&path, b"bad").unwrap();
        fs::write(sidecar(&path, ".bak"), b"old-save").unwrap();
        let mut faults = Faults {
            failures: vec![Step::SyncQuarantine],
            ..Default::default()
        };
        assert!(load_with(&path, 8, &mut faults).is_err());
        assert_eq!(fs::read(sidecar(&path, ".corrupt")).unwrap(), b"bad");
        assert_eq!(fs::read(sidecar(&path, ".bak")).unwrap(), b"old-save");
        assert_eq!(load(&path, 8).unwrap().unwrap(), b"old-save");
    }

    #[test]
    fn write_flush_sync_and_verify_all_precede_the_first_committed_name_change() {
        let path = fixture();
        fs::write(&path, b"old-save").unwrap();
        let mut observed = Faults::default();
        store_with(&path, b"new-save", 8, &mut observed).unwrap();
        let position = |step| {
            observed
                .events
                .iter()
                .position(|actual| *actual == step)
                .unwrap()
        };
        assert!(position(Step::WriteTemporary) < position(Step::FlushTemporary));
        assert!(position(Step::FlushTemporary) < position(Step::SyncTemporary));
        assert!(position(Step::SyncTemporary) < position(Step::VerifyTemporary));
        assert!(position(Step::VerifyTemporary) < position(Step::BackupRename));
        assert!(position(Step::BackupRename) < position(Step::SyncBackup));
        assert!(position(Step::SyncBackup) < position(Step::CommitRename));
        assert!(position(Step::CommitRename) < position(Step::SyncCommit));
    }

    #[test]
    fn size_limits_are_checked_before_reading_or_creating_files() {
        let path = fixture();
        for expected in [0, MAX_SAVE_BYTES + 1, usize::MAX] {
            assert!(load(&path, expected).is_err());
            assert!(store(&path, b"new-save", expected).is_err());
        }
        assert!(store(&path, b"short", 8).is_err());
        assert!(!sidecar(&path, ".tmp").exists());
        let huge = File::create(&path).unwrap();
        huge.set_len((MAX_SAVE_BYTES as u64) * 16).unwrap();
        assert!(load(&path, 8).is_err());
        assert_eq!(huge.metadata().unwrap().len(), (MAX_SAVE_BYTES as u64) * 16);
    }

    #[test]
    fn readback_handles_multiple_chunks_and_keeps_extensions_on_sidecars() {
        let path = fixture().with_extension("bin");
        let bytes: Vec<u8> = (0..(8192 * 2 + 31)).map(|i| (i % 251) as u8).collect();
        store(&path, &bytes, bytes.len()).unwrap();
        assert_eq!(load(&path, bytes.len()).unwrap().unwrap(), bytes);
        assert_eq!(
            sidecar(&path, ".tmp").file_name().unwrap(),
            "savedata000.bin.tmp"
        );
    }

    #[test]
    fn links_and_non_file_sidecars_cannot_redirect_or_disguise_a_save() {
        use std::os::unix::fs::symlink;
        for suffix in ["", ".tmp", ".bak", ".corrupt"] {
            let path = fixture();
            let target = path.with_file_name("synthetic-link-target");
            fs::write(&target, b"old-save").unwrap();
            symlink(&target, sidecar(&path, suffix)).unwrap();
            assert!(load(&path, 8).is_err(), "followed {suffix} symlink");
            assert!(store(&path, b"new-save", 8).is_err());
            assert_eq!(fs::read(&target).unwrap(), b"old-save");
        }
        let path = fixture();
        fs::create_dir(sidecar(&path, ".tmp")).unwrap();
        assert!(load(&path, 8).is_err());
        assert!(store(&path, b"new-save", 8).is_err());
        let path = fixture();
        let link = path.with_file_name("link");
        symlink(path.parent().unwrap(), &link).unwrap();
        assert!(store(&link.join("other-save"), b"new-save", 8).is_err());
        assert!(!path.with_file_name("other-save").exists());
    }

    #[test]
    fn concurrent_stores_retain_two_complete_generations() {
        let path = fixture();
        fs::write(&path, b"old-save").unwrap();
        std::thread::scope(|scope| {
            let left = scope.spawn(|| store(&path, b"one-save", 8));
            let right = scope.spawn(|| store(&path, b"two-save", 8));
            left.join().unwrap().unwrap();
            right.join().unwrap().unwrap();
        });
        let main = fs::read(&path).unwrap();
        let backup = fs::read(sidecar(&path, ".bak")).unwrap();
        assert!(main == b"one-save" || main == b"two-save");
        assert!(backup == b"one-save" || backup == b"two-save");
        assert_ne!(main, backup);
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn absolute_overrides_cannot_cross_namespaces_or_use_traversal() {
        assert!(
            Namespace::Dev
                .validate(&Namespace::Dev.path("witch/savedata000").unwrap())
                .is_ok()
        );
        assert!(
            Namespace::Production
                .validate(&Namespace::Production.path("witch/savedata000").unwrap())
                .is_ok()
        );
        for input in [
            "ux0:data/knife-woh/saves/witch/savedata000",
            "ux0:data/knife-woh-dev/saves2/save",
            "ux0:data/knife-woh-dev/saves/../other",
            "ux0:data/knife-woh-dev/saves/a//save",
            "ux0:data/knife-woh-dev/saves/./save",
            "ux0:data/knife-woh-dev/saves/a\\b",
            "ux0:data/knife-woh-dev/saves/ux0:save",
            "ux0:data/knife-woh-dev/saves/",
        ] {
            assert!(
                Namespace::Dev.validate(Path::new(input)).is_err(),
                "accepted {input:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_host_does_not_claim_a_directory_durability_barrier() {
        assert_eq!(
            sync_directory(Path::new(".")).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
}
