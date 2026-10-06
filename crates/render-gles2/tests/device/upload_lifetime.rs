use crate::test_support as support;
use crate::{Config, Gpu};
use krkr_protocol::graphics::{Rect, Size};
use std::{cell::Cell, ffi::c_void};

type Finish = unsafe extern "system" fn();
type TexImage = unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type TexSubImage = unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
thread_local! {
    static FINISH: Cell<Option<Finish>> = const { Cell::new(None) };
    static WAITS: Cell<usize> = const { Cell::new(0) };
    static TEX_IMAGE: Cell<Option<TexImage>> = const { Cell::new(None) };
    static TEX_SUB_IMAGE: Cell<Option<TexSubImage>> = const { Cell::new(None) };
    static IMAGES: Cell<usize> = const { Cell::new(0) };
    static EMPTY_IMAGES: Cell<usize> = const { Cell::new(0) };
    static SUB_IMAGES: Cell<usize> = const { Cell::new(0) };
}
fn hook(name: &str, pointer: *const c_void) -> *const c_void {
    if name == "glFinish" {
        FINISH.set(Some(unsafe {
            std::mem::transmute::<*const c_void, Finish>(pointer)
        }));
        finish as *const c_void
    } else if name == "glTexImage2D" {
        TEX_IMAGE.set(Some(unsafe {
            std::mem::transmute::<*const c_void, TexImage>(pointer)
        }));
        tex_image as *const c_void
    } else if name == "glTexSubImage2D" {
        TEX_SUB_IMAGE.set(Some(unsafe {
            std::mem::transmute::<*const c_void, TexSubImage>(pointer)
        }));
        tex_sub_image as *const c_void
    } else {
        pointer
    }
}
unsafe extern "system" fn tex_image(
    target: u32,
    level: i32,
    internal: i32,
    width: i32,
    height: i32,
    border: i32,
    format: u32,
    kind: u32,
    data: *const c_void,
) {
    IMAGES.set(IMAGES.get() + 1);
    if data.is_null() {
        EMPTY_IMAGES.set(EMPTY_IMAGES.get() + 1);
    }
    unsafe {
        TEX_IMAGE.get().unwrap()(
            target, level, internal, width, height, border, format, kind, data,
        )
    };
}
unsafe extern "system" fn tex_sub_image(
    target: u32,
    level: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    format: u32,
    kind: u32,
    data: *const c_void,
) {
    SUB_IMAGES.set(SUB_IMAGES.get() + 1);
    unsafe { TEX_SUB_IMAGE.get().unwrap()(target, level, x, y, width, height, format, kind, data) };
}
unsafe extern "system" fn finish() {
    WAITS.set(WAITS.get() + 1);
    unsafe { FINISH.get().unwrap()() };
}
fn gpu(context: &support::Context) -> Gpu {
    WAITS.set(0);
    unsafe {
        Gpu::new(
            context.gl_with(hook),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    }
}
fn rgba(size: Size) -> krkr_protocol::pixels::Pixels {
    let bytes = size.rgba_bytes().unwrap();
    krkr_protocol::pixels::Pixels {
        size,
        main: Some(
            krkr_protocol::pixels::Bytes::zeroed(bytes, &krkr_protocol::budget::Budget::new(bytes))
                .unwrap(),
        ),
        province: None,
    }
}

#[test]
fn transient_vertex_buffers_keep_storage_until_bounded_retirement_completes() {
    use glow::HasContext;
    use krkr_protocol::budget::Budget;

    let context = support::Context::new();
    let gpu = gpu(&context);
    let budget = Budget::new(8 * 1024 * 1024);
    let mut names = Vec::new();
    for index in 0..63 {
        let buffer = gpu
            .device
            .buffer(glow::ARRAY_BUFFER, &[0; 32], &budget)
            .unwrap();
        names.push(buffer.name);
        drop(buffer);
        gpu.maintain().unwrap();
        assert_eq!(WAITS.get(), 0, "tiny buffer {index} stalled maintenance");
        assert_eq!(budget.used(), (index + 1) * 32);
        assert!(
            names
                .iter()
                .all(|&name| unsafe { gpu.device.gl.is_buffer(name) })
        );
    }
    drop(
        gpu.device
            .buffer(glow::ARRAY_BUFFER, &[0; 32], &budget)
            .unwrap(),
    );
    gpu.maintain().unwrap();
    assert_eq!(WAITS.get(), 1);
    assert_eq!(budget.used(), 0);
    assert!(
        names
            .iter()
            .all(|&name| unsafe { !gpu.device.gl.is_buffer(name) })
    );

    // A single large mesh also meets the byte bound; it cannot wait for another
    // 63 allocations before its charged memory is returned.
    drop(
        gpu.device
            .buffer(glow::ARRAY_BUFFER, &vec![0; 4 * 1024 * 1024], &budget)
            .unwrap(),
    );
    gpu.maintain().unwrap();
    assert_eq!(WAITS.get(), 2);
    assert_eq!(budget.used(), 0);

    drop(
        gpu.device
            .buffer(glow::ARRAY_BUFFER, &[0; 32], &budget)
            .unwrap(),
    );
    gpu.collect().unwrap();
    assert_eq!(WAITS.get(), 3);
    assert_eq!(budget.used(), 0);
}

#[test]
fn partial_upload_consumes_client_bytes_without_a_retained_copy_or_wait() {
    let context = support::Context::new();
    let gpu = gpu(&context);
    let size = Size {
        width: 16,
        height: 16,
    };
    let image = gpu.create_image(size, 0xff102030).unwrap();
    let texture = &image.main.as_ref().unwrap().tiles[0].texture;
    let area = Rect {
        left: 3,
        top: 5,
        width: 2,
        height: 2,
    };
    let baseline = gpu.staging.used();
    let locked = gpu.staging.reserve(gpu.staging.available()).unwrap();
    let mut caller = [11, 71, 191, 255].repeat(4);
    gpu.device.upload_region(texture, area, &caller).unwrap();
    caller.fill(0xdd);
    assert_eq!(WAITS.get(), 0);
    drop(locked);
    assert_eq!(gpu.staging.used(), baseline);
    let actual = gpu.readback(&image, size.rect(), false).unwrap();
    let start = (5 * 16 + 3) * 4;
    assert_eq!(
        &actual.data.as_slice()[start..start + 8],
        &[11, 71, 191, 255].repeat(2)
    );
    assert_eq!(&actual.data.as_slice()[..4], &[16, 32, 48, 255]);
}

#[test]
fn owned_upload_releases_its_budget_when_the_call_returns() {
    let context = support::Context::new();
    let gpu = gpu(&context);
    let size = Size {
        width: 16,
        height: 16,
    };
    let image = gpu.create_image(size, 0xff102030).unwrap();
    let texture = &image.main.as_ref().unwrap().tiles[0].texture;
    let area = Rect {
        left: 3,
        top: 5,
        width: 2,
        height: 2,
    };
    let baseline = gpu.staging.used();
    let mut packed = krkr_protocol::pixels::Bytes::zeroed(16, &gpu.staging).unwrap();
    packed
        .as_mut_slice()
        .copy_from_slice(&[11, 71, 191, 255].repeat(4));
    gpu.device
        .upload_owned_region(texture, area, packed)
        .unwrap();
    assert_eq!(gpu.staging.used(), baseline);
    assert_eq!(WAITS.get(), 0);
    let actual = gpu.readback(&image, size.rect(), false).unwrap();
    let start = (5 * 16 + 3) * 4;
    assert_eq!(
        &actual.data.as_slice()[start..start + 8],
        &[11, 71, 191, 255].repeat(2)
    );
}

#[test]
fn partial_then_full_upload_preserves_gl_order_without_a_host_fence() {
    let context = support::Context::new();
    let gpu = gpu(&context);
    let size = Size {
        width: 8,
        height: 8,
    };
    let image = gpu.create_image(size, 0xff000000).unwrap();
    let texture = &image.main.as_ref().unwrap().tiles[0].texture;
    let area = Rect {
        width: 2,
        height: 2,
        ..Default::default()
    };
    gpu.device.upload_region(texture, area, &[23; 16]).unwrap();
    gpu.device
        .upload(texture, &[5, 9, 17, 255].repeat(64))
        .unwrap();
    assert_eq!(WAITS.get(), 0);
    let actual = gpu.readback(&image, size.rect(), false).unwrap();
    assert_eq!(actual.data.as_slice(), &[5, 9, 17, 255].repeat(64));
}

#[test]
fn decoded_image_fills_fresh_storage_without_redefinition() {
    let context = support::Context::new();
    let gpu = gpu(&context);
    let size = Size {
        width: 16,
        height: 16,
    };
    let mut pixels = rgba(size);
    for (i, pixel) in pixels
        .main
        .as_mut()
        .unwrap()
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        pixel.copy_from_slice(&[i as u8, (i / 16) as u8, 71, 255]);
    }
    let expected = pixels.main.as_ref().unwrap().as_slice().to_vec();
    let reservation = gpu.begin_upload(None, size, true, false).unwrap();
    IMAGES.set(0);
    SUB_IMAGES.set(0);
    let locked = gpu.staging.reserve(gpu.staging.available()).unwrap();
    let image = reservation.complete(&gpu, &pixels, None).unwrap();
    assert_eq!(IMAGES.get(), 0, "the already reserved backing must survive");
    assert_eq!(SUB_IMAGES.get(), 1);
    assert_eq!(WAITS.get(), 0);
    pixels.main.as_mut().unwrap().as_mut_slice().fill(0xdd);
    drop(locked);
    let actual = gpu.readback(&image, size.rect(), false).unwrap();
    assert_eq!(actual.data.as_slice(), expected);
}

#[test]
fn recycled_storage_uses_full_replacement_even_with_generation_zero() {
    use krkr_protocol::budget::Budget;

    let context = support::Context::new();
    let gpu = gpu(&context);
    let size = Size {
        width: 16,
        height: 16,
    };
    let budget = Budget::new(1024);
    let texture = gpu.device.sample_texture(size, &budget).unwrap();
    gpu.device.upload(&texture, &[71; 1024]).unwrap();
    let name = texture.name();
    drop(texture);
    let recycled = gpu.device.sample_texture(size, &budget).unwrap();
    assert_eq!(recycled.name(), name);
    assert_eq!(recycled.generation.get(), 0);
    IMAGES.set(0);
    SUB_IMAGES.set(0);
    gpu.device.upload(&recycled, &[83; 1024]).unwrap();
    assert_eq!(
        IMAGES.get(),
        1,
        "old GPU readers may still need this backing"
    );
    assert_eq!(SUB_IMAGES.get(), 0);
    assert_eq!(WAITS.get(), 0);
}

#[test]
fn unique_image_patch_borrows_pixels_without_staging_headroom() {
    let context = support::Context::new();
    let gpu = gpu(&context);
    let size = Size {
        width: 16,
        height: 16,
    };
    let mut initial = rgba(size);
    initial.main.as_mut().unwrap().as_mut_slice().fill(31);
    let mut image = gpu.upload_scaled(&initial, size).unwrap();
    let area = Rect {
        left: 3,
        top: 5,
        width: 2,
        height: 2,
    };
    let mut patch = rgba(Size {
        width: 2,
        height: 2,
    });
    patch.main.as_mut().unwrap().as_mut_slice().fill(83);
    let locked = gpu.staging.reserve(gpu.staging.available()).unwrap();
    gpu.patch_region(&mut image, area, &patch).unwrap();
    patch.main.as_mut().unwrap().as_mut_slice().fill(0xdd);
    assert_eq!(WAITS.get(), 0);
    drop(locked);
    let actual = gpu.readback(&image, size.rect(), false).unwrap();
    let start = (5 * 16 + 3) * 4;
    assert_eq!(&actual.data.as_slice()[start..start + 8], &[83; 8]);
    assert_eq!(&actual.data.as_slice()[..4], &[31; 4]);
}

#[test]
fn known_rgba_pixels_initialize_each_tile_once_and_preserve_edge_rows() {
    let context = support::Context::new();
    for work_framebuffer in [false, true] {
        for size in [
            Size {
                width: 16,
                height: 35,
            },
            Size {
                width: 35,
                height: 19,
            },
        ] {
            let gpu = unsafe {
                Gpu::new(
                    context.gl_with(hook),
                    Config {
                        work_framebuffer,
                        tile_edge: 16,
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            let mut pixels = rgba(size);
            for (index, pixel) in pixels
                .main
                .as_mut()
                .unwrap()
                .as_mut_slice()
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .enumerate()
            {
                pixel.copy_from_slice(&[(index % 251) as u8, (index / 31) as u8, 119, 255]);
            }
            let expected = pixels.main.as_ref().unwrap().as_slice().to_vec();
            IMAGES.set(0);
            EMPTY_IMAGES.set(0);
            SUB_IMAGES.set(0);
            WAITS.set(0);
            let scratch = if size.width > 16 { 16 * 16 * 4 } else { 0 };
            let locked = gpu
                .staging
                .reserve(gpu.staging.available() - scratch)
                .unwrap();
            let image = gpu.upload_scaled(&pixels, size).unwrap();
            assert_eq!(
                IMAGES.get(),
                (size.width.div_ceil(16) * size.height.div_ceil(16)) as usize
            );
            assert_eq!(
                EMPTY_IMAGES.get(),
                0,
                "upload must define the initial storage with pixels"
            );
            assert_eq!(SUB_IMAGES.get(), 0);
            assert_eq!(WAITS.get(), 0);
            pixels.main.as_mut().unwrap().as_mut_slice().fill(0xdd);
            drop(locked);
            let actual = gpu.readback(&image, size.rect(), false).unwrap();
            assert_eq!(
                actual.data.as_slice(),
                expected,
                "work={work_framebuffer} size={size:?}"
            );
        }
    }
}
