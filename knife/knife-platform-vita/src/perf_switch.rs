//! A small explicit switch; root takes precedence over an assets-local copy.
use std::{io::{self, Read}, path::{Path, PathBuf}};
const MAX_FLAG_BYTES: usize = 512;

fn enabled(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let Ok(text) = std::str::from_utf8(bytes) else { return false; };
    text.trim() == "1"
}
pub fn find_enabled(root: &Path) -> io::Result<Option<PathBuf>> {
    for path in [root.join("perf-enable.txt"), root.join("assets/perf-enable.txt")] {
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let mut bytes = Vec::new();
        file.take((MAX_FLAG_BYTES + 1) as u64).read_to_end(&mut bytes)?;
        return Ok((bytes.len() <= MAX_FLAG_BYTES && enabled(&bytes)).then_some(path));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ascii_and_bom_switches_accept_only_the_value_one() {
        for value in [b"1".as_slice(), b"1\r\n", b"\xef\xbb\xbf1\n", b" \t1 "] {
            assert!(enabled(value));
        }
        for value in [b"".as_slice(), b"0", b"10", b"1 garbage", b"\xff1", b"1\0"] {
            assert!(!enabled(value));
        }
    }
    #[test]
    fn assets_location_works_but_explicit_root_disable_wins() {
        let root = std::env::temp_dir().join(format!("knife-perf-switch-{}", std::process::id()));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        assert_eq!(find_enabled(&root).unwrap(), None);
        let asset_flag = root.join("assets/perf-enable.txt");
        std::fs::write(&asset_flag, b"1").unwrap();
        assert_eq!(find_enabled(&root).unwrap(), Some(asset_flag));
        let root_flag = root.join("perf-enable.txt");
        std::fs::write(&root_flag, b"0").unwrap();
        assert_eq!(find_enabled(&root).unwrap(), None);
        std::fs::write(&root_flag, b"\xef\xbb\xbf1\r\n").unwrap();
        assert_eq!(find_enabled(&root).unwrap(), Some(root_flag.clone()));
        std::fs::write(&root_flag, vec![b'1'; 513]).unwrap();
        assert_eq!(find_enabled(&root).unwrap(), None);
        std::fs::remove_file(root_flag).unwrap();
        std::fs::remove_file(root.join("assets/perf-enable.txt")).unwrap();
        std::fs::remove_dir(root.join("assets")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
