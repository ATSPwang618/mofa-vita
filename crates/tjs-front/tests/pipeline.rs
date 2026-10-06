use tjs_core::{Phase, RunBudget, SourceMap, Value, Vm, VmExit};
use tjs_front::{
    ast::{BinaryOp, ExprKind, Statement},
    compile, lexer, parser,
};

fn evaluate(input: &str, slice: u32) -> Value {
    let mut sources = SourceMap::new();
    let id = sources.add_utf8("test", input).unwrap();
    let module = compile(&sources, id).unwrap();
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    loop {
        match vm.run_slice(&mut heap, RunBudget::new(slice).unwrap()) {
            VmExit::Finished(value) => {
                assert_eq!(
                    vm.instructions_executed() as usize,
                    module.instructions().len()
                );
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
fn arithmetic_assignment_and_slicing_agree() {
    for (input, expected) in [
        ("1 + 2 * 3;", 7),
        ("(1 + 2) * 3;", 9),
        ("8 - 3 - 2;", 3),
        ("-2 * (3 + 4);", -14),
        ("var a = 2; a = a * 3; a;", 6),
        ("var a = 1; var b = a + 2; a + b;", 4),
        ("var a = 1; a + (a = 7);", 8),
        ("var a = 1; var b = 2; a = b = 9; a + b;", 18),
        ("var a = 1; a = (a = 3) + a; a;", 6),
        ("/* 外 /* 内 */ 外 */ 7;", 7),
        ("/// 文档\nvar a = 3; a;", 3),
    ] {
        for slice in [1, 2, 1_000] {
            assert_eq!(
                evaluate(input, slice).as_integer(),
                Some(expected),
                "{input}, slice={slice}"
            );
        }
    }
    assert!(matches!(evaluate("", 1), Value::Void));
    assert!(matches!(evaluate("var a = 3;", 1), Value::Void));
}

#[test]
fn ast_distinguishes_precedence_and_associativity() {
    let mut sources = SourceMap::new();
    let id = sources.add_utf8("test", "8 - 3 - 2; 1 + 2 * 3;").unwrap();
    let tree = parser::parse(&lexer::lex(&sources, id).unwrap()).unwrap();
    let Statement::Expression(first) = tree.statements()[0] else {
        panic!("expression expected")
    };
    let ExprKind::Binary {
        op: BinaryOp::Subtract,
        lhs,
        ..
    } = tree.expression(first).kind
    else {
        panic!("subtraction expected")
    };
    assert!(matches!(
        tree.expression(lhs).kind,
        ExprKind::Binary {
            op: BinaryOp::Subtract,
            ..
        }
    ));
    let Statement::Expression(second) = tree.statements()[1] else {
        panic!("expression expected")
    };
    let ExprKind::Binary {
        op: BinaryOp::Add,
        rhs,
        ..
    } = tree.expression(second).kind
    else {
        panic!("addition expected")
    };
    assert!(matches!(
        tree.expression(rhs).kind,
        ExprKind::Binary {
            op: BinaryOp::Multiply,
            ..
        }
    ));
}

#[test]
fn failures_keep_their_phase_and_source_location() {
    for (input, phase) in [
        ("1 + ;", Phase::Parse),
        ("(1 + 2;", Phase::Parse),
        ("1 = 2;", Phase::Parse),
        ("function f() { break; }", Phase::Compile),
        ("class {}", Phase::Parse),
    ] {
        let mut sources = SourceMap::new();
        let id = sources.add_utf8("test", input).unwrap();
        let error = compile(&sources, id).unwrap_err();
        assert_eq!(error.phase, phase, "{input}");
        assert!(sources.slice(error.span.unwrap()).is_some());
    }
    let mut sources = SourceMap::new();
    let id = sources
        .add_utf8("arithmetic-error", "/*中文*/ 1 % 0;")
        .unwrap();
    let module = compile(&sources, id).unwrap();
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    let VmExit::Fault(error) = vm.run_slice(&mut heap, RunBudget::new(1_000).unwrap()) else {
        panic!("division by zero must fail")
    };
    assert_eq!(error.phase, Phase::Runtime);
    let span = error.span.unwrap();
    assert_eq!(
        String::from_utf16(sources.slice(span).unwrap()).unwrap(),
        "1 % 0"
    );
    let executed = vm.instructions_executed();
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(1).unwrap()),
        VmExit::Fault(_)
    ));
    assert_eq!(vm.instructions_executed(), executed);
}

#[test]
fn adversarial_depth_is_rejected_without_recursive_ast_drop() {
    for input in [
        format!("{}1{};", "(".repeat(1_000), ")".repeat(1_000)),
        format!("1{};", "+1".repeat(1_000)),
    ] {
        let mut sources = SourceMap::new();
        let id = sources.add_utf8("deep", &input).unwrap();
        let error = compile(&sources, id).unwrap_err();
        assert_eq!(error.phase, Phase::Parse);
        assert!(error.message.contains("limit"));
    }
}

#[test]
fn generated_expression_family_matches_independent_integer_model() {
    for a in -5_i64..=5 {
        for b in -5_i64..=5 {
            for c in -3_i64..=3 {
                let input = format!("({a}) + ({b}) * ({c});");
                assert_eq!(evaluate(&input, 3).as_integer(), Some(a + b * c));
            }
        }
    }
}
