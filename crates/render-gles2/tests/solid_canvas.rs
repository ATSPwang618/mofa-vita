#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
use krkr_protocol::{
    graphics::{DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu, Image};

fn assert_pixels(gpu: &Gpu, actual: &Image, expected: &Image) {
    let actual = gpu.readback(actual, actual.size.rect(), false).unwrap();
    let expected = gpu.readback(expected, expected.size.rect(), false).unwrap();
    assert_eq!(actual.data.as_slice().len(), expected.data.as_slice().len());
    let differences: Vec<_> = actual
        .data
        .as_slice()
        .iter()
        .zip(expected.data.as_slice())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .take(12)
        .collect();
    assert!(
        differences.is_empty(),
        "resize pixel differences: {differences:?}"
    );
}

#[test]
fn masked_constant_fills_keep_virtual_storage_and_snapshot_channels() {
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
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut image = gpu.create_image(size, 0x40112233).unwrap();
    let snapshot = image.shared();
    for (face, hold_alpha, color, expected) in [
        (DrawFace::Mask, false, 0xdeadbe80, 0x80112233),
        (DrawFace::Opaque, true, 0xffabcdef, 0x80abcdef),
    ] {
        let fill = Fill {
            rectangle: size.rect(),
            color,
            face,
            hold_alpha,
        };
        let estimate = gpu.fill_write_bytes(&image, &[fill]);
        gpu.fill(&mut image, &[fill]).unwrap();
        assert!(estimate <= 4, "constant fill estimated {estimate} bytes");
        assert_eq!(image.resident_bytes(), 4);
        assert_eq!(gpu.pixel(&image, 12, 20, false).unwrap(), expected);
        assert_eq!(gpu.pixel(&snapshot, 12, 20, false).unwrap(), 0x40112233);
    }
    // A partial masked fill changes only this rectangle; later drawing must
    // still see the original alpha/RGB outside it and in retained snapshots.
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                left: 30,
                top: 40,
                width: 20,
                height: 12,
            },
            color: 0x23,
            face: DrawFace::Mask,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(gpu.pixel(&image, 35, 45, false).unwrap(), 0x23abcdef);
    assert_eq!(gpu.pixel(&image, 5, 5, false).unwrap(), 0x80abcdef);
    assert_eq!(gpu.pixel(&snapshot, 35, 45, false).unwrap(), 0x40112233);
}

#[test]
fn compact_resize_shares_sample_identical_tiles_and_preserves_later_writes() {
    let context = support::Context::new();
    // All extents and the 48-pixel inset retain the exact 15/16 sampling grid.
    let logical = Size {
        width: 2448,
        height: 656,
    };
    let stored = Size {
        width: 2295,
        height: 615,
    };
    let padded = Size {
        width: 2544,
        height: 752,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(Size {
                    width: 960,
                    height: 544,
                }),
                small_canvas_edge: 0,
                tile_edge: 1024,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(Size {
        width: 1024,
        height: 576,
    });
    let data: Vec<u8> = (0..stored.height)
        .flat_map(|y| {
            (0..stored.width).flat_map(move |x| {
                [
                    (x * 13 + y * 7) as u8,
                    (x * 3 + y * 17) as u8,
                    (x ^ y) as u8,
                    255,
                ]
            })
        })
        .collect();
    let mut source = gpu.reserve_upload(stored, true, false).unwrap();
    gpu.upload(
        &mut source,
        &Pixels {
            size: stored,
            main: Some(Bytes::with_permit(
                data,
                gpu.staging.reserve(stored.rgba_bytes().unwrap()).unwrap(),
            )),
            province: None,
        },
    )
    .unwrap();
    source.size = logical;
    let mut expected = gpu.create_image(padded, 0x12345678).unwrap();
    gpu.copy_rect(
        &mut expected,
        &source,
        logical.rect(),
        0,
        0,
        padded.rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    gpu.collect().unwrap();
    let before = gpu.resident.used();
    let pressure = gpu
        .resident
        .reserve(gpu.resident.available() - 16 * 1024)
        .unwrap();
    assert_eq!(gpu.resize_image_bytes(&source, padded, 0x12345678), 4);
    let mut grown = gpu.resize(&source, padded, 0x12345678).unwrap();
    drop(pressure);
    assert!(gpu.resident.used() - before <= 4);
    assert_pixels(&gpu, &grown, &expected);
    let previous = gpu.pixel(&source, 120, 110, false).unwrap();
    gpu.fill(
        &mut grown,
        &[Fill {
            rectangle: Rect {
                left: 100,
                top: 100,
                width: 40,
                height: 30,
            },
            color: 0xffabcdef,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(gpu.pixel(&grown, 120, 110, false).unwrap(), 0xffabcdef);
    assert_eq!(gpu.pixel(&source, 120, 110, false).unwrap(), previous);
    assert_eq!(gpu.pixel(&grown, 2540, 740, false).unwrap(), 0x12345678);

    // Real effect sequence: grow an assigned image, clear two edges, copy the
    // source into the inset, then clear the other edges. Keep both old versions.
    let mut padded_image = gpu.resize(&source, padded, 0xff000000).unwrap();
    gpu.collect().unwrap();
    let pressure = gpu
        .resident
        .reserve(gpu.resident.available() - 8192)
        .unwrap();
    for rectangle in [
        Rect {
            left: 0,
            top: 0,
            width: padded.width,
            height: 48,
        },
        Rect {
            left: 0,
            top: 48,
            width: 48,
            height: logical.height,
        },
    ] {
        gpu.fill(
            &mut padded_image,
            &[Fill {
                rectangle,
                color: 0,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
    }
    gpu.copy_rect(
        &mut padded_image,
        &source,
        logical.rect(),
        48,
        48,
        padded.rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    drop(pressure);
    assert_eq!(gpu.pixel(&padded_image, 0, 0, false).unwrap(), 0);
    assert_eq!(
        gpu.pixel(&padded_image, 2540, 740, false).unwrap(),
        0xff000000
    );
    assert_eq!(
        gpu.pixel(&padded_image, 168, 158, false).unwrap(),
        gpu.pixel(&source, 120, 110, false).unwrap()
    );
}

#[test]
fn compact_resize_resamples_when_padding_changes_storage_density() {
    let context = support::Context::new();
    let logical = Size {
        width: 2453,
        height: 661,
    };
    let stored = Size {
        width: 2300,
        height: 620,
    };
    let padded = Size {
        width: 2549,
        height: 757,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(Size {
                    width: 960,
                    height: 544,
                }),
                small_canvas_edge: 0,
                tile_edge: 1024,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(Size {
        width: 1024,
        height: 576,
    });
    let data: Vec<u8> = (0..stored.height)
        .flat_map(|y| {
            (0..stored.width).flat_map(move |x| {
                [
                    (x * 13 + y * 7) as u8,
                    (x * 3 + y * 17) as u8,
                    (x ^ y) as u8,
                    (x * 11 + y * 19) as u8,
                ]
            })
        })
        .collect();
    let mut source = gpu
        .assign_bitmap(
            None,
            &Pixels {
                size: stored,
                main: Some(Bytes::with_permit(
                    data,
                    gpu.staging.reserve(stored.rgba_bytes().unwrap()).unwrap(),
                )),
                province: None,
            },
        )
        .unwrap();
    source.size = logical;
    let mut expected = gpu.create_image(padded, 0x12345678).unwrap();
    gpu.copy_rect(
        &mut expected,
        &source,
        logical.rect(),
        0,
        0,
        padded.rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    // The nearest addresses would stay unchanged, but linear interpolation
    // still changes pixels. Sharing must not skip the ordinary filtered copy.
    let estimate = gpu.resize_image_bytes(&source, padded, 0x12345678);
    assert!(estimate > 4, "filtered resize estimated {estimate} bytes");
    let source_pixel = gpu.pixel(&source, 120, 110, false).unwrap();
    let grown = gpu.resize(&source, padded, 0x12345678).unwrap();
    assert_pixels(&gpu, &grown, &expected);
    assert_eq!(gpu.pixel(&source, 120, 110, false).unwrap(), source_pixel);
    assert_eq!(gpu.pixel(&grown, 2540, 740, false).unwrap(), 0x12345678);
}

#[test]
fn tiny_initial_copy_preserves_large_virtual_margins_under_pressure() {
    let context = support::Context::new();
    let canvas = Size {
        width: 1024,
        height: 576,
    };
    let size = Size {
        width: 1120,
        height: 672,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(Size {
                    width: 960,
                    height: 544,
                }),
                small_canvas_edge: 64,
                tile_edge: 512,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(canvas);
    let mut image = gpu.create_image(size, 0x00445566).unwrap();
    let snapshot = image.shared();
    let source = gpu
        .create_image(
            Size {
                width: 32,
                height: 32,
            },
            0xffaabbcc,
        )
        .unwrap();
    let area = Rect {
        left: 48,
        top: 48,
        width: 32,
        height: 32,
    };
    let charge = gpu.canvas_region_write_bytes(&image, area, false, false);
    assert!(charge <= 4096, "tiny copy admission charged {charge} bytes");
    gpu.collect().unwrap();
    let before = gpu.resident.used();
    let pressure = gpu
        .resident
        .reserve(gpu.resident.available() - 16 * 1024)
        .unwrap();
    gpu.copy_rect(
        &mut image,
        &source,
        source.size.rect(),
        48,
        48,
        size.rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    drop(pressure);
    gpu.collect().unwrap();
    assert!(gpu.resident.used() - before <= charge);
    assert_eq!(gpu.pixel(&image, 60, 60, false).unwrap(), 0xffaabbcc);
    assert_eq!(gpu.pixel(&image, 200, 200, false).unwrap(), 0x00445566);
    assert_eq!(gpu.pixel(&snapshot, 60, 60, false).unwrap(), 0x00445566);
    let copied = image.shared();
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                left: 100,
                top: 100,
                width: 16,
                height: 16,
            },
            color: 0xff112233,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(gpu.pixel(&image, 108, 108, false).unwrap(), 0xff112233);
    assert_eq!(gpu.pixel(&copied, 108, 108, false).unwrap(), 0x00445566);
    let resized = gpu
        .resize(
            &image,
            Size {
                width: 1200,
                height: 720,
            },
            0x80776655,
        )
        .unwrap();
    assert_eq!(gpu.pixel(&resized, 60, 60, false).unwrap(), 0xffaabbcc);
    assert_eq!(gpu.pixel(&resized, 108, 108, false).unwrap(), 0xff112233);
    assert_eq!(gpu.pixel(&resized, 1180, 700, false).unwrap(), 0x80776655);
}

#[test]
fn pressure_compaction_replaces_all_layer_aliases_but_never_a_pinned_snapshot() {
    let context = support::Context::new();
    let size = Size {
        width: 384,
        height: 256,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 512,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut original = gpu.create_image(size, 0x00445566).unwrap();
    gpu.fill(
        &mut original,
        &[
            Fill {
                rectangle: size.rect(),
                color: 0x00445566,
                face: DrawFace::Alpha,
                hold_alpha: false,
            },
            Fill {
                rectangle: Rect {
                    left: 130,
                    top: 100,
                    width: 60,
                    height: 40,
                },
                color: 0x80776655,
                face: DrawFace::Alpha,
                hold_alpha: false,
            },
        ],
    )
    .unwrap();
    // A whole-image color pass must not erase the known constant margins.
    gpu.adjust(
        &mut original,
        size.rect(),
        &krkr_protocol::graphics::Adjustment::GrayScale,
    )
    .unwrap();
    let before = gpu.readback(&original, size.rect(), false).unwrap();
    let snapshot = original.shared();
    let mut layers = vec![original.shared(), original.shared(), original];
    gpu.collect().unwrap();
    let resident = gpu.resident.used();
    let headroom = gpu.resident.available() + 100_000;
    gpu.compact_canvases(layers.iter_mut(), headroom).unwrap();
    assert_eq!(
        gpu.resident.used(),
        resident,
        "an external snapshot pins the full allocation"
    );
    drop(snapshot);
    gpu.compact_canvases(layers.iter_mut(), headroom).unwrap();
    assert!(
        resident - gpu.resident.used() > 300_000,
        "all aliases must release the same original allocation"
    );
    for image in &layers {
        assert_eq!(
            gpu.readback(image, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            before.data.as_slice()
        );
    }
    gpu.fill(
        &mut layers[0],
        &[Fill {
            rectangle: Rect {
                left: 140,
                top: 110,
                width: 10,
                height: 10,
            },
            color: 0xff123456,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(gpu.pixel(&layers[0], 145, 115, false).unwrap(), 0xff123456);
    let gray = (0x77 * 54 + 0x66 * 183 + 0x55 * 19) >> 8;
    assert_eq!(
        gpu.pixel(&layers[1], 145, 115, false).unwrap(),
        0x80000000 | (gray * 0x010101)
    );
}

#[test]
fn pressure_compaction_preserves_dense_borders_and_detaches_later_writes() {
    let context = support::Context::new();
    let size = Size {
        width: 384,
        height: 256,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 512,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut image = gpu.create_image(size, 0x00445566).unwrap();
    // A batched full clear and edit deliberately materializes dense storage;
    // a lone tiny edit now keeps the surrounding canvas virtual.
    let fill = |rectangle, color| Fill {
        rectangle,
        color,
        face: DrawFace::Alpha,
        hold_alpha: false,
    };
    gpu.fill(
        &mut image,
        &[
            fill(size.rect(), 0x00445566),
            fill(
                Rect {
                    left: 130,
                    top: 100,
                    width: 60,
                    height: 40,
                },
                0x80776655,
            ),
        ],
    )
    .unwrap();
    let before = gpu.readback(&image, size.rect(), false).unwrap();
    let snapshot = image.shared();
    assert!(
        !gpu.compact_canvas(&mut image, true).unwrap(),
        "shared nonuniform storage must not be duplicated"
    );
    drop(snapshot);
    gpu.collect().unwrap();
    let resident = gpu.resident.used();
    assert!(gpu.compact_canvas(&mut image, true).unwrap());
    gpu.collect().unwrap();
    assert!(resident - gpu.resident.used() > 300_000);
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        before.data.as_slice()
    );
    let snapshot = image.shared();
    gpu.fill(
        &mut image,
        &[fill(
            Rect {
                left: 2,
                top: 3,
                width: 7,
                height: 8,
            },
            0xffaabbcc,
        )],
    )
    .unwrap();
    assert_eq!(gpu.pixel(&image, 3, 4, false).unwrap(), 0xffaabbcc);
    assert_eq!(gpu.pixel(&snapshot, 3, 4, false).unwrap(), 0x00445566);
    assert_eq!(gpu.pixel(&image, 150, 120, false).unwrap(), 0x80776655);
    // The full readback also checks all four constant partitions after COW.
    assert_eq!(
        gpu.readback(&snapshot, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        before.data.as_slice()
    );
}

#[test]
fn pressure_discovers_exact_borders_after_upload_and_keeps_hidden_rgb() {
    let context = support::Context::new();
    let size = Size {
        width: 512,
        height: 256,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 256,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut data = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, p) in data
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        p.copy_from_slice(
            if i % 512 % 256 >= 40 && i % 512 % 256 < 60 && i / 512 >= 80 && i / 512 < 100 {
                &[250, 80, 30, 255]
            } else {
                &[17, 34, 51, 0]
            },
        );
    }
    let pixels = Pixels {
        size,
        main: Some(data),
        province: None,
    };
    let mut image = gpu.upload_scaled(&pixels, size).unwrap();
    let mut alias = image.shared();
    gpu.collect().unwrap();
    let before = gpu.resident.used();
    gpu.reclaim_canvas_borders([&mut image, &mut alias].into_iter(), gpu.resident.limit())
        .unwrap();
    gpu.collect().unwrap();
    assert!(before - gpu.resident.used() > 400_000);
    for image in [&image, &alias] {
        assert_eq!(
            gpu.readback(image, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            pixels.main.as_ref().unwrap().as_slice()
        );
    }
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                left: 0,
                top: 0,
                width: 50,
                height: 100,
            },
            color: 0xff778899,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(
        gpu.readback(&alias, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        pixels.main.as_ref().unwrap().as_slice()
    );
}

#[test]
fn pressure_compacts_more_than_sixty_four_tiles_without_changing_pixels() {
    let context = support::Context::new();
    let size = Size {
        width: 2304,
        height: 2304,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 256,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let fill = |rectangle, color| Fill {
        rectangle,
        color,
        face: DrawFace::Alpha,
        hold_alpha: false,
    };
    let mut image = gpu.create_image(size, 0x00112233).unwrap();
    let mut fills = vec![fill(size.rect(), 0x00112233)];
    for y in 0..9 {
        for x in 0..9 {
            fills.push(fill(
                Rect {
                    left: x * 256 + 32,
                    top: y * 256 + 40,
                    width: 20,
                    height: 30,
                },
                0xffabcdef,
            ));
        }
    }
    gpu.fill(&mut image, &fills).unwrap();
    let before_pixels = gpu
        .readback(&image, size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec();
    gpu.collect().unwrap();
    let before = gpu.resident.used();
    gpu.compact_canvases([&mut image].into_iter(), gpu.resident.limit())
        .unwrap();
    gpu.collect().unwrap();
    assert!(
        before.saturating_sub(gpu.resident.used()) > 1024 * 1024,
        "before={before} after={} image={image:?}",
        gpu.resident.used()
    );
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        before_pixels.as_slice()
    );
}

#[test]
fn pressure_compacts_shared_texture_borders_across_distinct_planes() {
    let context = support::Context::new();
    let size = Size {
        width: 512,
        height: 256,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 256,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let fill = |rectangle, color| Fill {
        rectangle,
        color,
        face: DrawFace::Alpha,
        hold_alpha: false,
    };
    let mut a = gpu.create_image(size, 0x00112233).unwrap();
    gpu.fill(
        &mut a,
        &[
            fill(size.rect(), 0x00112233),
            fill(
                Rect {
                    left: 32,
                    top: 40,
                    width: 20,
                    height: 30,
                },
                0xffabcdef,
            ),
            fill(
                Rect {
                    left: 300,
                    top: 40,
                    width: 20,
                    height: 30,
                },
                0xff123456,
            ),
        ],
    )
    .unwrap();
    let mut b = a.shared();
    gpu.fill(
        &mut b,
        &[fill(
            Rect {
                left: 300,
                top: 40,
                width: 20,
                height: 30,
            },
            0xff987654,
        )],
    )
    .unwrap();
    let before_a = gpu.readback(&a, size.rect(), false).unwrap();
    let before_b = gpu.readback(&b, size.rect(), false).unwrap();
    gpu.collect().unwrap();
    let before = gpu.resident.used();
    gpu.compact_canvases([&mut a, &mut b].into_iter(), gpu.resident.limit())
        .unwrap();
    gpu.collect().unwrap();
    assert!(
        before - gpu.resident.used() > 250_000,
        "before={before} after={} a={a:?} b={b:?}",
        gpu.resident.used()
    );
    assert_eq!(
        gpu.readback(&a, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        before_a.data.as_slice()
    );
    assert_eq!(
        gpu.readback(&b, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        before_b.data.as_slice()
    );
}

#[test]
fn inset_canvases_share_exact_margins_and_preserve_later_edits_uploads_and_snapshots() {
    let context = support::Context::new();
    let size = Size {
        width: 384,
        height: 256,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                tile_edge: 256,
                small_canvas_edge: 0,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let area = Rect {
        left: 32,
        top: 32,
        width: 320,
        height: 192,
    };
    let fill = |rectangle, color| Fill {
        rectangle,
        color,
        face: DrawFace::Alpha,
        hold_alpha: false,
    };
    let baseline = gpu.resident.used();
    let mut layers = Vec::new();
    for i in 0..12 {
        let mut image = gpu.create_image(size, 0x00223344).unwrap();
        gpu.fill(&mut image, &[fill(area, 0x80776600 + i)]).unwrap();
        layers.push(image);
    }
    gpu.collect().unwrap();
    assert!(
        gpu.resident.used() - baseline <= 320 * 192 * 4 * 12 + 16,
        "twelve inset canvases must store their untouched margins as single texels"
    );
    let read = |image: &krkr_render_gles2::Image| {
        gpu.readback(image, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
            .to_vec()
    };
    for (i, image) in layers.iter().enumerate() {
        let expected: Vec<u8> = (0..size.height)
            .flat_map(|y| {
                (0..size.width).flat_map(move |x| {
                    if (32..352).contains(&x) && (32..224).contains(&y) {
                        [0x77, 0x66, i as u8, 0x80]
                    } else {
                        [0x22, 0x33, 0x44, 0]
                    }
                })
            })
            .collect();
        assert_eq!(read(image), expected);
    }
    let original = read(&layers[0]);
    let mut snapshot = layers[0].shared();
    gpu.independ(&mut snapshot, false, true).unwrap();
    assert_eq!(read(&snapshot), original);
    gpu.fill(
        &mut layers[0],
        &[fill(
            Rect {
                left: 3,
                top: 2,
                width: 7,
                height: 6,
            },
            0xffabcdef,
        )],
    )
    .unwrap();
    assert_eq!(gpu.pixel(&layers[0], 4, 4, false).unwrap(), 0xffabcdef);
    assert_eq!(gpu.pixel(&layers[1], 4, 4, false).unwrap(), 0x00223344);
    assert_eq!(read(&snapshot), original);
    let mut bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, p) in bytes
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        p.copy_from_slice(&[i as u8, (i >> 8) as u8, (i * 17) as u8, 193]);
    }
    let expected = bytes.as_slice().to_vec();
    gpu.upload(
        &mut layers[0],
        &Pixels {
            size,
            main: Some(bytes),
            province: None,
        },
    )
    .unwrap();
    assert_eq!(read(&layers[0]), expected);
    assert_eq!(read(&snapshot), original);
    drop((layers, snapshot));
    gpu.collect().unwrap();
    assert_eq!(
        gpu.resident.used(),
        baseline,
        "margin cache must not retain pixels"
    );
}

#[test]
fn sparse_resize_preserves_content_and_blur_halos_under_pressure() {
    use krkr_protocol::graphics::Adjustment;
    let context = support::Context::new();
    let size = Size {
        width: 384,
        height: 256,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 256,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut image = gpu.create_image(size, 0x00123456).unwrap();
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                left: 32,
                top: 32,
                width: 320,
                height: 192,
            },
            color: 0x80776655,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let pixels = gpu.readback(&image, size.rect(), false).unwrap();
    let mut dense = gpu.reserve_upload(size, true, false).unwrap();
    gpu.upload(
        &mut dense,
        &Pixels {
            size,
            main: Some(pixels.data),
            province: None,
        },
    )
    .unwrap();
    let operation = Adjustment::BoxBlur {
        radius: [2, 2],
        alpha: true,
    };
    gpu.adjust(&mut dense, size.rect(), &operation).unwrap();
    gpu.collect().unwrap();
    let pressure = gpu
        .resident
        .reserve(gpu.resident.available() - 240_000)
        .unwrap();
    gpu.adjust(&mut image, size.rect(), &operation).unwrap();
    drop(pressure);
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        gpu.readback(&dense, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
    );
    let padded = Size {
        width: 432,
        height: 304,
    };
    let resized = gpu.resize(&image, padded, 0x00223344).unwrap();
    assert_eq!(
        gpu.readback(&resized, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
    );
    assert_eq!(gpu.pixel(&resized, 430, 302, false).unwrap(), 0x00223344);
    gpu.collect().unwrap();
    let full = gpu.resident.reserve(gpu.resident.available()).unwrap();
    assert_eq!(gpu.resize_image_bytes(&resized, padded, 0), 0);
    let same = gpu.resize(&resized, padded, 0).unwrap();
    drop(full);
    assert_eq!(gpu.pixel(&same, 430, 302, false).unwrap(), 0x00223344);
}

#[test]
fn clearing_part_of_a_large_constant_margin_does_not_expand_the_tile() {
    let context = support::Context::new();
    let size = Size {
        width: 384,
        height: 256,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 256,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let fill = |rectangle, color| Fill {
        rectangle,
        color,
        face: DrawFace::Alpha,
        hold_alpha: false,
    };
    let mut image = gpu.create_image(size, 0xff123456).unwrap();
    gpu.fill(
        &mut image,
        &[fill(
            Rect {
                left: 32,
                top: 32,
                width: 320,
                height: 192,
            },
            0x80776655,
        )],
    )
    .unwrap();
    let snapshot = image.shared();
    gpu.collect().unwrap();
    let pressure = gpu.resident.reserve(gpu.resident.available() - 64).unwrap();
    gpu.fill(
        &mut image,
        &[fill(
            Rect {
                left: 0,
                top: 16,
                width: 16,
                height: 200,
            },
            0,
        )],
    )
    .unwrap();
    drop(pressure);
    assert_eq!(gpu.pixel(&image, 8, 24, false).unwrap(), 0);
    assert_eq!(gpu.pixel(&image, 24, 24, false).unwrap(), 0xff123456);
    assert_eq!(gpu.pixel(&image, 80, 80, false).unwrap(), 0x80776655);
    assert_eq!(gpu.pixel(&snapshot, 8, 24, false).unwrap(), 0xff123456);
    assert_eq!(
        gpu.canvas_region_write_bytes(
            &image,
            Rect {
                left: 64,
                top: 64,
                width: 4,
                height: 4
            },
            false,
            false
        ),
        256 * 192 * 4
    );
    drop(snapshot);
    assert_eq!(
        gpu.canvas_region_write_bytes(
            &image,
            Rect {
                left: 64,
                top: 64,
                width: 4,
                height: 4
            },
            false,
            false
        ),
        0
    );
}

#[test]
fn blank_layers_share_storage_and_detach_only_edited_tiles() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 8,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 16,
        height: 16,
    };
    let color = 0x73456789;
    let baseline = gpu.resident.used();
    let mut layers = (0..24)
        .map(|_| gpu.create_image(size, color).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(gpu.resident.used() - baseline, 16 * 16 * 4);
    assert_eq!(gpu.create_image_bytes(size, color), 0);
    gpu.fill(
        &mut layers[0],
        &[Fill {
            rectangle: Rect {
                left: 1,
                top: 1,
                width: 1,
                height: 1,
            },
            color: 0xfedcba98,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(gpu.resident.used() - baseline, (16 * 16 + 8 * 8) * 4);
    assert_eq!(gpu.pixel(&layers[0], 1, 1, false).unwrap(), 0xfedcba98);
    assert_eq!(gpu.pixel(&layers[1], 1, 1, false).unwrap(), color);
    gpu.fill(
        &mut layers[0],
        &[Fill {
            rectangle: size.rect(),
            color,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    gpu.collect().unwrap();
    assert_eq!(gpu.resident.used() - baseline, 16 * 16 * 4);
    assert_eq!(gpu.pixel(&layers[0], 1, 1, false).unwrap(), color);
}

#[test]
fn sharing_and_uniform_resize_need_no_free_image_storage() {
    let context = support::Context::new();
    let size = Size {
        width: 8,
        height: 8,
    };
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let _pressure = gpu
        .resident
        .reserve(gpu.resident.available() - 8 * 8 * 4)
        .unwrap();
    let first = gpu.create_image(size, 0x6789abcd).unwrap();
    assert_eq!(gpu.resident.available(), 0);
    let second = gpu.create_image(size, 0x6789abcd).unwrap();
    let small = gpu.create_image(
        Size {
            width: 1,
            height: 1,
        },
        0x6789abcd,
    );
    assert!(small.is_err());
    let resized = gpu.resize(&first, size, 0x6789abcd).unwrap();
    assert_eq!(gpu.pixel(&second, 7, 7, false).unwrap(), 0x6789abcd);
    assert_eq!(gpu.pixel(&resized, 7, 7, false).unwrap(), 0x6789abcd);
    assert!(
        gpu.create_image(
            Size {
                width: 0,
                height: 8
            },
            0
        )
        .is_err()
    );
}

#[test]
fn unique_upload_and_mask_writes_invalidate_remembered_solids() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 4,
        height: 4,
    };
    let mut first = gpu.create_image(size, 0x12345678).unwrap();
    let mut bytes = Bytes::zeroed(64, &gpu.staging).unwrap();
    bytes.as_mut_slice().fill(0x99);
    gpu.upload(
        &mut first,
        &Pixels {
            size,
            main: Some(bytes),
            province: None,
        },
    )
    .unwrap();
    let second = gpu.create_image(size, 0x12345678).unwrap();
    assert_eq!(gpu.pixel(&first, 0, 0, false).unwrap(), 0x99999999);
    assert_eq!(gpu.pixel(&second, 0, 0, false).unwrap(), 0x12345678);
    drop(second);
    let mut masked = gpu.create_image(size, 0x12345678).unwrap();
    gpu.fill(
        &mut masked,
        &[Fill {
            rectangle: size.rect(),
            color: 0xfe,
            face: DrawFace::Mask,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let third = gpu.create_image(size, 0x12345678).unwrap();
    assert_eq!(gpu.pixel(&masked, 0, 0, false).unwrap(), 0xfe345678);
    assert_eq!(gpu.pixel(&third, 0, 0, false).unwrap(), 0x12345678);
    gpu.fill(
        &mut masked,
        &[Fill {
            rectangle: size.rect(),
            color: 61,
            face: DrawFace::Province,
            hold_alpha: false,
        }],
    )
    .unwrap();
    gpu.fill(
        &mut masked,
        &[Fill {
            rectangle: size.rect(),
            color: 0x12345678,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let resized = gpu
        .resize(
            &masked,
            Size {
                width: 7,
                height: 6,
            },
            0x12345678,
        )
        .unwrap();
    assert_eq!(gpu.pixel(&resized, 0, 0, true).unwrap(), 61);
    assert_eq!(gpu.pixel(&resized, 6, 5, true).unwrap(), 0);
    assert_eq!(gpu.pixel(&resized, 6, 5, false).unwrap(), 0x12345678);
}

#[test]
fn shared_compact_canvases_keep_logical_coordinates_and_density() {
    let context = support::Context::new();
    let size = Size {
        width: 16,
        height: 12,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                canvas_limit: Some(Size {
                    width: 8,
                    height: 6,
                }),
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut first = gpu.create_image(size, 0x12345678).unwrap();
    let second = gpu.create_image(size, 0x12345678).unwrap();
    assert_eq!(
        first.stored_size(),
        Some(Size {
            width: 1,
            height: 1
        })
    );
    gpu.fill(
        &mut first,
        &[Fill {
            rectangle: Rect {
                left: 8,
                top: 6,
                width: 8,
                height: 6,
            },
            color: 0xaabbccdd,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(first.size, size);
    assert_eq!(
        first.stored_size(),
        Some(Size {
            width: 8,
            height: 6
        })
    );
    assert_eq!(
        second.stored_size(),
        Some(Size {
            width: 1,
            height: 1
        })
    );
    assert_eq!(gpu.pixel(&first, 15, 11, false).unwrap(), 0xaabbccdd);
    assert_eq!(gpu.pixel(&first, 2, 2, false).unwrap(), 0x12345678);
    assert_eq!(gpu.pixel(&second, 15, 11, false).unwrap(), 0x12345678);
}

#[test]
fn differently_sized_blank_canvases_share_one_texel_until_the_first_edit() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    canvas_limit: Some(Size {
                        width: 960,
                        height: 544,
                    }),
                    work_framebuffer: work,
                    small_canvas_edge: 64,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        gpu.set_canvas_size(Size {
            width: 1920,
            height: 1080,
        });
        let before = gpu.resident.used();
        let mut images = Vec::new();
        for i in 0..32 {
            images.push(
                gpu.create_image(
                    Size {
                        width: 1920 + i,
                        height: 1080 + i,
                    },
                    0x80402010,
                )
                .unwrap(),
            );
        }
        assert_eq!(gpu.resident.used() - before, 4);
        for image in &images {
            assert_eq!(image.resident_bytes(), 4);
            assert_eq!(
                gpu.pixel(
                    image,
                    image.size.width as i32 - 1,
                    image.size.height as i32 - 1,
                    false
                )
                .unwrap(),
                0x80402010
            );
        }
        let fill = Fill {
            rectangle: Rect {
                left: 100,
                top: 80,
                width: 4,
                height: 4,
            },
            color: 0xe7,
            face: DrawFace::Mask,
            hold_alpha: false,
        };
        let pressure = gpu.resident.reserve(gpu.resident.available()).unwrap();
        assert!(gpu.fill(&mut images[0], &[fill]).is_err());
        assert_eq!(images[0].resident_bytes(), 4);
        assert_eq!(gpu.pixel(&images[0], 100, 80, false).unwrap(), 0x80402010);
        drop(pressure);
        gpu.fill(&mut images[0], &[fill]).unwrap();
        assert_eq!(
            images[0].stored_size(),
            Some(Size {
                width: 960,
                height: 540
            })
        );
        assert_eq!(gpu.pixel(&images[0], 100, 80, false).unwrap(), 0xe7402010);
        assert_eq!(gpu.pixel(&images[0], 20, 20, false).unwrap(), 0x80402010);
        assert_eq!(gpu.pixel(&images[1], 100, 80, false).unwrap(), 0x80402010);
        let logical = images[0].size;
        gpu.fill(
            &mut images[0],
            &[Fill {
                rectangle: logical.rect(),
                color: 0x80402010,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        gpu.collect().unwrap();
        assert_eq!(gpu.resident.used() - before, 4);
        // Small menu cells retain native detail, with an area and long-edge cap.
        for (size, expected) in [
            (
                Size {
                    width: 93,
                    height: 29,
                },
                Size {
                    width: 93,
                    height: 29,
                },
            ),
            (
                Size {
                    width: 300,
                    height: 10,
                },
                Size {
                    width: 150,
                    height: 5,
                },
            ),
            (
                Size {
                    width: 128,
                    height: 40,
                },
                Size {
                    width: 64,
                    height: 20,
                },
            ),
        ] {
            let mut image = gpu.create_image(size, 0).unwrap();
            gpu.fill(
                &mut image,
                &[Fill {
                    rectangle: Rect {
                        left: 4,
                        top: 2,
                        width: 2,
                        height: 2,
                    },
                    color: 0xffffffff,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            assert_eq!(image.stored_size(), Some(expected));
            assert_eq!(gpu.pixel(&image, 4, 2, false).unwrap(), 0xffffffff);
        }
    }
}

#[test]
fn pressure_releases_large_backing_kept_by_small_canvas_views() {
    use krkr_protocol::graphics::{Blend, BlendOptions};
    let context = support::Context::new();
    let large = Size {
        width: 512,
        height: 512,
    };
    let small = Size {
        width: 64,
        height: 64,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(large),
                small_canvas_edge: 0,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(large);
    let mut data = Bytes::zeroed(large.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, pixel) in data
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        pixel.copy_from_slice(&[(i % 251) as u8, (i / 512) as u8, 31, 255]);
    }
    let source = gpu
        .upload_scaled(
            &Pixels {
                size: large,
                main: Some(data),
                province: None,
            },
            large,
        )
        .unwrap();
    let mut views = Vec::new();
    for left in [64, 96] {
        let mut view = gpu.create_image(small, 0).unwrap();
        gpu.operate(
            &mut view,
            &source,
            Rect {
                left,
                top: 80,
                ..small.rect()
            },
            0,
            0,
            small.rect(),
            BlendOptions {
                mode: Blend::Opaque,
                face: DrawFace::Opaque,
                opacity: 255,
                hold_alpha: false,
            },
        )
        .unwrap();
        assert_eq!(view.resident_bytes(), large.rgba_bytes().unwrap());
        views.push(view);
    }
    drop(source);
    gpu.collect().unwrap();
    let expected: Vec<_> = views
        .iter()
        .map(|view| {
            gpu.readback(view, small.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .to_vec()
        })
        .collect();
    let snapshot = views[0].shared();
    let before = gpu.resident.used();
    gpu.compact_canvases(views.iter_mut(), gpu.resident.limit())
        .unwrap();
    assert_eq!(
        before,
        gpu.resident.used(),
        "external snapshots must retain their backing"
    );
    drop(snapshot);
    gpu.compact_canvases(views.iter_mut(), gpu.resident.limit())
        .unwrap();
    gpu.collect().unwrap();
    assert!(
        before - gpu.resident.used() > 900 * 1024,
        "before={before} after={}",
        gpu.resident.used()
    );
    for (view, expected) in views.iter().zip(expected) {
        assert_eq!(
            gpu.readback(view, small.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            expected
        );
    }
}
