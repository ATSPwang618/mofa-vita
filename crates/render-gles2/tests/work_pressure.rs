#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
use krkr_protocol::graphics::{DrawFace, Fill, Rect, Size};
use krkr_render_gles2::{Config, Gpu};
use std::{cell::Cell, ffi::c_void};

type Draw = unsafe extern "system" fn(u32, i32, i32);
type GetError = unsafe extern "system" fn() -> u32;
type Finish = unsafe extern "system" fn();
type Clear = unsafe extern "system" fn(u32);
type TexImage = unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
thread_local! {
    static DRAW: Cell<Option<Draw>> = const { Cell::new(None) };
    static ERROR: Cell<Option<GetError>> = const { Cell::new(None) };
    static FINISH: Cell<Option<Finish>> = const { Cell::new(None) };
    static CLEAR: Cell<Option<Clear>> = const { Cell::new(None) };
    static REMAINING: Cell<u32> = const { Cell::new(0) };
    static CODE: Cell<u32> = const { Cell::new(0) };
    static PENDING: Cell<u32> = const { Cell::new(0) };
    static ATTEMPTS: Cell<u32> = const { Cell::new(0) };
    static FINISHES: Cell<u32> = const { Cell::new(0) };
    static CLEARS: Cell<u32> = const { Cell::new(0) };
    static TEX_IMAGE: Cell<Option<TexImage>> = const { Cell::new(None) };
    static ALLOCATION_FAILURES: Cell<u32> = const { Cell::new(0) };
    static ALLOCATION_ATTEMPTS: Cell<u32> = const { Cell::new(0) };
}
fn intercept(name: &str, address: *const c_void) -> *const c_void {
    match name {
        "glTexImage2D" => {
            TEX_IMAGE.set(Some(unsafe {
                std::mem::transmute::<*const c_void, TexImage>(address)
            }));
            tex_image as *const c_void
        }
        "glDrawArrays" => {
            DRAW.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Draw>(address)
            }));
            draw as *const c_void
        }
        "glGetError" => {
            ERROR.set(Some(unsafe {
                std::mem::transmute::<*const c_void, GetError>(address)
            }));
            get_error as *const c_void
        }
        "glFinish" => {
            FINISH.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Finish>(address)
            }));
            finish as *const c_void
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
#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn tex_image(
    target: u32,
    level: i32,
    internal: i32,
    width: i32,
    height: i32,
    border: i32,
    format: u32,
    kind: u32,
    pixels: *const c_void,
) {
    ALLOCATION_ATTEMPTS.set(ALLOCATION_ATTEMPTS.get() + 1);
    if ALLOCATION_FAILURES.get() != 0 {
        ALLOCATION_FAILURES.set(ALLOCATION_FAILURES.get() - 1);
        PENDING.set(CODE.get());
    } else {
        unsafe {
            TEX_IMAGE.get().unwrap()(
                target, level, internal, width, height, border, format, kind, pixels,
            )
        };
    }
}
unsafe extern "system" fn draw(mode: u32, start: i32, count: i32) {
    ATTEMPTS.set(ATTEMPTS.get() + 1);
    if REMAINING.get() != 0 {
        REMAINING.set(REMAINING.get() - 1);
        PENDING.set(CODE.get());
    } else {
        unsafe { DRAW.get().unwrap()(mode, start, count) };
    }
}
unsafe extern "system" fn get_error() -> u32 {
    let pending = PENDING.replace(0);
    if pending != 0 {
        pending
    } else {
        unsafe { ERROR.get().unwrap()() }
    }
}
unsafe extern "system" fn finish() {
    FINISHES.set(FINISHES.get() + 1);
    unsafe { FINISH.get().unwrap()() };
}
unsafe extern "system" fn clear(mask: u32) {
    CLEARS.set(CLEARS.get() + 1);
    unsafe { CLEAR.get().unwrap()(mask) };
}

#[test]
fn allocation_oom_waits_for_the_gpu_even_without_retired_textures() {
    for (code, failures, attempts, success) in [
        (glow::OUT_OF_MEMORY, 1, 2, true),
        (glow::OUT_OF_MEMORY, 2, 2, false),
        (glow::INVALID_OPERATION, 1, 1, false),
    ] {
        let context = support::Context::new();
        let gpu = unsafe { Gpu::new(context.gl_with(intercept), Config::default()).unwrap() };
        let baseline = gpu.resident.used();
        CODE.set(code);
        ALLOCATION_FAILURES.set(failures);
        ALLOCATION_ATTEMPTS.set(0);
        FINISHES.set(0);
        let result = gpu.create_image(
            Size {
                width: 16,
                height: 16,
            },
            0xff123456,
        );
        assert_eq!(result.is_ok(), success);
        assert_eq!(ALLOCATION_ATTEMPTS.get(), attempts);
        assert_eq!(FINISHES.get(), u32::from(code == glow::OUT_OF_MEMORY));
        if let Ok(image) = result {
            assert_eq!(gpu.pixel(&image, 4, 4, false).unwrap(), 0xff123456);
        }
        gpu.collect().unwrap();
        assert_eq!(
            gpu.resident.used(),
            baseline,
            "failed allocations must release permits"
        );
    }
}

#[test]
fn collection_waits_once_and_preserves_glyph_pixels() {
    use krkr_protocol::{
        budget::Budget,
        pixels::Bytes,
        text::{Glyph, PlacedGlyph, Run, Style},
    };
    use std::sync::Arc;
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(intercept),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 32,
        height: 32,
    };
    let budget = Budget::new(1024);
    let mut mask = Bytes::zeroed(16, &budget).unwrap();
    mask.as_mut_slice().fill(255);
    let glyph = Arc::new(Glyph {
        id: 919191,
        size: Size {
            width: 4,
            height: 4,
        },
        origin: [0, 0],
        advance: [4, 0],
        levels: 256,
        mask,
    });
    let glyphs = vec![PlacedGlyph {
        glyph,
        x: 2,
        y: 3,
        color: 0xffffff,
    }];
    let run = Run {
        permit: budget.reserve(std::mem::size_of::<PlacedGlyph>()).unwrap(),
        glyphs,
    };
    let mut target = gpu.create_image(size, 0xff000000).unwrap();
    let retired = gpu.create_image(size, 0xff123456).unwrap();
    gpu.draw_text(
        &mut target,
        &run,
        Style {
            color: 0xffffff,
            opacity: 255,
            antialias: true,
            shadow_level: 0,
            shadow_color: 0,
            shadow_width: 0,
            shadow_offset: [0, 0],
            face: DrawFace::Opaque,
            hold_alpha: true,
        },
        size.rect(),
    )
    .unwrap();
    drop(retired);
    FINISHES.set(0);
    gpu.collect().unwrap();
    assert_eq!(
        FINISHES.get(),
        1,
        "the upload fence also covers retired resources"
    );
    assert_eq!(gpu.pixel(&target, 2, 3, false).unwrap(), 0xfffefefe);
    assert_eq!(gpu.pixel(&target, 0, 0, false).unwrap(), 0xff000000);
}

#[test]
fn target_promotion_retries_only_oom_once_before_any_caller_write() {
    for (code, failures, attempts, success) in [
        (glow::OUT_OF_MEMORY, 1, 2, true),
        (glow::OUT_OF_MEMORY, 2, 2, false),
        (glow::INVALID_OPERATION, 1, 1, false),
    ] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(intercept),
                Config {
                    work_framebuffer: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 16,
            height: 16,
        };
        let mut image = gpu.create_image(size, 0x71325476).unwrap();
        let other = gpu.create_image(size, 0x19283746).unwrap();
        gpu.flush().unwrap();
        CODE.set(code);
        REMAINING.set(failures);
        ATTEMPTS.set(0);
        FINISHES.set(0);
        CLEARS.set(0);
        let result = gpu.fill(
            &mut image,
            &[Fill {
                rectangle: Rect {
                    left: 4,
                    top: 4,
                    width: 4,
                    height: 4,
                },
                color: 0xe1234567,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        );
        assert_eq!(result.is_ok(), success);
        assert_eq!(ATTEMPTS.get(), attempts);
        assert_eq!(
            CLEARS.get(),
            u32::from(success),
            "never replay a caller clear"
        );
        assert_eq!(FINISHES.get(), u32::from(code == glow::OUT_OF_MEMORY));
        assert_eq!(
            gpu.pixel(&image, 4, 4, false).unwrap(),
            if success { 0xe1234567 } else { 0x71325476 }
        );
        assert_eq!(gpu.pixel(&image, 0, 0, false).unwrap(), 0x71325476);
        assert_eq!(gpu.pixel(&other, 4, 4, false).unwrap(), 0x19283746);
    }
}
