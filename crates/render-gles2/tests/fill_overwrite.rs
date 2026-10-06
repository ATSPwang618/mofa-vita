#![cfg(target_os = "linux")]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
    texture::{Compressed, Format},
};
use krkr_render_gles2::{Config, Gpu, Image};

fn fill(rectangle: Rect, color: u32) -> Fill {
    Fill {
        rectangle,
        color,
        face: DrawFace::Alpha,
        hold_alpha: false,
    }
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}
fn patterned(gpu: &Gpu, size: Size) -> Image {
    let mut bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, p) in bytes.as_mut_slice().chunks_exact_mut(4).enumerate() {
        p.copy_from_slice(&[
            (i * 37) as u8,
            (i * 13) as u8,
            (i * 7) as u8,
            (i * 11) as u8,
        ]);
    }
    let mut image = gpu.reserve_upload(size, true, false).unwrap();
    gpu.upload(
        &mut image,
        &Pixels {
            size,
            main: Some(bytes),
            province: None,
        },
    )
    .unwrap();
    image
}
fn apply(pixels: &mut [u8], size: Size, fills: &[Fill]) {
    for fill in fills {
        let Some(r) = fill.rectangle.intersection(size.rect()) else {
            continue;
        };
        let c = fill.color;
        for y in r.top as usize..r.top as usize + r.height as usize {
            for x in r.left as usize..r.left as usize + r.width as usize {
                let p = &mut pixels[(y * size.width as usize + x) * 4..][..4];
                match fill.face {
                    DrawFace::Mask => p[3] = c as u8,
                    DrawFace::Opaque if fill.hold_alpha => {
                        p[..3].copy_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8])
                    }
                    _ => p.copy_from_slice(&[
                        (c >> 16) as u8,
                        (c >> 8) as u8,
                        c as u8,
                        (c >> 24) as u8,
                    ]),
                }
            }
        }
    }
}

#[test]
fn unchanged_shared_borders_need_no_storage_or_gpu_commands() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    tile_edge: 32,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 64,
            height: 32,
        };
        let color = 0x71325476;
        let mut source = gpu.create_image(size, color).unwrap();
        gpu.fill(
            &mut source,
            &[fill(
                Rect {
                    left: 8,
                    top: 8,
                    width: 48,
                    height: 16,
                },
                0xe1234567,
            )],
        )
        .unwrap();
        let expected = read(&gpu, &source);
        let mut target = source.shared();
        let borders = [
            fill(
                Rect {
                    left: 0,
                    top: 0,
                    width: 64,
                    height: 8,
                },
                color,
            ),
            Fill {
                face: DrawFace::Opaque,
                hold_alpha: true,
                ..fill(
                    Rect {
                        left: 0,
                        top: 24,
                        width: 64,
                        height: 8,
                    },
                    color ^ 0xff000000,
                )
            },
            Fill {
                face: DrawFace::Mask,
                ..fill(
                    Rect {
                        left: 0,
                        top: 8,
                        width: 8,
                        height: 16,
                    },
                    color >> 24,
                )
            },
        ];
        gpu.collect().unwrap();
        let _pressure = gpu.resident.reserve(gpu.resident.available()).unwrap();
        assert_eq!(gpu.fill_write_bytes(&target, &borders), 0);
        traffic::reset();
        gpu.fill(&mut target, &borders).unwrap();
        assert_eq!(
            (
                traffic::draw_calls(),
                traffic::clear_calls(),
                traffic::store_calls()
            ),
            (0, 0, 0)
        );
        assert_eq!(read(&gpu, &target), expected);
        assert_eq!(read(&gpu, &source), expected);
        // A clear crossing the modified center must still detach and must not
        // be accepted under the exhausted budget.
        let crossing = [fill(
            Rect {
                left: 7,
                top: 7,
                width: 4,
                height: 4,
            },
            color,
        )];
        assert!(gpu.fill_write_bytes(&target, &crossing) > 0);
        assert!(gpu.fill(&mut target, &crossing).is_err());
        assert_eq!(read(&gpu, &target), expected);
    }
}

#[test]
fn ordered_clears_cannot_skip_a_later_restore() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 16,
        height: 16,
    };
    let mut image = gpu.create_image(size, 0x71325476).unwrap();
    let mut expected = read(&gpu, &image);
    let fills = [
        fill(
            Rect {
                left: 2,
                top: 2,
                width: 8,
                height: 8,
            },
            0x99123456,
        ),
        fill(
            Rect {
                left: 4,
                top: 4,
                width: 8,
                height: 8,
            },
            0x71325476,
        ),
    ];
    apply(&mut expected, size, &fills);
    gpu.fill(&mut image, &fills).unwrap();
    assert_eq!(read(&gpu, &image), expected);
}

#[test]
fn clipped_clear_restores_a_shared_compact_solid_without_detaching() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    canvas_limit: Some(Size {
                        width: 48,
                        height: 24,
                    }),
                    tile_edge: 32,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 64,
            height: 32,
        };
        gpu.set_canvas_size(size);
        let original = gpu.create_image(size, 0x71325476).unwrap();
        let mut edited = original.shared();
        let patch = Rect {
            left: 8,
            top: 8,
            width: 48,
            height: 16,
        };
        gpu.fill(&mut edited, &[fill(patch, 0xe1234567)]).unwrap();
        assert_eq!(
            edited.stored_size(),
            Some(Size {
                width: 48,
                height: 24
            })
        );
        let old_pixels = read(&gpu, &edited);
        let mut cleared = edited.shared();
        let restore = [fill(patch, 0x71325476)];
        gpu.collect().unwrap();
        let _pressure = gpu.resident.reserve(gpu.resident.available()).unwrap();
        assert_eq!(gpu.fill_write_bytes(&cleared, &restore), 0);
        traffic::reset();
        gpu.fill(&mut cleared, &restore).unwrap();
        assert_eq!(
            (
                traffic::draw_calls(),
                traffic::clear_calls(),
                traffic::store_calls()
            ),
            (0, 0, 0)
        );
        assert_eq!(cleared.resident_bytes(), 4);
        assert_eq!(read(&gpu, &cleared), read(&gpu, &original));
        assert_eq!(read(&gpu, &edited), old_pixels);
        let incomplete = [fill(
            Rect {
                width: patch.width - 4,
                ..patch
            },
            0x71325476,
        )];
        let mut untouched = edited.shared();
        assert!(gpu.fill_write_bytes(&untouched, &incomplete) > 0);
        assert!(gpu.fill(&mut untouched, &incomplete).is_err());
        assert_eq!(read(&gpu, &untouched), old_pixels);
    }
}

#[test]
fn complete_shared_frame_overwrite_avoids_old_texture_transfer() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 960,
            height: 544,
        };
        let old = patterned(&gpu, size);
        let expected_old = read(&gpu, &old);
        let mut baseline = old.shared();
        gpu.flush().unwrap();
        traffic::reset();
        // The former fill path detached shared storage with a full copy.
        gpu.independ(&mut baseline, false, true).unwrap();
        gpu.fill(&mut baseline, &[fill(size.rect(), 0x8c234567)])
            .unwrap();
        gpu.flush().unwrap();
        let before = (traffic::loaded_pixels(), traffic::stored_pixels());
        let expected = read(&gpu, &baseline);
        let mut optimized = old.shared();
        traffic::reset();
        gpu.fill(&mut optimized, &[fill(size.rect(), 0x8c234567)])
            .unwrap();
        gpu.flush().unwrap();
        let after = (traffic::loaded_pixels(), traffic::stored_pixels());
        assert_eq!(traffic::read_calls(), 0);
        if work {
            assert!(after.0 <= 1);
            assert!(before.0 >= 960 * 544);
            assert_eq!(after.1, 960 * 544);
        } else {
            assert_eq!(before.1 - after.1, 960 * 544);
        }
        assert_eq!(read(&gpu, &optimized), expected);
        assert_eq!(read(&gpu, &old), expected_old);
        eprintln!("work={work}: shared 960x544 clear load/store pixels {before:?} -> {after:?}");
    }
}

#[test]
fn tiled_batches_preserve_holes_channels_order_and_old_snapshots() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    tile_edge: 8,
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 24,
            height: 16,
        };
        let old = patterned(&gpu, size);
        let original = read(&gpu, &old);
        let r = |x, y, w, h| Rect {
            left: x,
            top: y,
            width: w,
            height: h,
        };
        let cases = [
            vec![
                fill(r(0, 0, 8, 8), 0x91827364),
                fill(r(16, 8, 8, 8), 0x21436587),
            ],
            vec![
                fill(r(-1, 0, 10, 8), 0x12345678),
                fill(r(16, 8, 8, 8), 0x91234567),
            ],
            vec![
                Fill {
                    face: DrawFace::Mask,
                    ..fill(size.rect(), 97)
                },
                fill(r(0, 0, 8, 8), 0xabcdef12),
            ],
            vec![
                fill(r(0, 0, 8, 8), 0x18345672),
                Fill {
                    face: DrawFace::Opaque,
                    hold_alpha: true,
                    ..fill(size.rect(), 0x6789abcd)
                },
            ],
            vec![Fill {
                face: DrawFace::Opaque,
                hold_alpha: true,
                ..fill(size.rect(), 0x6789abcd)
            }],
            vec![Fill {
                face: DrawFace::Mask,
                ..fill(size.rect(), 84)
            }],
        ];
        for (index, fills) in cases.iter().enumerate() {
            let mut image = old.shared();
            let mut expected = original.clone();
            apply(&mut expected, size, fills);
            let before = gpu.resident.used();
            let required = gpu.fill_write_bytes(&image, fills);
            gpu.fill(&mut image, fills).unwrap();
            if index == 0 {
                assert_eq!(required, 2 * 8 * 8 * 4);
                assert_eq!(
                    gpu.resident.used() - before,
                    required,
                    "untouched tiles must remain shared"
                );
            }
            assert_eq!(read(&gpu, &image), expected, "work={work} case={index}");
            assert_eq!(read(&gpu, &old), original);
            drop(image);
            gpu.collect().unwrap();
        }
    }
}

#[test]
fn complete_fill_can_replace_native_compressed_storage_without_sampling_it() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    tile_edge: 8,
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 16,
            height: 16,
        };
        let tile = Size {
            width: 8,
            height: 8,
        };
        let mut bytes = Bytes::zeroed(128, &gpu.staging).unwrap();
        for block in bytes.as_mut_slice().chunks_exact_mut(8) {
            block.copy_from_slice(&[0x24, 0x68, 0xac, 0, 0x12, 0x34, 0x56, 0x78]);
        }
        let texture = Compressed::tiled(size, tile, Format::Etc1, bytes, 0).unwrap();
        assert!(gpu.supports_compressed(&texture));
        let old = gpu.load_compressed(&texture).unwrap();
        let original = read(&gpu, &old);
        let mut image = old.shared();
        traffic::reset();
        gpu.fill(&mut image, &[fill(size.rect(), 0x67452301)])
            .unwrap();
        gpu.flush().unwrap();
        assert_eq!(traffic::read_calls(), 0);
        assert_eq!(
            traffic::draw_calls(),
            if work { 4 } else { 0 },
            "no ETC1-to-RGBA copy pass"
        );
        assert_eq!(old.resident_bytes(), 128);
        assert_eq!(read(&gpu, &old), original);
        assert!(
            read(&gpu, &image)
                .chunks_exact(4)
                .all(|p| p == [0x45, 0x23, 1, 0x67])
        );
    }
}

#[test]
fn allocation_failure_during_clear_does_not_publish_uninitialized_tiles() {
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
        width: 24,
        height: 8,
    };
    let old = patterned(&gpu, size);
    let expected = read(&gpu, &old);
    let mut image = old.shared();
    let _pressure = gpu
        .resident
        .reserve(gpu.resident.available() - 256)
        .unwrap();
    assert!(
        gpu.fill(&mut image, &[fill(size.rect(), 0x12345678)])
            .is_err()
    );
    assert_eq!(read(&gpu, &image), expected);
    assert_eq!(read(&gpu, &old), expected);
}

#[test]
fn complete_compact_fill_skips_resampling_the_discarded_picture() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    tile_edge: 8,
                    work_framebuffer: work,
                    canvas_limit: Some(Size {
                        width: 8,
                        height: 8,
                    }),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let logical = Size {
            width: 37,
            height: 23,
        };
        gpu.set_canvas_size(logical);
        let old = gpu
            .logical_image(
                patterned(
                    &gpu,
                    Size {
                        width: 32,
                        height: 24,
                    },
                ),
                logical,
            )
            .unwrap();
        let original = read(&gpu, &old);
        let mut image = old.shared();
        let fills = [fill(
            Rect {
                left: -2,
                top: -2,
                width: 41,
                height: 27,
            },
            0x893456ab,
        )];
        assert_eq!(gpu.fill_write_bytes(&image, &fills), 4);
        traffic::reset();
        gpu.fill(&mut image, &fills).unwrap();
        gpu.flush().unwrap();
        assert_eq!(
            image.stored_size(),
            Some(Size {
                width: 1,
                height: 1
            })
        );
        assert_eq!(
            traffic::draw_calls(),
            usize::from(work),
            "no resampling the original bitmap"
        );
        assert_eq!(traffic::read_calls(), 0);
        assert!(
            read(&gpu, &image)
                .chunks_exact(4)
                .all(|p| p == [0x34, 0x56, 0xab, 0x89])
        );
        assert_eq!(read(&gpu, &old), original);
    }
}

#[test]
fn mixed_plane_admission_does_not_promise_the_same_idle_tile_twice() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 8,
                work_framebuffer: true,
                canvas_limit: Some(Size {
                    width: 8,
                    height: 8,
                }),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 8,
        height: 8,
    };
    let mut image = gpu
        .logical_image(
            patterned(
                &gpu,
                Size {
                    width: 16,
                    height: 16,
                },
            ),
            size,
        )
        .unwrap();
    let snapshot = image.shared();
    let idle = gpu.reserve_upload(size, true, false).unwrap();
    gpu.flush().unwrap();
    drop(idle);
    gpu.maintain().unwrap();
    let fills = [
        fill(size.rect(), 0x673412ab),
        Fill {
            face: DrawFace::Province,
            ..fill(size.rect(), 97)
        },
    ];
    assert_eq!(gpu.fill_write_bytes(&image, &fills), 256);
    let before = gpu.resident.used();
    gpu.fill(&mut image, &fills).unwrap();
    // The main clear now needs one texel; province takes the reusable tile.
    // Mixed-plane admission remains conservative before executing either fill.
    assert_eq!(gpu.resident.used() - before, 4);
    assert_eq!(gpu.pixel(&image, 2, 3, false).unwrap(), 0x673412ab);
    assert_eq!(gpu.pixel(&image, 2, 3, true).unwrap(), 97);
    assert!(!snapshot.has_province());
}
