use tjs_core::{Phase, RunBudget, SourceMap, Value, Vm, VmExit, VmLimits};
use tjs_front::compile;

fn evaluate(input: &str, slice: u32) -> Value {
    let mut sources = SourceMap::new();
    let id = sources.add_utf8("functions", input).unwrap();
    let module = compile(&sources, id).unwrap_or_else(|error| panic!("{input}: {error}"));
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    loop {
        assert!(vm.instructions_executed() < 100_000);
        match vm.run_slice(&mut heap, RunBudget::new(slice).unwrap()) {
            VmExit::Finished(value) => {
                assert_eq!(vm.call_depth(), 1);
                assert_eq!(vm.stack_value_count(), module.register_count() as usize);
                return value;
            }
            VmExit::Yielded => {}
            VmExit::Fault(error) => panic!("{input}: {error}"),
            VmExit::Thrown(exception) => panic!("unexpected script exception: {exception:?}"),
            VmExit::Inspecting(request) => panic!("unexpected inspection: {request:?}"),
            VmExit::Waiting(request) => panic!("unexpected native wait: {request:?}"),
            VmExit::CompileRequest(request) => panic!("unexpected compile request: {request:?}"),
        }
    }
}

#[test]
fn calls_recursion_returns_and_argument_windows_survive_slices() {
    for (input, expected) in [
        ("function add(a, b) { return a + b; } add(2, 3);", 5),
        (
            "function sub(a, b) { return a - b; } sub(10, sub(8, 3));",
            5,
        ),
        (
            "function fact(n) { if (n <= 1) return 1; return n * fact(n - 1); } fact(10);",
            3_628_800,
        ),
        (
            "function fib(n) { if (n < 2) return n; return fib(n - 1) + fib(n - 2); } fib(10);",
            55,
        ),
        (
            "function even(n) { if (n == 0) return 1; return odd(n - 1); } function odd(n) { if (n == 0) return 0; return even(n - 1); } even(20);",
            1,
        ),
        ("function f(x) { x = 99; return x; } var x = 3; f(x); x;", 3),
        (
            "function add(a, b) { return a + b; } var x = 0; add(x = 1, x = x + 1);",
            3,
        ),
        ("function f() { while (1) { return 7; } return 8; } f();", 7),
        (
            "function f(x) { if (x) return 11; return 12; } f(0) + f(1);",
            23,
        ),
        ("function f(a) { return a; } f(4, 5, 6);", 4),
        ("f(); function f() { return 4; } f();", 4),
        (
            "function square(x) { return x * x; } var i = 0; var sum = 0; while (i < 10) { sum = sum + square(i); i = i + 1; } sum;",
            285,
        ),
    ] {
        for slice in [1, 3, 100_000] {
            assert_eq!(
                evaluate(input, slice).as_integer(),
                Some(expected),
                "{input}, slice={slice}"
            );
        }
    }
    for input in [
        "function f(a) { return a; } f();",
        "function f() { return; } f();",
        "function f() { 42; } f();",
    ] {
        assert!(matches!(evaluate(input, 1), Value::Void));
    }
}

#[test]
fn raw_arguments_are_preserved_after_parameters_change() {
    let mut sources = SourceMap::new();
    let id = sources
        .add_utf8("arguments", "function f(a) { a = 99; return a; } f(3, 7);")
        .unwrap();
    let module = compile(&sources, id).unwrap();
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    let budget = RunBudget::new(1).unwrap();
    let mut observed = false;
    loop {
        match vm.run_slice(&mut heap, budget) {
            VmExit::Yielded => {
                if vm.call_depth() == 2 && vm.registers()[1].as_integer() == Some(99) {
                    assert_eq!(
                        vm.original_arguments()
                            .iter()
                            .map(|value| value.as_integer())
                            .collect::<Vec<_>>(),
                        [Some(3), Some(7)]
                    );
                    observed = true;
                }
            }
            VmExit::Finished(value) => {
                assert_eq!(value.as_integer(), Some(99));
                break;
            }
            VmExit::Fault(error) => panic!("{error}"),
            VmExit::Thrown(exception) => panic!("unexpected script exception: {exception:?}"),
            VmExit::Inspecting(request) => panic!("unexpected inspection: {request:?}"),
            VmExit::Waiting(request) => panic!("unexpected native wait: {request:?}"),
            VmExit::CompileRequest(request) => panic!("unexpected compile request: {request:?}"),
        }
    }
    assert!(observed);
}

#[test]
fn local_operand_aliasing_matches_reference_codegen_paths() {
    let pack = "function pack(a,b) { return a * 10 + b; }";
    for (body, expected) in [
        ("var x = 1; return pack(x, x = 7);", 77),
        ("var x = 1; return pack(x + 0, x = 7);", 17),
        ("var x = 1; var y = 0; return pack(y = x, x = 7);", 77),
        ("var x = 1; if (x == (x = 7)) return 9; return 0;", 9),
        ("var x = 1; if (!(x == (x = 7))) return 9; return 0;", 0),
        (
            "var x = 1; var comparison = x == (x = 7); return comparison;",
            0,
        ),
    ] {
        let script = format!("{pack} function probe() {{ {body} }} probe();");
        assert_eq!(evaluate(&script, 1).as_integer(), Some(expected), "{body}");
    }
    // Top-level property reads capture a value, unlike local register operands.
    assert_eq!(
        evaluate(&format!("{pack} var x = 1; pack(x, x = 7);"), 1).as_integer(),
        Some(17)
    );
}

#[test]
fn deep_recursion_uses_the_explicit_stack_and_reset_discards_frames() {
    let mut sources = SourceMap::new();
    let id = sources
        .add_utf8(
            "deep",
            "function f(n) { if (n == 0) return 0; return f(n - 1); } f(3000);",
        )
        .unwrap();
    let module = compile(&sources, id).unwrap();
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::with_limits(
        &module,
        VmLimits {
            max_call_depth: 4_000,
            max_stack_values: 100_000,
        },
    )
    .unwrap();
    let mut deepest = 0;
    loop {
        match vm.run_slice(&mut heap, RunBudget::new(7).unwrap()) {
            VmExit::Yielded => deepest = deepest.max(vm.call_depth()),
            VmExit::Finished(value) => {
                assert_eq!(value.as_integer(), Some(0));
                break;
            }
            VmExit::Fault(error) => panic!("{error}"),
            VmExit::Thrown(exception) => panic!("unexpected script exception: {exception:?}"),
            VmExit::Inspecting(request) => panic!("unexpected inspection: {request:?}"),
            VmExit::Waiting(request) => panic!("unexpected native wait: {request:?}"),
            VmExit::CompileRequest(request) => panic!("unexpected compile request: {request:?}"),
        }
    }
    assert!(deepest >= 3_000);
    vm.reset();
    while vm.call_depth() < 10 {
        assert!(matches!(
            vm.run_slice(&mut heap, RunBudget::new(1).unwrap()),
            VmExit::Yielded
        ));
    }
    vm.reset();
    assert_eq!(vm.call_depth(), 1);
    assert_eq!(vm.stack_value_count(), module.register_count() as usize);
    assert_eq!(vm.pc(), 0);
}

#[test]
fn call_limits_fail_at_the_call_site_without_partial_frame_changes() {
    let mut sources = SourceMap::new();
    let id = sources
        .add_utf8("limit", "function f(n) { return f(n); } f(1);")
        .unwrap();
    let module = compile(&sources, id).unwrap();
    for (limits, message) in [
        (
            VmLimits {
                max_call_depth: 5,
                max_stack_values: 1_000,
            },
            "call depth",
        ),
        (
            VmLimits {
                max_call_depth: 100,
                max_stack_values: module.register_count() as usize,
            },
            "value stack",
        ),
    ] {
        let mut heap = tjs_core::Heap::new();
        let mut vm = Vm::with_limits(&module, limits).unwrap();
        let VmExit::Fault(error) = vm.run_slice(&mut heap, RunBudget::new(1_000).unwrap()) else {
            panic!("limit should stop recursion")
        };
        assert!(error.message.contains(message));
        assert!(error.span.is_some());
        assert!(vm.call_depth() <= limits.max_call_depth);
        assert!(vm.stack_value_count() <= limits.max_stack_values);
        let count = vm.instructions_executed();
        assert!(matches!(
            vm.run_slice(&mut heap, RunBudget::new(1).unwrap()),
            VmExit::Fault(_)
        ));
        assert_eq!(vm.instructions_executed(), count);
    }
    // Receiver conversion fails before creating a frame, even at the depth limit.
    let id = sources.add_utf8("void-call", "void.m(); 7;").unwrap();
    let module = compile(&sources, id).unwrap();
    let mut vm = Vm::with_limits(
        &module,
        VmLimits {
            max_call_depth: 1,
            max_stack_values: module.register_count() as usize,
        },
    )
    .unwrap();
    let mut heap = tjs_core::Heap::new();
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(100).unwrap()),
        VmExit::Fault(error) if error.message.contains("requires an object")
    ));
}

#[test]
fn invalid_function_declarations_report_parse_errors() {
    for input in [
        "function f(a,) {}",
        "function f() return 1;",
        "var f = function named() {};",
    ] {
        let mut sources = SourceMap::new();
        let id = sources.add_utf8("unsupported", input).unwrap();
        assert_eq!(
            compile(&sources, id).unwrap_err().phase,
            Phase::Parse,
            "{input}"
        );
    }
}
