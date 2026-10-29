#[cfg(any(target_os = "vita", test))]
mod dev_log {
    use std::{fs::{File, OpenOptions}, io::{self, Write}, path::{Path, PathBuf}};

    pub struct PublishedLog {
        path: PathBuf,
        file: Option<File>,
    }

    impl PublishedLog {
        pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
            let path = path.as_ref().to_path_buf();
            Ok(Self { file: Some(File::create(&path)?), path })
        }
    }

    impl Write for PublishedLog {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.file.as_mut().ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "DEV log is closed"))?.write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.file.as_mut().ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "DEV log is closed"))?.flush()?;
            // Vita3K publishes its guest FILE buffer on close. Complete that
            // close before reopening the fixed DEV path for further appends.
            drop(self.file.take());
            self.file = Some(OpenOptions::new().append(true).open(&self.path)?);
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        fn log_path() -> PathBuf {
            static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!("knife-text-log-{}-{}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&path).unwrap();
            path.join("text-probe.log")
        }

        #[test]
        fn flush_reopens_the_fixed_path_after_the_previous_handle_closes() {
            let path = log_path();
            let previous = path.with_extension("previous");
            let mut log = PublishedLog::create(&path).unwrap();
            writeln!(log, "STAGE EN FRAME READY").unwrap();
            std::fs::rename(&path, &previous).unwrap();
            std::fs::write(&path, b"new fixed path\n").unwrap();
            log.flush().unwrap();
            writeln!(log, "STAGE ZC FRAME READY").unwrap();
            log.flush().unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "new fixed path\nSTAGE ZC FRAME READY\n");
            assert_eq!(std::fs::read_to_string(&previous).unwrap(), "STAGE EN FRAME READY\n");
        }

        #[test]
        fn repeated_flushes_preserve_both_stage_markers_and_the_result() {
            let path = log_path();
            let mut log = PublishedLog::create(&path).unwrap();
            for stage in ["STAGE EN FRAME READY", "STAGE ZC FRAME READY", "RESULT PASS"] {
                writeln!(log, "{stage}").unwrap();
                log.flush().unwrap();
                log.flush().unwrap();
            }
            drop(log);
            assert_eq!(std::fs::read_to_string(path).unwrap(), "STAGE EN FRAME READY\nSTAGE ZC FRAME READY\nRESULT PASS\n");
        }
    }
}

#[cfg(target_os = "vita")]
fn run() -> anyhow::Result<()> {
    use std::{io::Write, path::Path};
    unsafe extern "C" { fn kv_init_data_dir() -> i32; }
    // SAFETY: fixed SDK directory helper, no pointer arguments.
    let status = unsafe { kv_init_data_dir() };
    anyhow::ensure!(status >= 0, "initialize DEV directory: {status:#010x}");
    let mut log = dev_log::PublishedLog::create(knife_platform_vita::data_path("text-probe.log").map_err(anyhow::Error::msg)?)?;
    writeln!(log, "Knife Vita DEV original FontAtlas/text probe v1")?;
    writeln!(log, "TITLE_ID KNIF00008")?;
    writeln!(log, "This checks font loading/layout/upload/submission; visibility needs an independent frame oracle.")?;
    log.flush()?;
    let result = (|| {
        let mut renderer = knife_render::Renderer::new()?;
        knife_vita_text_probe::run_text_probe(&mut renderer,
            Path::new("ux0:data/knife-woh-dev/text-assets"), &mut log)
    })();
    if let Err(ref error) = result { writeln!(log, "{error:#}")?; }
    writeln!(log, "RESULT {}", if result.is_ok() { "PASS" } else { "FAIL" })?;
    log.flush()?;
    result
}

#[cfg(target_os = "vita")]
fn main() { if let Err(error) = run() { eprintln!("text probe failed: {error:#}"); std::process::exit(1); } }
#[cfg(not(target_os = "vita"))]
fn main() { eprintln!("Use cargo test for the host recorder; this binary requires the Vita target."); std::process::exit(2); }
