use krkr_protocol::graphics::{Blend, BlendOptions, DrawFace, Fill, Rect, Size};
use krkr_render_wgpu::{Gpu, Image};
use std::time::{Duration, Instant};

fn gpu() -> Gpu {
    pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap()
}
fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    let mut read = gpu.readback(image, image.size.rect(), false).unwrap();
    let start = Instant::now();
    loop {
        gpu.poll().unwrap();
        if let Some(result) = read.take() {
            return result.unwrap().data.as_slice().to_vec();
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn options(mode: Blend, opacity: u8) -> BlendOptions {
    BlendOptions {
        mode,
        opacity,
        face: DrawFace::Opaque,
        hold_alpha: true,
    }
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn all_drawable_modes_use_legacy_integer_colors() {
    // Formula fixtures from krkrz visual/gl blend_functor_c.h and its lookup
    // tables. Screen uses the complement shared by HDA and SSE2 paths.
    // D = RGBA(80,150,220,100), S = RGBA(200,100,40,160), HDA enabled.
    let cases = [
        (1, [200, 100, 40], [140, 125, 130]),
        (2, [155, 118, 107], [117, 134, 163]),
        (3, [255, 250, 255], [180, 200, 240]),
        (4, [25, 0, 5], [53, 73, 113]),
        (5, [62, 58, 34], [71, 104, 127]),
        (8, [255, 247, 255], [131, 186, 238]),
        (9, [80, 100, 40], [80, 125, 130]),
        (10, [200, 150, 220], [140, 150, 220]),
        (11, [218, 192, 226], [150, 171, 223]),
        (12, [229, 155, 121], [154, 152, 170]),
        (13, [155, 118, 107], [117, 134, 163]),
        (14, [189, 212, 241], [134, 181, 230]),
        (15, [45, 56, 85], [62, 103, 152]),
        (16, [68, 92, 103], [74, 121, 161]),
        (17, [166, 176, 223], [123, 163, 221]),
        (18, [108, 135, 204], [94, 142, 212]),
        (19, [141, 129, 125], [110, 139, 172]),
        (20, [105, 139, 206], [92, 144, 213]),
        (21, [189, 210, 241], [134, 180, 230]),
        (22, [156, 198, 243], [105, 170, 230]),
        (23, [50, 56, 102], [65, 103, 161]),
        (24, [155, 150, 220], [117, 150, 220]),
        (25, [80, 118, 107], [80, 134, 163]),
        (26, [105, 87, 195], [92, 118, 207]),
        (27, [45, 88, 195], [18, 119, 208]),
        (28, [126, 139, 202], [103, 144, 211]),
    ];
    let gpu = gpu();
    let size = Size {
        width: 52,
        height: 1,
    };
    let mut destination = gpu.create_image(size, 0x645096dc).unwrap();
    let source = gpu.create_image(size, 0xa0c86428).unwrap();
    let mut expected = Vec::new();
    for (index, (mode, full, half)) in cases.into_iter().enumerate() {
        for (column, (opacity, rgb)) in [(255, full), (128, half)].into_iter().enumerate() {
            let x = (index * 2 + column) as i32;
            gpu.operate_rect(
                &mut destination,
                &source.source(),
                Rect {
                    left: 0,
                    top: 0,
                    width: 1,
                    height: 1,
                },
                x,
                0,
                size.rect(),
                options(Blend::from_legacy(mode).unwrap(), opacity),
            )
            .unwrap();
            expected.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 100]);
        }
    }
    assert_eq!(pixels(&gpu, &destination), expected);
    // One 321 KiB lookup, one clipped pixel per outstanding draw; no full
    // source or destination-sized blend copies are required by this batch.
    assert_eq!(gpu.resident.used(), 256 * (256 + 65) * 4 + 2 * 52 * 4);
    assert!(gpu.scratch.used() <= 52 * 4);
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn color_and_alpha_faces_preserve_their_distinct_semantics() {
    let gpu = gpu();
    let size = Size {
        width: 1,
        height: 1,
    };
    let source = gpu.create_image(size, 0xa0c86428).unwrap();
    for (mode, face, opacity, hold_alpha, expected) in [
        (
            Blend::Alpha,
            DrawFace::Opaque,
            255,
            false,
            // TVPAlphaBlend interpolates all four bytes on an opaque face:
            // 100 + ((160 - 100) * 160 >> 8) = 137, as in the GLES fixture.
            [155, 118, 107, 137],
        ),
        (
            Blend::Alpha,
            DrawFace::Alpha,
            255,
            false,
            [176, 109, 75, 198],
        ),
        (
            Blend::Alpha,
            DrawFace::Alpha,
            128,
            true,
            [144, 123, 123, 149],
        ),
        (
            Blend::Alpha,
            DrawFace::AddAlpha,
            255,
            true,
            [154, 117, 106, 198],
        ),
        (
            Blend::AddAlpha,
            DrawFace::Alpha,
            255,
            false,
            [80, 150, 220, 100],
        ),
        (
            Blend::AddAlpha,
            DrawFace::AddAlpha,
            128,
            true,
            [154, 152, 170, 149],
        ),
        (
            Blend::Opaque,
            DrawFace::Alpha,
            255,
            true,
            [200, 100, 40, 255],
        ),
        (
            Blend::Opaque,
            DrawFace::AddAlpha,
            128,
            true,
            [239, 174, 149, 178],
        ),
        (
            Blend::PsDifference5,
            DrawFace::Mask,
            255,
            false,
            [45, 88, 195, 100],
        ),
    ] {
        let mut destination = gpu.create_image(size, 0x645096dc).unwrap();
        gpu.operate_rect(
            &mut destination,
            &source.source(),
            size.rect(),
            0,
            0,
            size.rect(),
            BlendOptions {
                mode,
                face,
                opacity,
                hold_alpha,
            },
        )
        .unwrap();
        assert_eq!(
            pixels(&gpu, &destination),
            expected,
            "{mode:?} {face:?} {opacity}"
        );
    }
    for (face, opacity, expected) in [
        (DrawFace::Opaque, 128, [139, 124, 129, 100]),
        (DrawFace::Alpha, 128, [165, 114, 91, 179]),
        (DrawFace::AddAlpha, 128, [139, 124, 129, 178]),
        (DrawFace::Alpha, -128, [80, 150, 220, 49]),
        (DrawFace::Alpha, -255, [80, 150, 220, 0]),
        (DrawFace::Opaque, -1, [79, 149, 219, 100]),
        (DrawFace::Opaque, 255, [200, 100, 40, 100]),
        (DrawFace::Alpha, 255, [200, 100, 40, 255]),
        (DrawFace::Mask, 0, [80, 150, 220, 40]),
    ] {
        let mut destination = gpu.create_image(size, 0x645096dc).unwrap();
        gpu.color_rect(&mut destination, size.rect(), 0xc86428, opacity, face)
            .unwrap();
        assert_eq!(
            pixels(&gpu, &destination),
            expected,
            "color {face:?} {opacity}"
        );
    }
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn overlap_cow_and_clipping_use_owned_snapshots() {
    let gpu = gpu();
    let size = Size {
        width: 4,
        height: 1,
    };
    let mut image = gpu.create_image(size, 0x64646464).unwrap();
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                width: 1,
                ..size.rect()
            },
            color: 0x64141414,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let source = image.source();
    gpu.operate_rect(
        &mut image,
        &source,
        Rect {
            width: 3,
            ..size.rect()
        },
        1,
        0,
        size.rect(),
        options(Blend::Additive, 255),
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &image),
        [
            20, 20, 20, 100, 120, 120, 120, 100, 200, 200, 200, 100, 200, 200, 200, 100
        ]
    );
    let saved = image.shared();
    let source = image.source();
    // Logical sharing detaches the write plane; the read dependency retains
    // the original allocation even when its source and destination overlap.
    gpu.operate_rect(
        &mut image,
        &source,
        size.rect(),
        -1,
        0,
        Rect {
            left: 1,
            width: 2,
            ..size.rect()
        },
        options(Blend::Subtractive, 255),
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &image),
        [
            20, 20, 20, 100, 65, 65, 65, 100, 145, 145, 145, 100, 200, 200, 200, 100
        ]
    );
    assert_eq!(
        pixels(&gpu, &saved),
        [
            20, 20, 20, 100, 120, 120, 120, 100, 200, 200, 200, 100, 200, 200, 200, 100
        ]
    );
    let source = image.source();
    gpu.operate_rect(
        &mut image,
        &source,
        size.rect(),
        0,
        0,
        size.rect(),
        options(Blend::Additive, 255),
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &image),
        [
            40, 40, 40, 100, 130, 130, 130, 100, 255, 255, 255, 100, 255, 255, 255, 100
        ]
    );
}
