use crate::test_traffic as traffic;
use crate::{Config, Gpu, Image, test_support::Context};
use krkr_protocol::graphics::{DrawFace, Rect, Size};
use std::{collections::HashSet, rc::Rc};

#[test]
fn terminal_color_rects_use_clears_and_preserve_alpha_and_snapshots() {
    let context = Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                tile_edge: 16,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 32,
        height: 24,
    };
    let area = Rect {
        left: 5,
        top: 3,
        width: 23,
        height: 18,
    };
    for (face, opacity) in [
        (DrawFace::Opaque, 255),
        (DrawFace::Alpha, 255),
        (DrawFace::AddAlpha, 255),
        (DrawFace::Alpha, -255),
    ] {
        let mut image = gpu.create_image(size, 0x71123456).unwrap();
        gpu.color(
            &mut image,
            Rect {
                left: 0,
                top: 0,
                width: 12,
                height: 24,
            },
            0x00807060,
            93,
            DrawFace::Alpha,
        )
        .unwrap();
        let snapshot = image.shared();
        let previous = gpu.readback(&snapshot, size.rect(), false).unwrap();
        traffic::reset();
        gpu.color(&mut image, area, 0x13579bdf, opacity, face)
            .unwrap();
        assert!(traffic::clear_calls() > 0, "{face:?}/{opacity}");
        let pixels = gpu.readback(&image, size.rect(), false).unwrap();
        for y in 0..size.height {
            for x in 0..size.width {
                let at = (y * size.width + x) as usize * 4;
                let mut expected: [u8; 4] =
                    previous.data.as_slice()[at..at + 4].try_into().unwrap();
                if (5..28).contains(&x) && (3..21).contains(&y) {
                    if opacity < 0 {
                        expected[3] = 0;
                    } else {
                        expected[..3].copy_from_slice(&[0x57, 0x9b, 0xdf]);
                        if face != DrawFace::Opaque {
                            expected[3] = 255;
                        }
                    }
                }
                assert_eq!(
                    pixels.data.as_slice()[at..at + 4],
                    expected,
                    "{face:?}/{opacity}/{x},{y}"
                );
            }
        }
        assert_eq!(
            gpu.readback(&snapshot, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            previous.data.as_slice()
        );
    }
}

#[test]
fn cropped_views_merge_once_and_read_under_a_sub_row_staging_budget() {
    use crate::image::{Plane, Tile};
    use krkr_protocol::pixels::{Bytes, Pixels};
    let context = Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 16,
        height: 8,
    };
    let expected = (0..128u8)
        .flat_map(|p| [p, 255 - p, p.wrapping_mul(3), 137])
        .collect::<Vec<_>>();
    let mut bytes = Bytes::zeroed(expected.len(), &gpu.staging).unwrap();
    bytes.as_mut_slice().copy_from_slice(&expected);
    let mut image = gpu
        .upload_scaled(
            &Pixels {
                size,
                main: Some(bytes),
                province: None,
            },
            size,
        )
        .unwrap();
    let texture = image.main.as_ref().unwrap().tiles[0].texture.clone();
    image.main = Some(Rc::new(Plane {
        size,
        budget: gpu.resident.clone(),
        tiles: [(0, 0), (8, 0), (0, 4), (8, 4)]
            .map(|(left, top)| Tile {
                rectangle: Rect {
                    left,
                    top,
                    width: 8,
                    height: 4,
                },
                backing: Some(size.rect()),
                texture: texture.clone(),
            })
            .to_vec(),
    }));
    traffic::reset();
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        expected
    );
    assert_eq!(
        traffic::read_calls(),
        1,
        "four crop views should share one synchronous read"
    );
    assert_eq!(
        traffic::store_calls(),
        0,
        "shared views need no merge texture"
    );
    let mut bytes = Bytes::zeroed(expected.len(), &gpu.staging).unwrap();
    bytes.as_mut_slice().copy_from_slice(&expected);
    let other = gpu
        .upload_scaled(
            &Pixels {
                size,
                main: Some(bytes),
                province: None,
            },
            size,
        )
        .unwrap();
    Rc::get_mut(image.main.as_mut().unwrap()).unwrap().tiles[0].texture =
        other.main.as_ref().unwrap().tiles[0].texture.clone();
    traffic::reset();
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        expected
    );
    assert_eq!(
        traffic::read_calls(),
        1,
        "independent views should merge before reading"
    );
    gpu.collect().unwrap();
    let pressure = gpu
        .staging
        .reserve(gpu.staging.available() - expected.len() - 16)
        .unwrap();
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        expected
    );
    drop(pressure);
}

fn textures(image: &Image) -> usize {
    image
        .main
        .as_ref()
        .unwrap()
        .tiles
        .iter()
        .map(|tile| Rc::as_ptr(&tile.texture))
        .collect::<HashSet<_>>()
        .len()
}

#[test]
fn fragmented_views_repack_exact_pixels_without_mutating_a_snapshot() {
    use crate::image::{Plane, Tile};
    use krkr_protocol::pixels::{Bytes, Pixels};
    let context = Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 512,
        height: 384,
    };
    let backing = Size {
        width: 512,
        height: 512,
    };
    let mut image = gpu.create_image(size, 0x89345678).unwrap();
    let margin = gpu
        .canvas_solid_tile(
            Size {
                width: 1,
                height: 1,
            },
            0x89345678,
            true,
        )
        .unwrap();
    let mut tiles = vec![
        Tile {
            rectangle: Rect {
                height: 64,
                ..size.rect()
            },
            backing: None,
            texture: margin.clone(),
        },
        Tile {
            rectangle: Rect {
                top: 320,
                height: 64,
                ..size.rect()
            },
            backing: None,
            texture: margin,
        },
    ];
    let mut expected = [0x34, 0x56, 0x78, 0x89].repeat(size.width as usize * size.height as usize);
    for index in 0..8 {
        let mut data = Bytes::zeroed(backing.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (i, pixel) in data
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            let (x, y) = (i % 512, i / 512);
            pixel.copy_from_slice(&[x as u8, y as u8, index, (x + y + index as usize) as u8]);
        }
        let start = (64 + index as usize * 32) * 512 * 4;
        expected[start..start + 32 * 512 * 4].copy_from_slice(&data.as_slice()[..32 * 512 * 4]);
        let source = gpu
            .upload_scaled(
                &Pixels {
                    size: backing,
                    main: Some(data),
                    province: None,
                },
                backing,
            )
            .unwrap();
        tiles.push(Tile {
            rectangle: Rect {
                left: 0,
                top: 64 + index as i32 * 32,
                width: 512,
                height: 32,
            },
            backing: Some(Rect {
                top: 64 + index as i32 * 32,
                ..backing.rect()
            }),
            texture: source.main.as_ref().unwrap().tiles[0].texture.clone(),
        });
    }
    image.main = Some(Rc::new(Plane {
        size,
        budget: gpu.resident.clone(),
        tiles,
    }));
    let snapshot = image.shared();
    gpu.collect().unwrap();
    let packed = 512 * 256 * 4 + 4;
    let pressure = gpu
        .resident
        .reserve(gpu.resident.available() - packed / 2)
        .unwrap();
    assert!(!gpu.compact_fragmented_canvas(&mut image).unwrap());
    assert_eq!(
        image.resident_bytes(),
        backing.rgba_bytes().unwrap() * 8 + 4
    );
    drop(pressure);
    assert!(gpu.compact_fragmented_canvas(&mut image).unwrap());
    assert_eq!(image.resident_bytes(), packed);
    assert_eq!(image.stored_size(), Some(size));
    gpu.collect().unwrap();
    // Neither native readback nor reading the old crop views may require any
    // temporary GPU allocation when the pool is completely full.
    let pressure = gpu.resident.reserve(gpu.resident.available()).unwrap();
    let scratch_pressure = gpu.scratch.reserve(gpu.scratch.available()).unwrap();
    for source in [&image, &snapshot] {
        assert_eq!(
            gpu.readback(source, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            expected
        );
        let mask = gpu.read_hit_plane(source, false).unwrap();
        for (x, y) in [(0, 0), (350, 70), (511, 255), (511, 383)] {
            let at = (y as usize * 512 + x as usize) * 4;
            assert_eq!(mask.sample(x, y), expected[at + 3]);
        }
    }
    drop(pressure);
    drop(scratch_pressure);
}

#[test]
fn compressed_readback_shrinks_to_one_row_when_scratch_is_full() {
    use krkr_protocol::{
        pixels::Bytes,
        texture::{Compressed, Format},
    };
    let context = Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 64,
        height: 64,
    };
    let mut data = Bytes::zeroed(Format::Bc1Rgb.byte_len(size).unwrap(), &gpu.staging).unwrap();
    for block in data.as_mut_slice().as_chunks_mut::<8>().0 {
        block[..2].copy_from_slice(&0xf800u16.to_le_bytes());
    }
    let image = gpu
        .load_compressed(&Compressed::new(size, Format::Bc1Rgb, data, 0).unwrap())
        .unwrap();
    gpu.collect().unwrap();
    let pressure = gpu
        .scratch
        .reserve(gpu.scratch.available() - 25 * 4)
        .unwrap();
    let result = gpu.readback(&image, size.rect(), false).unwrap();
    assert!(
        result
            .data
            .as_slice()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [255, 0, 0, 255])
    );
    drop(pressure);
}

// PartBgPlugin draws its inner gradient one column at a time. An earlier
// centered write has already separated the top, middle and bottom margins.
// Count physical textures as well as bytes: every tiny texture costs a native
// sync object on Vita even when its pixels barely consume any memory.
#[test]
fn gradient_strips_bound_texture_count_and_match_dense_pixels() {
    for horizontal in [false, true] {
        let render = |streamed| {
            let context = Context::new();
            let orient_size = |width, height| {
                if horizontal {
                    Size {
                        width: height,
                        height: width,
                    }
                } else {
                    Size { width, height }
                }
            };
            let orient_rect = |left, top, width, height| {
                if horizontal {
                    Rect {
                        left: top,
                        top: left,
                        width: height,
                        height: width,
                    }
                } else {
                    Rect {
                        left,
                        top,
                        width,
                        height,
                    }
                }
            };
            let size = orient_size(512, 676);
            let gpu = unsafe {
                Gpu::new(
                    context.gl(),
                    Config {
                        work_framebuffer: streamed,
                        canvas_limit: Some(size),
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            gpu.set_canvas_size(size);
            let mut image = gpu.create_image(size, 0x89345678).unwrap();
            gpu.color(
                &mut image,
                orient_rect(64, 45, 384, 586),
                0xffb4c8a2,
                137,
                DrawFace::Alpha,
            )
            .unwrap();
            let snapshot = image.clone();
            let snapshot_pixels = gpu
                .readback(&snapshot, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .to_vec();
            for i in 1..41 {
                for x in [i, 511 - i] {
                    let area = orient_rect(x, 0, 1, 676);
                    let estimate = gpu.canvas_blend_write_bytes(&image, area, false);
                    let before = gpu.resident.used();
                    gpu.color(
                        &mut image,
                        area,
                        0xff102030,
                        (i * 5) as i16,
                        DrawFace::Alpha,
                    )
                    .unwrap();
                    assert!(
                        gpu.resident.used().saturating_sub(before) <= estimate,
                        "strip admission underestimated allocation"
                    );
                }
            }
            let count = textures(&image);
            eprintln!(
                "strip horizontal={horizontal} streamed={streamed} textures={count} bytes={}",
                image.resident_bytes()
            );
            assert_eq!(
                gpu.readback(&snapshot, size.rect(), false)
                    .unwrap()
                    .data
                    .as_slice(),
                snapshot_pixels
            );
            let pixels = gpu
                .readback(&image, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .to_vec();
            (pixels, count)
        };
        let (expected, _) = render(false);
        let (actual, count) = render(true);
        assert_eq!(
            actual, expected,
            "gradient and untouched margins must match dense storage"
        );
        assert!(count <= 16, "gradient retained {count} physical textures");
    }
}

#[test]
fn gradient_strip_falls_back_to_exact_storage_when_cell_does_not_fit() {
    let context = Context::new();
    let size = Size {
        width: 512,
        height: 676,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut image = gpu.create_image(size, 0x89345678).unwrap();
    gpu.color(
        &mut image,
        Rect {
            left: 64,
            top: 45,
            width: 384,
            height: 586,
        },
        0xffb4c8a2,
        137,
        DrawFace::Alpha,
    )
    .unwrap();
    let snapshot = image.clone();
    gpu.collect().unwrap();
    let exact = size.height as usize * 4;
    let pressure = gpu
        .resident
        .reserve(gpu.resident.available() - exact)
        .unwrap();
    let area = Rect {
        left: 1,
        top: 0,
        width: 1,
        height: size.height,
    };
    assert_eq!(gpu.solid_region_write_bytes(&image, area), Some(exact));
    assert!(gpu.canvas_blend_write_bytes(&image, area, false) >= exact);
    let admitted = gpu.color_write_bytes(&image, area, 0xff102030, 255, DrawFace::Alpha);
    assert!(admitted <= exact);
    let before = gpu.resident.used();
    gpu.color(&mut image, area, 0xff102030, 255, DrawFace::Alpha)
        .unwrap();
    assert!(gpu.resident.used() - before <= admitted);
    drop(pressure);
    for y in [0, 44, 45, 630, 631, 675] {
        assert_eq!(gpu.pixel(&image, 1, y, false).unwrap(), 0xff102030);
        assert_eq!(gpu.pixel(&image, 0, y, false).unwrap(), 0x89345678);
        assert_eq!(gpu.pixel(&image, 2, y, false).unwrap(), 0x89345678);
        assert_eq!(gpu.pixel(&snapshot, 1, y, false).unwrap(), 0x89345678);
    }
}
