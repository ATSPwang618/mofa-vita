use krkr_assets::{
    Limits, name,
    xp3::{Archive, Compression, Reader, Writer},
};
use std::{
    io::{Cursor, Read},
    sync::Arc,
};

#[test]
fn unsupported_index_reports_the_path_offset_and_two_observed_headers() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bad.xp3");
    let index_at = 19u64;
    let mut data = krkr_assets::xp3::SIGNATURE.to_vec();
    data.extend_from_slice(&index_at.to_le_bytes());
    data.extend_from_slice(&[0x05, 0, 0, 0, 0, 0, 0, 0, 0]);
    std::fs::write(&path, data).unwrap();
    let message = match Archive::load(&path, Limits::default()) {
        Ok(_) => panic!("unsupported index must not be accepted"),
        Err(error) => error.to_string(),
    };
    assert!(message.contains("bad.xp3"), "{message}");
    assert!(
        message.contains("offset=19 file_bytes=28 flags=0x05"),
        "{message}"
    );
    assert!(
        message.contains("header=[05, 00, 00, 00, 00, 00, 00, 00, 00]"),
        "{message}"
    );
    assert!(
        message.contains("reread=[05, 00, 00, 00, 00, 00, 00, 00, 00] check=Ok(())"),
        "{message}"
    );
}

#[test]
fn large_archive_fits_vita_index_budget_and_keeps_sorted_lookup_and_payloads() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.xp3");
    let mut writer = Writer::new(
        Cursor::new(Vec::new()),
        Compression::None,
        Limits::default(),
    )
    .unwrap();
    // Comparable to the 52,615-entry Yosuga package: the old BTreeMap, duplicate
    // names and per-entry Arc/Vecs exceeded 16 MiB for this workload.
    for i in (0..53_000u32).rev() {
        let name = name::units(&format!("音声/voice_{i:05}_dialogue.at9"));
        writer
            .add(&name, &mut Cursor::new(i.to_le_bytes()))
            .unwrap();
    }
    std::fs::write(&path, writer.finish().unwrap().into_inner()).unwrap();
    let limits = Limits {
        max_index_bytes: 8 * 1024 * 1024,
        ..Limits::default()
    };
    let archive = Arc::new(Archive::load_strict(&path, limits).unwrap());
    assert_eq!(archive.entries.len(), 53_000);
    assert!(
        archive.index_bytes < 7 * 1024 * 1024,
        "{}",
        archive.index_bytes
    );
    let names: Vec<_> = archive.entries.keys().collect();
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    for i in [0u32, 12345, 52999] {
        let key = name::units(&format!("音声/voice_{i:05}_dialogue.at9"));
        let entry = archive.entries.get(&key).unwrap();
        let mut bytes = Vec::new();
        Reader::new(archive.clone(), entry, None)
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, i.to_le_bytes());
    }
    assert!(archive.entries.get(&name::units("missing")).is_none());
    let prefix = name::units("音声/voice_5299");
    assert_eq!(
        archive
            .entries
            .names_from(&prefix)
            .take_while(|n| n.starts_with(&prefix))
            .count(),
        10
    );
    assert!(matches!(
        Archive::load(
            &path,
            Limits {
                max_index_bytes: 16,
                ..limits
            }
        ),
        Err(krkr_assets::Error::Limit(_))
    ));
}
