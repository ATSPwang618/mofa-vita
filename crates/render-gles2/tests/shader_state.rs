#![cfg(target_os = "linux")]
mod support;
use krkr_protocol::graphics::{DrawFace, Rect, Size};
use krkr_render_gles2::{Config, Gpu};
use std::{cell::Cell, ffi::c_void};

thread_local! { static CALLS: Cell<usize> = const { Cell::new(0) }; }
type CreateShader = unsafe extern "system" fn(u32) -> u32;
thread_local! {
    static CREATE_SHADER: Cell<Option<CreateShader>> = const { Cell::new(None) };
    static VERTICES: Cell<usize> = const { Cell::new(0) };
}
unsafe extern "system" fn create_shader(kind: u32) -> u32 {
    if kind == glow::VERTEX_SHADER {
        VERTICES.set(VERTICES.get() + 1);
    }
    unsafe { CREATE_SHADER.get().unwrap()(kind) }
}
macro_rules! observe {
    ($alias:ident, $saved:ident, $hook:ident, $($arg:ident),+) => {
        type $alias = unsafe extern "system" fn(i32, $(observe!(@ty $arg)),+);
        thread_local! { static $saved: Cell<Option<$alias>> = const { Cell::new(None) }; }
        unsafe extern "system" fn $hook(location: i32, $($arg: f32),+) {
            CALLS.set(CALLS.get() + 1);
            unsafe { $saved.get().unwrap()(location, $($arg),+) };
        }
    };
    (@ty $arg:ident) => { f32 };
}
observe!(One, ONE, one, x);
observe!(Two, TWO, two, x, y);
observe!(Three, THREE, three, x, y, z);
observe!(Four, FOUR, four, x, y, z, w);
fn intercept(name: &str, address: *const c_void) -> *const c_void {
    match name {
        "glCreateShader" => {
            CREATE_SHADER.set(Some(unsafe {
                std::mem::transmute::<*const c_void, CreateShader>(address)
            }));
            create_shader as *const c_void
        }
        "glUniform1f" => {
            ONE.set(Some(unsafe {
                std::mem::transmute::<*const c_void, One>(address)
            }));
            one as *const c_void
        }
        "glUniform2f" => {
            TWO.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Two>(address)
            }));
            two as *const c_void
        }
        "glUniform3f" => {
            THREE.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Three>(address)
            }));
            three as *const c_void
        }
        "glUniform4f" => {
            FOUR.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Four>(address)
            }));
            four as *const c_void
        }
        _ => address,
    }
}

#[test]
fn draw_variants_reuse_one_vertex_shader_even_after_program_eviction() {
    use krkr_protocol::graphics::{Blend, BlendOptions};
    let context = support::Context::new();
    VERTICES.set(0);
    let gpu = unsafe { Gpu::new(context.gl_with(intercept), Config::default()).unwrap() };
    let size = Size {
        width: 4,
        height: 4,
    };
    let source = gpu.create_image(size, 0x79573193).unwrap();
    let mut target = gpu.create_image(size, 0xa1597917).unwrap();
    for _ in 0..2 {
        for mode in (1..=28).filter_map(Blend::from_legacy) {
            gpu.operate(
                &mut target,
                &source,
                size.rect(),
                0,
                0,
                size.rect(),
                BlendOptions::for_composition(mode, DrawFace::Alpha, 193),
            )
            .unwrap();
        }
    }
    gpu.pixel(&target, 0, 0, false).unwrap();
    assert_eq!(
        VERTICES.get(),
        1,
        "fragment variants share a compiled vertex stage"
    );
}

#[test]
fn repeated_uniforms_survive_texture_and_program_switches() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl_with(intercept), Config::default()).unwrap() };
    let size = Size {
        width: 16,
        height: 16,
    };
    let small = Size {
        width: 8,
        height: 8,
    };
    let red = gpu.create_image(small, 0x917f2010).unwrap();
    let blue = gpu.create_image(small, 0xb010207f).unwrap();
    let mut target = gpu.create_image(size, 0xff102030).unwrap();
    let mut other = gpu.create_image(small, 0xff204060).unwrap();
    let copy = |target: &mut _, source, x, y| {
        gpu.copy_rect(
            target,
            source,
            small.rect(),
            x,
            y,
            size.rect(),
            DrawFace::Alpha,
            false,
        )
        .unwrap();
    };
    copy(&mut target, &red, 2, 3);
    CALLS.set(0);
    copy(&mut target, &blue, 2, 3);
    assert_eq!(CALLS.get(), 0, "texture switches retain unchanged uniforms");
    gpu.color(&mut other, small.rect(), 0x00107090, 127, DrawFace::Alpha)
        .unwrap();
    CALLS.set(0);
    copy(&mut target, &red, 2, 3);
    assert_eq!(
        CALLS.get(),
        0,
        "each linked program owns its uniform values"
    );
    copy(&mut target, &blue, 6, 7);
    assert!(CALLS.get() > 0, "changed geometry must update uniforms");

    let result = gpu.readback(&target, size.rect(), false).unwrap();
    for y in 0..16 {
        for x in 0..16 {
            let contains = |area: Rect| {
                area.intersection(Rect {
                    left: x,
                    top: y,
                    width: 1,
                    height: 1,
                })
                .is_some()
            };
            let expected = if contains(Rect {
                left: 6,
                top: 7,
                ..small.rect()
            }) {
                [0x10, 0x20, 0x7f, 0xb0]
            } else if contains(Rect {
                left: 2,
                top: 3,
                ..small.rect()
            }) {
                [0x7f, 0x20, 0x10, 0x91]
            } else {
                [0x10, 0x20, 0x30, 0xff]
            };
            let at = (y * 16 + x) as usize * 4;
            assert_eq!(
                &result.data.as_slice()[at..at + 4],
                expected,
                "pixel ({x}, {y})"
            );
        }
    }
}
