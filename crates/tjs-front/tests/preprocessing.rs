use tjs_core::{Heap, Phase, RunBudget, SourceMap, Value, Vm, VmExit};
use tjs_front::{Preprocessor, compile, compile_with_preprocessor, lexer};

fn compile_in(input: &str, definitions: &mut Preprocessor) -> tjs_core::Module {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("preprocessing", input).unwrap();
    compile_with_preprocessor(&sources, source, definitions)
        .unwrap_or_else(|e| panic!("{input}: {e}"))
}

#[test]
fn cache_revisions_follow_host_defaults_clones_and_compile_time_assignments() {
    let mut a = Preprocessor::default();
    let mut b = a.clone();
    let before = a.revision();
    assert_eq!(before, b.revision());
    a.set_default("version", 123);
    assert_eq!(a.revision(), before);
    a.set("flag", 7);
    let changed = a.revision();
    assert_ne!(changed, before);
    a.set("flag", 7);
    assert_eq!(a.revision(), changed);
    b.set("flag", 9);
    assert_ne!(b.revision(), a.revision());
    compile_in("@set(flag=8) @set(flag=7)", &mut a);
    assert_eq!(a.get("flag"), 7);
    assert_ne!(a.revision(), changed);
    let before = a.revision();
    compile_in("@if(flag==7) 3; @endif", &mut a);
    assert_eq!(a.revision(), before);
    assert_ne!(
        Preprocessor::default().revision(),
        Preprocessor::default().revision()
    );
}

#[test]
fn preprocessor_is_eager_i32_with_reference_precedence_and_persistent_symbols() {
    let mut definitions = Preprocessor::default();
    definitions.set("HOST", 3);
    compile_in(
        "@set(a = 1 | 2, b = (1 | 2), c = d = 7, e = 1 + f = 2) @set(0 && (side = 9)) @set(1 || (other = 8))",
        &mut definitions,
    );
    for (name, value) in [
        ("a", 1),
        ("b", 3),
        ("c", 7),
        ("d", 7),
        ("e", 3),
        ("f", 2),
        ("side", 9),
        ("other", 8),
        ("undefined", 0),
    ] {
        assert_eq!(definitions.get(name), value, "{name}");
    }
    compile_in(
        "@set(n = 0xffffffff, wrap = 2147483647 + 1, wrap2 = -(-2147483647 - 1), real = 7.9, mod = -7 % 3, div = -7 / 3, 中文 = HOST + 4)",
        &mut definitions,
    );
    for (name, value) in [
        ("n", -1),
        ("wrap", i32::MIN),
        ("wrap2", i32::MIN),
        ("real", 7),
        ("mod", -1),
        ("div", -2),
        ("中文", 7),
    ] {
        assert_eq!(definitions.get(name), value, "{name}");
    }
    compile_in(
        "@set(ok = (!missing && (2 * 3 + 1 == 7) && (4 >= 4) && (3 < 4) && (3 != 2)), bits = ((7 & 3) ^ 2))",
        &mut definitions,
    );
    assert_eq!(definitions.get("ok"), 1);
    assert_eq!(definitions.get("bits"), 1);
}

#[test]
fn skipped_branches_ignore_invalid_code_and_side_effects_and_compose_with_literals() {
    let script = r#"
        @set(enabled = 1)
        @if(missing)
            not valid TJS ?! ~~
            // @endif in a comment is ignored
            /* nested /* @endif */ comment */
            @set(enabled = 0)
            @if(1 / 0) invalid @endif
        @endif
        @if(enabled)
            function data() { return (const)%['items'=>(const)[2,3]]; }
            @if(1) var text = @"value=${@if(enabled) data().items[1] @endif}"; @endif
        @endif
        text;
    "#;
    let mut definitions = Preprocessor::default();
    let module = compile_in(script, &mut definitions);
    assert_eq!(definitions.get("enabled"), 1);
    let mut vm = Vm::new(&module);
    let mut heap = Heap::new();
    loop {
        let exit = vm.run_slice(&mut heap, RunBudget::new(1).unwrap());
        heap.collect(vm.roots());
        match exit {
            VmExit::Yielded => {}
            VmExit::Finished(value) => {
                assert_eq!(heap.display(value).unwrap(), "value=3");
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    // Disabled code follows the reference's raw scan, not string tokenization.
    let module = compile_in("@if(0) 'unterminated @endif 8;", &mut definitions);
    assert!(matches!(
        Vm::new(&module).run_slice(&mut Heap::new(), RunBudget::new(20).unwrap()),
        VmExit::Finished(Value::Int(8))
    ));
}

#[test]
fn diagnostics_keep_utf16_offsets_and_reject_non_reference_directives() {
    for input in [
        "@else",
        "@include('x')",
        "@endif",
        "@ set(x=1)",
        "@if(1)",
        "@if(0)",
        "@set",
        "@set(",
        "@set()",
        "@set(a=1<<2)",
        "@set(1/0)",
        "@set(1%0)",
        "@set((a)=2)",
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("invalid", input).unwrap();
        assert_eq!(
            compile(&sources, source).unwrap_err().phase,
            Phase::Lex,
            "{input}"
        );
    }
    let mut sources = SourceMap::new();
    let input = "/* 😀 */ @set(a=1/0)";
    let source = sources.add_utf8("position", input).unwrap();
    let error = compile(&sources, source).unwrap_err();
    assert_eq!(sources.slice(error.span.unwrap()).unwrap(), &[47]);
    let source = sources
        .add_utf8("trivia", "@if(0) ~invalid @endif /// docs\n42;")
        .unwrap();
    let tokens = lexer::lex(&sources, source).unwrap();
    assert_eq!(tokens.tokens()[0].kind, lexer::TokenKind::Integer(42));
    assert!(
        tokens
            .trivia()
            .iter()
            .any(|t| t.kind == lexer::TriviaKind::Disabled)
    );
    assert!(
        tokens
            .trivia()
            .iter()
            .any(|t| t.kind == lexer::TriviaKind::DocLine)
    );
    let deep = format!("@set({}1{})", "(".repeat(1000), ")".repeat(1000));
    let source = sources.add_utf8("deep", &deep).unwrap();
    assert!(
        compile(&sources, source)
            .unwrap_err()
            .message
            .contains("nesting limit")
    );
    let mut definitions = Preprocessor::default();
    let source = sources
        .add_utf8("effects before error", "@set(kept=8) var = ;")
        .unwrap();
    assert!(compile_with_preprocessor(&sources, source, &mut definitions).is_err());
    assert_eq!(definitions.get("kept"), 8);
}
