use tjs_core::{Heap, HeapCounts, Module, Phase, RunBudget, SourceMap, Value, Vm, VmExit};
use tjs_front::{compile, lexer};

fn compile_script(script: &str) -> Module {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("interpolation", script).unwrap();
    compile(&sources, source).unwrap_or_else(|error| panic!("{script}: {error}"))
}

fn run(vm: &mut Vm, heap: &mut Heap, slice: u32) -> VmExit {
    loop {
        let exit = vm.run_slice(heap, RunBudget::new(slice).unwrap());
        heap.collect(vm.roots());
        if !matches!(exit, VmExit::Yielded) {
            return exit;
        }
        assert!(vm.work_executed() < 100_000);
    }
}

fn check(script: &str, expected: &str) {
    let module = compile_script(script);
    for slice in [1, 10_000] {
        let mut heap = Heap::new();
        let mut vm = Vm::new(&module);
        for _ in 0..2 {
            let VmExit::Finished(value) = run(&mut vm, &mut heap, slice) else {
                panic!("{script}")
            };
            assert_eq!(heap.display(value).unwrap(), expected, "{script}");
            vm.reset();
        }
        drop(vm);
        assert_eq!(heap.collect([]).after, HeapCounts::default());
    }
}

#[test]
fn both_interpolation_forms_convert_before_concatenating_and_preserve_precedence() {
    for (script, expected) in [
        (r#"var name = '世界'; @"你好，&name;";"#, "你好，世界"),
        (r#"@"sum=${1 + 2}";"#, "sum=3"),
        (r#"@"${1}${2}";"#, "12"),
        (r#"@"&1 + 2;";"#, "3"),
        (r#"@"&1, 2;";"#, "2"),
        (r#"@"${void}:${1.5}:${0}:${"str"}";"#, ":1.5:0:str"),
        (r#"@"${'value'}".length;"#, "5"),
        (r#"@"${1}" + 2 * 3;"#, "16"),
        (r#"@"".length;"#, "0"),
        (r#"@"${void}".length;"#, "0"),
        (r#"@ 'a&2;' 'b';"#, "a2b"),
        (r#"@"${(1 ? 2 : 3)}";"#, "2"),
    ] {
        check(script, expected);
    }
}

#[test]
fn text_segments_share_ordinary_escape_adjacency_and_nul_rules() {
    for (script, expected) in [
        (r#"@"\&name;\${name}";"#, "&name;${name}"),
        (r#"@"$ {name}";"#, "$ {name}"),
        (r#"@"\x0026n;\x24{x}";"#, "&n;${x}"),
        (r#"@"a\0hidden${7}b";"#, "a7b"),
        (r#"@"\0hidden${7}";"#, "7"),
        (r#"@"A" "B${2}" "C";"#, "AB2C"),
        (r#"@"\xD83D${$0xde00}";"#, "😀"),
        ("@\"a\r\nb\rc${4}\";", "a\nb\nc4"),
        (
            r#"@"slashes // /* remain text */";"#,
            "slashes // /* remain text */",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn nested_literals_functions_containers_and_comments_do_not_close_outer_holes() {
    for (script, expected) in [
        (r#"@"outer ${@"inner &2 + 3;"} end";"#, "outer inner 5 end"),
        (
            r#"@"${function() { var x = 1; return %[value: x + 1]; }().value}";"#,
            "2",
        ),
        (r#"@"&function() { return 3; }();tail";"#, "3tail"),
        (r#"@"${['}', ';'][1]}";"#, ";"),
        (r#"@"${<%7b 7d 3b%>[2]}";"#, "59"),
        (r#"@"${1 /* } ; ${ */ + 2}";"#, "3"),
        ("@\"&1 // ;\n + 2;\";", "3"),
        (r#"@"&@"${2}";";"#, "2"),
    ] {
        check(script, expected);
    }
}

#[test]
fn getters_side_effects_contexts_and_exceptions_follow_existing_vm_order() {
    for (script, expected) in [
        (
            r#"var n = 0; property p { getter { return ++n; } } @"${p}:${p}:${n}";"#,
            "1:2:2",
        ),
        (
            r#"function f() { var n = 1; return @"${n}:${n = 2}:${n}"; } f();"#,
            "1:2:2",
        ),
        (
            r#"class C { var n = 7; function text() { return @"n=${n}"; } } var c = new C(); c.text();"#,
            "n=7",
        ),
        (
            r#"function nested(n) { if(n) return @"${n}:${nested(n - 1)}"; return 'end'; } nested(3);"#,
            "3:2:1:end",
        ),
        (
            r#"var n = 0; function fail() { throw 'stop'; } try { @"${++n}:${fail()}:${++n}"; } catch(e) { @"${e}:${n}"; }"#,
            "stop:1",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn interpolation_diagnostics_point_into_original_utf16_source() {
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8("runtime span", r#"var 文 = @"前${9 \ 0}后";"#)
        .unwrap();
    let module = compile(&sources, source).unwrap();
    let VmExit::Fault(error) = run(&mut Vm::new(&module), &mut Heap::new(), 1) else {
        panic!()
    };
    assert_eq!(error.phase, Phase::Runtime);
    assert_eq!(
        String::from_utf16(sources.slice(error.span.unwrap()).unwrap()).unwrap(),
        "${9 \\ 0}"
    );
    let source = sources
        .add_utf8("syntax span", r#"@"前${1 + }後";"#)
        .unwrap();
    let error = compile(&sources, source).unwrap_err();
    assert_eq!(error.phase, Phase::Parse);
    assert_eq!(
        String::from_utf16(sources.slice(error.span.unwrap()).unwrap()).unwrap(),
        "}"
    );
    for script in [r#"@"${}";"#, r#"@"&;";"#] {
        let source = sources.add_utf8("empty expression", script).unwrap();
        assert_eq!(compile(&sources, source).unwrap_err().phase, Phase::Parse);
    }
    // Interpolation uses ordinary TJS conversion, including its errors.
    let module = compile_script(r#"@"${<%ff%>}";"#);
    assert!(matches!(
        run(&mut Vm::new(&module), &mut Heap::new(), 1),
        VmExit::Fault(_)
    ));
}

#[test]
fn lexer_modes_are_iterative_bounded_and_report_unterminated_input() {
    for input in [
        "@",
        "@  ",
        "@\"abc",
        "@\"&",
        "@\"&1",
        "@\"${",
        "@\"${function() {",
        "@\"${@\"nested",
        "@\"${1 /* comment",
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("incomplete", input).unwrap();
        let error = lexer::lex(&sources, source).unwrap_err();
        assert!(sources.slice(error.span.unwrap()).is_some());
    }
    let mut sources = SourceMap::new();
    let script = format!("{}1{}", "@\"${".repeat(1000), "}\"".repeat(1000));
    let source = sources.add_utf8("nested", &script).unwrap();
    assert!(lexer::lex(&sources, source).is_ok());
    assert_eq!(compile(&sources, source).unwrap_err().phase, Phase::Parse);
    let source = sources
        .add_utf8("expanded tokens", &"@\"${1}\"".repeat(20_000))
        .unwrap();
    let error = lexer::lex(&sources, source).unwrap_err();
    assert!(error.message.contains("token limit"));
}

#[test]
fn exported_functions_keep_interpolation_constants_after_the_creator_is_dropped() {
    let mut heap = Heap::new();
    let compiled = compile_script(r#"function text(n) { return @"值=${n}"; } text;"#);
    let mut creator = Vm::new(&compiled);
    let VmExit::Finished(function) = run(&mut creator, &mut heap, 1) else {
        panic!()
    };
    let root = heap.root(function);
    drop(creator);
    drop(compiled);
    heap.collect([]);
    let global = heap.alloc_global();
    let name = heap.intern(&"text".encode_utf16().collect::<Vec<_>>());
    heap.set_member(global, name, function).unwrap();
    heap.release_root(root);
    let compiled = compile_script("text(42);");
    let mut caller = Vm::with_global(&compiled, global);
    let VmExit::Finished(value @ Value::Str(_)) = run(&mut caller, &mut heap, 1) else {
        panic!()
    };
    assert_eq!(heap.display(value).unwrap(), "值=42");
    drop(caller);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}
