use tjs_core::{Phase, RunBudget, SourceMap, Value, Vm, VmExit};
use tjs_front::compile;

fn evaluate(script: &str, slice: u32) -> Value {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("parameters", script).unwrap();
    let module = compile(&sources, source).unwrap_or_else(|error| panic!("{script}: {error}"));
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    loop {
        assert!(vm.instructions_executed() < 10_000);
        match vm.run_slice(&mut heap, RunBudget::new(slice).unwrap()) {
            VmExit::Finished(value) => return value,
            VmExit::Yielded => {}
            VmExit::Fault(error) => panic!("{script}: {error}"),
            VmExit::Thrown(exception) => panic!("unexpected script exception: {exception:?}"),
            VmExit::Inspecting(request) => panic!("unexpected inspection: {request:?}"),
            VmExit::Waiting(request) => panic!("unexpected native wait: {request:?}"),
            VmExit::CompileRequest(request) => panic!("unexpected compile request: {request:?}"),
        }
    }
}

#[test]
fn defaults_distinguish_void_from_zero_and_run_in_parameter_order() {
    for (script, expected) in [
        ("function f(a = 7) { return a; } f();", 7),
        ("function f(a = 7) { return a; } f(void);", 7),
        ("function f(a = 7) { return a; } f(0);", 0),
        (
            "function f(a = 1, b = a + 2, c = b + 3) { return a * 100 + b * 10 + c; } f();",
            136,
        ),
        (
            "function f(a, b = (a = a + 1)) { return a * 10 + b; } f(1);",
            22,
        ),
        (
            "function f(a, b = (a = a + 1)) { return a * 10 + b; } f(1, 9);",
            19,
        ),
        (
            "function inner() { return 7; } function f(a, b = (a = a + 1) + inner()) { return a * 100 + b; } f(1);",
            209,
        ),
        (
            "function f(a = 9223372036854775807 + 1) { return a; } f(5);",
            5,
        ),
    ] {
        for slice in [1, 2, 13, 10_000] {
            assert_eq!(
                evaluate(script, slice).as_integer(),
                Some(expected),
                "{script}, slice={slice}"
            );
        }
    }
    assert!(matches!(
        evaluate("function f(a = a) { return a; } f();", 1),
        Value::Void
    ));
}

#[test]
fn forwarding_uses_original_arguments_even_after_defaults_and_assignment() {
    let functions = "function sink(a = 4, b = 5, c = 6) { return a * 100 + b * 10 + c; } function relay(a = 9) { a = 99; return sink(...); }";
    for (call, expected) in [
        ("relay()", 456),
        ("relay(1, 2, 3)", 123),
        ("relay(void, 2, 3)", 423),
        ("relay(1, , 3)", 153),
        ("relay(1, 2, )", 126),
        ("relay(, , )", 456),
    ] {
        for slice in [1, 7, 10_000] {
            assert_eq!(
                evaluate(&format!("{functions} {call};"), slice).as_integer(),
                Some(expected)
            );
        }
    }
    assert_eq!(evaluate("function sink(a = 4) { return a; } function relay(a = 9) { return sink(...); } function relay2(a = 8) { return relay(...); } relay2();", 1).as_integer(), Some(4));
    assert_eq!(
        evaluate("function f(a = 7) { return a; } f(...);", 1).as_integer(),
        Some(7)
    );
}

#[test]
fn empty_slots_and_extra_arguments_keep_their_count_and_positions() {
    for (call, expected) in [
        ("relay()", vec![]),
        ("relay(void)", vec![None]),
        ("relay(,)", vec![None, None]),
        ("relay(1,,3,)", vec![Some(1), None, Some(3), None]),
        ("relay(1,2,3,4)", vec![Some(1), Some(2), Some(3), Some(4)]),
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("raw arguments", &format!("function sink(a = 8) {{ return a; }} function relay(a = 9) {{ a = 99; return sink(...); }} {call};")).unwrap();
        let module = compile(&sources, source).unwrap();
        let mut heap = tjs_core::Heap::new();
        let mut vm = Vm::new(&module);
        let mut observed = false;
        loop {
            match vm.run_slice(&mut heap, RunBudget::new(1).unwrap()) {
                VmExit::Yielded => {
                    if vm.current_function() == "sink" {
                        assert_eq!(
                            vm.original_arguments()
                                .iter()
                                .map(|value| value.as_integer())
                                .collect::<Vec<_>>(),
                            expected,
                            "{call}"
                        );
                        observed = true;
                    }
                }
                VmExit::Finished(_) => break,
                VmExit::Fault(error) => panic!("{error}"),
                VmExit::Thrown(exception) => panic!("unexpected script exception: {exception:?}"),
                VmExit::Inspecting(request) => panic!("unexpected inspection: {request:?}"),
                VmExit::Waiting(request) => panic!("unexpected native wait: {request:?}"),
                VmExit::CompileRequest(request) => {
                    panic!("unexpected compile request: {request:?}")
                }
            }
        }
        assert!(observed);
    }
}

#[test]
fn unsupported_parameter_forms_and_forwarding_mixtures_report_errors() {
    for (script, phase) in [
        ("function f(a = ) {}", Phase::Parse),
        ("function f() {} f(..., 1);", Phase::Parse),
        ("function f() {} f(1, ...);", Phase::Parse),
        ("function f(a*, b) {}", Phase::Parse),
        ("function f(a* = []) {}", Phase::Parse),
        ("function f() {} f(a = []*);", Phase::Parse),
        ("function f() {} f(a + b*);", Phase::Parse),
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("invalid", script).unwrap();
        assert_eq!(
            compile(&sources, source).unwrap_err().phase,
            phase,
            "{script}"
        );
    }
}
