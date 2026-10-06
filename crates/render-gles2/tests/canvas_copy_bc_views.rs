#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, Rect, Size},
    pixels::Bytes,
    texture::{Compressed, Format, reorder_bc},
};
use krkr_render_gles2::{Config, Gpu, Image};

fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

fn sprite(gpu: &Gpu, format: Format) -> Image {
    let size = Size {
        width: 32,
        height: 16,
    };
    let tile = Size {
        width: 16,
        height: 16,
    };
    let length = Compressed::payload_len(size, tile, format).unwrap();
    let mut bytes = Bytes::zeroed(length, &Budget::new(length)).unwrap();
    let stride = if format.linear() == Format::Bc3Rgba {
        16
    } else {
        8
    };
    for (i, block) in bytes.as_mut_slice().chunks_exact_mut(stride).enumerate() {
        if stride == 16 {
            block[..8].copy_from_slice(&[255, 0, 0x88, 0xc6, 0xfa, 0x88, 0xc6, 0xfa]);
        }
        let endpoint = [0xf800u16, 0x07e0, 0x001f][i % 3];
        let color = &mut block[stride - 8..];
        color[..2].copy_from_slice(&endpoint.to_le_bytes());
        color[2..4].copy_from_slice(&0u16.to_le_bytes());
        color[4..].copy_from_slice(&[0xe4, 0x1b, 0xb1, 0x4e]);
    }
    if format.is_vita() {
        let tile_bytes = format.byte_len(tile).unwrap();
        for data in bytes.as_mut_slice().chunks_exact_mut(tile_bytes) {
            let linear = data.to_vec();
            reorder_bc(tile, format, &linear, data, true).unwrap();
        }
    }
    let compressed = Compressed::tiled(size, tile, format, bytes, 0).unwrap();
    assert!(
        gpu.supports_compressed(&compressed),
        "native {format:?} required"
    );
    let before = gpu.resident.used();
    let image = gpu.load_compressed(&compressed).unwrap();
    assert_eq!(gpu.resident.used() - before, length);
    image
}

fn overwrite(expected: &mut [u8], raw: &[u8], src: Rect, dst: Rect, width: u32) {
    for y in 0..src.height {
        for x in 0..src.width {
            let from = (((src.top as u32 + y) * 32 + src.left as u32 + x) * 4) as usize;
            let to = (((dst.top as u32 + y) * width + dst.left as u32 + x) * 4) as usize;
            expected[to..to + 4].copy_from_slice(&raw[from..from + 4]);
        }
    }
}

#[test]
fn clipped_bc_views_retain_compressed_tiles_and_preserve_later_writes() {
    for work_framebuffer in [false, true] {
        for format in [
            Format::Bc1Rgb,
            Format::Bc3Rgba,
            Format::Bc1RgbVita,
            Format::Bc3RgbaVita,
        ] {
            let context = support::Context::new();
            let gpu = unsafe {
                Gpu::new(
                    context.gl_with(traffic::intercept),
                    Config {
                        work_framebuffer,
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            let mut input = sprite(&gpu, format);
            let raw = pixels(&gpu, &input);
            let size = Size {
                width: 64,
                height: 32,
            };
            let mut target = gpu.create_image(size, 0).unwrap();
            let src = Rect {
                left: 3,
                top: 2,
                width: 24,
                height: 12,
            };
            let dst = Rect {
                left: 5,
                top: 7,
                ..src
            };
            let charged = gpu.resident.used();
            assert_eq!(
                gpu.copy_is_view(&target, &input, src, dst),
                work_framebuffer
            );
            traffic::reset();
            gpu.copy_rect(
                &mut target,
                &input,
                src,
                dst.left,
                dst.top,
                size.rect(),
                DrawFace::Alpha,
                false,
            )
            .unwrap();
            if work_framebuffer {
                assert_eq!(gpu.resident.used(), charged);
                assert_eq!(traffic::texture_allocations(), 0);
                assert_eq!(traffic::draw_calls(), 0);
                assert_eq!(traffic::read_calls(), 0);
                assert!(target.shares_main_storage(&input));
            }
            let mut expected = vec![0; size.rgba_bytes().unwrap()];
            overwrite(&mut expected, &raw, src, dst, size.width);
            assert_eq!(pixels(&gpu, &target), expected, "{format:?}");
            let original = expected.clone();
            let snapshot = target.shared();
            // Keep BC views outside a second inset, including partial blocks.
            let second_src = Rect {
                left: 9,
                top: 1,
                width: 12,
                height: 10,
            };
            let second_dst = Rect {
                left: 11,
                top: 4,
                ..second_src
            };
            assert_eq!(
                gpu.copy_is_view(&target, &input, second_src, second_dst),
                work_framebuffer
            );
            gpu.copy_rect(
                &mut target,
                &input,
                second_src,
                second_dst.left,
                second_dst.top,
                size.rect(),
                DrawFace::Alpha,
                false,
            )
            .unwrap();
            if work_framebuffer {
                assert_eq!(gpu.resident.used(), charged);
            }
            overwrite(&mut expected, &raw, second_src, second_dst, size.width);
            assert_eq!(pixels(&gpu, &target), expected);
            gpu.fill(
                &mut input,
                &[Fill {
                    rectangle: src,
                    color: 0xff123456,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            assert_eq!(pixels(&gpu, &target), expected);
            assert_eq!(pixels(&gpu, &snapshot), original);
            let before_write = target.shared();
            let fill = Rect {
                left: 13,
                top: 10,
                width: 7,
                height: 5,
            };
            gpu.fill(
                &mut target,
                &[Fill {
                    rectangle: fill,
                    color: 0xffabcdef,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            assert_eq!(pixels(&gpu, &before_write), expected);
            for y in 10..15 {
                for x in 13..20 {
                    let offset = ((y * size.width + x) * 4) as usize;
                    expected[offset..offset + 4].copy_from_slice(&[0xab, 0xcd, 0xef, 255]);
                }
            }
            assert_eq!(pixels(&gpu, &target), expected);
        }
    }
}
