use tjs_core::{Heap, HeapCounts, Phase, RunBudget, SourceMap, Value, Vm, VmExit};
use tjs_front::compile;

fn evaluate(script: &str, slice: u32) -> String {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("strings", script).unwrap();
    let module = compile(&sources, source).unwrap();
    let mut heap = Heap::new();
    let mut vm = Vm::new(&module);
    let value = loop {
        let exit = vm.run_slice(&mut heap, RunBudget::new(slice).unwrap());
        heap.collect(vm.roots()); // Include even the boundary after Return/Throw.
        match exit {
            VmExit::Yielded => {}
            VmExit::Finished(value) => break value,
            exit => panic!("{script}: {exit:?}"),
        }
    };
    let result = heap.display(value).unwrap();
    drop(vm);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
    result
}

#[test]
fn strings_defaults_forwarding_concatenation_and_catches_survive_gc_at_every_slice() {
    for (script, expected) in [
        (r#""你好😀";"#, "你好😀"),
        (r#"'a' 'b' + "c";"#, "abc"),
        (r#""answer=" + (6 * 7);"#, "answer=42"),
        (r#"7 + "!" + void;"#, "7!"),
        (
            r#"function join(a, b = "!") { return a + b; } join("hi");"#,
            "hi!",
        ),
        (
            r#"function sink(a) { return a; } function relay(a) { a = "changed"; return sink(...); } relay("ori" + "ginal");"#,
            "original",
        ),
        (
            r#"function f(n, message) { if (n == 0) throw message + "!"; return f(n - 1, message); } try { f(20, "catch"); } catch (e) { e; }"#,
            "catch!",
        ),
        (
            r#"var s = ""; var i = 0; while (i < 10) { s = s + i; i = i + 1; } s;"#,
            "0123456789",
        ),
        (r#"("same" == "sa" + "me") + ("a" != "b");"#, "2"),
        (r#"if (null) 1; else !null;"#, "1"),
        ("null == null;", "1"),
    ] {
        for slice in [1, 2, 7, 10_000] {
            assert_eq!(evaluate(script, slice), expected, "slice={slice}, {script}");
        }
    }
}

#[test]
fn literal_escapes_and_source_units_follow_tjs_rules() {
    assert_eq!(
        evaluate(r#""\a\b\f\n\r\t\v\\\"\'\q";"#, 1),
        "\x07\x08\x0c\n\r\t\x0b\\\"'q"
    );
    assert_eq!(evaluate(r#""\x41\X0042\0103";"#, 1), "ABC");
    assert_eq!(evaluate(r#""\x0041f";"#, 1), "Af");
    // TJS uses \x, not JavaScript's \u escape.
    assert_eq!(evaluate(r#""\u0041";"#, 1), "u0041");
    assert_eq!(evaluate(r#""\xD83D\xDE00";"#, 1), "😀");
    assert_eq!(evaluate(r#""\xD800";"#, 1), "\\u{D800}");
    assert_eq!(evaluate(r#""before\0after";"#, 1), "before");
    assert_eq!(evaluate(r#""\x0000";"#, 1), "");
    assert_eq!(evaluate("\"a\r\nb\rc\";", 1), "a\nb\nc");
    assert_eq!(evaluate("\"a\\\r\nb\";", 1), "a\nb");

    let mut sources = SourceMap::new();
    let source = sources.add_utf16("raw", vec![34, 0xdc00, 34, 59]).unwrap();
    let module = compile(&sources, source).unwrap();
    let mut vm = Vm::new(&module);
    let mut heap = Heap::new();
    let VmExit::Finished(Value::Str(id)) = vm.run_slice(&mut heap, RunBudget::new(100).unwrap())
    else {
        panic!("string result");
    };
    assert_eq!(heap.string(id).unwrap(), &[0xdc00]);
}

#[test]
fn shared_heap_collects_roots_from_multiple_vms_and_host_retained_results() {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("shared", r#""left" + "right";"#).unwrap();
    let module = compile(&sources, source).unwrap();
    let mut first = Vm::new(&module);
    let mut second = Vm::new(&module);
    let mut heap = Heap::new();
    let budget = RunBudget::new(100).unwrap();
    let VmExit::Finished(result) = first.run_slice(&mut heap, budget) else {
        panic!()
    };
    second.run_slice(&mut heap, RunBudget::new(2).unwrap());
    heap.collect(first.roots().chain(second.roots()));
    assert_eq!(heap.display(result).unwrap(), "leftright");
    let root = heap.root(result);
    drop(first);
    heap.collect(second.roots());
    assert_eq!(heap.display(result).unwrap(), "leftright");
    assert!(matches!(
        second.run_slice(&mut heap, budget),
        VmExit::Finished(_)
    ));
    second.reset();
    heap.collect(second.roots()); // Reset clears frames/results but retains cached literals.
    assert_eq!(
        heap.display(heap.rooted(root).unwrap()).unwrap(),
        "leftright"
    );
    heap.release_root(root);
    drop(second);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
    let Value::Str(id) = result else { panic!() };
    assert!(heap.string(id).is_err());
}

#[test]
fn uncaught_string_values_survive_unwinding_and_terminal_reentry() {
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8("throw", r#"function f() { throw "bad" + "!"; } f();"#)
        .unwrap();
    let module = compile(&sources, source).unwrap();
    let mut heap = Heap::new();
    let mut vm = Vm::new(&module);
    let budget = RunBudget::new(1).unwrap();
    let thrown = loop {
        let exit = vm.run_slice(&mut heap, budget);
        heap.collect(vm.roots());
        match exit {
            VmExit::Thrown(exception) => break exception,
            VmExit::Yielded => {}
            exit => panic!("{exit:?}"),
        }
    };
    assert_eq!(heap.display(thrown.value).unwrap(), "bad!");
    assert!(thrown.diagnostic.message.contains("bad!"));
    assert!(matches!(vm.run_slice(&mut heap, budget), VmExit::Thrown(_)));
}

#[test]
fn unsupported_conversions_and_invalid_literals_are_explicit() {
    for script in [r#""unfinished"#, "'unfinished\\", r#"'a' "b";"#] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("invalid", script).unwrap();
        let error = compile(&sources, source).unwrap_err();
        assert!(matches!(error.phase, Phase::Lex | Phase::Parse), "{script}");
    }
    for script in [r#"null - "b";"#, r#""a" < null;"#] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("unsupported", script).unwrap();
        let module = compile(&sources, source).unwrap();
        let mut vm = Vm::new(&module);
        let mut heap = Heap::new();
        assert!(matches!(
            vm.run_slice(&mut heap, RunBudget::new(100).unwrap()),
            VmExit::Fault(_)
        ));
    }
}
