use krkr_assets::{
    Limits, name,
    xp3::{Archive, Compression, Reader, Writer},
};
use std::{
    fs,
    io::{Cursor, Read, Seek, SeekFrom},
    sync::Arc,
};

#[test]
fn adaptive_texture_segments_preserve_bytes_seeks_and_direct_media() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auto.xp3");
    let mut data = vec![0; 256 * 1024 * 3 + 193];
    data[..12].copy_from_slice(b"\xabKTX 11\xbb\r\n\x1a\n");
    let mut seed = 0x12345678u32;
    for byte in &mut data[256 * 1024..512 * 1024] {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        *byte = seed as u8;
    }
    let mut writer = Writer::new(
        Cursor::new(Vec::new()),
        Compression::Auto,
        Limits::default(),
    )
    .unwrap();
    writer
        .add(&name::units("texture.png"), &mut Cursor::new(&data))
        .unwrap();
    writer
        .add(
            &name::units("audio.at9"),
            &mut Cursor::new(vec![0; 800_000]),
        )
        .unwrap();
    writer
        .add(&name::units("empty.tjs"), &mut Cursor::new([]))
        .unwrap();
    fs::write(&path, writer.finish().unwrap().into_inner()).unwrap();
    let archive = Arc::new(Archive::load_strict(&path, Limits::default()).unwrap());
    let entry = archive.entries.get(&name::units("texture.png")).unwrap();
    assert_eq!(entry.segments.len(), 4);
    assert_eq!(
        entry
            .segments
            .iter()
            .map(|s| s.compressed)
            .collect::<Vec<_>>(),
        [true, false, true, true]
    );
    assert_eq!(entry.hash, adler2::adler32_slice(&data));
    let mut reader = Reader::new(archive.clone(), entry, None);
    for start in [262130, 524280, 10, 786420, 300000, 0] {
        reader.seek(SeekFrom::Start(start)).unwrap();
        let mut bytes = [0; 64];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, data[start as usize..start as usize + bytes.len()]);
    }
    reader.seek(SeekFrom::Start(0)).unwrap();
    let mut decoded = Vec::new();
    reader.read_to_end(&mut decoded).unwrap();
    assert_eq!(decoded, data);
    let media = archive.entries.get(&name::units("audio.at9")).unwrap();
    assert_eq!(media.segments.len(), 1);
    assert!(!media.segments[0].compressed);
    assert_eq!(media.segments[0].stored, 800_000);
    let empty = archive.entries.get(&name::units("empty.tjs")).unwrap();
    assert_eq!(empty.size, 0);
    assert!(!empty.segments[0].compressed);
}

#[test]
fn adaptive_segments_obey_index_budget_and_poison_writer() {
    let limits = Limits {
        max_index_bytes: 120,
        ..Limits::default()
    };
    let mut writer = Writer::new(Cursor::new(Vec::new()), Compression::Auto, limits).unwrap();
    assert!(
        writer
            .add(&name::units("x.tjs"), &mut Cursor::new(vec![0; 300_000]))
            .is_err()
    );
    assert!(writer.finish().is_err());
}

fn archive(path: &std::path::Path, data: &[u8], compression: Compression) -> Arc<Archive> {
    let mut writer = Writer::new(Cursor::new(Vec::new()), compression, Limits::default()).unwrap();
    writer
        .add(&name::units("data"), &mut Cursor::new(data))
        .unwrap();
    fs::write(path, writer.finish().unwrap().into_inner()).unwrap();
    Arc::new(Archive::load(path, Limits::default()).unwrap())
}

#[test]
fn repeated_rewinds_discard_decoder_buffers_and_keep_independent_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.xp3");
    let data: Vec<_> = (0..65536)
        .map(|i| ((i * 31 + i / 257) % 251) as u8)
        .collect();
    for compression in [Compression::None, Compression::Zlib] {
        let archive = archive(&path, &data, compression);
        let entry = archive.entries.get(&name::units("data")).unwrap();
        let mut a = Reader::new(archive.clone(), entry.clone(), None);
        let mut b = Reader::new(archive, entry, None);
        for offset in [65000, 0, 8192, 4096, 16000, 3, 62000, 127, 32000, 0] {
            a.seek(SeekFrom::Start(offset)).unwrap();
            let mut result = [0; 257];
            a.read_exact(&mut result).unwrap();
            assert_eq!(result, data[offset as usize..offset as usize + 257]);
        }
        let mut first = [0; 257];
        b.read_exact(&mut first).unwrap();
        assert_eq!(first, data[..257]);
        a.seek(SeekFrom::End(-12)).unwrap();
        let mut tail = Vec::new();
        a.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, data[data.len() - 12..]);
        a.seek(SeekFrom::Start(0)).unwrap();
        a.read_exact(&mut first).unwrap();
        assert_eq!(
            first,
            data[..257],
            "rewind after EOF must reset inflate state"
        );

        // A reused decoder must not conceal replacement/truncation on rewind.
        fs::write(&path, b"changed archive").unwrap();
        a.seek(SeekFrom::Start(0)).unwrap();
        assert!(a.read(&mut first).is_err());
    }
}

#[test]
fn switching_compressed_segments_resets_input_and_checks_output_extent() {
    // Construct a stream fixture with two independent zlib members. This tests
    // the reader directly rather than relying on the writer's segment policy.
    use krkr_assets::xp3::{Entry, Segment, Version};
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("segments.bin");
    let mut stored = Vec::new();
    let mut segments = Vec::new();
    for (index, byte) in [17, 239].into_iter().enumerate() {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), Default::default());
        encoder.write_all(&[byte; 4096]).unwrap();
        let packed = encoder.finish().unwrap();
        segments.push(Segment {
            offset: stored.len() as u64,
            logical: index as u64 * 4096,
            original: 4096,
            stored: packed.len() as u64,
            compressed: true,
        });
        stored.extend(packed);
    }
    fs::write(&path, &stored).unwrap();
    let archive = Arc::new(Archive {
        version: Version::of(&fs::File::open(&path).unwrap()).unwrap(),
        path,
        entries: Default::default(),
        index_bytes: 0,
    });
    let entry = Arc::new(Entry {
        name: name::units("data"),
        size: 8192,
        hash: 0,
        protected: false,
        segments,
    });
    let mut reader = Reader::new(archive.clone(), entry.clone(), None);
    for (offset, expected) in [
        (
            4080,
            [17; 16].into_iter().chain([239; 16]).collect::<Vec<_>>(),
        ),
        (5000, vec![239; 32]),
        (100, vec![17; 32]),
        (4096, vec![239; 32]),
    ] {
        reader.seek(SeekFrom::Start(offset)).unwrap();
        let mut bytes = [0; 32];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes.as_slice(), expected);
    }
    drop(reader);
    let mut bad = Arc::try_unwrap(entry).unwrap();
    bad.size -= 1;
    bad.segments[1].original -= 1;
    // The normal reader also checks for excess decompressed bytes at EOF.
    let mut reader = Reader::new(archive, Arc::new(bad), None);
    assert!(reader.read_to_end(&mut Vec::new()).is_err());
}
