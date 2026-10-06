#![cfg(target_os = "linux")]
#![allow(unsafe_code)]
#[path = "../../render-gles2/tests/support/mod.rs"]
mod support;
use krkr_host_vita::graphics::{Graphics, Snapshot};
use krkr_protocol::{
    graphics::{Blend, Command, ImageRef, Node, Scene, Size},
    image_cache::Cache,
    pixels::{Bytes, Pixels},
    window::Response,
};
use krkr_render_gles2::{Config, Gpu};
use std::{cell::Cell, ffi::c_void, sync::Arc};

type TexImage = unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type TexSub = unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type Clear = unsafe extern "system" fn(u32);
type GetError = unsafe extern "system" fn() -> u32;
thread_local! {
    static IMAGE:Cell<Option<TexImage>>=const{Cell::new(None)};
    static SUB:Cell<Option<TexSub>>=const{Cell::new(None)};
    static CLEAR:Cell<Option<Clear>>=const{Cell::new(None)};
    static SENT:Cell<usize>=const{Cell::new(0)};
    static CLEARS:Cell<usize>=const{Cell::new(0)};
    static ERROR:Cell<Option<GetError>>=const{Cell::new(None)};
    static FAIL_AFTER:Cell<usize>=const{Cell::new(0)};
    static PENDING_ERROR:Cell<u32>=const{Cell::new(glow::NO_ERROR)};
}
unsafe extern "system" fn tex_image(
    t: u32,
    l: i32,
    i: i32,
    w: i32,
    h: i32,
    b: i32,
    f: u32,
    k: u32,
    p: *const c_void,
) {
    if !p.is_null() {
        SENT.set(SENT.get() + w as usize * h as usize * 4);
    }
    unsafe { IMAGE.get().unwrap()(t, l, i, w, h, b, f, k, p) };
    if !p.is_null() && FAIL_AFTER.get() != 0 {
        FAIL_AFTER.set(FAIL_AFTER.get() - 1);
        if FAIL_AFTER.get() == 0 {
            PENDING_ERROR.set(glow::INVALID_OPERATION);
        }
    }
}
unsafe extern "system" fn tex_sub(
    t: u32,
    l: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    f: u32,
    k: u32,
    p: *const c_void,
) {
    if !p.is_null() {
        SENT.set(SENT.get() + w as usize * h as usize * 4);
    }
    unsafe { SUB.get().unwrap()(t, l, x, y, w, h, f, k, p) };
}
unsafe extern "system" fn clear(bits: u32) {
    CLEARS.set(CLEARS.get() + 1);
    unsafe { CLEAR.get().unwrap()(bits) };
}
unsafe extern "system" fn get_error() -> u32 {
    let error = PENDING_ERROR.replace(glow::NO_ERROR);
    if error != glow::NO_ERROR {
        error
    } else {
        unsafe { ERROR.get().unwrap()() }
    }
}
fn intercept(name: &str, address: *const c_void) -> *const c_void {
    match name {
        "glGetError" => {
            ERROR.set(Some(unsafe {
                std::mem::transmute::<*const c_void, GetError>(address)
            }));
            get_error as *const c_void
        }
        "glTexImage2D" => {
            IMAGE.set(Some(unsafe {
                std::mem::transmute::<*const c_void, TexImage>(address)
            }));
            tex_image as *const c_void
        }
        "glTexSubImage2D" => {
            SUB.set(Some(unsafe {
                std::mem::transmute::<*const c_void, TexSub>(address)
            }));
            tex_sub as *const c_void
        }
        "glClear" => {
            CLEAR.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Clear>(address)
            }));
            clear as *const c_void
        }
        _ => address,
    }
}
fn reference(ids: &mut slotmap::SlotMap<krkr_protocol::graphics::ImageId, ()>) -> ImageRef {
    ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    }
}
fn pixels(gpu: &Gpu, size: Size, main: bool, province: bool) -> Arc<Pixels> {
    let mut rgba = main.then(|| Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap());
    if let Some(bytes) = rgba.as_mut() {
        for (i, p) in bytes.as_mut_slice().chunks_exact_mut(4).enumerate() {
            p.copy_from_slice(&[
                (i * 7) as u8,
                (i * 19) as u8,
                (i * 31) as u8,
                (i * 13 + 23) as u8,
            ]);
        }
    }
    let mut regions =
        province.then(|| Bytes::zeroed(size.rgba_bytes().unwrap() / 4, &gpu.staging).unwrap());
    if let Some(bytes) = regions.as_mut() {
        for (i, p) in bytes.as_mut_slice().iter_mut().enumerate() {
            *p = (i * 17 + 5) as u8;
        }
    }
    Arc::new(Pixels {
        size,
        main: rgba,
        province: regions,
    })
}
fn snapshot(host: &mut Graphics, image: &ImageRef, size: Size) -> Result<Snapshot, String> {
    host.capture(Scene {
        nodes: vec![Node {
            parent: None,
            visible: true,
            opacity: 255,
            cache: None,
            image: Some(image.clone()),
            neutral_color: 0,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
        }],
        ..Default::default()
    })
}
fn read(host: &Graphics, image: &krkr_render_gles2::Image, province: bool) -> Vec<u8> {
    host.gpu
        .readback(image, image.size.rect(), province)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

#[test]
fn private_uploads_avoid_the_zero_image_transfer_and_publish_complete_planes() {
    for work in [false, true] {
        for (size, province, edge) in [
            (
                Size {
                    width: 960,
                    height: 544,
                },
                false,
                1024,
            ),
            (
                Size {
                    width: 31,
                    height: 19,
                },
                true,
                8,
            ),
        ] {
            let context = support::Context::new();
            let gpu = unsafe {
                Gpu::new(
                    context.gl_with(intercept),
                    Config {
                        work_framebuffer: work,
                        tile_edge: edge,
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            let mut host = Graphics::new(gpu, Cache::new(0));
            let mut ids = slotmap::SlotMap::with_key();
            let data = pixels(&host.gpu, size, true, province);
            let mut traffic = Vec::new();
            for private in [false, true] {
                let image = reference(&mut ids);
                SENT.set(0);
                CLEARS.set(0);
                let begin = if private {
                    Command::BeginUpload {
                        staging_bytes: 0,
                        image: image.clone(),
                        size,
                        main: true,
                        province,
                        source: None,
                    }
                } else {
                    Command::PrepareUpload {
                        image: image.clone(),
                        size,
                        main: true,
                        province,
                        source: None,
                    }
                };
                host.execute(&begin).unwrap();
                if private {
                    assert_eq!(SENT.get(), 0);
                    assert_eq!(CLEARS.get(), 0);
                    assert!(
                        snapshot(&mut host, &image, size).is_err(),
                        "uninitialized image must stay private"
                    );
                    assert!(
                        host.execute(&Command::ReadImage {
                            image: image.clone()
                        })
                        .is_err()
                    );
                }
                let response = host
                    .execute(&Command::Upload {
                        image: image.clone(),
                        pixels: data.clone(),
                    })
                    .unwrap();
                let bytes = size.rgba_bytes().unwrap() * (1 + usize::from(province));
                if private {
                    assert!(matches!(response,Response::ImageStorage(n) if n==bytes));
                }
                traffic.push((SENT.get(), CLEARS.get()));
                let shot = snapshot(&mut host, &image, size).unwrap();
                assert_eq!(
                    read(&host, &shot.images[&image.id], false),
                    data.main.as_ref().unwrap().as_slice()
                );
                if province {
                    assert_eq!(
                        read(&host, &shot.images[&image.id], true),
                        data.province.as_ref().unwrap().as_slice()
                    );
                }
            }
            let bytes = size.rgba_bytes().unwrap() * (1 + usize::from(province));
            assert_eq!(traffic[1], (bytes, 0));
            if work {
                assert_eq!(traffic[0].0, bytes * 2);
            } else {
                assert!(traffic[0].1 > 0);
            }
            eprintln!(
                "work={work} size={size:?} province={province}: upload bytes/clears {:?} -> {:?}",
                traffic[0], traffic[1]
            );
        }
    }
}

#[test]
fn pending_loads_cancel_cleanly_and_reject_incomplete_or_malformed_planes() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 8,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let mut host = Graphics::new(gpu, Cache::new(0));
    let mut ids = slotmap::SlotMap::with_key();
    let size = Size {
        width: 24,
        height: 16,
    };
    let source = reference(&mut ids);
    host.execute(&Command::Create {
        image: source.id,
        lifetime: Arc::downgrade(&source.lifetime),
        size,
        color: 0x73456789,
    })
    .unwrap();
    host.gpu.collect().unwrap();
    let baseline = host.gpu.resident.used();
    {
        // Reserving a texture must need no second CPU buffer full of zeroes.
        let _locked = host
            .gpu
            .staging
            .reserve(host.gpu.staging.available())
            .unwrap();
        let cancelled = reference(&mut ids);
        host.execute(&Command::BeginUpload {
            staging_bytes: 0,
            image: cancelled,
            size,
            main: true,
            province: true,
            source: Some(source.clone()),
        })
        .unwrap();
    }
    host.maintain().unwrap();
    host.gpu.collect().unwrap();
    assert_eq!(host.gpu.resident.used(), baseline);
    let old = snapshot(&mut host, &source, size).unwrap();
    let expected = read(&host, &old.images[&source.id], false);
    for mode in 0..4 {
        let image = reference(&mut ids);
        host.execute(&Command::BeginUpload {
            staging_bytes: 0,
            image: image.clone(),
            size,
            main: true,
            province: true,
            source: Some(source.clone()),
        })
        .unwrap();
        let mut bad = pixels(&host.gpu, size, true, true);
        let p = Arc::get_mut(&mut bad).unwrap();
        match mode {
            0 => p.main = None,
            1 => p.province = None,
            2 => p.size.width -= 1,
            _ => p.province = Some(Bytes::zeroed(1, &host.gpu.staging).unwrap()),
        }
        assert!(
            host.execute(&Command::Upload {
                image: image.clone(),
                pixels: bad
            })
            .is_err()
        );
        assert!(snapshot(&mut host, &image, size).is_err());
        assert_eq!(read(&host, &old.images[&source.id], false), expected);
        drop(image);
        host.maintain().unwrap();
        host.gpu.collect().unwrap();
        assert_eq!(host.gpu.resident.used(), baseline);
    }
}

#[test]
fn province_loads_keep_compact_main_and_scaled_uploads_need_no_second_allocation() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 8,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let mut host = Graphics::new(gpu, Cache::new(0));
    let mut ids = slotmap::SlotMap::with_key();
    let stored = Size {
        width: 13,
        height: 9,
    };
    let logical = Size {
        width: 26,
        height: 18,
    };
    let source = reference(&mut ids);
    host.execute(&Command::BeginUpload {
        staging_bytes: 0,
        image: source.clone(),
        size: stored,
        main: true,
        province: false,
        source: None,
    })
    .unwrap();
    let rgba = pixels(&host.gpu, stored, true, false);
    let lock = host
        .gpu
        .resident
        .reserve(host.gpu.resident.available())
        .unwrap();
    host.execute(&Command::UploadScaled {
        image: source.clone(),
        pixels: rgba,
        logical_size: logical,
    })
    .unwrap();
    drop(lock);
    let original = snapshot(&mut host, &source, logical).unwrap();
    let expected = read(&host, &original.images[&source.id], false);
    let destination = reference(&mut ids);
    host.execute(&Command::BeginUpload {
        staging_bytes: 0,
        image: destination.clone(),
        size: logical,
        main: false,
        province: true,
        source: Some(source.clone()),
    })
    .unwrap();
    let province = pixels(&host.gpu, logical, false, true);
    host.execute(&Command::Upload {
        image: destination.clone(),
        pixels: province.clone(),
    })
    .unwrap();
    let shot = snapshot(&mut host, &destination, logical).unwrap();
    let result = &shot.images[&destination.id];
    assert_eq!(result.stored_size(), Some(stored));
    assert_eq!(read(&host, result, false), expected);
    assert_eq!(
        read(&host, result, true),
        province.province.as_ref().unwrap().as_slice()
    );
    assert!(!original.images[&source.id].has_province());
}

#[test]
fn partially_accepted_gpu_upload_keeps_previous_image_and_reclaims_reservation() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(intercept),
            Config {
                work_framebuffer: true,
                tile_edge: 8,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let mut host = Graphics::new(gpu, Cache::new(0));
    let mut ids = slotmap::SlotMap::with_key();
    let size = Size {
        width: 24,
        height: 16,
    };
    let image = reference(&mut ids);
    host.execute(&Command::Create {
        image: image.id,
        lifetime: Arc::downgrade(&image.lifetime),
        size,
        color: 0x67112233,
    })
    .unwrap();
    let previous = snapshot(&mut host, &image, size).unwrap();
    let expected = read(&host, &previous.images[&image.id], false);
    host.gpu.collect().unwrap();
    let baseline = host.gpu.resident.used();
    host.execute(&Command::BeginUpload {
        staging_bytes: 0,
        image: image.clone(),
        size,
        main: true,
        province: false,
        source: Some(image.clone()),
    })
    .unwrap();
    let data = pixels(&host.gpu, size, true, false);
    FAIL_AFTER.set(2);
    assert!(
        host.execute(&Command::Upload {
            image: image.clone(),
            pixels: data.clone()
        })
        .is_err()
    );
    assert_eq!(
        FAIL_AFTER.get(),
        0,
        "failure must follow the second real tile upload"
    );
    let current = snapshot(&mut host, &image, size).unwrap();
    assert_eq!(read(&host, &current.images[&image.id], false), expected);
    host.maintain().unwrap();
    host.gpu.collect().unwrap();
    assert_eq!(host.gpu.resident.used(), baseline);
    host.execute(&Command::BeginUpload {
        staging_bytes: 0,
        image: image.clone(),
        size,
        main: true,
        province: false,
        source: Some(image.clone()),
    })
    .unwrap();
    host.execute(&Command::Upload {
        image: image.clone(),
        pixels: data.clone(),
    })
    .unwrap();
    let uploaded = snapshot(&mut host, &image, size).unwrap();
    assert_eq!(
        read(&host, &uploaded.images[&image.id], false),
        data.main.as_ref().unwrap().as_slice()
    );
    assert_eq!(read(&host, &previous.images[&image.id], false), expected);
}
