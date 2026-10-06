#![cfg(target_os = "linux")]

use pvr_compiler::{Compiler, Stage};
#[test]
fn twelve_nagano_variants_compile_for_sgx543() {
    let mut compiler = Compiler::new().unwrap();
    let mut failures = Vec::new();
    for mode in 0..12 {
        let source = format!(
            "#version 100\nprecision highp float;\nprecision highp int;\n#define NAGANO_MODE {mode}\n{}",
            include_str!("../src/nagano.frag")
        );
        let result = compiler.compile_binary(Stage::Fragment, &source).unwrap();
        if !result.success || result.binary.is_empty() {
            failures.push(format!("mode {mode}: {}", result.log));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
