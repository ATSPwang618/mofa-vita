#[path = "../src/video_format.rs"]
mod format;
use krkr_protocol::{graphics::Size, pixels::Yuv420};
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};

fn atom(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    [(data.len() as u32 + 8).to_be_bytes().as_slice(), kind, data].concat()
}
fn movie(version: u8) -> Vec<u8> {
    movie_with_timing(version, &[(150, 1001)])
}
fn movie_with_timing(version: u8, entries: &[(u32, u32)]) -> Vec<u8> {
    let mut mdhd = vec![0; if version == 1 { 32 } else { 20 }];
    mdhd[0] = version;
    let offset = if version == 1 { 20 } else { 12 };
    mdhd[offset..offset + 4].copy_from_slice(&30000u32.to_be_bytes());
    let mut avc = vec![0; 78];
    avc[24..28].copy_from_slice(&[0x03, 0xc0, 0x02, 0x1c]); // 960 x 540
    let stsd = atom(
        b"stsd",
        &[vec![0, 0, 0, 0, 0, 0, 0, 1], atom(b"avc1", &avc)].concat(),
    );
    let stts = atom(
        b"stts",
        &[0u32, entries.len() as u32]
            .into_iter()
            .chain(entries.iter().flat_map(|&(count, delta)| [count, delta]))
            .flat_map(u32::to_be_bytes)
            .collect::<Vec<_>>(),
    );
    let stbl = atom(b"stbl", &[stsd, stts].concat());
    let mdia = atom(
        b"mdia",
        &[
            atom(b"mdhd", &mdhd),
            atom(b"hdlr", b"\0\0\0\0\0\0\0\0vide"),
            atom(b"minf", &stbl),
        ]
        .concat(),
    );
    [
        atom(b"mdat", &[0; 128]),
        atom(b"moov", &atom(b"trak", &mdia)),
    ]
    .concat()
}

#[test]
fn container_dimensions_and_sample_rate_are_independent_of_decoder_padding() {
    for version in [0, 1] {
        let bytes = movie(version);
        let result = format::read(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
        assert_eq!(
            result.size,
            Size {
                width: 960,
                height: 540
            }
        );
        assert!((result.fps - 30000. / 1001.).abs() < 1e-10);
        assert_eq!(result.frames, 150);
    }
}

#[test]
fn invalid_box_lengths_and_timing_are_rejected() {
    let bytes = movie(0);
    for cut in 1..bytes.len() {
        assert!(format::read(&mut Cursor::new(&bytes[..cut]), cut as u64).is_err());
    }
    for size in [2u32, 7, u32::MAX] {
        let mut malformed = bytes.clone();
        malformed[..4].copy_from_slice(&size.to_be_bytes());
        assert!(format::read(&mut Cursor::new(&malformed), malformed.len() as u64).is_err());
    }
    let mut invalid = bytes.clone();
    let stts = invalid.windows(4).position(|s| s == b"stts").unwrap();
    invalid[stts + 16..stts + 20].fill(0); // zero sample duration
    assert!(format::read(&mut Cursor::new(&invalid), invalid.len() as u64).is_err());
}

#[test]
fn supports_extended_and_terminal_box_sizes_without_reading_media_payload() {
    let mut bytes = movie(0);
    let moov = bytes.windows(4).position(|s| s == b"moov").unwrap() - 4;
    bytes[moov..moov + 4].copy_from_slice(&0u32.to_be_bytes());
    assert!(format::read(&mut Cursor::new(&bytes), bytes.len() as u64).is_ok());
    let mut extended = [1u32.to_be_bytes().as_slice(), b"free", &16u64.to_be_bytes()].concat();
    extended.extend(bytes);
    assert!(format::read(&mut Cursor::new(&extended), extended.len() as u64).is_ok());
}

#[test]
fn compact_nv12_keeps_uv_after_padded_luma_and_removes_row_padding() {
    for (storage, visible) in [
        (
            Size {
                width: 960,
                height: 544,
            },
            Size {
                width: 960,
                height: 540,
            },
        ),
        (
            Size {
                width: 16,
                height: 16,
            },
            Size {
                width: 10,
                height: 6,
            },
        ),
        (
            Size {
                width: 16,
                height: 16,
            },
            Size {
                width: 16,
                height: 16,
            },
        ),
    ] {
        let mut source = vec![0xee; Yuv420::byte_len(storage).unwrap()];
        let mut expected = Vec::new();
        let stride = storage.width as usize;
        for row in 0..visible.height as usize {
            for x in 0..visible.width as usize {
                let v = ((row + x) % 200) as u8;
                source[row * stride + x] = v;
                expected.push(v);
            }
        }
        let uv = stride * storage.height as usize;
        for row in 0..visible.height as usize / 2 {
            for x in 0..visible.width as usize {
                let v = if x % 2 == 0 { 32 + row as u8 % 50 } else { 192 };
                source[uv + row * stride + x] = v;
                expected.push(v);
            }
        }
        let mut target = vec![0; expected.len()];
        let mut transfers = Vec::new();
        format::copy_nv12(&source, storage, &mut target, visible, |to, from| {
            transfers.push(from.len());
            to.copy_from_slice(from);
        })
        .unwrap();
        assert_eq!(target, expected);
        if storage == visible {
            assert_eq!(transfers, [target.len()]);
        } else if storage.width == visible.width {
            let y = (visible.width * visible.height) as usize;
            assert_eq!(
                transfers,
                [y, y / 2],
                "copy whole planes when only bottom padding differs"
            );
        } else {
            assert_eq!(
                transfers,
                vec![visible.width as usize; visible.height as usize * 3 / 2]
            );
        }
        assert!(
            format::copy_nv12(
                &source[..source.len() - 1],
                storage,
                &mut target,
                visible,
                |_, _| panic!("invalid buffers must not submit transfers")
            )
            .is_err()
        );
        assert!(
            format::copy_nv12(&source, visible, &mut target, storage, |to, from| to
                .copy_from_slice(from))
            .is_err()
                || storage == visible
        );
    }
}

#[test]
fn nv12_copy_initializes_each_destination_element_once_without_padding() {
    for (storage, visible) in [
        (
            Size {
                width: 16,
                height: 16,
            },
            Size {
                width: 16,
                height: 14,
            },
        ),
        (
            Size {
                width: 16,
                height: 16,
            },
            Size {
                width: 14,
                height: 10,
            },
        ),
        (
            Size {
                width: 960,
                height: 544,
            },
            Size {
                width: 960,
                height: 540,
            },
        ),
    ] {
        let source: Vec<_> = (0..(storage.width * storage.height * 3 / 2))
            .map(|i| (i % 251) as u8)
            .collect();
        // Option marks uninitialized destinations without unsafe test code.
        let mut target = vec![None; (visible.width * visible.height * 3 / 2) as usize];
        format::copy_nv12(&source, storage, &mut target, visible, |to, from| {
            assert_eq!(to.len(), from.len());
            for (to, &from) in to.iter_mut().zip(from) {
                assert!(to.replace(from).is_none(), "overlapping destination ranges");
            }
        })
        .unwrap();
        let stride = storage.width as usize;
        let width = visible.width as usize;
        let height = visible.height as usize;
        for row in 0..height * 3 / 2 {
            let offset = if row < height {
                row * stride
            } else {
                (storage.height as usize + row - height) * stride
            };
            for column in 0..width {
                assert_eq!(target[row * width + column], Some(source[offset + column]));
            }
        }
    }
}

#[test]
fn metadata_tables_reuse_buffered_reads_instead_of_reopening_archive_segments() {
    struct Counted {
        source: Cursor<Vec<u8>>,
        reads: usize,
    }
    impl Read for Counted {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            self.source.read(bytes)
        }
    }
    impl Seek for Counted {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.source.seek(to)
        }
    }
    let entries = vec![(1, 1001); 4096];
    let bytes = movie_with_timing(0, &entries);
    let length = bytes.len() as u64;
    let mut input = BufReader::with_capacity(
        64 * 1024,
        Counted {
            source: Cursor::new(bytes),
            reads: 0,
        },
    );
    let result = format::read(&mut input, length).unwrap();
    assert_eq!(result.frames, entries.len() as u64);
    assert!((result.fps - 30000. / 1001.).abs() < 1e-10);
    assert_eq!(input.get_ref().reads, 1);
}

#[test]
fn parses_real_mp4_fixture() {
    // Generated test pattern: 16x14 visible pixels, 16x16 AVC macroblock.
    let bytes = include_bytes!("data/padded-avc.mp4");
    let result = format::read(&mut Cursor::new(bytes), bytes.len() as u64).unwrap();
    assert_eq!(
        result.size,
        Size {
            width: 16,
            height: 14
        }
    );
    assert_eq!(result.frames, 5);
    assert!((result.fps - 30000. / 1001.).abs() < 1e-10);
    // Optional local game regression; never checks a private asset into tests.
    if let Ok(path) = std::env::var("KRKR_TEST_LOGO_MP4") {
        let mut file = std::fs::File::open(path).unwrap();
        let length = file.metadata().unwrap().len();
        let result = format::read(&mut file, length).unwrap();
        assert_eq!(
            result.size,
            Size {
                width: 960,
                height: 540
            }
        );
        assert_eq!(result.frames, 150);
        assert!((result.fps - 30000. / 1001.).abs() < 1e-10);
    }
}
