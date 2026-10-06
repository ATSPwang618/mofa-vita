#![cfg(target_os = "linux")]
#![allow(unsafe_code)]
#[path = "../../render-gles2/tests/support/mod.rs"]
mod support;
use krkr_host_vita::graphics::Graphics;
use krkr_protocol::{
    budget::Budget,
    graphics::{Command, DrawFace, Fill, ImageRef, Rect, Size},
    pixels::{Bytes, Pixels},
    texture::{Compressed, Format},
    window::Response,
};
use krkr_render_gles2::{Config, Gpu};
use std::sync::{Arc, atomic::AtomicBool};

fn asset(size: Size) -> Compressed {
    let count = Format::Etc1.byte_len(size).unwrap();
    let mut bytes = Bytes::zeroed(count, &Budget::new(count)).unwrap();
    for (i, block) in bytes.as_mut_slice().chunks_exact_mut(8).enumerate() {
        let base = if i % 3 == 0 {
            [0x82, 0x8f, 0x94]
        } else {
            [0x12 + (i % 7) as u8, 0x34, 0x56]
        };
        block.copy_from_slice(&[
            base[0],
            base[1],
            base[2],
            ((i % 8) as u8) << 5 | (i % 2) as u8 | if i % 3 == 0 { 2 } else { 0 },
            0x93,
            0x69,
            0xa5,
            0x5a,
        ]);
    }
    Compressed::new(size, Format::Etc1, bytes, 0).unwrap()
}
fn read(gpu: &Gpu, image: &krkr_render_gles2::Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

#[test]
fn pvrtc_host_preserves_alpha_and_logical_size_with_desktop_fallback() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 32,
        height: 16,
    };
    let pvr = include_bytes!("../../krkr-image/tests/data/pvrtc-gradient.pvr");
    let mut bytes = Bytes::zeroed(pvr.len() - 52, &Budget::new(4096)).unwrap();
    bytes.as_mut_slice().copy_from_slice(&pvr[52..]);
    let texture = Arc::new(Compressed::new(size, Format::Pvrtc1Rgba4, bytes, 0).unwrap());
    let native = gpu.supports_compressed(&texture);
    let mut host = Graphics::new(gpu, krkr_protocol::image_cache::Cache::new(1024 * 1024));
    let mut ids = slotmap::SlotMap::with_key();
    let image = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let logical = Size {
        width: 64,
        height: 32,
    };
    let response = host
        .execute(&Command::LoadCompressed {
            image: image.clone(),
            texture: texture.clone(),
            logical_size: logical,
        })
        .unwrap();
    let Response::ImageStorage(storage) = response else {
        panic!("storage")
    };
    assert_eq!(
        storage,
        if native {
            texture.data().len()
        } else {
            size.rgba_bytes().unwrap()
        }
    );
    let Response::Image(actual) = host.execute(&Command::ReadImage { image }).unwrap() else {
        panic!("readback")
    };
    let expected =
        krkr_image::compressed::decode(&texture, &host.gpu.staging, &AtomicBool::new(false))
            .unwrap();
    let expanded = krkr_image::scale::expand(
        expected.main.as_ref().unwrap(),
        size,
        logical,
        &host.gpu.staging,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(actual.size, logical);
    assert_eq!(actual.main.unwrap().as_slice(), expanded.as_slice());
    assert!(expanded.as_slice().chunks_exact(4).any(|p| p[3] < 128));
}
#[test]
fn native_blocks_match_cpu_decode_and_expand_only_on_write_or_readback() {
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let texture = asset(Size {
            width: 64,
            height: 32,
        });
        assert!(
            gpu.supports_compressed(&texture),
            "Mesa must expose ETC1 for native texture tests"
        );
        let before = gpu.resident.used();
        let mut image = gpu.load_compressed(&texture).unwrap();
        assert_eq!(gpu.resident.used() - before, texture.data().len());
        assert_eq!(image.resident_bytes(), texture.data().len());
        assert_eq!(image.write_bytes(false), texture.size.rgba_bytes().unwrap());
        let expected =
            krkr_image::compressed::decode(&texture, &gpu.staging, &AtomicBool::new(false))
                .unwrap();
        assert_eq!(
            read(&gpu, &image),
            expected.main.as_ref().unwrap().as_slice()
        );
        assert_eq!(
            image.resident_bytes(),
            texture.data().len(),
            "readback must not expand retained asset"
        );
        let snapshot = image.shared();
        let area = Rect {
            left: 5,
            top: 7,
            width: 9,
            height: 4,
        };
        gpu.fill(
            &mut image,
            &[Fill {
                rectangle: area,
                color: 0xff29374b,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let mut expected_edit = expected.main.as_ref().unwrap().as_slice().to_vec();
        for y in 7..11 {
            for x in 5..14 {
                expected_edit[(y * 64 + x) * 4..][..4].copy_from_slice(&[41, 55, 75, 255]);
            }
        }
        assert_eq!(read(&gpu, &image), expected_edit);
        assert_eq!(
            read(&gpu, &snapshot),
            expected.main.as_ref().unwrap().as_slice()
        );
        assert_eq!(image.resident_bytes(), texture.size.rgba_bytes().unwrap());
        assert_eq!(snapshot.resident_bytes(), texture.data().len());
        let mut copied = snapshot.shared();
        gpu.independ(&mut copied, false, true).unwrap();
        assert_eq!(read(&gpu, &copied), read(&gpu, &snapshot));
        let mut overwritten = snapshot.shared();
        let main = Bytes::zeroed(texture.size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        gpu.upload(
            &mut overwritten,
            &Pixels {
                size: texture.size,
                main: Some(main),
                province: None,
            },
        )
        .unwrap();
        assert!(read(&gpu, &overwritten).iter().all(|&b| b == 0));
        assert_eq!(read(&gpu, &snapshot), expected.main.unwrap().as_slice());
    }
}
#[test]
fn compressed_admission_succeeds_without_room_for_rgba_and_failed_write_keeps_pixels() {
    let context = support::Context::new();
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
    let texture = asset(Size {
        width: 128,
        height: 64,
    });
    let lock = gpu
        .resident
        .reserve(gpu.resident.available() - texture.data().len())
        .unwrap();
    let mut image = gpu.load_compressed(&texture).unwrap();
    let before = read(&gpu, &image);
    assert!(
        gpu.fill(
            &mut image,
            &[Fill {
                rectangle: texture.size.rect(),
                color: 0xffffffff,
                face: DrawFace::Alpha,
                hold_alpha: false
            }]
        )
        .is_err()
    );
    assert_eq!(read(&gpu, &image), before);
    drop(lock);
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: texture.size.rect(),
            color: 0xffffffff,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert!(read(&gpu, &image).iter().all(|&b| b == 255));
}
#[test]
fn host_upload_preserves_logical_dimensions_and_falls_back_for_tiled_sources() {
    for (edge, width) in [(32, 64), (1024, 64), (1024, 63), (1024, 4)] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    tile_edge: edge,
                    work_framebuffer: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let mut host = Graphics::new(gpu, krkr_protocol::image_cache::Cache::new(1024 * 1024));
        let texture = Arc::new(asset(Size { width, height: 32 }));
        assert_eq!(
            host.gpu.supports_compressed(&texture),
            edge == 1024 && width == 64
        );
        let logical = Size {
            width: 126,
            height: 70,
        };
        let mut ids = slotmap::SlotMap::with_key();
        let image = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let response = host
            .execute(&Command::LoadCompressed {
                image: image.clone(),
                texture: texture.clone(),
                logical_size: logical,
            })
            .unwrap();
        let Response::ImageStorage(bytes) = response else {
            panic!("storage response")
        };
        assert_eq!(
            bytes,
            if edge == 1024 && width == 64 {
                texture.data().len()
            } else {
                texture.size.rgba_bytes().unwrap()
            }
        );
        let Response::Image(actual) = host.execute(&Command::ReadImage { image }).unwrap() else {
            panic!("pixels response")
        };
        let decoded =
            krkr_image::compressed::decode(&texture, &host.gpu.staging, &AtomicBool::new(false))
                .unwrap();
        let expected = krkr_image::scale::expand(
            decoded.main.as_ref().unwrap(),
            texture.size,
            logical,
            &host.gpu.staging,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(actual.size, logical);
        assert_eq!(
            actual.main.as_ref().unwrap().as_slice(),
            expected.as_slice()
        );
    }
}

#[test]
fn native_compressed_grid_matches_cpu_and_only_written_tiles_expand() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 32,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 96,
        height: 32,
    };
    let tile_size = Size {
        width: 32,
        height: 16,
    };
    let length = Compressed::payload_len(size, tile_size, Format::Etc1).unwrap();
    let mut bytes = Bytes::zeroed(length, &Budget::new(length)).unwrap();
    for (i, block) in bytes.as_mut_slice().chunks_exact_mut(8).enumerate() {
        block.copy_from_slice(&[0x11 + ((i / 32) * 0x11) as u8, 0x34, 0x56, 0, 0, 0, 0, 0]);
    }
    let texture = Compressed::tiled(size, tile_size, Format::Etc1, bytes, 0).unwrap();
    assert!(gpu.supports_compressed(&texture));
    let before = gpu.resident.used();
    let mut image = gpu.load_compressed(&texture).unwrap();
    assert_eq!(gpu.resident.used() - before, length);
    let expected = krkr_image::compressed::decode(&texture, &gpu.staging, &AtomicBool::new(false))
        .unwrap()
        .main
        .unwrap();
    assert_eq!(read(&gpu, &image), expected.as_slice());
    let snapshot = image.shared();
    // A rectangle crossing both axes exercises four distinct compressed tiles.
    let rectangle = Rect {
        left: 30,
        top: 14,
        width: 5,
        height: 5,
    };
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle,
            color: 0xffaabbcc,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(read(&gpu, &snapshot), expected.as_slice());
    let actual = read(&gpu, &image);
    for y in 0..32usize {
        for x in 0..96usize {
            let at = (y * 96 + x) * 4;
            let pixel: &[u8] = if (30..35).contains(&x) && (14..19).contains(&y) {
                &[0xaa, 0xbb, 0xcc, 255]
            } else {
                &expected.as_slice()[at..at + 4]
            };
            assert_eq!(&actual[at..at + 4], pixel, "pixel {x},{y}");
        }
    }
    // Six immutable compressed tiles plus four writable RGBA tiles, with no
    // full-canvas expansion or duplicate reservation for the shared snapshot.
    assert_eq!(
        gpu.resident.used() - before,
        length + tile_size.rgba_bytes().unwrap() * 4
    );
}

#[test]
fn host_compressed_grid_preserves_logical_coordinates_on_native_and_fallback_paths() {
    for edge in [16, 32] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    tile_edge: edge,
                    work_framebuffer: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 96,
            height: 32,
        };
        let tile = Size {
            width: 32,
            height: 16,
        };
        let blocks = asset(tile).data().repeat(6);
        let mut bytes = Bytes::zeroed(blocks.len(), &Budget::new(blocks.len())).unwrap();
        bytes.as_mut_slice().copy_from_slice(&blocks);
        let texture = Arc::new(Compressed::tiled(size, tile, Format::Etc1, bytes, 0).unwrap());
        let mut host = Graphics::new(gpu, krkr_protocol::image_cache::Cache::new(1024 * 1024));
        let mut ids = slotmap::SlotMap::with_key();
        let image = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let logical = Size {
            width: 192,
            height: 64,
        };
        let Response::ImageStorage(storage) = host
            .execute(&Command::LoadCompressed {
                image: image.clone(),
                texture: texture.clone(),
                logical_size: logical,
            })
            .unwrap()
        else {
            panic!("storage response")
        };
        assert_eq!(
            storage,
            if edge == 32 {
                blocks.len()
            } else {
                size.rgba_bytes().unwrap()
            }
        );
        let Response::Image(actual) = host.execute(&Command::ReadImage { image }).unwrap() else {
            panic!("pixels response")
        };
        let decoded =
            krkr_image::compressed::decode(&texture, &host.gpu.staging, &AtomicBool::new(false))
                .unwrap();
        let expected = krkr_image::scale::expand(
            decoded.main.as_ref().unwrap(),
            size,
            logical,
            &host.gpu.staging,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(actual.size, logical);
        assert_eq!(actual.main.unwrap().as_slice(), expected.as_slice());
    }
}

#[test]
fn compressed_grid_affine_and_filtered_stretch_match_rgba_with_different_tile_layout() {
    use krkr_protocol::transform::{Filter, ImageOperation, Sampling, StretchRect, Transform};
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    tile_edge: 32,
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 96,
            height: 32,
        };
        let tile = Size {
            width: 32,
            height: 16,
        };
        let blocks = asset(tile).data().repeat(6);
        let mut bytes = Bytes::zeroed(blocks.len(), &Budget::new(blocks.len())).unwrap();
        bytes.as_mut_slice().copy_from_slice(&blocks);
        let texture = Compressed::tiled(size, tile, Format::Etc1, bytes, 0).unwrap();
        let native = gpu.load_compressed(&texture).unwrap();
        let pixels =
            krkr_image::compressed::decode(&texture, &gpu.staging, &AtomicBool::new(false))
                .unwrap();
        let rgba = gpu.assign_bitmap(None, &pixels).unwrap();
        let output = Size {
            width: 53,
            height: 29,
        };
        for filter in [Filter::Nearest, Filter::FastLinear, Filter::Cubic] {
            for transform in [
                Transform::Stretch(StretchRect {
                    left: 0,
                    top: 0,
                    width: 53,
                    height: 29,
                }),
                Transform::Affine([[1.2, 3.1], [50.3, -2.7], [6.6, 27.2]]),
            ] {
                if filter == Filter::Cubic && matches!(transform, Transform::Affine(_)) {
                    continue;
                }
                let render = |source| {
                    let mut target = gpu.create_image(output, 0x79452317).unwrap();
                    gpu.transform(
                        &mut target,
                        source,
                        size.rect(),
                        transform,
                        Sampling {
                            filter,
                            sharpness: -1.,
                            no_clip: false,
                        },
                        ImageOperation::Copy { hold_alpha: false },
                        output.rect(),
                        None,
                    )
                    .unwrap();
                    read(&gpu, &target)
                };
                assert_eq!(
                    render(&native),
                    render(&rgba),
                    "{work_framebuffer} {filter:?} {transform:?}"
                );
            }
        }
    }
}
