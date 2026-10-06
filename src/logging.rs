use std::{fs, io::Write, path::Path};
use tjs_core::NativeResult;

pub struct Logs;

fn units(path: &Path) -> Vec<u16> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().collect()
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().encode_utf16().collect()
    }
}

impl krkr_engine::debug::LogOutput for Logs {
    fn enabled(&self) -> bool {
        krkr_engine::protocol::diagnostics::enabled()
    }
    fn timestamp(&mut self) -> String {
        jiff::Zoned::now().strftime("%H:%M:%S").to_string()
    }
    fn console(&mut self, line: &[u16]) {
        eprintln!("{}", String::from_utf16_lossy(line));
    }
    fn file_output(&mut self) -> Option<&mut dyn krkr_engine::debug::FileOutput> {
        Some(self)
    }
}
impl krkr_engine::debug::FileOutput for Logs {
    fn normalize_directory(&mut self, directory: &[u16]) -> NativeResult<Vec<u16>> {
        let path = if directory.is_empty() {
            std::env::current_dir().map_err(crate::storage::io)?
        } else {
            std::path::absolute(crate::storage::path(directory)?).map_err(crate::storage::io)?
        };
        Ok(units(&path))
    }
    fn write_file(&mut self, directory: &[u16], text: &[u16], clear: bool) -> NativeResult<()> {
        let directory = crate::storage::path(directory)?;
        fs::create_dir_all(&directory).map_err(crate::storage::io)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(!clear)
            .truncate(clear)
            .open(directory.join("krkr.console.log"))
            .map_err(crate::storage::io)?;
        if file.metadata().map_err(crate::storage::io)?.len() == 0 {
            file.write_all(&[0xff, 0xfe]).map_err(crate::storage::io)?;
        }
        let bytes = text
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect::<Vec<_>>();
        file.write_all(&bytes).map_err(crate::storage::io)
    }
}
