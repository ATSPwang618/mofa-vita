use krkr_protocol::graphics::{DrawFace, Fill, Rect, Size};
use krkr_render_wgpu::{Gpu, Image, Pixels};
use std::time::{Duration, Instant};

// These lifetime tests need an initially unique plane, independent of the
// optional cache used by create_image() for tiny uniform images.
fn unique_image(gpu: &Gpu, size: Size, color: u32) -> Image {
    use krkr_protocol::pixels::{Bytes, Pixels as Upload};

    let mut image = gpu.reserve_upload(size, true, false).unwrap();
    let [a, r, g, b] = color.to_be_bytes();
    let mut data = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for pixel in data.as_mut_slice().as_chunks_mut::<4>().0.iter_mut() {
        pixel.copy_from_slice(&[r, g, b, a]);
    }
    gpu.upload(
        &mut image,
        &Upload {
            size,
            main: Some(data),
            province: None,
        },
    )
    .unwrap();
    image
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn padded_stored_image_can_exceed_logical_width() {
    use krkr_protocol::pixels::{Bytes, Pixels as Upload};

    let gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
    let stored = Size {
        width: 4,
        height: 2,
    };
    let logical = Size {
        width: 3,
        height: 2,
    };
    let mut data = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for row in 0..2 {
        for column in 0..4 {
            let pixel = &mut data.as_mut_slice()[(row * 4 + column) * 4..][..4];
            pixel.copy_from_slice(&[10 + column as u8 * 10, row as u8 * 20, 90, 255]);
        }
    }
    let mut image = gpu.reserve_upload(stored, true, false).unwrap();
    gpu.upload_scaled(
        &mut image,
        &Upload {
            size: stored,
            main: Some(data),
            province: None,
        },
        logical,
    )
    .unwrap();
    assert_eq!(image.size, logical);
    let output = pixels(&gpu, &image, false);
    for row in 0..2 {
        for (column, expected_red) in [10, 30, 40].into_iter().enumerate() {
            let pixel = &output.data.as_slice()[(row * 3 + column) * 4..][..4];
            assert_eq!(pixel, &[expected_red, row as u8 * 20, 90, 255]);
        }
    }
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn full_upload_replaces_shared_or_compact_pixels_without_resolving_old_content() {
    use krkr_protocol::{
        budget::Budget,
        pixels::{Bytes, Pixels as Upload},
    };
    let mut gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
    let size = Size {
        width: 32,
        height: 16,
    };
    let mut data = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, pixel) in data
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        pixel.copy_from_slice(&[i as u8, (i / 32) as u8, 17, 137]);
    }
    let upload = Upload {
        size,
        main: Some(data),
        province: None,
    };
    for compact in [false, true] {
        let stored = if compact {
            Size {
                width: 4,
                height: 2,
            }
        } else {
            size
        };
        let original = gpu.create_image(stored, 0x80304050).unwrap();
        let mut target = gpu.logical_image(original, size).unwrap();
        let snapshot = target.shared();
        let resident = gpu.resident.used();
        let staging = std::mem::replace(&mut gpu.staging, Budget::new(0));
        assert!(gpu.upload(&mut target, &upload).is_err());
        assert_eq!(
            gpu.resident.used(),
            resident,
            "failed admission must not rasterize or detach"
        );
        gpu.staging = staging;
        let scratch = std::mem::replace(&mut gpu.scratch, Budget::new(0));
        gpu.upload(&mut target, &upload).unwrap();
        gpu.scratch = scratch;
        assert_eq!(
            pixels(&gpu, &target, false).data.as_slice(),
            upload.main.as_ref().unwrap().as_slice()
        );
        assert_eq!(
            pixels(&gpu, &snapshot, false).data.as_slice(),
            [0x30, 0x40, 0x50, 0x80].repeat(512)
        );
    }
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn sharing_detaches_only_written_planes_and_gpu_pins_do_not_cause_copies() {
    let mut gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
    let size = Size {
        width: 4,
        height: 1,
    };
    let mut a = unique_image(&gpu, size, 0x81112233);
    let mut write = Fill {
        rectangle: Rect {
            width: 1,
            ..size.rect()
        },
        color: 0x82445566,
        face: DrawFace::Alpha,
        hold_alpha: false,
    };
    gpu.fill(&mut a, &[write]).unwrap();
    assert_eq!(
        gpu.resident.used(),
        16,
        "in-flight allocation pins must not cause COW"
    );
    let mut b = a.shared();
    assert_eq!(gpu.resident.used(), 16, "assignment must share the texture");
    write.face = DrawFace::Mask;
    write.color = 99;
    gpu.fill(&mut b, &[write]).unwrap();
    assert_eq!(gpu.resident.used(), 32);
    assert_eq!(pixels(&gpu, &a, false).data.as_slice()[3], 130);
    assert_eq!(pixels(&gpu, &b, false).data.as_slice()[3], 99);
    write.face = DrawFace::Province;
    write.color = 7;
    gpu.fill(&mut a, &[write]).unwrap();
    let mut c = a.shared();
    let used = gpu.resident.used();
    write.color = 42;
    gpu.fill(&mut c, &[write]).unwrap();
    assert_eq!(
        gpu.resident.used(),
        used + 4,
        "province COW must not copy RGBA"
    );
    assert_eq!(pixels(&gpu, &a, true).data.as_slice(), [7, 0, 0, 0]);
    assert_eq!(pixels(&gpu, &c, true).data.as_slice(), [42, 0, 0, 0]);
    // A failed detach leaves both owners' pixels intact.
    let before = pixels(&gpu, &c, false).data.as_slice().to_vec();
    let budget = gpu.resident.clone();
    gpu.resident = krkr_protocol::budget::Budget::new(0);
    write.face = DrawFace::Alpha;
    write.color = 0xffffffff;
    assert!(gpu.fill(&mut c, &[write]).is_err());
    assert_eq!(pixels(&gpu, &c, false).data.as_slice(), before);
    assert_eq!(pixels(&gpu, &a, false).data.as_slice(), before);
    gpu.resident = budget;
    let source = a.source();
    gpu.copy_rect(
        &mut c,
        &source,
        size.rect(),
        0,
        0,
        size.rect(),
        DrawFace::Opaque,
        false,
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &c, false).data.as_slice(), before);
    let mut province = gpu.create_province(size).unwrap();
    assert!(gpu.readback(&province, size.rect(), false).is_err());
    let source = c.source();
    gpu.copy_rect(
        &mut province,
        &source,
        size.rect(),
        0,
        0,
        size.rect(),
        DrawFace::Province,
        false,
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &province, true).data.as_slice(), [42, 0, 0, 0]);
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn gpu_upload_uses_padded_rows_and_preserves_old_images_on_failure() {
    use krkr_protocol::pixels::{Bytes, Pixels as Upload};
    let mut gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
    let size = Size {
        width: 3,
        height: 2,
    };
    let old = gpu.create_image(size, 0x7f123456).unwrap();
    let mut staged = gpu.prepare_upload(&old, size, true, true).unwrap();
    let mut main = Bytes::zeroed(24, &gpu.staging).unwrap();
    let rgba: Vec<_> = (0..6).flat_map(|x| [x, 255 - x, x * 10, 33 + x]).collect();
    main.as_mut_slice().copy_from_slice(&rgba);
    let mut province = Bytes::zeroed(6, &gpu.staging).unwrap();
    province
        .as_mut_slice()
        .copy_from_slice(&[7, 42, 13, 0, 255, 1]);
    let upload = Upload {
        size,
        main: Some(main),
        province: Some(province),
    };
    gpu.upload(&mut staged, &upload).unwrap();
    drop(upload);
    assert_eq!(pixels(&gpu, &staged, false).data.as_slice(), rgba);
    assert_eq!(
        pixels(&gpu, &staged, true).data.as_slice(),
        [7, 42, 13, 0, 255, 1]
    );
    assert_eq!(
        pixels(&gpu, &old, false).data.as_slice(),
        [0x12, 0x34, 0x56, 0x7f].repeat(6)
    );
    // Province-only loading shares the main allocation and replaces only R8.
    let mut replacement = gpu.prepare_upload(&staged, size, false, true).unwrap();
    let mut province = Bytes::zeroed(6, &gpu.staging).unwrap();
    province.as_mut_slice().fill(91);
    gpu.upload(
        &mut replacement,
        &Upload {
            size,
            main: None,
            province: Some(province),
        },
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &replacement, false).data.as_slice(), rgba);
    assert_eq!(pixels(&gpu, &replacement, true).data.as_slice(), [91; 6]);
    assert_eq!(
        pixels(&gpu, &staged, true).data.as_slice(),
        [7, 42, 13, 0, 255, 1]
    );
    let prior = gpu.staging.clone();
    gpu.staging = krkr_protocol::budget::Budget::new(1);
    let data = Upload {
        size,
        main: Some(Bytes::zeroed(24, &prior).unwrap()),
        province: Some(Bytes::zeroed(6, &prior).unwrap()),
    };
    assert!(gpu.upload(&mut staged, &data).is_err());
    gpu.staging = prior;
    drop(data);
    assert_eq!(pixels(&gpu, &staged, false).data.as_slice(), rgba);
    let resident = gpu.resident.used();
    gpu.resident = krkr_protocol::budget::Budget::new(25);
    // Main fits, province does not: the partial reservation must be released.
    assert!(gpu.prepare_upload(&old, size, true, true).is_err());
    assert_eq!(gpu.resident.used(), 0);
    assert!(resident > 0);
}

fn pixels(gpu: &Gpu, image: &Image, province: bool) -> Pixels {
    let mut read = gpu.readback(image, image.size.rect(), province).unwrap();
    let start = Instant::now();
    loop {
        gpu.poll().unwrap();
        if let Some(result) = read.take() {
            return result.unwrap();
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "GPU readback did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn gpu_copy_clipping_overlap_resize_and_group_opacity() {
    use krkr_protocol::graphics::{Blend, ImageRef, Node, Scene};
    use std::{collections::HashMap, sync::Arc};
    let gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
    let mut image = gpu
        .create_image(
            Size {
                width: 4,
                height: 1,
            },
            0x40112233,
        )
        .unwrap();
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                left: 1,
                top: 0,
                width: 1,
                height: 1,
            },
            color: 0x90778899,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let source = image.source();
    gpu.copy_rect(
        &mut image,
        &source,
        Rect {
            left: 0,
            top: 0,
            width: 3,
            height: 1,
        },
        1,
        0,
        Size {
            width: 4,
            height: 1,
        }
        .rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &image, false).data.as_slice(),
        [
            17, 34, 51, 64, 17, 34, 51, 64, 119, 136, 153, 144, 17, 34, 51, 64
        ]
    );
    gpu.resize(
        &mut image,
        Size {
            width: 5,
            height: 2,
        },
        0xfedcba98,
    )
    .unwrap();
    let resized = pixels(&gpu, &image, false);
    assert_eq!(
        &resized.data.as_slice()[..16],
        &[
            17, 34, 51, 64, 17, 34, 51, 64, 119, 136, 153, 144, 17, 34, 51, 64
        ]
    );
    assert_eq!(
        &resized.data.as_slice()[16..],
        &[220, 186, 152, 254].repeat(6)
    );
    let mut destination = gpu
        .create_image(
            Size {
                width: 2,
                height: 1,
            },
            0xaa000000,
        )
        .unwrap();
    gpu.copy_rect(
        &mut destination,
        &image.source(),
        Rect {
            left: 0,
            top: 0,
            width: 4,
            height: 1,
        },
        -1,
        0,
        Size {
            width: 2,
            height: 1,
        }
        .rect(),
        DrawFace::Opaque,
        true,
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &destination, false).data.as_slice(),
        [17, 34, 51, 170, 119, 136, 153, 170]
    );
    gpu.copy_rect(
        &mut destination,
        &image.source(),
        Rect {
            left: 0,
            top: 0,
            width: 2,
            height: 1,
        },
        0,
        0,
        Size {
            width: 2,
            height: 1,
        }
        .rect(),
        DrawFace::Mask,
        false,
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &destination, false).data.as_slice(),
        [17, 34, 51, 64, 119, 136, 153, 64]
    );

    let mut ids = slotmap::SlotMap::with_key();
    let mut images = HashMap::new();
    let mut make = |color| {
        let id = ids.insert(());
        images.insert(
            id,
            gpu.create_image(
                Size {
                    width: 2,
                    height: 1,
                },
                color,
            )
            .unwrap(),
        );
        ImageRef {
            id,
            lifetime: Arc::default(),
        }
    };
    let base = make(0xff000000);
    let group = make(0xffff0000);
    let child = make(0xff00ff00);
    let full = Size {
        width: 2,
        height: 1,
    }
    .rect();
    let mut scene = Scene {
        viewport: Default::default(),
        transitions: Vec::new(),
        requires_op_seq: 0,
        nodes: vec![
            Node {
                cache: None,
                visible: true,
                parent: None,
                image: Some(base),
                neutral_color: 0,
                rectangle: full,
                image_left: 0,
                image_top: 0,
                blend: Blend::Opaque,
                opacity: 255,
            },
            Node {
                cache: None,
                visible: true,
                parent: Some(0),
                image: Some(group),
                neutral_color: 0,
                rectangle: full,
                image_left: 0,
                image_top: 0,
                blend: Blend::Alpha,
                opacity: 128,
            },
            Node {
                cache: None,
                visible: true,
                parent: Some(1),
                image: Some(child),
                neutral_color: 0,
                rectangle: Rect {
                    left: 1,
                    top: 0,
                    width: 1,
                    height: 1,
                },
                image_left: 0,
                image_top: 0,
                blend: Blend::Alpha,
                opacity: 255,
            },
        ],
    };
    let mut output = gpu
        .create_surface_image(Size {
            width: 2,
            height: 1,
        })
        .unwrap();
    gpu.compose(&mut output, &scene, &images).unwrap();
    // Stock integer alpha: the green child leaves [0,254,0] inside the
    // group; 255*128>>8 is 127, and 254*127>>8 is 126. Flattening would
    // leave a red tint instead of blending the completed group once.
    assert_eq!(
        pixels(&gpu, &output, false).data.as_slice(),
        [126, 0, 0, 255, 0, 126, 0, 255]
    );
    scene.nodes[1].image = None;
    gpu.compose(&mut output, &scene, &images).unwrap();
    assert_eq!(
        pixels(&gpu, &output, false).data.as_slice(),
        [0, 0, 0, 255, 0, 126, 0, 255]
    );
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn gpu_pixels_keep_main_alpha_and_province_planes_separate_and_charge_pending_work() {
    let gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
    eprintln!("adapter: {:?}", gpu.adapter.get_info());
    let mut image = unique_image(
        &gpu,
        Size {
            width: 4,
            height: 2,
        },
        0x12345678,
    );
    let rectangle = Rect {
        left: 1,
        top: 0,
        width: 2,
        height: 1,
    };
    gpu.fill(
        &mut image,
        &[
            Fill {
                rectangle,
                color: 0x90abcdef,
                face: DrawFace::Alpha,
                hold_alpha: true,
            },
            Fill {
                rectangle: Rect {
                    left: 0,
                    top: 1,
                    width: 4,
                    height: 1,
                },
                color: 0xfedcba98,
                face: DrawFace::Opaque,
                hold_alpha: true,
            },
            Fill {
                rectangle: Rect {
                    left: 2,
                    top: 0,
                    width: 20,
                    height: 1,
                },
                color: 37,
                face: DrawFace::Mask,
                hold_alpha: false,
            },
            Fill {
                rectangle,
                color: 291,
                face: DrawFace::Province,
                hold_alpha: false,
            },
        ],
    )
    .unwrap();
    let data = pixels(&gpu, &image, false);
    assert_eq!(
        data.data.as_slice(),
        [
            0x34, 0x56, 0x78, 0x12, 0xab, 0xcd, 0xef, 0x90, 0xab, 0xcd, 0xef, 37, 0x34, 0x56, 0x78,
            37, 0xdc, 0xba, 0x98, 0x12, 0xdc, 0xba, 0x98, 0x12, 0xdc, 0xba, 0x98, 0x12, 0xdc, 0xba,
            0x98, 0x12,
        ]
    );
    let province = pixels(&gpu, &image, true);
    assert_eq!(province.data.as_slice(), [0, 35, 35, 0, 0, 0, 0, 0]);
    assert_eq!(gpu.resident.used(), 40);
    drop(image);
    gpu.poll().unwrap();
    gpu.trim_resident_pool();
    assert_eq!(gpu.resident.used(), 0);
    assert_eq!(
        gpu.staging.used(),
        40,
        "completed readback retains only CPU bytes, not aligned GPU buffers"
    );
    drop(data);
    drop(province);
    assert_eq!(gpu.staging.used(), 0);

    let image = unique_image(
        &gpu,
        Size {
            width: 8,
            height: 8,
        },
        0xff112233,
    );
    let read = gpu.readback(&image, image.size.rect(), false).unwrap();
    drop(read);
    drop(image);
    let start = Instant::now();
    while gpu.resident.used() != 0 || gpu.staging.used() != 0 {
        gpu.poll().unwrap();
        gpu.trim_resident_pool();
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "cancelled readback retained its budget"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
#[ignore = "requires a real desktop GPU"]
fn regional_upload_preserves_neighbors_snapshots_and_province() {
    let mut gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();

    let size = Size {
        width: 8,
        height: 6,
    };
    let rectangle = Rect {
        left: 3,
        top: 2,
        width: 3,
        height: 2,
    };
    let patch_size = Size {
        width: rectangle.width,
        height: rectangle.height,
    };
    let mut data =
        krkr_protocol::pixels::Bytes::zeroed(patch_size.rgba_bytes().unwrap(), &gpu.staging)
            .unwrap();
    let patch_bytes = [
        17, 31, 47, 0, 11, 99, 27, 127, 255, 128, 1, 255, 63, 8, 99, 3, 171, 19, 39, 201, 31, 97,
        211, 93,
    ];
    data.as_mut_slice().copy_from_slice(&patch_bytes);
    let patch = krkr_protocol::pixels::Pixels {
        size: patch_size,
        main: Some(data),
        province: None,
    };
    for compact in [false, true] {
        let stored = if compact {
            Size {
                width: 2,
                height: 2,
            }
        } else {
            size
        };
        let source = gpu.create_image(stored, 0x80112233).unwrap();
        let mut target = gpu.logical_image(source, size).unwrap();
        gpu.fill(
            &mut target,
            &[Fill {
                rectangle: size.rect(),
                color: 73,
                face: DrawFace::Province,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let snapshot = target.shared();
        let original = pixels(&gpu, &target, false).data.as_slice().to_vec();
        let staging = std::mem::replace(&mut gpu.staging, krkr_protocol::budget::Budget::new(0));
        assert!(gpu.patch_region(&mut target, rectangle, &patch).is_err());
        gpu.staging = staging;
        assert_eq!(
            pixels(&gpu, &target, false).data.as_slice().to_vec(),
            original,
            "failed patch must preserve pixels"
        );
        assert!(
            gpu.patch_region(
                &mut target,
                Rect {
                    left: -1,
                    ..rectangle
                },
                &patch
            )
            .is_err()
        );
        gpu.patch_region(&mut target, rectangle, &patch).unwrap();
        let mut expected = original.clone();
        for row in 0..rectangle.height as usize {
            let dst = ((rectangle.top as usize + row) * size.width as usize
                + rectangle.left as usize)
                * 4;
            let src = row * rectangle.width as usize * 4;
            expected[dst..dst + 12].copy_from_slice(&patch_bytes[src..src + 12]);
        }
        assert_eq!(
            pixels(&gpu, &target, false).data.as_slice().to_vec(),
            expected,
            "region write must copy straight RGBA exactly"
        );
        assert_eq!(
            pixels(&gpu, &snapshot, false).data.as_slice().to_vec(),
            original,
            "shared source changed"
        );
        assert_eq!(
            pixels(&gpu, &target, true).data.as_slice().to_vec(),
            vec![73; 48],
            "main patch changed province pixels"
        );
    }
}
