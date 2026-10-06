#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
use krkr_protocol::{
    graphics::{DrawFace, Fill, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu};

#[test]
fn moderately_compressible_canvas_keeps_chunks_without_a_second_output_buffer() {
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
        width: 512,
        height: 512,
    };
    let bytes = size.rgba_bytes().unwrap();
    let mut raw = Bytes::zeroed(bytes, &gpu.staging).unwrap();
    let mut random = 314159u32;
    for pixel in raw.as_mut_slice().as_chunks_mut::<4>().0 {
        random ^= random << 13;
        random ^= random >> 17;
        random ^= random << 5;
        pixel.copy_from_slice(&[random as u8, (random >> 8) as u8, 33, 255]);
    }
    let expected = raw.as_slice().to_vec();
    let image = gpu
        .upload_scaled(
            &Pixels {
                size,
                main: Some(raw),
                province: None,
            },
            size,
        )
        .unwrap();
    let lock = gpu
        .staging
        .reserve(gpu.staging.available() - 1792 * 1024)
        .unwrap();
    let saved = gpu.spill_canvas(&image).unwrap().unwrap();
    let packed = saved.staging_bytes();
    assert!(
        packed > bytes / 2 && packed < bytes,
        "packed {packed} of {bytes}"
    );
    eprintln!("parked canvas: {bytes} -> {packed} bytes, 1792 KiB staging available");
    drop(image);
    gpu.collect().unwrap();
    let restored = gpu.restore_canvas(&saved).unwrap();
    drop(lock);
    assert_eq!(
        gpu.readback(&restored, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        expected
    );
}

#[test]
fn new_surface_clears_with_only_one_staging_strip() {
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
        width: 960,
        height: 539,
    };
    let pressure = gpu
        .staging
        .reserve(gpu.staging.available() - 64 * 1024)
        .unwrap();
    let image = gpu.create_surface_image(size).unwrap();
    drop(pressure);
    let pixels = gpu.readback(&image, size.rect(), false).unwrap();
    assert!(pixels.data.as_slice().iter().all(|&byte| byte == 0));
}

#[test]
fn compressed_canvas_restores_with_only_a_strip_of_staging() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 1024,
                ..Default::default()
            },
        )
        .unwrap()
    };
    // A non-uniform, compressible tile with a final partial strip.
    let size = Size {
        width: 1000,
        height: 999,
    };
    let mut raw = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, pixel) in raw
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        pixel.copy_from_slice(&[(i % 251) as u8, (i / 16000 % 251) as u8, 91, 255]);
    }
    let pixels = Pixels {
        size,
        main: Some(raw),
        province: None,
    };
    let image = gpu.upload_scaled(&pixels, size).unwrap();
    let expected = pixels.main.as_ref().unwrap().as_slice().to_vec();
    drop(pixels);
    let spill_lock = gpu
        .staging
        .reserve(gpu.staging.available() - 2 * 1024 * 1024)
        .unwrap();
    let saved = gpu.spill_canvas(&image).unwrap().unwrap();
    drop(spill_lock);
    drop(image);
    gpu.collect().unwrap();
    assert!(gpu.staging.used() < size.rgba_bytes().unwrap() / 2);
    let before = gpu.staging.used();
    let lock = gpu
        .staging
        .reserve(gpu.staging.available() - 192 * 1024)
        .unwrap();
    let restored = gpu.restore_canvas(&saved).unwrap();
    drop(lock);
    assert_eq!(gpu.staging.used(), before);
    assert_eq!(
        gpu.readback(&restored, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        expected
    );
}

#[test]
fn incompressible_canvas_spills_and_restores_without_extra_staging() {
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
        width: 512,
        height: 512,
    };
    let len = size.rgba_bytes().unwrap();
    let mut noise = vec![0u8; len];
    let mut seed = 17u32;
    for byte in &mut noise {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        *byte = seed as u8;
    }
    for allow_compression in [true, false] {
        let mut raw = Bytes::zeroed(len, &gpu.staging).unwrap();
        raw.as_mut_slice().copy_from_slice(&noise);
        let image = gpu
            .upload_scaled(
                &Pixels {
                    size,
                    main: Some(raw),
                    province: None,
                },
                size,
            )
            .unwrap();
        gpu.collect().unwrap();
        let lock = (!allow_compression)
            .then(|| gpu.staging.reserve(gpu.staging.available() - len).unwrap());
        let before = gpu.resident.used();
        let saved = gpu
            .spill_canvas(&image)
            .unwrap()
            .expect("a noisy image must also release GPU memory");
        drop(image);
        gpu.collect().unwrap();
        assert!(before - gpu.resident.used() >= len);
        // Raw restore must work even with no room for a second CPU tile or codec.
        let rest = gpu.staging.reserve(gpu.staging.available()).unwrap();
        let restored = gpu.restore_canvas(&saved).unwrap();
        drop((rest, lock));
        assert_eq!(
            gpu.readback(&restored, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            noise
        );
        drop((restored, saved));
        gpu.collect().unwrap();
        assert_eq!(gpu.staging.used(), 0);
    }
}

#[test]
fn lossless_spill_releases_gpu_storage_preserves_density_and_rejects_shared_owners() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(Size {
                    width: 512,
                    height: 512,
                }),
                tile_edge: 256,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let stored = Size {
        width: 512,
        height: 512,
    };
    let logical = Size {
        width: 1024,
        height: 1024,
    };
    let mut raw = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, p) in raw
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        p.copy_from_slice(&[(i % 256) as u8, 71, 239, if i % 7 == 0 { 0 } else { 255 }]);
    }
    let pixels = Pixels {
        size: stored,
        main: Some(raw),
        province: None,
    };
    let image = gpu.upload_scaled(&pixels, logical).unwrap();
    let alias = image.shared();
    assert_eq!(gpu.spillable_bytes(&image), 0);
    assert!(gpu.spill_canvas(&image).unwrap().is_none());
    assert_eq!(gpu.spill_group_bytes(&[&image, &image]), 0);
    assert_eq!(
        gpu.spill_group_bytes(&[&image, &alias]),
        stored.rgba_bytes().unwrap()
    );
    assert_eq!(
        gpu.spill_group_reclaim_bytes(&[&image, &alias]),
        stored.rgba_bytes().unwrap()
    );
    let shared_saved = gpu.spill_canvas_group(&[&image, &alias]).unwrap().unwrap();
    drop(shared_saved);
    // Resized canvases can contain cropped/shared tile views. Their saved
    // bytes must follow the backing offset, not start at texture pixel (0, 0).
    let grown = gpu
        .resize(
            &image,
            Size {
                width: 1280,
                height: 1152,
            },
            0,
        )
        .unwrap();
    assert!(gpu.spill_group_reclaim_bytes(&[&grown]) < 256 * 1024);
    assert_eq!(gpu.spill_group_reclaim_bytes(&[&image, &alias]), 0);
    let expected = gpu.readback(&grown, grown.size.rect(), false).unwrap();
    let grown_saved = gpu.spill_canvas(&grown).unwrap().unwrap();
    let grown_restored = gpu.restore_canvas(&grown_saved).unwrap();
    assert_eq!(
        gpu.readback(&grown_restored, grown_restored.size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        expected.data.as_slice()
    );
    drop((grown, grown_saved, grown_restored, expected));
    drop(alias);
    // Failed admission must leave the GPU image intact and release temporary
    // accounting; callers can continue using it without a partial spill.
    let staging_before = gpu.staging.used();
    let lock = gpu
        .staging
        .reserve(gpu.staging.available() - 128 * 1024)
        .unwrap();
    assert!(gpu.spill_canvas(&image).unwrap().is_none());
    drop(lock);
    assert_eq!(gpu.staging.used(), staging_before);
    let before = gpu.resident.used();
    let saved = gpu.spill_canvas(&image).unwrap().unwrap();
    drop(image);
    gpu.collect().unwrap();
    assert!(before - gpu.resident.used() >= stored.rgba_bytes().unwrap());
    let mut restored = gpu.restore_canvas(&saved).unwrap();
    assert_eq!(restored.size, logical);
    assert_eq!(restored.stored_size(), Some(stored));
    restored.size = stored;
    assert_eq!(
        gpu.readback(&restored, stored.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        pixels.main.as_ref().unwrap().as_slice()
    );
    let snapshot = restored.shared();
    gpu.fill(
        &mut restored,
        &[Fill {
            rectangle: stored.rect(),
            color: 0xff001122,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(
        gpu.readback(&snapshot, stored.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        pixels.main.as_ref().unwrap().as_slice()
    );
    drop((restored, snapshot, saved, pixels));
    gpu.collect().unwrap();
    assert_eq!(gpu.staging.used(), 0);
}

#[test]
fn wide_upload_packs_strips_when_staging_cannot_hold_another_tile() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 1024,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 1057,
        height: 1031,
    };
    let mut raw = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, pixel) in raw
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        pixel.copy_from_slice(&[
            (i % 251) as u8,
            (i / size.width as usize % 251) as u8,
            73,
            255,
        ]);
    }
    let pixels = Pixels {
        size,
        main: Some(raw),
        province: None,
    };
    let lock = gpu
        .staging
        .reserve(gpu.staging.available() - 64 * 1024)
        .unwrap();
    let mut image = gpu.upload_scaled(&pixels, size).unwrap();
    gpu.upload_scaled_into(&mut image, &pixels, size).unwrap();
    drop(lock);
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        pixels.main.as_ref().unwrap().as_slice()
    );
}
