#![cfg(target_os = "linux")]

#[path = "../src/resample_source.rs"]
mod resample_source;

use pvr_compiler::{Compiler, Stage};

#[test]
fn direct_rgba_resampling_compiles_for_sgx543() {
    let mut compiler = Compiler::new().unwrap();
    for taps in resample_source::TAPS {
        let source = format!(
            "#version 100\nprecision highp float;\nprecision highp int;\n{}",
            resample_source::fragment(taps)
        );
        let result = compiler.compile_binary(Stage::Fragment, &source).unwrap();
        assert!(result.success, "taps={taps}: {}", result.log);
        assert!(result.binary.len() > 32);
    }
}

#[test]
fn quad_vertex_and_fragment_shaders_compile_for_sgx543() {
    let mut compiler = Compiler::new().unwrap();
    let vertex = compiler
        .compile_binary(Stage::Vertex, include_str!("../src/quad.vert"))
        .unwrap();
    assert!(vertex.success, "{}", vertex.log);
    assert!(vertex.binary.len() > 32);
    for fragment in [
        include_str!("../src/glyph.frag"),
        include_str!("../src/video.frag"),
    ] {
        let source =
            format!("#version 100\nprecision highp float;\nprecision highp int;\n{fragment}");
        let result = compiler.compile_binary(Stage::Fragment, &source).unwrap();
        assert!(result.success, "{}", result.log);
        assert!(result.binary.len() > 32);
    }
}
