use tjs_core::{
    ArgumentSource, CallArguments, CallSite, CallTarget, Function, FunctionId, Instruction, Module,
    Register,
};

#[test]
fn discarded_calls_do_not_initialize_a_destination_register() {
    for dst in [None, Some(Register(1)), Some(Register(2))] {
        let function = Function::new(
            "discard",
            0,
            2,
            vec![
                Instruction::LoadNull { dst: Register(0) },
                Instruction::Call { site: 0 },
                Instruction::Return { src: Register(1) },
            ],
            vec![None; 3],
            vec![CallSite {
                target: CallTarget::Value(Register(0)),
                dst,
                arguments: CallArguments::Registers {
                    start: Register(0),
                    count: 0,
                },
            }],
        );
        assert_eq!(function.is_ok(), dst == Some(Register(1)));
    }
}

#[test]
fn dynamic_call_operands_must_be_in_bounds_and_initialized_before_the_call() {
    let r = Register;
    for operand in [1, 2, 3] {
        for target in [
            CallTarget::Value(r(operand)),
            CallTarget::Construct(r(operand)),
            CallTarget::Name { key: r(operand) },
            CallTarget::Member {
                object: r(0),
                key: r(operand),
                computed: true,
            },
            CallTarget::Member {
                object: r(operand),
                key: r(0),
                computed: false,
            },
        ] {
            let result = Function::new(
                "caller",
                0,
                3,
                vec![
                    Instruction::LoadNull { dst: r(0) },
                    Instruction::LoadNull { dst: r(1) },
                    Instruction::Call { site: 0 },
                    Instruction::Return { src: r(2) },
                ],
                vec![None; 4],
                vec![CallSite {
                    target,
                    dst: Some(r(2)),
                    arguments: CallArguments::Registers {
                        start: r(0),
                        count: 1,
                    },
                }],
            );
            // r2 is also the destination: the call must not initialize its own input.
            assert_eq!(result.is_ok(), operand == 1, "{target:?}");
        }
    }
}

#[test]
fn function_constants_are_linked_and_context_operands_are_verified() {
    let r = Register;
    for callee in [0, 1] {
        let module = Module::new(
            1,
            vec![
                Instruction::LoadFunction {
                    dst: r(0),
                    function: FunctionId(callee),
                },
                Instruction::Return { src: r(0) },
            ],
            vec![None; 2],
        );
        assert_eq!(module.is_ok(), callee == 0);
    }
    for context in [0, 1, 2] {
        let module = Module::new(
            2,
            vec![
                Instruction::LoadNull { dst: r(0) },
                Instruction::BindContext {
                    dst: r(1),
                    object: r(0),
                    context: r(context),
                },
                Instruction::Return { src: r(1) },
            ],
            vec![None; 3],
        );
        assert_eq!(module.is_ok(), context == 0);
    }
}

#[test]
fn expanded_arguments_and_array_stores_validate_source_registers() {
    let r = Register;
    for operand in [1, 2, 3] {
        for source in [
            ArgumentSource::Value(r(operand)),
            ArgumentSource::Array(r(operand)),
        ] {
            let function = Function::new(
                "expanded",
                0,
                3,
                vec![
                    Instruction::NewArray { dst: r(1) },
                    Instruction::Call { site: 0 },
                    Instruction::Return { src: r(2) },
                ],
                vec![None; 3],
                vec![CallSite {
                    target: CallTarget::Direct(FunctionId(0)),
                    dst: Some(r(2)),
                    arguments: CallArguments::Expanded(vec![source].into_boxed_slice()),
                }],
            );
            assert_eq!(function.is_ok(), operand == 1);
        }
        let module = Module::new(
            3,
            vec![
                Instruction::NewArray { dst: r(0) },
                Instruction::LoadInt {
                    dst: r(1),
                    value: 7,
                },
                Instruction::ArrayPush {
                    array: r(0),
                    value: r(operand),
                },
                Instruction::Return { src: r(0) },
            ],
            vec![None; 4],
        );
        assert_eq!(module.is_ok(), operand == 1);
    }
}
