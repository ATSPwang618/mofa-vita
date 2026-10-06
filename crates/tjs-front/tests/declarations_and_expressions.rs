use tjs_core::{Phase, RunBudget, SourceMap, Vm, VmExit};

fn compile(script: &str) -> Result<tjs_core::Module, tjs_core::Diagnostic> {
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8("declarations and expressions", script)
        .unwrap();
    tjs_front::compile(&sources, source)
}

fn check(script: &str, expected: &str) {
    let module = compile(script).unwrap_or_else(|error| panic!("{script}: {error}"));
    for slice in [1, 10000] {
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&module);
        loop {
            assert!(vm.work_executed() < 100_000, "{script}");
            let exit = vm.run_slice(&mut heap, RunBudget::new(slice).unwrap());
            heap.collect(vm.roots());
            match exit {
                VmExit::Yielded => {}
                VmExit::Finished(value) => {
                    assert_eq!(heap.display(value).unwrap(), expected, "{script}");
                    break;
                }
                exit => panic!("{script}: {exit:?}"),
            }
        }
    }
}

#[test]
fn declarations_initialize_in_order_redeclare_and_keep_const_mutable_like_tjs() {
    for (script, expected) in [
        ("var a; a;", "void"),
        ("var a = 2, b = a + 3, c; b;", "5"),
        ("const a = 1, b = 2; a += b; a;", "3"),
        ("var a = 1, a = a + 2; var a = a * 3; a;", "9"),
        ("var a = 1; var a; a;", "void"),
        (
            "function f() { var a = 2, b = a + 1; var a = a + b; return a; } f();",
            "5",
        ),
        ("function f() { var a = 2; var a; return a; } f();", "void"),
        ("var a = 9; { var a = a + 1, b = a; a = b + 1; } a;", "9"),
        (
            "var i = 99, sum = 0; for(var i = 0, j = 4; i < j; ++i, --j) sum += i + j; sum + i;",
            "107",
        ),
        (
            "var x = 1; var a = (x = 2, x + 1), b = 7; a * 10 + b;",
            "37",
        ),
        ("var d = %[\"true\" => 7]; d.true + true + false;", "8"),
    ] {
        check(script, expected);
    }
    let short = compile("function f() { var a = 1; var a = a + 1; return a; } f();").unwrap();
    let long = compile(&format!(
        "function f() {{ var a = 1; {} return a; }} f();",
        "var a = a + 1;".repeat(300)
    ))
    .unwrap();
    assert_eq!(
        short.functions()[1].register_count(),
        long.functions()[1].register_count()
    );
}

#[test]
fn comma_preserves_side_effect_order_local_addresses_and_argument_separators() {
    for (script, expected) in [
        ("1, 2, 3;", "3"),
        ("var a = 0, b = 0; a = 1, b = a + 1; a * 10 + b;", "12"),
        ("var a = [(1, 2), 3,]; a.count * 10 + a[0];", "32"),
        (
            "var d = %[(1, \"a\") => (2, 7), b: 8]; d.a * 10 + d.b;",
            "78",
        ),
        (
            "function f(a = (1, 2), b = 3) { return a * 10 + b; } f();",
            "23",
        ),
        (
            "function pair(a, b) { return a * 10 + b; } function f() { var x = 1; return pair((0, x), x = 4); } f();",
            "44",
        ),
        (
            "var d = %[x: 2]; var n = 0; (n++, d.x) += 3; (n++, d.x)++; n * 10 + d.x;",
            "26",
        ),
        ("var x = 1, n = 0; (n++, x) = 8; n * 10 + x;", "18"),
        (
            "function f() { var x = 1; if((0, x == (x = 2))) return 7; return 8; } f();",
            "7",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn logical_operators_short_circuit_and_return_boolean_integers() {
    for (script, expected) in [
        ("true && 7;", "1"),
        ("false || 7;", "1"),
        ("0 || 0;", "0"),
        ("7 && 0;", "0"),
        ("null || %[x:1];", "1"),
        ("var n = 0; 1 || ++n; 0 && ++n; n;", "0"),
        ("var n = 0; (0 || ++n) && ++n; n;", "2"),
        ("true || false && false;", "1"),
        ("1 == 1 && 3 > 2 || 0;", "1"),
        ("function bad() { throw 8; } true || bad();", "1"),
        (
            "function bad() { throw 8; } var n = 0; try { false || bad(); } catch(e) { n = e; } n;",
            "8",
        ),
        (
            "function f() { var x = 1; return (x == (x = 2)) && 1; } f();",
            "1",
        ),
        ("5 + (0 || 7) * 10 + (1 && 4);", "16"),
    ] {
        check(script, expected);
    }
}

#[test]
fn conditional_selects_one_branch_and_preserves_value_and_condition_contexts() {
    for (script, expected) in [
        (
            "var n = 0; var x = true ? ++n : (n = 99); x * 10 + n;",
            "11",
        ),
        ("false ? missing() : \"kept\";", "kept"),
        ("true ? false ? 1 : 2 : 3;", "2"),
        ("false ? 1 : true ? 2 : 3;", "2"),
        ("0 || 1 ? 7 : 8;", "7"),
        ("100 + (true ? 2 : 3) * (false ? 4 : 5);", "110"),
        ("var a = true ? %[name: \"kept\"] : null; a.name;", "kept"),
        ("var d = %[a: true ? 7 : 8, b: 2]; d.a + d.b;", "9"),
        (
            "function f(a = true ? 2 : 3, b = 4) { return a * 10 + b; } f();",
            "24",
        ),
        (
            "function f() { var x = 1; return true ? x == (x = 2) : 0; } f();",
            "0",
        ),
        (
            "function f() { var x = 1; if(true ? x == (x = 2) : 0) return 7; return 8; } f();",
            "7",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn invalid_declarations_and_conditional_arms_report_parse_errors() {
    for script in [
        "var ;",
        "var x,;",
        "var x = ;",
        "true ? 1;",
        "true ? : 2;",
        "true ? x = 1 : 2;",
        "var x = 0; (x, 1) = 2;",
    ] {
        assert_eq!(compile(script).unwrap_err().phase, Phase::Parse, "{script}");
    }
    for script in [
        format!("{}1;", "true ? 1 : ".repeat(1000)),
        format!("{}1;", "1 && ".repeat(1000)),
        format!("{}1;", "1, ".repeat(1000)),
    ] {
        assert_eq!(compile(&script).unwrap_err().phase, Phase::Parse);
    }
}

#[test]
fn reserved_access_words_reject_bindings_but_remain_valid_member_names() {
    for word in ["goto", "private", "protected", "public"] {
        for script in [format!("var {word}=1;"), format!("function f({word}) {{}}")] {
            assert_eq!(compile(&script).unwrap_err().phase, Phase::Lex, "{script}");
        }
        check(&format!("var d=%['{word}'=>7]; d.{word};"), "7");
    }
    // Both numeric readers consume the octal prefix 0 and leave the 8. The
    // parser then rejects adjacent number tokens; numeric coercion returns 0.
    assert_eq!(compile("08;").unwrap_err().phase, Phase::Parse);
    check("int '08';", "0");
}
