use super::*;
use std::sync::Arc;
use tjs_core::{CompileRequest, ScriptSource};

fn caller(runtime: &mut Runtime) -> Vm {
    let source = runtime.sources.add_utf8("caller.tjs", "0;").unwrap();
    Vm::new(&tjs_front::compile(&runtime.sources, source).unwrap())
}
fn request(text: &str) -> CompileRequest {
    CompileRequest {
        source: ScriptSource::Text(Arc::from(text.encode_utf16().collect::<Vec<_>>())),
        output: None,
        expression: true,
        result_needed: true,
        name: "condition.tjs".into(),
        line_offset: 0,
    }
}
fn source(module: &Module) -> SourceId {
    module
        .functions()
        .iter()
        .find_map(|f| (0..f.instructions().len()).find_map(|i| f.span_at(i).map(|s| s.source())))
        .unwrap()
}

#[test]
fn reuse_preserves_diagnostic_names_offsets_and_removed_sources() {
    let mut runtime = Runtime::new();
    let vm = caller(&mut runtime);
    let mut request = request("null.x");
    let first = runtime.compile_request(&vm, &request).unwrap();
    let second = runtime.compile_request(&vm, &request).unwrap();
    let hit = runtime.compile_request(&vm, &request).unwrap();
    assert_ne!(
        source(&first),
        source(&second),
        "first occurrence is not retained"
    );
    assert_eq!(
        source(&second),
        source(&hit),
        "subsequent evaluation reuses compiled code"
    );
    drop((first, second, hit));
    for (name, line) in [("other.tjs", 0), ("other.tjs", 17), ("condition.tjs", -3)] {
        request.name = name.into();
        request.line_offset = line;
        drop(runtime.compile_request(&vm, &request).unwrap());
        let second = runtime.compile_request(&vm, &request).unwrap();
        let hit = runtime.compile_request(&vm, &request).unwrap();
        assert_eq!(source(&second), source(&hit));
        let file = runtime.sources.get(source(&hit)).unwrap();
        assert_eq!(file.name(), name);
        assert_eq!(file.line_offset(), line);
        runtime
            .sources
            .set_line_offset(source(&hit), line + 1)
            .unwrap();
        let fresh = runtime.compile_request(&vm, &request).unwrap();
        assert_ne!(source(&fresh), source(&hit));
        assert_eq!(
            runtime.sources.get(source(&fresh)).unwrap().line_offset(),
            line
        );
        runtime.sources.remove(source(&fresh));
        let restored = runtime.compile_request(&vm, &request).unwrap();
        assert_ne!(source(&restored), source(&fresh));
        assert_eq!(runtime.sources.get(source(&restored)).unwrap().name(), name);
    }
}

#[test]
fn eviction_reclaims_debug_sources_and_never_drops_code_owned_elsewhere() {
    let mut runtime = Runtime::new();
    runtime.set_compile_cache_limit(32 * 1024);
    let vm = caller(&mut runtime);
    let req = request("value + 1");
    drop(runtime.compile_request(&vm, &req).unwrap());
    let escaped = runtime.compile_request(&vm, &req).unwrap();
    let keep = source(&escaped);
    for i in 0..300 {
        let req = request(&format!("value + {}", i + 2));
        drop(runtime.compile_request(&vm, &req).unwrap());
        drop(runtime.compile_request(&vm, &req).unwrap());
        assert!(runtime.compile_cache_bytes() <= 32 * 1024);
    }
    runtime.collect([]);
    assert!(runtime.sources.get(keep).is_some());
    assert!(
        runtime.compiled_sources.len() < 128,
        "evicted source text must not accumulate"
    );
    runtime.set_compile_cache_limit(0);
    assert_eq!(runtime.compile_cache_bytes(), 0);
    assert_eq!(runtime.compiled_sources.len(), 1);
    drop(escaped);
    runtime.collect([]);
    assert!(runtime.compiled_sources.is_empty());
    assert!(runtime.sources.get(keep).is_none());
}
