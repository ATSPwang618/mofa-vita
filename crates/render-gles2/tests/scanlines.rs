#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
use krkr_protocol::{
    graphics::{Rect, Size},
    pixels::{Bytes, Pixels},
    scanlines::Scanlines,
};
use krkr_render_gles2::{Config, Gpu};

#[test]
fn haze_streams_one_backdrop_strip_when_full_snapshot_does_not_fit() {
    let context = support::Context::new();
    let size = Size {
        width: 1024,
        height: 576,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                scratch: krkr_protocol::budget::Budget::new(5 * 1024 * 1024),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let source = gpu.create_image(size, 0xff112233).unwrap();
    let mut target = gpu.create_image(size, 0x80605040).unwrap();
    let mut words = vec![
        0,
        size.height as i32,
        3,
        size.width as i32,
        size.height as i32,
        0,
        0,
        0,
    ];
    for y in 0..size.height {
        words.extend_from_slice(&[0, size.width as i32, 0, y as i32, 0, 0, 0, 0]);
    }
    let rows = Scanlines {
        rectangle: size.rect(),
        _permit: gpu.staging.reserve(words.len() * 4).unwrap(),
        words,
    };
    assert_eq!(
        gpu.scanline_upload_bytes(&target, &rows),
        512 * 1024 + 256 * 18 * 4
    );
    gpu.copy_scanlines(&mut target, &source, &rows).unwrap();
    assert!(gpu.scratch.used() <= 4 * 1024 * 1024 + 512 * 1024 + 256 * 9 * 4);
    let read = gpu.readback(&target, size.rect(), false).unwrap();
    for (index, pixel) in read.data.as_slice().as_chunks::<4>().0.iter().enumerate() {
        let expected = match index % size.width as usize {
            0 => [17, 34, 51, 255],
            1023 => [96, 80, 64, 128],
            _ => [56, 57, 57, 191],
        };
        assert_eq!(*pixel, expected, "pixel {index}");
    }
}

#[test]
fn scanline_table_preserves_coordinates_outside_the_packed_range() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let stored = Size {
        width: 4,
        height: 1,
    };
    let data = [
        11, 22, 33, 255, 44, 55, 66, 255, 77, 88, 99, 255, 111, 122, 133, 255,
    ];
    let source = gpu
        .upload_scaled(
            &Pixels {
                size: stored,
                main: Some(Bytes::with_permit(
                    data.to_vec(),
                    gpu.staging.reserve(data.len()).unwrap(),
                )),
                province: None,
            },
            stored,
        )
        .unwrap();
    let logical = Size {
        width: 70_000,
        height: 1,
    };
    let source = gpu.logical_image(source, logical).unwrap();
    let size = Size {
        width: 1,
        height: 1,
    };
    let mut target = gpu.create_image(size, 0).unwrap();
    let words = vec![0, 1, 0, 70_000, 1, 0, 0, 0, 0, 1, 65_536, 0, 0, 0, 0, 0];
    let rows = Scanlines {
        rectangle: size.rect(),
        _permit: gpu.staging.reserve(words.len() * 4).unwrap(),
        words,
    };
    gpu.copy_scanlines(&mut target, &source, &rows).unwrap();
    assert_eq!(gpu.pixel(&target, 0, 0, false).unwrap(), 0xff6f7a85);
}
fn fixture(gpu: &Gpu, size: Size, mode: i32) -> Scanlines {
    let mut words = vec![0, 4, mode, size.width as i32, size.height as i32, 0, 0, 0];
    for row in [
        [0, 13, -2, 0, 0, 0, 0, 0],
        [-4, 16, 2, 0, 123, 0, 0, 0],
        [1, 11, 3, 0, 255, 0, 0, 0],
        [i32::MIN + 10, i32::MAX, i32::MIN + 10, 0, 1, 0, 0, 0],
    ] {
        words.extend_from_slice(&row);
    }
    Scanlines {
        rectangle: Rect {
            left: 0,
            top: 0,
            width: 13,
            height: 4,
        },
        _permit: gpu.staging.reserve(words.len() * 4).unwrap(),
        words,
    }
}
fn reference(source: &[u8], source_size: Size, rows: &Scanlines) -> Vec<u8> {
    let mut out = [80u8, 64, 48, 128].repeat(13 * 4);
    let n = (source_size.width * source_size.height) as i64;
    let get = |index: i64| -> [u32; 4] {
        if index < 0 || index >= n {
            [0; 4]
        } else {
            let pixel: [u8; 4] = source[index as usize * 4..index as usize * 4 + 4]
                .try_into()
                .unwrap();
            pixel.map(u32::from)
        }
    };
    for y in 0..4 {
        let row = &rows.words[8 + y * 8..16 + y * 8];
        for x in 0..13 {
            let dx = x as i64 - i64::from(row[0]);
            if dx < 0 || dx >= i64::from(row[1]) {
                continue;
            }
            let at = i64::from(row[3]) * i64::from(source_size.width) + i64::from(row[2]) + dx;
            if at < 0 || at >= n {
                continue;
            }
            let mut color = get(at);
            let mode = rows.words[2];
            let old = [80, 64, 48, 128];
            if mode >= 2 && dx != 0 {
                if row[4] != 0 {
                    let last = i64::from(row[1]) - 4;
                    if row[1] < 6 || dx == 1 || dx > last {
                        continue;
                    }
                    if dx == last {
                        for i in 0..4 {
                            color[i] = (old[i] + color[i]) / 2;
                        }
                    } else {
                        let left = get(at - 1);
                        for i in 0..4 {
                            color[i] = if mode == 3 {
                                old[i] / 2 + left[i] / 4 + color[i] / 4
                            } else {
                                (left[i] + color[i]) / 2
                            };
                        }
                    }
                } else {
                    if dx >= i64::from(row[1]) - 1 {
                        continue;
                    }
                    if mode == 3 {
                        for i in 0..4 {
                            color[i] = (old[i] + color[i]) / 2;
                        }
                    }
                }
            } else if mode == 1 {
                let next = get((at + 1).min(n - 1));
                let fraction = row[4] as u32;
                for i in 0..4 {
                    color[i] = (color[i] * (256 - fraction) + next[i] * fraction) / 256;
                }
            }
            let to = (y * 13 + x) * 4;
            for i in 0..4 {
                out[to + i] = color[i] as u8;
            }
        }
    }
    out
}

#[test]
fn scanline_filters_cross_rows_tiles_and_large_signed_offsets() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let stored = Size {
        width: 4,
        height: 3,
    };
    let data: Vec<u8> = (0..12)
        .flat_map(|i| {
            [
                (i * 37 + 11) as u8,
                (i * 53 + 21) as u8,
                (i * 19 + 7) as u8,
                (i * 23 + 40) as u8,
            ]
        })
        .collect();
    for compact in [false, true] {
        let logical = if compact {
            Size {
                width: 8,
                height: 6,
            }
        } else {
            stored
        };
        let mut bytes = Bytes::zeroed(data.len(), &gpu.staging).unwrap();
        bytes.as_mut_slice().copy_from_slice(&data);
        let mut source = gpu.reserve_upload(stored, true, false).unwrap();
        gpu.upload(
            &mut source,
            &Pixels {
                size: stored,
                main: Some(bytes),
                province: None,
            },
        )
        .unwrap();
        if compact {
            source = gpu.logical_image(source, logical).unwrap();
        }
        let oracle: Vec<u8> = (0..logical.height)
            .flat_map(|y| {
                (0..logical.width).flat_map({
                    let data = &data;
                    move |x| {
                        let at = if compact {
                            ((y / 2) * 4 + x / 2) as usize * 4
                        } else {
                            (y * 4 + x) as usize * 4
                        };
                        data[at..at + 4].to_vec()
                    }
                })
            })
            .collect();
        for mode in 0..=3 {
            let rows = fixture(&gpu, logical, mode);
            let size = Size {
                width: 13,
                height: 4,
            };
            let mut target = gpu.create_image(size, 0x80504030).unwrap();
            let snapshot = target.shared();
            gpu.copy_scanlines(&mut target, &source, &rows).unwrap();
            assert_eq!(
                gpu.readback(&target, size.rect(), false)
                    .unwrap()
                    .data
                    .as_slice(),
                reference(&oracle, logical, &rows),
                "mode {mode}, compact {compact}"
            );
            assert_eq!(
                gpu.readback(&snapshot, size.rect(), false)
                    .unwrap()
                    .data
                    .as_slice(),
                [80u8, 64, 48, 128].repeat(52)
            );
        }
    }
}
