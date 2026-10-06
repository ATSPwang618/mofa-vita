use tjs_core::{Phase, RunBudget, SourceMap, Value, Vm, VmExit, VmLimits};
use tjs_front::compile;

fn evaluate(script: &str, slice: u32) -> Value {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("exceptions", script).unwrap();
    let module = compile(&sources, source).unwrap_or_else(|error| panic!("{script}: {error}"));
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    loop {
        assert!(vm.work_executed() < 100_000);
        match vm.run_slice(&mut heap, RunBudget::new(slice).unwrap()) {
            VmExit::Finished(value) => {
                assert_eq!(vm.call_depth(), 1);
                assert_eq!(vm.stack_value_count(), module.register_count() as usize);
                return value;
            }
            VmExit::Yielded => {}
            exit => panic!("{script}: {exit:?}"),
        }
    }
}

#[test]
fn catches_and_rethrows_survive_instruction_and_unwind_boundaries() {
    for (script, expected) in [
        ("try { throw 7; } catch (e) { e + 1; }", 8),
        ("try { 4; } catch (e) { 9; }", 4),
        ("try throw 7; catch (e) e;", 7),
        ("try { throw 7; } catch { 8; }", 8),
        ("try { throw 7; } catch () { 8; }", 8),
        (
            "try { try { throw 3; } catch (e) { throw e + 4; } } catch (e) { e * 2; }",
            14,
        ),
        ("var e = 1; try { throw 7; } catch (e) { e = 8; } e;", 1),
        (
            "function fail() { throw 7; } function middle() { return fail(); } try { middle(); } catch (e) { e + 1; }",
            8,
        ),
        (
            "function fail() { throw 7; } function f(a = fail()) { return a; } try { f(); } catch (e) { e; }",
            7,
        ),
        (
            "function f() { try { throw 8; } catch (e) { return e; } } f();",
            8,
        ),
        (
            "function f() { try { return 9; } catch (e) { return 3; } } f();",
            9,
        ),
        (
            "var i = 0; var sum = 0; while (i < 5) { i = i + 1; try { throw i; } catch (e) { sum = sum + e; continue; } } sum;",
            15,
        ),
        (
            "var i = 0; while (1) { try { break; } catch (e) { i = 9; } } i;",
            0,
        ),
        (
            "var x = 1; try { x = 2; throw x; } catch (e) { x = x + e; } x;",
            4,
        ),
        (
            "function fail() { throw 7; } function sink(a) { return a; } try { sink(fail()); } catch (e) { e; }",
            7,
        ),
        (
            "function bad() { throw 7; } function good() { return 4; } var n = 0; try { bad(); } catch (e) { n = e; } n + good();",
            11,
        ),
    ] {
        for slice in [1, 2, 7, 10_000] {
            assert_eq!(
                evaluate(script, slice).as_integer(),
                Some(expected),
                "{script}, slice={slice}"
            );
        }
    }
    assert!(matches!(
        evaluate("try { throw void; } catch (e) { e; }", 1),
        Value::Void
    ));
}

#[test]
fn deep_unwind_is_sliced_and_original_arguments_survive_in_the_catcher() {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("deep unwind", "function descend(n) { if (n == 0) throw 7; return descend(n - 1); } function catch_it(a) { try { descend(1500); } catch (e) { return a + e; } } catch_it(4);").unwrap();
    let module = compile(&sources, source).unwrap();
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::with_limits(
        &module,
        VmLimits {
            max_call_depth: 2_000,
            max_stack_values: 100_000,
        },
    )
    .unwrap();
    let budget = RunBudget::new(1).unwrap();
    while !vm.is_unwinding() {
        assert!(matches!(vm.run_slice(&mut heap, budget), VmExit::Yielded));
    }
    let instructions = vm.instructions_executed();
    let work = vm.work_executed();
    for _ in 0..100 {
        assert!(matches!(vm.run_slice(&mut heap, budget), VmExit::Yielded));
        assert!(vm.is_unwinding());
    }
    assert_eq!(vm.instructions_executed(), instructions);
    assert_eq!(vm.work_executed(), work + 100);
    while vm.is_unwinding() {
        assert!(matches!(vm.run_slice(&mut heap, budget), VmExit::Yielded));
    }
    assert_eq!(vm.current_function(), "catch_it");
    assert_eq!(vm.call_depth(), 2);
    assert_eq!(vm.original_arguments()[0].as_integer(), Some(4));
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(100).unwrap()),
        VmExit::Finished(Value::Int(11))
    ));
}

#[test]
fn uncaught_values_preserve_the_origin_trace_and_terminal_state() {
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8(
            "uncaught",
            "function leaf() { throw 42; } function middle() { return leaf(); } middle();",
        )
        .unwrap();
    let module = compile(&sources, source).unwrap();
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    let exception = loop {
        match vm.run_slice(&mut heap, RunBudget::new(1).unwrap()) {
            VmExit::Thrown(exception) => break exception,
            VmExit::Yielded => {}
            exit => panic!("expected uncaught value, got {exit:?}"),
        }
    };
    assert_eq!(exception.value.as_integer(), Some(42));
    assert_eq!(
        exception
            .diagnostic
            .trace
            .iter()
            .map(|frame| frame.function.as_str())
            .collect::<Vec<_>>(),
        ["leaf", "middle", "<script>"]
    );
    let spans: Vec<_> = exception
        .diagnostic
        .trace
        .iter()
        .map(|frame| String::from_utf16(sources.slice(frame.span.unwrap()).unwrap()).unwrap())
        .collect();
    assert_eq!(spans, ["throw 42;", "leaf()", "middle()"]);
    let work = vm.work_executed();
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(100).unwrap()),
        VmExit::Thrown(_)
    ));
    assert_eq!(vm.work_executed(), work);
}

#[test]
fn reset_cancels_a_pending_unwind_without_reusing_stale_handlers() {
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8(
            "reset",
            "function f() { throw 7; } try { f(); } catch (e) { e; }",
        )
        .unwrap();
    let module = compile(&sources, source).unwrap();
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    while !vm.is_unwinding() {
        assert!(matches!(
            vm.run_slice(&mut heap, RunBudget::new(1).unwrap()),
            VmExit::Yielded
        ));
    }
    vm.reset();
    assert!(!vm.is_unwinding());
    assert_eq!(vm.work_executed(), 0);
    assert_eq!(vm.call_depth(), 1);
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(100).unwrap()),
        VmExit::Finished(Value::Int(7))
    ));
}

#[test]
fn catch_binding_scope_and_try_syntax_are_checked() {
    for (script, phase) in [
        ("try {}", Phase::Parse),
        ("catch (e) {}", Phase::Parse),
        ("throw;", Phase::Parse),
        ("try {} catch (1) {}", Phase::Parse),
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

#[test]
fn runtime_failures_are_catchable_exceptions_and_uncaught_failures_keep_host_diagnostics() {
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8(
            "unsupported",
            "try { 1\\0; } catch (e) { e instanceof \"Exception\"; }",
        )
        .unwrap();
    let module = compile(&sources, source).unwrap();
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(100).unwrap()),
        VmExit::Finished(Value::Int(1))
    ));
    let source = sources.add_utf8("uncaught", "1\\0;").unwrap();
    let module = compile(&sources, source).unwrap();
    let mut vm = Vm::new(&module);
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(100).unwrap()),
        VmExit::Fault(_)
    ));
}
