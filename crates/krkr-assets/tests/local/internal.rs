use super::*;
use std::io::{Error, ErrorKind};

#[test]
fn newlib_missing_directory_error_allows_a_negative_snapshot() {
    let root = tempfile::tempdir().unwrap();
    for name in ["temp", "temp2", "#patch"] {
        let path = root.path().join(name);
        // newlib open(O_DIRECTORY) uses ENOTDIR for this absent directory.
        let error = directory_error(&path, Error::from(ErrorKind::NotADirectory));
        assert_eq!(error.kind(), ErrorKind::NotFound);
    }
}

#[test]
fn directory_error_keeps_existing_paths_and_other_failures() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file");
    fs::write(&file, b"not a directory").unwrap();
    for path in [root.path(), file.as_path()] {
        let error = directory_error(path, Error::new(ErrorKind::NotADirectory, "original"));
        assert_eq!(error.kind(), ErrorKind::NotADirectory);
        assert_eq!(error.to_string(), "original");
    }
    for kind in [ErrorKind::PermissionDenied, ErrorKind::Other] {
        let error = directory_error(&root.path().join("absent"), Error::new(kind, "original"));
        assert_eq!(error.kind(), kind);
        assert_eq!(error.to_string(), "original");
    }
}
