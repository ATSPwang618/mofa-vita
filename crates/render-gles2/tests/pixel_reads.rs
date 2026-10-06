#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu};

#[test]
fn adjacent_reads_reuse_blocks_and_writes_invalidate_without_pinning_textures() {
    let context = support::Context::new();
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
        width: 35,
        height: 33,
    };
    let mut data = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, p) in data
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        p.copy_from_slice(&[i as u8, 71, 93, 255]);
    }
    let pixels = Pixels {
        size,
        main: Some(data),
        province: None,
    };
    let mut image = gpu.reserve_upload(size, true, false).unwrap();
    gpu.upload(&mut image, &pixels).unwrap();
    drop(pixels);
    traffic::reset();
    for y in 0..33 {
        for x in 0..35 {
            assert_eq!(
                gpu.pixel(&image, x, y, false).unwrap(),
                0xff00475d | (((y * 35 + x) as u8 as u32) << 16)
            );
        }
    }
    assert_eq!(traffic::read_calls(), 3);
    assert_eq!(
        image.write_bytes(false),
        0,
        "cached reads must not force copy-on-write"
    );
    let old = image.shared();
    let fill = Fill {
        rectangle: Rect {
            left: 3,
            top: 4,
            width: 1,
            height: 1,
        },
        color: 0xff123456,
        face: DrawFace::Alpha,
        hold_alpha: false,
    };
    gpu.fill(&mut image, &[fill]).unwrap();
    assert_eq!(gpu.pixel(&old, 3, 4, false).unwrap(), 0xff8f475d);
    assert_eq!(gpu.pixel(&image, 3, 4, false).unwrap(), 0xff123456);
    drop(old);
    gpu.fill(
        &mut image,
        &[Fill {
            color: 0xffabcdef,
            ..fill
        }],
    )
    .unwrap();
    assert_eq!(gpu.pixel(&image, 3, 4, false).unwrap(), 0xffabcdef);
    assert!(gpu.pixel(&image, -1, 0, false).is_err());
    assert!(gpu.pixel(&image, 35, 0, false).is_err());
}

#[test]
fn cached_logical_pixels_match_compact_readback_and_keep_province_separate() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                tile_edge: 16,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 29,
        height: 21,
    };
    let mut data = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, p) in data
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        p.copy_from_slice(&[i as u8, 92, 131, (i % 251) as u8]);
    }
    let mut image = gpu
        .upload_scaled(
            &Pixels {
                size,
                main: Some(data),
                province: None,
            },
            Size {
                width: 43,
                height: 35,
            },
        )
        .unwrap();
    let reference = gpu.readback(&image, image.size.rect(), false).unwrap();
    traffic::reset();
    for y in 0..35 {
        for x in 0..43 {
            let p = &reference.data.as_slice()[((y * 43 + x) * 4) as usize..];
            assert_eq!(
                gpu.pixel(&image, x, y, false).unwrap(),
                u32::from_be_bytes([p[3], p[0], p[1], p[2]])
            );
        }
    }
    assert_eq!(traffic::read_calls(), 9);
    let dense = gpu
        .logical_image(
            image.shared(),
            Size {
                width: 13,
                height: 9,
            },
        )
        .unwrap();
    let mask = gpu.read_hit_plane(&dense, false).unwrap();
    assert!(mask.bytes() <= 13 * 9);
    for y in 0..9 {
        for x in 0..13 {
            assert_eq!(
                mask.sample(x, y),
                (gpu.pixel(&dense, x as i32, y as i32, false).unwrap() >> 24) as u8
            );
        }
    }
    let area = image.size.rect();
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: area,
            color: 37,
            face: DrawFace::Province,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(gpu.pixel(&image, 0, 0, true).unwrap(), 37);
    assert_ne!(gpu.pixel(&image, 0, 0, false).unwrap(), 37);
}

#[test]
fn single_pixel_read_still_works_when_a_cache_block_does_not_fit() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                staging: Budget::new(1024 * 1024),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let image = gpu
        .create_image(
            Size {
                width: 32,
                height: 32,
            },
            0xff234567,
        )
        .unwrap();
    let before = gpu.staging.used();
    let held = gpu.staging.reserve(gpu.staging.available() - 8).unwrap();
    assert_eq!(gpu.pixel(&image, 3, 4, false).unwrap(), 0xff234567);
    drop(held);
    assert_eq!(gpu.staging.used(), before);
}

#[test]
fn wide_row_scans_reuse_previous_rows_without_retaining_gpu_storage() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl_with(traffic::intercept), Config::default()).unwrap() };
    let size = Size {
        width: 1024,
        height: 48,
    };
    let mut data = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, pixel) in data
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        pixel.copy_from_slice(&[(i / 1024) as u8, i as u8, 93, 255]);
    }
    let pixels = Pixels {
        size,
        main: Some(data),
        province: None,
    };
    let mut image = gpu.reserve_upload(size, true, false).unwrap();
    gpu.upload(&mut image, &pixels).unwrap();
    drop(pixels);
    let staging_before = gpu.staging.used();
    traffic::reset();
    for y in 0..48 {
        for x in 0..1024 {
            assert_eq!(
                gpu.pixel(&image, x, y, false).unwrap(),
                0xff00005d | ((y as u32) << 16) | (((x as u8) as u32) << 8)
            );
        }
    }
    assert_eq!(traffic::read_calls(), 48);
    assert_eq!(image.write_bytes(false), 0);
    // Pixel data plus weak version metadata, charged to the staging budget.
    assert!(gpu.staging.used() - staging_before < 80 * 1024);
}
