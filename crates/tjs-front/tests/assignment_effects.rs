use tjs_core::{Diagnostic, Module, Phase, RunBudget, SourceMap, Vm, VmExit};

fn compile(script: &str) -> Result<Module, Diagnostic> {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("assignment effects", script).unwrap();
    tjs_front::compile(&sources, source)
}

fn check(script: &str, expected: &str) {
    let module = compile(script).unwrap_or_else(|error| panic!("{script}: {error}"));
    for slice in [1, 10_000] {
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&module);
        loop {
            let exit = vm.run_slice(&mut heap, RunBudget::new(slice).unwrap());
            heap.collect(vm.roots());
            match exit {
                VmExit::Finished(value) => {
                    assert_eq!(heap.display(value).unwrap(), expected, "{script}");
                    break;
                }
                VmExit::Yielded => assert!(vm.work_executed() < 100_000, "{script}"),
                exit => panic!("{script}: {exit:?}"),
            }
        }
    }
}

#[test]
fn logical_assignments_evaluate_rhs_first_and_always_store_a_boolean() {
    for (script, expected) in [
        ("var x = 0; x &&= '1e100'; x;", "0"),
        ("var x = 1; x ||= '1e100'; x;", "1"),
        (
            "var calls = 0, a = 0, b = 2; function rhs() { calls++; return 3; } a &&= rhs(); b ||= rhs(); calls * 100 + a * 10 + b;",
            "201",
        ),
        ("var a = 3, b = 0; a &&= b ||= 7; a * 10 + b;", "11"),
        (
            "var a = '0.5', b = '2', c = %[], d = null; a ||= b; b &&= a; c &&= 0.5; d ||= void; a * 1000 + b * 100 + c * 10 + d;",
            "1110",
        ),
        (
            "function f() { var x = 0; var n = (x ||= 3) + (x = 8); return n; } f();",
            "9",
        ),
        (
            "function pair(a,b) { return a * 10 + b; } function f() { var x = 0; return pair(x ||= 3, x = 8); } f();",
            "88",
        ),
        (
            "function pair(a,b) { return a * 10 + b; } var x = 0; pair(x ||= 3, x = 8);",
            "18",
        ),
        ("var x = 0; var n = (x ||= 3) + (x = 8); n;", "9"),
        (
            "var log = '', n = 0; property p { getter { log += 'g'; return n; } setter(v) { log += 's'; n = v; } } function rhs() { log += 'r'; return 7; } p &&= rhs(); p ||= rhs(); log + ':' + n;",
            "rgsrgs:1",
        ),
        (
            "var log = ''; var d = %[x: 1]; function rhs() { log += 'r'; return 0; } function target() { log += 't'; return d; } function key() { log += 'k'; return 'x'; } target()[key()] ||= rhs(); log + ':' + d.x;",
            "rtk:1",
        ),
        (
            "function f() { var x = 0; var a = [1]; a[(x = 1, 0)] &&= x; return a[0]; } f();",
            "1",
        ),
        (
            "property p { getter { return 2; } setter(v) { global.result = v; } } function f() { var ref = &p; *ref &&= 0; } var result; f(); result;",
            "0",
        ),
        ("var a = [1,2,3]; a.count &&= 4; a.length;", "1"),
    ] {
        check(script, expected);
    }
}

#[test]
fn swap_snapshots_the_left_value_and_reevaluates_targets_for_writes() {
    for (script, expected) in [
        ("var a = 1, b = 2; a <-> b; a * 10 + b;", "21"),
        (
            "function f() { var a = 1, b = 2; a <-> b; a <-> a; return a * 10 + b; } f();",
            "21",
        ),
        (
            "var log = ''; var d = %[x: 1, y: 2]; function target(k) { log += k; return d; } target('l').x <-> target('r').y; log + ':' + d.x + d.y;",
            "lrlr:21",
        ),
        (
            "var a = [1,2,3,4], i = 0; a[i++] <-> a[i++]; a[0] * 1000 + a[1] * 100 + a[2] * 10 + a[3] + i * 10000;",
            "41221",
        ),
        (
            "function f() { var d = [1,2,3], i = 0; d[i++] <-> i; return i * 100 + d[1]; } f();",
            "102",
        ),
        (
            "var log = '', x = 1, y = 2; property p { getter { log += 'p'; return x; } setter(v) { log += 'P'; x = v; } } property q { getter { log += 'q'; return y; } setter(v) { log += 'Q'; y = v; } } p <-> q; log + ':' + x + y;",
            "pqPQ:21",
        ),
        (
            "property p { getter { return 4; } } property q { getter { return 8; } } &p <-> &q; p * 10 + q;",
            "84",
        ),
        (
            "var x = 1, y = 2; property p { getter { return x; } setter(v) { x = v; } } function f() { var ref = &p; *ref <-> y; } f(); x * 10 + y;",
            "21",
        ),
        (
            "var a = [1,2], b = [3,4,5,6]; a.count <-> b.count; a.count * 10 + b.count;",
            "42",
        ),
        (
            "var d = %[x: 'retained', y: %[n: 9]]; d.x <-> d.y; d.x.n + ':' + d.y;",
            "9:retained",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn postfix_if_checks_condition_before_body_and_binds_below_comma() {
    for (script, expected) in [
        ("var x = 0; x = 7 if 0; x = 9 if 1; x;", "9"),
        ("var x = 0; x = 1, x = 2 if 0; x;", "0"),
        (
            "var log = ''; function body() { log += 'b'; } function cond() { log += 'c'; return 1; } body() if cond(); log;",
            "cb",
        ),
        (
            "function f() { var x = 1, n = 0; n = 7 if x == (x = 2); return n; } f();",
            "7",
        ),
        ("var x = 1, n = 0; n = 7 if x == (x = 2); n;", "0"),
        (
            "var n = 0; for(var i = 0; i < 5; i++, (n++ if i & 1)) {} n;",
            "3",
        ),
        ("var x = 0; if (1) x = 2 if 0; else x = 3; x;", "0"),
        ("var n = 0; (n = 7 if 1) if 1; n;", "7"),
        ("var a = 1, b = 2; a <-> b if 1; a * 10 + b;", "21"),
        (
            "var n = 0; property p { getter { n++; return 1; } setter(v) {} } p++ if 1; n;",
            "1",
        ),
        (
            "var n = 0; class C { function finalize() { global.n++; } } var c = new C(); invalidate c if 1; n;",
            "1",
        ),
        ("var x = 0; ((x = 4 if 1), 8);", "8"),
        ("var a = 1, b = 2; a <-> b;", "void"),
        ("var x = 0; x = 7 if 0;", "void"),
        ("var a = 1, b = 2; 1 ? (a <-> b) : 7;", "void"),
        ("var a = 1, b = 2; 0 ? (a <-> b) : 7;", "7"),
    ] {
        check(script, expected);
    }
}

#[test]
fn throws_interrupt_each_operation_without_rolling_back_earlier_writes() {
    for (script, expected) in [
        (
            "var read = 0, caught = 0; property p { getter { read++; return 0; } setter(v) {} } function rhs() { throw 7; } try { p &&= rhs(); } catch(e) { caught = e; } caught * 10 + read;",
            "70",
        ),
        (
            "var x = 1, caught = 0; property p { getter { return 2; } setter(v) { throw 7; } } try { x <-> p; } catch(e) { caught = e; } x * 10 + caught;",
            "27",
        ),
        (
            "var writes = 0; property p { getter { throw 7; } setter(v) { writes++; } } var x = 1; try { x <-> p; } catch(e) {} x * 10 + writes;",
            "10",
        ),
        (
            "var x = 0, caught = 0; function cond() { throw 8; } try { x = 7 if cond(); } catch(e) { caught = e; } x * 10 + caught;",
            "8",
        ),
        (
            "var x = 0; function body() { throw 9; } try { body() if 1; } catch(e) { x = e; } x;",
            "9",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn conditional_targets_apply_only_the_selected_read_or_write_branch() {
    for (script, expected) in [
        (
            "var a = 0, b = 2; (1 ? a : b) ||= 7; (0 ? a : b) &&= 0; a * 10 + b;",
            "10",
        ),
        (
            "var choice = 0, a = 1, b = 2; (choice ? a : b) = (choice = 1, 8); a * 10 + b;",
            "82",
        ),
        (
            "function f() { var a = 1, b = 2; var old = (1 ? a : b)++; var now = ++(0 ? a : b); return old * 100 + a * 10 + now; } f();",
            "123",
        ),
        (
            "function pair(a,b) { return a * 10 + b; } function f() { var a = 0, b = 0; return pair((1 ? a : b) ||= 1, a = 8); } f();",
            "18",
        ),
        (
            "var choice = 1, a = 3, b = 5, c = 7; (choice ? a : b) <-> (choice = 0, c); a * 100 + b * 10 + c;",
            "373",
        ),
        (
            "property p { getter { return 4; } } property q { getter { throw 1; } } function f() { var ref = &(1 ? p : q); return *ref; } f();",
            "4",
        ),
        (
            "property p { getter { return 4; } } property q { getter { throw 1; } } &(0 ? p : q) = &p; p * 10 + q;",
            "44",
        ),
        (
            "var stored = 0; property p { getter { return 8; } setter(v) { stored = v; } } property q { getter { throw 1; } setter(v) { throw 2; } } (1 ? p : q) &&= 1; stored;",
            "1",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn effect_only_operators_reject_value_contexts_and_invalid_targets() {
    for script in [
        "var a = 1, b = 2; var c = (a <-> b);",
        "var a = 1, b = 2; function f() { return a <-> b; }",
        "var a = 1, b = 2; function f(x) {} f(a <-> b);",
        "var a = 1, b = 2; if (a <-> b) {}",
        "var x = (7 if 1);",
        "function f() { return 7 if 1; }",
        "7 if 1 if 1;",
        "var a = 1, b = 2; var n = 1 ? (a <-> b) : 7;",
        "property p { getter { return 1; } } &p &&= 1;",
    ] {
        let error = compile(script).unwrap_err();
        assert_eq!(error.phase, Phase::Compile, "{script}: {error}");
        assert!(error.span.is_some());
    }
    for script in [
        "1 &&= 2;",
        "var x; x <-> 3;",
        "var x; 3 <-> x;",
        "var a, b, c; a <-> b <-> c;",
        "var x = 7 if 1;",
        "var x; x <-> ;",
    ] {
        assert_eq!(compile(script).unwrap_err().phase, Phase::Parse, "{script}");
    }
}
