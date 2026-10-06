use tjs_core::{Phase, RunBudget, SourceMap, Value, Vm, VmExit};
use tjs_front::compile;

fn evaluate(input: &str, slice: u32) -> Value {
    let mut sources = SourceMap::new();
    let id = sources.add_utf8("control-flow", input).unwrap();
    let module = compile(&sources, id).unwrap_or_else(|error| panic!("{input}: {error}"));
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    loop {
        assert!(
            vm.instructions_executed() < 100_000,
            "unexpected infinite loop: {input}"
        );
        match vm.run_slice(&mut heap, RunBudget::new(slice).unwrap()) {
            VmExit::Finished(value) => return value,
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
fn branches_loops_and_scopes_survive_every_instruction_boundary() {
    for (input, expected) in [
        ("if (0) 10; else 20;", 20),
        ("if (1) if (0) 10; else 20;", 20),
        ("if (0) { 10; } else if (1) { 20; } else { 30; }", 20),
        ("var a = 1; { var a = 2; a = 9; } a;", 1),
        ("var a = 1; { a = 2; } a;", 2),
        (
            "var i = 0; var sum = 0; while (i < 100) { i = i + 1; sum = sum + i; } sum;",
            5050,
        ),
        ("var i = 0; while (i < 3) i = i + 1; i;", 3),
        (
            "var i = 0; var sum = 0; while (i < 10) { i = i + 1; if (i == 3) continue; if (i == 7) break; sum = sum + i; } sum;",
            18,
        ),
        (
            "var i = 0; var n = 0; while (i < 3) { i = i + 1; var j = 0; while (j < 5) { j = j + 1; if (j == 2) continue; if (j == 4) break; n = n + 1; } } n;",
            6,
        ),
        (
            "var i = 0; var n = 0; while (i < 3) { var x = 10; n = n + x; i = i + 1; } n;",
            30,
        ),
        ("var i = 0; while ((i = i + 1) < 3) {} i;", 3),
        ("var a = 1; if (a == (a = 2)) a = 9; a;", 2),
        ("while (1) { break; 1 + 1; } 4;", 4),
    ] {
        for slice in [1, 2, 7, 10_000] {
            assert_eq!(
                evaluate(input, slice).as_integer(),
                Some(expected),
                "{input}, slice={slice}"
            );
        }
    }
    assert!(matches!(evaluate("if (0) 1;", 1), Value::Void));
    assert!(matches!(evaluate("while (0) 1;", 1), Value::Void));
}

#[test]
fn comparison_precedence_and_large_integer_precision() {
    for (input, expected) in [
        ("1 + 2 < 4;", 1),
        ("1 == 2 < 3;", 1),
        ("!1 == 0;", 1),
        ("!!3;", 1),
        ("!-1;", 0),
        ("1 != 2;", 1),
        ("1 >= 1;", 1),
        ("1 <= 0;", 0),
        ("2 > 1;", 1),
        ("2 < 2;", 0),
        ("1 == 2;", 0),
        ("9007199254740992 == 9007199254740993;", 0),
        ("9223372036854775806 < 9223372036854775807;", 1),
    ] {
        assert_eq!(evaluate(input, 1).as_integer(), Some(expected), "{input}");
    }
}

#[test]
fn invalid_scopes_and_control_statements_have_source_errors() {
    for (input, phase) in [
        ("break;", Phase::Compile),
        ("continue;", Phase::Compile),
        ("else 1;", Phase::Parse),
        ("while 1 {}", Phase::Parse),
        ("if (1) { 2;", Phase::Parse),
    ] {
        let mut sources = SourceMap::new();
        let id = sources.add_utf8("invalid", input).unwrap();
        let error = compile(&sources, id).unwrap_err();
        assert_eq!(error.phase, phase, "{input}");
        assert!(error.span.is_some());
    }
}

#[test]
fn infinite_loop_yields_and_reset_restarts_it() {
    let mut sources = SourceMap::new();
    let id = sources.add_utf8("loop", "while (1) { continue; }").unwrap();
    let module = compile(&sources, id).unwrap();
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    for expected in [17, 34, 51] {
        assert!(matches!(
            vm.run_slice(&mut heap, RunBudget::new(17).unwrap()),
            VmExit::Yielded
        ));
        assert_eq!(vm.instructions_executed(), expected);
    }
    vm.reset();
    assert_eq!(vm.pc(), 0);
    assert_eq!(vm.instructions_executed(), 0);
}

#[test]
fn block_locals_reuse_registers_and_deep_blocks_report_an_error() {
    let mut sources = SourceMap::new();
    let short = sources.add_utf8("short", "{ var x = 1; x; }").unwrap();
    let long = sources
        .add_utf8("long", &"{ var x = 1; x; }".repeat(100))
        .unwrap();
    assert_eq!(
        compile(&sources, short).unwrap().register_count(),
        compile(&sources, long).unwrap().register_count()
    );
    let deep = sources
        .add_utf8(
            "deep",
            &format!("{}1;{}", "{".repeat(1000), "}".repeat(1000)),
        )
        .unwrap();
    assert_eq!(compile(&sources, deep).unwrap_err().phase, Phase::Parse);
}

#[test]
fn loop_results_match_a_host_model_over_many_trip_counts() {
    for count in 0_i64..=40 {
        let script = format!(
            "var i = 0; var sum = 0; while (i < {count}) {{ i = i + 1; sum = sum + i * i; }} sum;"
        );
        let expected: i64 = (1..=count).map(|value| value * value).sum();
        assert_eq!(evaluate(&script, 3).as_integer(), Some(expected));
    }
}
