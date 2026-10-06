#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
use krkr_protocol::graphics::{DrawFace, Fill, Rect, Size};
use krkr_render_gles2::{Config, Gpu};
use std::{cell::Cell, ffi::c_void};

type BufferData = unsafe extern "system" fn(u32, isize, *const c_void, u32);
type GetError = unsafe extern "system" fn() -> u32;
type Finish = unsafe extern "system" fn();
type BindBuffer = unsafe extern "system" fn(u32, u32);
thread_local! {
    static DATA: Cell<Option<BufferData>> = const { Cell::new(None) };
    static ERROR: Cell<Option<GetError>> = const { Cell::new(None) };
    static FINISH: Cell<Option<Finish>> = const { Cell::new(None) };
    static BIND: Cell<Option<BindBuffer>> = const { Cell::new(None) };
    static FAILURES: Cell<u32> = const { Cell::new(0) };
    static CODE: Cell<u32> = const { Cell::new(0) };
    static PENDING: Cell<u32> = const { Cell::new(0) };
    static ATTEMPTS: Cell<u32> = const { Cell::new(0) };
    static FINISHES: Cell<u32> = const { Cell::new(0) };
    static SKIP: Cell<u32> = const { Cell::new(0) };
}
fn intercept(name: &str, address: *const c_void) -> *const c_void {
    match name {
        "glBufferData" => {
            DATA.set(Some(unsafe {
                std::mem::transmute::<*const c_void, BufferData>(address)
            }));
            data as *const c_void
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
        "glBindBuffer" => {
            BIND.set(Some(unsafe {
                std::mem::transmute::<*const c_void, BindBuffer>(address)
            }));
            address
        }
        _ => address,
    }
}
unsafe extern "system" fn data(target: u32, size: isize, data: *const c_void, usage: u32) {
    ATTEMPTS.set(ATTEMPTS.get() + 1);
    if SKIP.get() != 0 {
        SKIP.set(SKIP.get() - 1);
        unsafe { DATA.get().unwrap()(target, size, data, usage) };
    } else if FAILURES.get() != 0 {
        FAILURES.set(FAILURES.get() - 1);
        PENDING.set(CODE.get());
    } else {
        unsafe { DATA.get().unwrap()(target, size, data, usage) };
    }
}

#[test]
fn text_batches_survive_pressure_with_pending_uploads_and_earlier_glyphs() {
    use krkr_protocol::{
        pixels::Bytes,
        text::{Glyph, PlacedGlyph, Run, Style},
    };
    use std::sync::Arc;

    let render = |failures, skip| {
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
            width: 64,
            height: 64,
        };
        let glyph = Arc::new(Glyph {
            id: 371993,
            size: Size {
                width: 2,
                height: 2,
            },
            origin: [0, 0],
            advance: [4, 0],
            levels: 256,
            mask: Bytes::with_permit(vec![96, 192, 255, 32], gpu.staging.reserve(4).unwrap()),
        });
        let glyphs: Vec<_> = (0..130)
            .map(|i| PlacedGlyph {
                glyph: glyph.clone(),
                x: (i % 13) * 4 + 1,
                y: (i / 13) * 4 + 2,
                color: 0xffffff,
            })
            .collect();
        let run = Run {
            permit: gpu
                .staging
                .reserve(glyphs.len() * std::mem::size_of::<PlacedGlyph>())
                .unwrap(),
            glyphs,
        };
        let mut image = gpu.create_image(size, 0xff123456).unwrap();
        drop(gpu.create_image(size, 0xff987654).unwrap());
        CODE.set(glow::OUT_OF_MEMORY);
        FAILURES.set(failures);
        SKIP.set(skip);
        ATTEMPTS.set(0);
        gpu.draw_text(
            &mut image,
            &run,
            Style {
                color: 0xffffff,
                opacity: 173,
                antialias: true,
                shadow_level: 0,
                shadow_color: 0,
                shadow_width: 0,
                shadow_offset: [0, 0],
                face: DrawFace::Alpha,
                hold_alpha: false,
            },
            size.rect(),
        )
        .unwrap();
        assert!(
            ATTEMPTS.get() >= 3,
            "must exercise multiple glyph vertex buffers"
        );
        assert_eq!(FAILURES.get(), 0);
        assert_eq!(SKIP.get(), 0);
        let pixels = gpu.readback(&image, size.rect(), false).unwrap();
        pixels.data.as_slice().to_vec()
    };
    let expected = render(0, 0);
    assert_eq!(render(1, 0), expected);
    assert_eq!(render(1, 1), expected);
}
unsafe extern "system" fn get_error() -> u32 {
    match PENDING.replace(0) {
        0 => unsafe { ERROR.get().unwrap()() },
        code => code,
    }
}
unsafe extern "system" fn finish() {
    FINISHES.set(FINISHES.get() + 1);
    unsafe {
        FINISH.get().unwrap()();
        // Pressure collection may bind other geometry while storing a dirty
        // work surface. A retry must explicitly restore its own buffer.
        BIND.get().unwrap()(glow::ARRAY_BUFFER, 0);
    }
}

#[test]
fn buffer_pressure_retries_before_drawing_and_releases_failed_storage() {
    for work in [false, true] {
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
                        work_framebuffer: work,
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            let size = Size {
                width: 32,
                height: 32,
            };
            let mut target = gpu.create_image(size, 0xff123456).unwrap();
            gpu.collect().unwrap();
            let baseline = gpu.staging.used();
            let fills: Vec<_> = (0..8)
                .map(|i| Fill {
                    rectangle: Rect {
                        left: i * 2,
                        top: 2,
                        width: 1,
                        height: 4,
                    },
                    color: 0xffabcdef,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                })
                .collect();
            CODE.set(code);
            FAILURES.set(failures);
            ATTEMPTS.set(0);
            FINISHES.set(0);
            let result = gpu.fill(&mut target, &fills);
            assert_eq!(
                result.is_ok(),
                success,
                "work={work} code={code:x}: {result:?}"
            );
            assert_eq!(ATTEMPTS.get(), attempts);
            assert_eq!(FINISHES.get(), u32::from(code == glow::OUT_OF_MEMORY));
            if let Err(error) = result {
                assert!(error.to_string().contains("buffer allocation"), "{error}");
            }
            gpu.collect().unwrap();
            assert!(
                gpu.staging.used() <= baseline,
                "buffer staging permit leaked"
            );
            let pixels = gpu.readback(&target, size.rect(), false).unwrap();
            for y in 0..size.height {
                for x in 0..size.width {
                    let changed = success && x < 16 && x % 2 == 0 && (2..6).contains(&y);
                    let offset = ((y * size.width + x) * 4) as usize;
                    assert_eq!(
                        &pixels.data.as_slice()[offset..offset + 4],
                        if changed {
                            &[0xab, 0xcd, 0xef, 0xff]
                        } else {
                            &[0x12, 0x34, 0x56, 0xff]
                        },
                        "at {x},{y}"
                    );
                }
            }
            FAILURES.set(0);
        }
    }
}
