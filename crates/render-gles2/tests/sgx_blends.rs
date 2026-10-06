#![cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "../src/draw_source.rs"]
mod draw_source;
use draw_source::{Key, Kind, Sampling};
use pvr_compiler::{Compiler, Stage};

#[test]
fn dodge5_compiles_with_logical_sampling_and_backdrops_for_sgx543() {
    compile_blend(22);
}

#[test]
fn multiply_compiles_with_logical_sampling_and_backdrops_for_sgx543() {
    compile_blend(16);
}

fn compile_blend(mode: u8) {
    let mut compiler = Compiler::new().unwrap();
    for sampling in [
        Sampling::Nearest,
        Sampling::Logical,
        Sampling::Affine,
        Sampling::Linear,
        Sampling::LogicalAffine,
        Sampling::LogicalLinear,
    ] {
        for face in [0, 1, 4] {
            for constant_backdrop in [false, true] {
                // Coverage specialization only applies to overwriting raw draws;
                // a blend reads its destination and retains fragment rejection.
                let key = Key {
                    kind: Kind::Blend,
                    sampling,
                    mode,
                    face,
                    constant_backdrop,
                    ..Key::raw()
                };
                let fragment = draw_source::fragment_source(&draw_source::fragment(key));
                let result = compiler.compile_binary(Stage::Fragment, &fragment).unwrap();
                assert!(result.success, "{key:?}: {}", result.log);
            }
        }
    }
}
