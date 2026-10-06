#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
#![allow(unsafe_code)]
#[path = "../../render-gles2/tests/support/mod.rs"]
mod support;

use krkr_host_vita::graphics::Graphics;
use krkr_protocol::{
    graphics::{Command, DrawFace, ImageRef, Rect, Size},
    image_cache::{Cache, Entry, Key},
    pixels::{Bytes, Pixels},
    texture::{Compressed, Format},
    window::Response,
};
use krkr_render_gles2::{Config, Gpu};
use std::sync::Arc;

#[test]
fn clipped_copy_keeps_cached_source_under_resident_pressure() {
    clipped_copy(false);
}

#[test]
fn clipped_bc_copy_keeps_cached_source_under_resident_pressure() {
    clipped_copy(true);
}

fn clipped_copy(compressed: bool) {
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
    let cache = Cache::new(1024 * 1024);
    let mut host = Graphics::new(gpu, cache.clone());
    let mut ids = slotmap::SlotMap::with_key();
    let source = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let target = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let size = Size {
        width: 32,
        height: 32,
    };
    let storage_bytes;
    let raw;
    if compressed {
        let format = Format::Bc3RgbaVita;
        let length = format.byte_len(size).unwrap();
        let mut bytes = Bytes::zeroed(length, &host.gpu.staging).unwrap();
        for block in bytes.as_mut_slice().as_chunks_mut::<16>().0 {
            block.copy_from_slice(&[
                255, 0, 0x88, 0xc6, 0xfa, 0x88, 0xc6, 0xfa, 0, 0xf8, 0, 0, 0xe4, 0x1b, 0xb1, 0x4e,
            ]);
        }
        let texture = Arc::new(Compressed::new(size, format, bytes, 0).unwrap());
        assert!(host.gpu.supports_compressed(&texture));
        host.execute(&Command::LoadCompressed {
            image: source.clone(),
            texture,
            logical_size: size,
        })
        .unwrap();
        let Response::Image(pixels) = host
            .execute(&Command::ReadImage {
                image: source.clone(),
            })
            .unwrap()
        else {
            panic!("expected source image")
        };
        raw = pixels.main.unwrap().as_slice().to_vec();
        storage_bytes = length;
    } else {
        host.execute(&Command::PrepareUpload {
            image: source.clone(),
            source: None,
            size,
            main: true,
            province: false,
        })
        .unwrap();
        raw = (0..size.height)
            .flat_map(|y| (0..size.width).flat_map(move |x| [x as u8, y as u8, 71, (x + y) as u8]))
            .collect::<Vec<u8>>();
        let mut bytes = Bytes::zeroed(raw.len(), &host.gpu.staging).unwrap();
        bytes.as_mut_slice().copy_from_slice(&raw);
        host.execute(&Command::UploadScaled {
            image: source.clone(),
            logical_size: size,
            pixels: Arc::new(Pixels {
                size,
                main: Some(bytes),
                province: None,
            }),
        })
        .unwrap();
        storage_bytes = raw.len();
    }
    let output = Size {
        width: 64,
        height: 48,
    };
    host.execute(&Command::Create {
        image: target.id,
        lifetime: Arc::downgrade(&target.lifetime),
        size: output,
        color: 0,
    })
    .unwrap();
    let key = Key {
        names: [
            Some("sprite.png".encode_utf16().collect()),
            None,
            None,
            None,
        ],
        color_key: 0,
        rule_size: None,
    };
    cache.insert(
        key.clone(),
        Entry {
            image: source.clone(),
            size,
            tags: Arc::default(),
            bytes: storage_bytes,
        },
        cache.generation(),
    );
    host.gpu.collect().unwrap();
    let _pressure = host
        .gpu
        .resident
        .reserve(host.gpu.resident.available() - 4)
        .unwrap();
    let charged = host.gpu.resident.used();
    host.execute(&Command::Copy {
        image: target.clone(),
        source,
        rectangle: size.rect(),
        x: -5,
        y: 10,
        clip: Rect {
            left: 8,
            top: 4,
            width: 30,
            height: 34,
        },
        face: DrawFace::Alpha,
        hold_alpha: false,
    })
    .unwrap();
    assert_eq!(host.gpu.resident.used(), charged);
    assert!(cache.get(&key).is_some());
    let Response::Image(pixels) = host.execute(&Command::ReadImage { image: target }).unwrap()
    else {
        panic!("expected image")
    };
    for (i, pixel) in pixels
        .main
        .unwrap()
        .as_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
    {
        let x = i as u32 % output.width;
        let y = i as u32 / output.width;
        let expected = if (8..27).contains(&x) && (10..38).contains(&y) {
            let at = (((y - 10) * size.width + x + 5) * 4) as usize;
            &raw[at..at + 4]
        } else {
            &[0; 4]
        };
        assert_eq!(pixel, expected, "{x},{y}");
    }
}
