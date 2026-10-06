use super::*;
#[cfg(feature = "sgx-binaries")]
use crate::draw_source::{Key, fragment, fragment_source};
use crate::test_support::Context;
use std::cell::Cell;
type GetError = unsafe extern "system" fn() -> u32;
thread_local! {
    static UPLOADS: Cell<usize> = const { Cell::new(0) };
    static GET_ERROR: Cell<Option<GetError>> = const { Cell::new(None) };
    static REJECT_CODE: Cell<u32> = const { Cell::new(0) };
    static PENDING_ERROR: Cell<u32> = const { Cell::new(0) };
}

unsafe extern "C" fn reject(_: i32, _: *const u32, _: u32, _: *const c_void, _: i32) {
    UPLOADS.set(UPLOADS.get() + 1);
    PENDING_ERROR.set(REJECT_CODE.get());
}
unsafe extern "system" fn get_error() -> u32 {
    let error = PENDING_ERROR.replace(0);
    if error == 0 {
        unsafe { GET_ERROR.get().unwrap()() }
    } else {
        error
    }
}
fn hook(name: &str, address: *const c_void) -> *const c_void {
    if name == "glGetError" {
        GET_ERROR.set(Some(unsafe {
            std::mem::transmute::<*const c_void, GetError>(address)
        }));
        get_error as *const c_void
    } else {
        address
    }
}

#[test]
fn binary_errors_propagate_without_disabling_the_loader() {
    let context = Context::new();
    let gl = context.gl_with(hook);
    let loader = Loader {
        upload: Some(reject),
    };
    let shader = unsafe { gl.create_shader(glow::VERTEX_SHADER).unwrap() };
    UPLOADS.set(0);
    for (index, error) in [glow::OUT_OF_MEMORY, glow::INVALID_VALUE, glow::INVALID_ENUM]
        .into_iter()
        .enumerate()
    {
        REJECT_CODE.set(error);
        let result = loader.load_binary(&gl, shader, loader.upload.unwrap(), &[0; 4]);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains(&format!("{error:#x}"))
        );
        assert_eq!(UPLOADS.get(), index + 1);
        assert_eq!(unsafe { gl.get_error() }, glow::NO_ERROR);
    }
    REJECT_CODE.set(glow::NO_ERROR);
    assert!(
        loader
            .load_binary(&gl, shader, reject, &[0; 4])
            .unwrap_err()
            .to_string()
            .contains("rejected")
    );
    unsafe { gl.delete_shader(shader) };
}

#[test]
fn binary_upload_preserves_earlier_errors_and_accepts_a_compiled_shader() {
    let context = Context::new();
    let gl = context.gl_with(hook);
    let loader = Loader {
        upload: Some(reject),
    };
    let shader = unsafe { gl.create_shader(glow::VERTEX_SHADER).unwrap() };
    UPLOADS.set(0);
    unsafe { gl.enable(u32::MAX) };
    let error = loader
        .load_binary(&gl, shader, reject, &[0; 4])
        .unwrap_err();
    assert!(error.to_string().contains("before shader binary"));
    assert_eq!(UPLOADS.get(), 0);
    unsafe {
        gl.shader_source(shader, "void main() { gl_Position = vec4(0.0); }");
        gl.compile_shader(shader);
    }
    REJECT_CODE.set(glow::NO_ERROR);
    loader.load_binary(&gl, shader, reject, &[0; 4]).unwrap();
    assert_eq!(UPLOADS.get(), 1);
    unsafe { gl.delete_shader(shader) };
}

#[test]
#[cfg(feature = "sgx-binaries")]
fn catalog_is_bounded_unique_and_requires_exact_source_and_stage() {
    let bytes: usize = BINARIES.iter().map(|(_, _, b)| b.len()).sum();
    assert!(bytes > 1024 && bytes < 2 * 1024 * 1024);
    for sampling in [
        crate::draw_source::Sampling::Nearest,
        crate::draw_source::Sampling::Logical,
    ] {
        let source = fragment_source(&fragment(Key {
            sampling,
            covered: true,
            ..Key::raw()
        }));
        assert!(
            BINARIES
                .iter()
                .any(|(kind, text, _)| *kind == glow::FRAGMENT_SHADER && *text == source)
        );
    }
    for layers in 2..=4 {
        for face in [0, 1, 4] {
            for constant in [false, true] {
                for clipped in [false, true] {
                    let source = fragment_source(&crate::scene_batch_source::fragment(
                        crate::scene_batch_source::Key {
                            layers,
                            face,
                            constant,
                            clipped,
                        },
                    ));
                    assert!(BINARIES.iter().any(|(kind, text, _)| {
                        *kind == glow::FRAGMENT_SHADER && *text == source
                    }));
                }
            }
        }
    }
    // First use of a transition must not invoke the multi-second SGX compiler.
    for face in [0, 1, 4] {
        for mode in [13, 14, 15, 16, 17, 18, 19] {
            let source = fragment_source(&fragment(Key {
                kind: crate::draw_source::Kind::Blend,
                sampling: crate::draw_source::Sampling::Logical,
                mode,
                face,
                clear: false,
                constant_backdrop: false,
                covered: false,
            }));
            assert!(
                BINARIES
                    .iter()
                    .any(|(kind, text, _)| *kind == glow::FRAGMENT_SHADER && *text == source)
            );
        }
        for rule in [false, true] {
            for direct in [false, true] {
                let source =
                    fragment_source(&crate::transition_source::fragment(rule, face, direct));
                assert!(
                    BINARIES
                        .iter()
                        .any(|(kind, text, _)| *kind == glow::FRAGMENT_SHADER && *text == source)
                );
            }
        }
    }
    for mode in 0..=4 {
        let custom = fragment_source(&crate::transition_source::custom(mode));
        assert!(
            BINARIES
                .iter()
                .any(|(kind, text, _)| *kind == glow::FRAGMENT_SHADER && *text == custom)
        );
    }
    for (i, (kind, source, binary)) in BINARIES.iter().enumerate() {
        assert!(
            !BINARIES[..i]
                .iter()
                .any(|(k, s, _)| k == kind && s == source)
        );
        assert!(binary.len() > 32);
    }
    let context = Context::new();
    let gl = context.gl();
    let loader = Loader {
        upload: Some(reject),
    };
    let source = fragment_source(&fragment(Key::raw()));
    let shader = unsafe { gl.create_shader(glow::FRAGMENT_SHADER).unwrap() };
    UPLOADS.set(0);
    assert!(
        !loader
            .load(&gl, shader, glow::VERTEX_SHADER, &source)
            .unwrap()
    );
    assert!(
        !loader
            .load(&gl, shader, glow::FRAGMENT_SHADER, &(source + "\n"))
            .unwrap()
    );
    assert_eq!(UPLOADS.get(), 0);
    // Errors from earlier drawing must not be mistaken for binary rejection.
    unsafe { gl.enable(u32::MAX) };
    assert!(
        loader
            .load(
                &gl,
                shader,
                glow::FRAGMENT_SHADER,
                &fragment_source(&fragment(Key::raw()))
            )
            .is_err()
    );
    assert_eq!(UPLOADS.get(), 0);
    unsafe { gl.delete_shader(shader) };
}
