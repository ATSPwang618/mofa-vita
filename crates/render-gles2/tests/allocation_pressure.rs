#![cfg(target_os = "linux")]
mod support;
use krkr_protocol::graphics::Size;
use krkr_render_gles2::{Config, Gpu};
use std::{cell::Cell, ffi::c_void};

type TexImage = unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type GetError = unsafe extern "system" fn() -> u32;
thread_local! {
    static IMAGE: Cell<Option<TexImage>> = const { Cell::new(None) };
    static ERROR: Cell<Option<GetError>> = const { Cell::new(None) };
    static FAILURES: Cell<usize> = const { Cell::new(0) };
    static FAILURE_KIND: Cell<u32> = const { Cell::new(glow::OUT_OF_MEMORY) };
    static PENDING: Cell<u32> = const { Cell::new(glow::NO_ERROR) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
}
fn intercept(name: &str, address: *const c_void) -> *const c_void {
    unsafe {
        match name {
            "glTexImage2D" => {
                IMAGE.set(Some(std::mem::transmute::<*const c_void, TexImage>(
                    address,
                )));
                tex_image as *const c_void
            }
            "glGetError" => {
                ERROR.set(Some(std::mem::transmute::<*const c_void, GetError>(
                    address,
                )));
                get_error as *const c_void
            }
            _ => address,
        }
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
    CALLS.set(CALLS.get() + 1);
    if FAILURES.get() != 0 {
        FAILURES.set(FAILURES.get() - 1);
        PENDING.set(FAILURE_KIND.get());
    } else {
        unsafe {
            IMAGE.get().unwrap()(
                target, level, internal, width, height, border, format, kind, data,
            )
        };
    }
}
unsafe extern "system" fn get_error() -> u32 {
    let error = PENDING.replace(glow::NO_ERROR);
    if error != glow::NO_ERROR {
        error
    } else {
        unsafe { ERROR.get().unwrap()() }
    }
}
fn fail(count: usize, error: u32) {
    CALLS.set(0);
    FAILURES.set(count);
    FAILURE_KIND.set(error);
}

#[test]
fn driver_oom_reclaims_idle_storage_and_retries_only_unpublished_allocations() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(intercept),
                Config {
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let live = gpu
            .create_image(
                Size {
                    width: 16,
                    height: 16,
                },
                0xff314159,
            )
            .unwrap();
        drop(
            gpu.create_image(
                Size {
                    width: 96,
                    height: 64,
                },
                0xff123456,
            )
            .unwrap(),
        );
        gpu.maintain().unwrap();
        let before = gpu.resident.used();
        fail(1, glow::OUT_OF_MEMORY);
        let next = gpu
            .create_image(
                Size {
                    width: 31,
                    height: 27,
                },
                0xff192837,
            )
            .unwrap();
        assert_eq!(
            CALLS.get(),
            2,
            "one retry after reclaiming idle allocations"
        );
        if work {
            assert!(gpu.resident.used() < before);
        }
        assert_eq!(gpu.pixel(&next, 17, 15, false).unwrap(), 0xff192837);
        assert_eq!(gpu.pixel(&live, 4, 5, false).unwrap(), 0xff314159);

        fail(2, glow::OUT_OF_MEMORY);
        let error = match gpu.create_image(
            Size {
                width: 43,
                height: 37,
            },
            0,
        ) {
            Ok(_) => panic!("persistent driver failure must propagate"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("43x37") && error.contains("0x0505"),
            "{error}"
        );
        assert_eq!(CALLS.get(), 2);
        assert_eq!(gpu.pixel(&live, 4, 5, false).unwrap(), 0xff314159);

        fail(1, glow::INVALID_VALUE);
        assert!(
            gpu.create_image(
                Size {
                    width: 47,
                    height: 39
                },
                0
            )
            .is_err()
        );
        assert_eq!(CALLS.get(), 1, "only allocation exhaustion is retried");
        assert_eq!(gpu.pixel(&live, 4, 5, false).unwrap(), 0xff314159);
        fail(0, glow::OUT_OF_MEMORY);
    }
}
