use super::*;

#[test]
fn dropped_replaced_and_rejected_writes_preserve_data_without_directories() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("save");
    fs::write(&target, b"old").unwrap();
    let mut staged = Staged::new_in(root.path()).unwrap();
    staged.write_all(b"abandoned").unwrap();
    assert!(
        fs::read_dir(root.path())
            .unwrap()
            .all(|p| p.unwrap().file_type().unwrap().is_file())
    );
    drop(staged);
    assert_eq!(fs::read(&target).unwrap(), b"old");
    let mut staged = Staged::new_in(root.path()).unwrap();
    staged.write_all(b"new").unwrap();
    staged.publish(&target).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new");
    let directory = root.path().join("directory");
    fs::create_dir(&directory).unwrap();
    let mut staged = Staged::new_in(root.path()).unwrap();
    staged.write_all(b"rejected").unwrap();
    assert!(staged.publish(&directory).is_err());
    assert!(directory.is_dir());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}

#[test]
fn failed_publication_restores_old_file_or_preserves_backup_on_failed_rollback() {
    for fail_rollback in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("save");
        fs::write(&target, b"original").unwrap();
        let mut staged = Staged::new_in(root.path()).unwrap();
        staged.write_all(b"replacement").unwrap();
        let temporary = staged.temporary.clone().unwrap();
        let mut moved = false;
        let mut backup = None;
        let error = staged
            .publish_with(&target, |from, to| {
                if moved && (from == temporary || fail_rollback) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected failure",
                    ));
                }
                rename_no_replace(from, to)?;
                if from == target {
                    moved = true;
                    backup = Some(to.to_owned());
                }
                Ok(())
            })
            .unwrap_err();
        assert!(moved);
        assert!(!temporary.exists());
        if fail_rollback {
            let backup = backup.unwrap();
            assert!(error.to_string().contains(&*backup.to_string_lossy()));
            assert_eq!(fs::read(backup).unwrap(), b"original");
            assert!(!target.exists());
        } else {
            assert_eq!(fs::read(&target).unwrap(), b"original");
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

#[test]
fn a_backup_collision_is_retried_without_overwriting_recovered_data() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("save");
    fs::write(&target, b"old").unwrap();
    let mut staged = Staged::new_in(root.path()).unwrap();
    staged.write_all(b"new").unwrap();
    let mut collision = None;
    staged
        .publish_with(&target, |from, to| {
            if from == target && collision.is_none() {
                fs::write(to, b"earlier backup").unwrap();
                collision = Some(to.to_owned());
            }
            rename_no_replace(from, to)
        })
        .unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(fs::read(collision.unwrap()).unwrap(), b"earlier backup");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}

#[test]
fn new_files_publish_and_concurrent_staging_handles_remain_independent() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("save");
    let mut first = Staged::new_in(root.path()).unwrap();
    let mut second = Staged::new_in(root.path()).unwrap();
    assert_ne!(first.temporary, second.temporary);
    first.write_all(b"first").unwrap();
    second.write_all(b"second").unwrap();
    first.publish(&target).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"first");
    second.publish(&target).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"second");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}
