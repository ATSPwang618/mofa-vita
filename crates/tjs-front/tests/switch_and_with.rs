use tjs_core::{Phase, RunBudget, SourceMap, Vm, VmExit};

fn check(script: &str, expected: &str) {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("switch and with", script).unwrap();
    let module =
        tjs_front::compile(&sources, source).unwrap_or_else(|error| panic!("{script}: {error}"));
    for slice in [1, 10_000] {
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&module);
        loop {
            assert!(vm.work_executed() < 100_000, "unexpected loop: {script}");
            let exit = vm.run_slice(&mut heap, RunBudget::new(slice).unwrap());
            heap.collect(vm.roots());
            match exit {
                VmExit::Yielded => {}
                VmExit::Finished(value) => {
                    assert_eq!(heap.display(value).unwrap(), expected, "{script}");
                    break;
                }
                error => panic!("{script}: {error:?}"),
            }
        }
    }
}

#[test]
fn switch_preserves_test_order_selector_snapshot_and_default_fallthrough() {
    for (script, expected) in [
        (
            "var n = 0; switch(2) { case 1: n = 1; break; case 2: n = 2; case 3: n += 3; } n;",
            "5",
        ),
        ("var n = 0; switch(4) { case 1: n = 1; } n;", "0"),
        (
            "var n = 0; switch(4) { default: n += 10; case 2: n += 2; } n;",
            "12",
        ),
        (
            "var n = 0; switch(2) { default: n += 10; case 2: n += 2; } n;",
            "2",
        ),
        (
            "var n = 0; switch(2) { case 2: n += 2; default: n += 10; case 3: n += 3; } n;",
            "15",
        ),
        (
            "var n = 0; switch(9) { default: n = 1; break; default: n = 2; } n;",
            "2",
        ),
        (
            "var n = 0; switch(1) { case 1: case 1: n++; break; default: n = 9; } n;",
            "1",
        ),
        ("switch(1) {} 7;", "7"),
        ("var n = 0; switch(1) { n++; break; n = 9; } n;", "1"),
        (
            "var n = 0; switch(2) { n += 10; case 1: n++; break; case 2: n += 2; } n;",
            "12",
        ),
        (
            r#"var n = 0; switch(2) { case "2": n = 7; break; default: n = 9; } n;"#,
            "7",
        ),
        (
            "var n = 0; var tests = 0; switch(2) { case ++tests: n = 1; break; default: n = 9; break; case ++tests: n = 2; case ++tests: n += 3; } tests * 10 + n;",
            "25",
        ),
        (
            "function f() { var x = 1; var result = 0; switch(x) { case (x = 2): result = 20; break; case 1: result = 10; } return result + x; } f();",
            "12",
        ),
        (
            "var calls = 0; function select() { calls++; return 2; } switch(select()) { case 2: break; } calls;",
            "1",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn each_case_has_its_own_scope_including_its_test_expression() {
    for (script, expected) in [
        (
            "var x = 7, n = 0; switch(1) { case 1: var x = 2; n += x; case 2: n += x; var x = 3; n += x; } n + x;",
            "19",
        ),
        (
            "var x = 2, n = 0; switch(2) { case 1: var x = 9; break; case x: n = x; } n;",
            "2",
        ),
        (
            "var x = 2, n = 0; switch(2) { var x = 9; n += x; case x: n += x; } n;",
            "11",
        ),
        (
            "var x = 8; switch(2) { case 1: var x = 1; break; case 2: var x = 2; x++; } x;",
            "8",
        ),
        (
            "function f() { var x = 4, n = 0; switch(1) { case 1: var x = 2; n += x; default: n += x; } return n + x; } f();",
            "10",
        ),
        (
            "var n = 0; switch(1) { case 1: var x = 2; switch(x) { case 2: var x = 7; n = x; break; } n += x; } n;",
            "9",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn nested_switches_loops_and_handlers_keep_break_and_continue_targets() {
    for (script, expected) in [
        (
            "var n = 0; for(var i = 0; i < 5; i++) { switch(i) { case 1: continue; case 3: break; default: n += i; } n += 10; } n;",
            "46",
        ),
        (
            "var i = 0, n = 0; while(i++ < 3) { switch(i) { case 2: continue; default: n += i; } n += 10; } n;",
            "24",
        ),
        (
            "var i = 0, n = 0; do { switch(++i) { case 2: continue; default: n += i; } n += 10; } while(i < 3); n;",
            "24",
        ),
        (
            "var n = 0; switch(1) { case 1: for(var i = 0; i < 5; i++) { if(i == 2) break; n++; } n += 10; break; default: n = 99; } n;",
            "12",
        ),
        (
            "var n = 0; switch(1) { case 1: switch(2) { case 2: n++; break; } n += 10; break; default: n = 99; } n;",
            "11",
        ),
        (
            "var n = 0; try { switch(1) { case 1: try { throw 3; } catch(e) { n = e; break; } } throw 4; } catch(e) { n += e; } n;",
            "7",
        ),
        (
            "function fail() { throw 7; } var n = 0; try { switch(1) { case fail(): n = 99; } } catch(e) { n = e; } n;",
            "7",
        ),
        (
            "function f(n) { switch(n) { case 1: return 3; default: return 7; } } f(1) + f(9);",
            "10",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn with_binds_only_dot_members_and_evaluates_its_object_once() {
    for (script, expected) in [
        ("var x = 9, d = %[x: 2]; with(d) { .x += x; } d.x;", "11"),
        ("var d = %[x: 2]; with(d) .x++; d.x;", "3"),
        ("var x = 9, d = %[x: 2]; with(d) var x = .x; global.x;", "9"),
        (
            "var x = 5, d = %[x: 2]; with(d) { var x = .x; .x += x; } d.x + x;",
            "9",
        ),
        (
            "var d = %[x: 1, child: %[x: 2]]; with(d) { with(.child) { .x += 3; } .x += 10; } d.x * 10 + d.child.x;",
            "115",
        ),
        (
            "function f() { var d = %[x: 1]; var saved = d; with(d) { d = %[x: 9]; .x = 3; } return saved.x * 10 + d.x; } f();",
            "39",
        ),
        (
            "var calls = 0; function make() { calls++; return %[x: 3]; } var n = 0; with(make()) { n += .x; n += .x; } calls * 100 + n;",
            "106",
        ),
        (
            "var d = %[]; with(d) { .class = 7; .switch = 3; .default = 2; } d.class + d.switch + d.default;",
            "12",
        ),
        ("var d = %[x: 1]; with(d) { delete .x; } d.x;", "void"),
        (
            "var x = 7; function f() { var x = 2; .x += 3; return .x + x; } f();",
            "12",
        ),
        ("with(null) { 7; }", "7"),
    ] {
        check(script, expected);
    }
}

#[test]
fn with_context_survives_calls_gc_and_abrupt_control_flow() {
    for (script, expected) in [
        (
            "function read() { return this.x; } var a = %[x: 4, read: read incontextof null]; var b = %[x: 9]; function f(a) { with(a) { return this.x * 10 + .read(); } } (f incontextof b)(a);",
            "94",
        ),
        (
            "var a = []; with(a) { .push(4); .push(5); .length++; } a.length * 10 + a[1];",
            "35",
        ),
        (
            "var n = 0; with(%[x: 7]) { var i = 0; while(i++ < 3) { with(%[x: 2]) { if(i == 2) continue; n += .x; break; } } n += .x; } n;",
            "9",
        ),
        (
            "var n = 0; with(%[x: 7]) { try { with(%[x: 2]) { throw .x; } } catch(e) { n = .x + e; } } n;",
            "9",
        ),
        (
            "var n = 0; with(%[x: 7]) { switch(.x) { case 7: with(%[x: 2]) { n = .x; break; } } n += .x; } n;",
            "9",
        ),
        (
            "function make() { return %[x: 8, child: %[x: 2]]; } var n = 0; with(make()) { with(.child) { n = .x; } n += .x; } n;",
            "10",
        ),
        (
            r#"function key() { return "match" + 7; } var n = 0; switch(key()) { case "no": break; case key(): n = 1; } n;"#,
            "1",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn misplaced_labels_and_control_transfers_report_source_errors() {
    for (script, phase) in [
        ("case 1: 2;", Phase::Compile),
        ("default: 2;", Phase::Compile),
        ("switch(1) { { case 1: 2; } }", Phase::Compile),
        ("switch(1) { if(1) case 1: 2; }", Phase::Compile),
        ("switch(1) { with(null) case 1: 2; }", Phase::Compile),
        ("switch(1) { case 1: continue; }", Phase::Compile),
        ("with(null) break;", Phase::Compile),
        ("switch(1) 2;", Phase::Parse),
        ("switch(1) { case 1 2; }", Phase::Parse),
        ("switch(1) { default 2; }", Phase::Parse),
        ("with(1) { .; }", Phase::Parse),
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("invalid", script).unwrap();
        let error = tjs_front::compile(&sources, source).unwrap_err();
        assert_eq!(error.phase, phase, "{script}: {error}");
        assert!(error.span.is_some());
    }
    for script in [
        format!("{};", "with(null) ".repeat(1000)),
        format!(
            "{};{}",
            "switch(1) { case 1: ".repeat(1000),
            "}".repeat(1000)
        ),
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("deep contexts", &script).unwrap();
        assert_eq!(
            tjs_front::compile(&sources, source).unwrap_err().phase,
            Phase::Parse
        );
    }
}
