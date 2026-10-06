use tjs_core::{Phase, RunBudget, SourceMap, Vm, VmExit};

fn check(script: &str, expected: &str) {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("loops and updates", script).unwrap();
    let module =
        tjs_front::compile(&sources, source).unwrap_or_else(|error| panic!("{script}: {error}"));
    for slice in [1, 10000] {
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&module);
        loop {
            assert!(
                vm.instructions_executed() < 100_000,
                "unexpected infinite loop: {script}"
            );
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
fn for_and_do_preserve_scope_continue_targets_and_nested_exception_flow() {
    for (script, expected) in [
        (
            "var n = 0; for(var i = 0; i < 10; i++) { if(i == 3) continue; if(i == 7) break; n += i; } n;",
            "18",
        ),
        (
            "var i = 99; var n = 0; for(var i = 0; i < 3; ++i) n += i; n + i;",
            "102",
        ),
        ("var n = 0; for(;;) { ++n; if(n == 4) break; } n;", "4"),
        ("var n = 0; for(; n < 4;) n++; n;", "4"),
        ("var i = 9; for(i = 0; i < 4; i += 1) {} i;", "4"),
        ("var i = 0; for(; 0; i++) ++i; i;", "0"),
        ("var n = 0; do { ++n; } while(0); n;", "1"),
        (
            "var n = 0; var tests = 0; do { ++n; continue; } while(++tests < 3); n * 10 + tests;",
            "33",
        ),
        (
            "var n = 0; for(var i = 0; i < 3; i++) { var j = 0; do { if(++j == 2) continue; n++; } while(j < 3); } n;",
            "6",
        ),
        (
            "function f() { var n = 0; for(var i = 0; i < 4; i++) { try { if(i == 1) throw i; n += i; } catch(e) { continue; } } return n; } f();",
            "5",
        ),
        (
            "function f() { for(var i = 0; i < 10; i++) { if(i == 4) return i; } return 99; } f();",
            "4",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn updates_preserve_values_and_evaluate_rhs_receiver_and_key_once() {
    for (script, expected) in [
        (
            "var x = 2; var a = x++; var b = ++x; x *= 3; x -= 1; a * 100 + b * 10 + x;",
            "251",
        ),
        (
            "var x = 2; var a = x--; var b = --x; a * 100 + b * 10 + x;",
            "200",
        ),
        ("var x = 1; var y = 2; x += y *= 3; x * 10 + y;", "76"),
        ("var x = 1; x += (x = 3); x;", "6"),
        (
            "function f() { var x = 1; return ++x + (x = 4); } f();",
            "6",
        ),
        (
            "function pair(a, b) { return a * 10 + b; } function f() { var x = 1; return pair(++x, x = 4); } f();",
            "44",
        ),
        (
            "function pair(a, b) { return a * 10 + b; } function f() { var x = 1; return pair(x++, x = 4); } f();",
            "14",
        ),
        ("var a = [4]; var i = 0; a[i++] += 3; a[0] * 10 + i;", "71"),
        (
            "var a = []; var old = a.count++; var now = ++a.length; old * 100 + now * 10 + a.count;",
            "22",
        ),
        (
            "var order = 0; var d = %[x: 4]; function rhs() { order = order * 10 + 1; return 3; } function receiver() { order = order * 10 + 2; return d; } function key() { order = order * 10 + 3; return \"x\"; } receiver()[key()] += rhs(); order * 10 + d.x;",
            "1237",
        ),
        ("var d = %[text: \"a\"]; d.text += \"b\"; d.text;", "ab"),
        ("var x = 3; x = x++; x;", "3"),
    ] {
        check(script, expected);
    }
}

#[test]
fn malformed_loops_and_nonassignable_updates_are_parse_errors() {
    for script in [
        "for(var i = 0 i < 3; i++) {}",
        "do {} while(1)",
        "for(;;",
        "1++;",
        "++(1 + 2);",
        "f() += 2;",
        "var x = 1; x++++;",
        "a--b;",
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("invalid update", script).unwrap();
        assert_eq!(
            tjs_front::compile(&sources, source).unwrap_err().phase,
            Phase::Parse,
            "{script}"
        );
    }
    for script in [
        format!("{};", "for(;;)".repeat(1000)),
        format!("{};{}", "do ".repeat(1000), "while(1);".repeat(1000)),
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("deep loop", &script).unwrap();
        assert_eq!(
            tjs_front::compile(&sources, source).unwrap_err().phase,
            Phase::Parse
        );
    }
}
