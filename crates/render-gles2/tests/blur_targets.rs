#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
use krkr_protocol::{
    graphics::{Adjustment, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu};
use std::{cell::Cell, ffi::c_void};

type Attach = unsafe extern "system" fn(u32, u32, u32, u32, i32);
type GetError = unsafe extern "system" fn() -> u32;
thread_local! {
    static ATTACH: Cell<Option<Attach>> = const { Cell::new(None) };
    static ERROR: Cell<Option<GetError>> = const { Cell::new(None) };
    static REJECT_AT: Cell<usize> = const { Cell::new(0) };
    static PENDING: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
}
fn intercept(name: &str, address: *const c_void) -> *const c_void {
    match name {
        "glFramebufferTexture2D" => {
            ATTACH.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Attach>(address)
            }));
            attach as *const c_void
        }
        "glGetError" => {
            ERROR.set(Some(unsafe {
                std::mem::transmute::<*const c_void, GetError>(address)
            }));
            get_error as *const c_void
        }
        _ => address,
    }
}
unsafe extern "system" fn attach(a: u32, b: u32, c: u32, d: u32, e: i32) {
    CALLS.set(CALLS.get() + 1);
    if REJECT_AT.get() != 0 && CALLS.get() >= REJECT_AT.get() {
        PENDING.set(true);
    } else {
        unsafe { ATTACH.get().unwrap()(a, b, c, d, e) };
    }
}
unsafe extern "system" fn get_error() -> u32 {
    if PENDING.replace(false) {
        glow::OUT_OF_MEMORY
    } else {
        unsafe { ERROR.get().unwrap()() }
    }
}
#[test]
fn repeated_blurs_reuse_targets_and_attachment_oom_falls_back_without_pixel_changes() {
    let mut reference = None;
    for reject_at in [0, 1, 2] {
        REJECT_AT.set(0);
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(intercept),
                Config {
                    work_framebuffer: true,
                    tile_edge: 1024,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 96,
            height: 72,
        };
        let mut bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (i, p) in bytes
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            p.copy_from_slice(&[(i * 3) as u8, (i * 7) as u8, (i * 11) as u8, (i * 13) as u8]);
        }
        let source = gpu
            .assign_bitmap(
                None,
                &Pixels {
                    size,
                    main: Some(bytes),
                    province: None,
                },
            )
            .unwrap();
        CALLS.set(0);
        REJECT_AT.set(reject_at);
        for _ in 0..4 {
            let mut image = source.shared();
            gpu.adjust(
                &mut image,
                size.rect(),
                &Adjustment::BoxBlur {
                    radius: [30, 10],
                    alpha: true,
                },
            )
            .unwrap();
            let result = gpu
                .readback(&image, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .to_vec();
            if let Some(expected) = &reference {
                assert_eq!(&result, expected);
            } else {
                reference = Some(result);
            }
        }
        assert_eq!(
            CALLS.get(),
            if reject_at == 0 { 2 } else { reject_at },
            "attachments must stay bounded across repeated blurs"
        );
        REJECT_AT.set(0);
    }
}
