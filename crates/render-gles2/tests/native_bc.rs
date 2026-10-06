#![cfg(target_os = "linux")]
mod support;
use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, Rect, Size},
    pixels::Bytes,
    texture::{Compressed, Format, reorder_bc},
};
use krkr_render_gles2::{Config, Gpu};
use std::sync::atomic::AtomicBool;

#[test]
fn cropped_compressed_snapshots_preserve_gpu_samples_and_source_ownership() {
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
    let size = Size {
        width: 32,
        height: 16,
    };
    let tile = Size {
        width: 16,
        height: 8,
    };
    let output = Size {
        width: 20,
        height: 10,
    };
    let area = Rect {
        left: 5,
        top: 3,
        ..output.rect()
    };
    for format in [
        Format::Bc1Rgb,
        Format::Bc3Rgba,
        Format::Bc1RgbVita,
        Format::Bc3RgbaVita,
    ] {
        let budget = Budget::new(65536);
        let tile_bytes = format.byte_len(tile).unwrap();
        let mut bytes = Bytes::zeroed(tile_bytes * 4, &budget).unwrap();
        for (index, out) in bytes
            .as_mut_slice()
            .chunks_exact_mut(tile_bytes)
            .enumerate()
        {
            let block_size = if format.linear() == Format::Bc3Rgba {
                16
            } else {
                8
            };
            let mut linear = vec![0; tile_bytes];
            for block in linear.chunks_exact_mut(block_size) {
                let rgb = if block_size == 16 {
                    block[..8].copy_from_slice(&[231, 23, 0x88, 0xc6, 0xfa, 0x88, 0xc6, 0xfa]);
                    &mut block[8..]
                } else {
                    block
                };
                rgb[..4].copy_from_slice(&[0x42, 0xef, 0x7d, 0x18]);
                rgb[4..]
                    .copy_from_slice(&0xe4e4e4e4u32.rotate_left((index * 2) as u32).to_le_bytes());
            }
            if format.is_vita() {
                reorder_bc(tile, format, &linear, out, true).unwrap();
            } else {
                out.copy_from_slice(&linear);
            }
        }
        let texture = Compressed::tiled(size, tile, format, bytes, 0).unwrap();
        let image = gpu.load_compressed(&texture).unwrap();
        let original = gpu.readback(&image, size.rect(), false).unwrap();
        let mut snapshot = gpu.create_image(output, 0).unwrap();
        assert!(gpu.copy_is_view(&snapshot, &image, area, output.rect()));
        gpu.operate(
            &mut snapshot,
            &image,
            area,
            0,
            0,
            output.rect(),
            krkr_protocol::graphics::BlendOptions {
                mode: krkr_protocol::graphics::Blend::Opaque,
                face: DrawFace::Opaque,
                opacity: 255,
                hold_alpha: false,
            },
        )
        .unwrap();
        let crop = gpu.readback(&snapshot, output.rect(), false).unwrap();
        for y in 0..output.height as usize {
            let start = ((y + 3) * size.width as usize + 5) * 4;
            assert_eq!(
                &crop.data.as_slice()[y * 80..(y + 1) * 80],
                &original.data.as_slice()[start..start + 80],
                "{format:?} row={y}"
            );
        }
        gpu.fill(
            &mut snapshot,
            &[Fill {
                rectangle: Rect {
                    left: 1,
                    top: 1,
                    width: 2,
                    height: 2,
                },
                color: 0xff123456,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        assert_eq!(
            gpu.readback(&image, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            original.data.as_slice()
        );
    }
}

#[test]
fn empty_bc3_tiles_share_one_texel_but_hidden_rgb_is_preserved() {
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
    let size = Size {
        width: 256,
        height: 256,
    };
    for format in [Format::Bc3Rgba, Format::Bc3RgbaVita] {
        let budget = Budget::new(1024 * 1024);
        let data = Bytes::zeroed(format.byte_len(size).unwrap(), &budget).unwrap();
        let texture = Compressed::tiled(size, size, format, data, 0).unwrap();
        let image = gpu.load_compressed(&texture).unwrap();
        assert_eq!(image.resident_bytes(), 4);
        assert!(
            gpu.readback(&image, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .iter()
                .all(|&v| v == 0)
        );
        let mut hidden = Bytes::zeroed(format.byte_len(size).unwrap(), &budget).unwrap();
        for block in hidden.as_mut_slice().as_chunks_mut::<16>().0 {
            block[9] = 0xf8;
        }
        let texture = Compressed::tiled(size, size, format, hidden, 0).unwrap();
        let colored = gpu.load_compressed(&texture).unwrap();
        assert_eq!(colored.resident_bytes(), format.byte_len(size).unwrap());
        assert_eq!(gpu.pixel(&colored, 0, 0, false).unwrap(), 0x00ff0000);
    }
}

#[test]
fn native_bc_matches_gpu_sampling_and_detaches_only_written_tiles() {
    for work in [true, false] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    tile_edge: 16,
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let tile = Size {
            width: 16,
            height: 8,
        };
        let size = Size {
            width: 32,
            height: 16,
        };
        for format in [
            Format::Bc1Rgb,
            Format::Bc3Rgba,
            Format::Bc1RgbVita,
            Format::Bc3RgbaVita,
        ] {
            let tile_bytes = format.byte_len(tile).unwrap();
            let budget = Budget::new(65536);
            let mut bytes = Bytes::zeroed(tile_bytes * 4, &budget).unwrap();
            for (tile_index, out) in bytes
                .as_mut_slice()
                .chunks_exact_mut(tile_bytes)
                .enumerate()
            {
                let block_bytes = if format.linear() == Format::Bc3Rgba {
                    16
                } else {
                    8
                };
                let mut linear = vec![0; tile_bytes];
                for (i, block) in linear.chunks_exact_mut(block_bytes).enumerate() {
                    let rgb = if block_bytes == 16 {
                        // Both alpha interpolation modes, plus zero/one alpha.
                        let (a, b) = if i % 2 == 0 { (23, 231) } else { (227, 17) };
                        block[0] = a;
                        block[1] = b;
                        let indices = (0..16).fold(0u64, |n, p| n | ((p % 8) << (p * 3)));
                        block[2..8].copy_from_slice(&indices.to_le_bytes()[..6]);
                        &mut block[8..]
                    } else {
                        block
                    };
                    let (a, b) = if i % 2 == 0 {
                        (0x187du16, 0xef42u16)
                    } else {
                        (0xef42, 0x187d)
                    };
                    rgb[..2].copy_from_slice(&a.to_le_bytes());
                    rgb[2..4].copy_from_slice(&b.to_le_bytes());
                    let indices = 0xe4e4e4e4u32.rotate_left((tile_index * 2) as u32);
                    rgb[4..8].copy_from_slice(&indices.to_le_bytes());
                }
                if format.is_vita() {
                    reorder_bc(tile, format, &linear, out, true).unwrap();
                } else {
                    out.copy_from_slice(&linear);
                }
            }
            let texture = Compressed::tiled(size, tile, format, bytes, 0).unwrap();
            assert!(gpu.supports_compressed(&texture));
            let expected =
                krkr_image::compressed::decode(&texture, &budget, &AtomicBool::new(false))
                    .unwrap()
                    .main
                    .unwrap();
            let image = gpu.load_compressed(&texture).unwrap();
            assert_eq!(image.resident_bytes(), tile_bytes * 4);
            let read = gpu.readback(&image, size.rect(), false).unwrap();
            for (i, (a, b)) in read
                .data
                .as_slice()
                .iter()
                .zip(expected.as_slice())
                .enumerate()
            {
                assert!(
                    a.abs_diff(*b) <= 1,
                    "{format:?} work={work} byte={i}: GPU={a} CPU={b}"
                );
            }
            let mut edited = image.shared();
            gpu.fill(
                &mut edited,
                &[Fill {
                    rectangle: Rect {
                        left: 2,
                        top: 2,
                        width: 2,
                        height: 2,
                    },
                    color: 0xffa03070,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            let read = gpu.readback(&edited, size.rect(), false).unwrap();
            assert_eq!(
                edited.resident_bytes(),
                tile_bytes * 3 + tile.rgba_bytes().unwrap()
            );
            assert_eq!(image.resident_bytes(), tile_bytes * 4);
            for y in 0..size.height {
                for x in 0..size.width {
                    let index = (y * size.width + x) as usize * 4;
                    let actual = &read.data.as_slice()[index..index + 4];
                    if (2..4).contains(&x) && (2..4).contains(&y) {
                        assert_eq!(actual, &[0xa0, 0x30, 0x70, 255]);
                    } else {
                        for (a, b) in actual.iter().zip(&expected.as_slice()[index..index + 4]) {
                            assert!(a.abs_diff(*b) <= 1);
                        }
                    }
                }
            }
            gpu.collect().unwrap();
        }
    }
}
