use tjs_core::{
    CallArguments, CallSite, CallTarget, CatchHandler, Function, FunctionId, Instruction, Register,
};

#[test]
fn throwing_calls_do_not_initialize_their_return_destination() {
    let make = |result| {
        Function::with_handlers(
            "caller",
            0,
            3,
            vec![
                Instruction::LoadInt {
                    dst: Register(0),
                    value: 7,
                },
                Instruction::Call { site: 0 },
                Instruction::Return { src: Register(1) },
                Instruction::Return {
                    src: Register(result),
                },
            ],
            vec![None; 4],
            vec![CallSite {
                target: CallTarget::Direct(FunctionId(0)),
                dst: Some(Register(1)),
                arguments: CallArguments::Registers {
                    start: Register(0),
                    count: 0,
                },
            }],
            vec![CatchHandler {
                start: 1,
                end: 2,
                target: 3,
                exception: Register(2),
            }],
        )
    };
    assert!(make(0).is_ok()); // Value initialized before the call survives.
    assert!(make(2).is_ok()); // The exceptional edge initializes the catch binding.
    assert!(make(1).unwrap_err().message.contains("not initialized"));
}

#[test]
fn handler_ranges_must_be_valid_and_properly_nested() {
    let outer = CatchHandler {
        start: 0,
        end: 3,
        target: 5,
        exception: Register(0),
    };
    let make = |handlers| {
        Function::with_handlers(
            "ranges",
            0,
            1,
            vec![
                Instruction::LoadVoid { dst: Register(0) },
                Instruction::Throw { src: Register(0) },
                Instruction::Jump { target: 5 },
                Instruction::Jump { target: 5 },
                Instruction::Jump { target: 5 },
                Instruction::Return { src: Register(0) },
            ],
            vec![None; 6],
            vec![],
            handlers,
        )
    };
    assert!(make(vec![outer]).is_ok());
    assert!(
        make(vec![
            outer,
            CatchHandler {
                start: 1,
                end: 2,
                target: 3,
                ..outer
            }
        ])
        .is_ok()
    );
    for handlers in [
        vec![CatchHandler { start: 3, ..outer }],
        vec![CatchHandler { target: 6, ..outer }],
        vec![CatchHandler { target: 2, ..outer }],
        vec![CatchHandler {
            exception: Register(1),
            ..outer
        }],
        vec![outer, outer],
        vec![
            outer,
            CatchHandler {
                start: 1,
                end: 4,
                ..outer
            },
        ],
    ] {
        assert!(make(handlers).is_err());
    }
}
