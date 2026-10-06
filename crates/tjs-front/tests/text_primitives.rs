use tjs_core::{Heap, HeapCounts, Module, RunBudget, SourceMap, Vm, VmExit};
use tjs_front::{compile, lexer};

fn module(script: &str) -> Module {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("text primitives", script).unwrap();
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
    let module = module(script);
    for slice in [1, 10_000] {
        let mut heap = Heap::new();
        let mut vm = Vm::new(&module);
        let exit = run(&mut vm, &mut heap, slice);
        let VmExit::Finished(value) = exit else {
            panic!("{script}: {exit:?}")
        };
        assert_eq!(heap.display(value).unwrap(), expected, "{script}");
        vm.reset();
        let VmExit::Finished(value) = run(&mut vm, &mut heap, slice) else {
            panic!("reset")
        };
        assert_eq!(heap.display(value).unwrap(), expected);
        drop(vm);
        assert_eq!(heap.collect([]).after, HeapCounts::default());
    }
}

#[test]
fn octets_preserve_reference_nibbles_comments_and_value_semantics() {
    for (script, expected) in [
        ("<% %>.length;", "0"),
        ("<% 1 2 3, 4, 56 %> === <%12,03,04,56%>;", "1"),
        ("<% a/* 12 /*34*/ 56 */b, // 78\n c %> == <%ab,0c%>;", "1"),
        ("<% a--b; xyz 1?f %> == <%ab 1f%>;", "1"),
        (
            "var a = <%00 ff%>, b = a; a += <%1%>; (b == <%00 ff%>) * 100 + a.length;",
            "103",
        ),
        (
            "function f(a = <%10 20%>) { function g(x) { return x + <%30%>; } return g(a); } f()[2];",
            "48",
        ),
        (
            "function f() { throw <%00 ff%> + <%7%>; } try { f(); } catch (e) { e[2]; }",
            "7",
        ),
        (
            "function g(x) { return x[1]; } function f(a) { a = <%%>; return g(...); } f(<%10 20%>);",
            "32",
        ),
        ("!<%%> + !!<%00%>;", "2"),
        ("typeof <%12%>;", "Octet"),
        ("<%12%> instanceof 'Octet';", "1"),
    ] {
        check(script, expected);
    }
}

#[test]
fn character_operators_convert_utf16_units_and_evaluate_operands_once() {
    for (script, expected) in [
        (r##"#"A" + #"" + #void;"##, "65"),
        ("#123;", "49"),
        ("$(65 + 1) + $'67';", "BC"),
        ("#$0;", "0"),
        ("#$65536;", "0"),
        ("#$-1;", "65535"),
        ("#$0xd800;", "55296"),
        ("#'😀';", "55357"),
        ("$0xd83d + $0xde00;", "😀"),
        (
            "var reads = 0; property p { getter { reads++; return 'Z'; } } var code = #p; code * 10 + reads;",
            "901",
        ),
        (
            "var n = 64; function next() { return ++n; } $next() + $next();",
            "AB",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn primitive_properties_use_reference_key_and_terminator_rules() {
    for (script, expected) in [
        ("'你😀'.length;", "3"),
        ("#'😀'[1];", "56832"),
        ("'abc'[3] == '';", "1"),
        ("''[0].length;", "0"),
        ("'abc'[1.9];", "b"),
        ("'abc'['1suffix'];", "b"),
        ("'abc'[4294967297];", "b"),
        ("<%00 fe ff%>['1.9'];", "254"),
        ("<%00 fe ff%>[2.9];", "255"),
        ("&'abc'.length;", "3"),
        ("typeof 'abc'[1.5];", "String"),
        ("typeof <%01%>[0.9];", "Integer"),
        ("typeof <%01%>.length;", "Integer"),
        (
            "class C { property p { getter { return '你好'; } } } var c = new C(); c.p[1];",
            "好",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn primitive_range_missing_and_write_errors_are_not_suppressed_by_typeof() {
    for (script, message) in [
        ("'ab'[-1];", "out of range"),
        ("'ab'[3];", "out of range"),
        ("<%%>[0];", "out of range"),
        ("<%01%>[1];", "out of range"),
        ("typeof 'ab'[3];", "out of range"),
        ("typeof 'ab'.unknown;", "does not exist"),
        ("typeof <%00%>['unknown'];", "does not exist"),
        ("'ab'['-1'];", "does not exist"),
        ("'ab'[' 1'];", "does not exist"),
        ("'ab'[void];", "require a string or number"),
        ("'ab'[0] = 'c';", "read-only"),
        // OperatePropertyDirect converts the receiver to an object closure
        // before it attempts a primitive property write.
        ("'ab'.length++;", "requires an object"),
        ("<%01%>[NaN] = 1;", "read-only"),
        ("&<%01%>.length = 0;", "read-only"),
        ("#<%01%>;", "not implemented"),
    ] {
        let module = module(script);
        let mut heap = Heap::new();
        let mut vm = Vm::new(&module);
        let VmExit::Fault(error) = run(&mut vm, &mut heap, 1) else {
            panic!("{script}")
        };
        assert!(error.message.contains(message), "{script}: {error}");
    }
}

#[test]
fn unicode_names_work_across_classes_functions_and_named_members() {
    check(
        "class 台詞 { var 内容 = '你好'; function 読む(番号) { return 内容[番号]; } } function 表示() { var 台本 = new 台詞(); return 台本.読む(1); } 表示();",
        "好",
    );
    check(
        "var Ā = 7, 😀 = 8; var 辞書 = %[名前: Ā + 😀]; 辞書.名前;",
        "15",
    );
    // The reference classifies UTF-16 code units, not Unicode scalar identifiers.
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf16(
            "surrogate name",
            vec![118, 97, 114, 32, 0xd800, 61, 55, 59, 0xd800, 59],
        )
        .unwrap();
    let module = compile(&sources, source).unwrap();
    let mut heap = Heap::new();
    let VmExit::Finished(value) = run(&mut Vm::new(&module), &mut heap, 1) else {
        panic!()
    };
    assert_eq!(value.as_integer(), Some(7));
    let source = sources
        .add_utf8("dollar operator", "var $name = 1;")
        .unwrap();
    assert!(compile(&sources, source).is_err());
}

#[test]
fn unfinished_octets_report_errors_at_original_source_spans() {
    for input in ["<%", "<%01", "<%/* unfinished", "<%01 // %>"] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("incomplete", input).unwrap();
        let error = lexer::lex(&sources, source).unwrap_err();
        assert!(sources.slice(error.span.unwrap()).is_some());
    }
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8("octet span", "<%01 /*comment*/ 02%> + 3;")
        .unwrap();
    let lexed = lexer::lex(&sources, source).unwrap();
    assert_eq!(lexed.octets(), &[vec![1, 2].into_boxed_slice()]);
    assert_eq!(
        String::from_utf16(sources.slice(lexed.tokens()[0].span).unwrap()).unwrap(),
        "<%01 /*comment*/ 02%>"
    );
    assert_eq!(
        lexed
            .trivia()
            .iter()
            .filter(|t| t.kind == lexer::TriviaKind::BlockComment)
            .count(),
        1
    );
}
