use std::sync::Arc;

use tjs_core::{
    Constant, Function, FunctionId, FunctionKind, FunctionMember, Heap, HeapCounts, Instruction,
    Module, Phase, Register, RunBudget, Value, Vm, VmExit,
};

fn function(members: Vec<FunctionMember>) -> Function {
    Function::new(
        "entry",
        0,
        1,
        vec![
            Instruction::LoadFunction {
                dst: Register(0),
                function: FunctionId(0),
            },
            Instruction::Return { src: Register(0) },
        ],
        vec![None; 2],
        vec![],
    )
    .unwrap()
    .with_members(members)
}

#[test]
fn class_and_property_metadata_is_verified_before_execution() {
    let name = Constant::String(Arc::from("C".encode_utf16().collect::<Vec<_>>()));
    for kind in [
        FunctionKind::Class {
            constructor: 1,
            bases: vec![],
        },
        FunctionKind::Class {
            constructor: 0,
            bases: vec![FunctionId(4)],
        },
        FunctionKind::Class {
            constructor: 0,
            bases: vec![FunctionId(1)],
        },
        FunctionKind::Property {
            getter: Some(FunctionId(4)),
            setter: None,
        },
        FunctionKind::Property {
            getter: Some(FunctionId(1)),
            setter: None,
        },
    ] {
        assert!(
            Module::with_constants(
                vec![function(vec![]), function(vec![]).with_kind(kind)],
                vec![name.clone()]
            )
            .is_err()
        );
    }
    let invalid = Function::new(
        "ordinary",
        0,
        1,
        vec![
            Instruction::RegisterMembers,
            Instruction::LoadVoid { dst: Register(0) },
            Instruction::Return { src: Register(0) },
        ],
        vec![None; 3],
        vec![],
    )
    .unwrap();
    assert!(Module::from_functions(vec![invalid]).is_err());
}

#[test]
fn member_metadata_is_linked_and_cycles_are_loaded_and_collected_iteratively() {
    let name = Constant::String(Arc::from("child".encode_utf16().collect::<Vec<_>>()));
    for member in [
        FunctionMember {
            name: 0,
            function: FunctionId(1),
        },
        FunctionMember {
            name: 1,
            function: FunctionId(0),
        },
    ] {
        assert_eq!(
            Module::with_constants(vec![function(vec![member])], vec![name.clone()])
                .unwrap_err()
                .phase,
            Phase::Verify
        );
    }
    assert!(
        Module::with_constants(
            vec![function(vec![FunctionMember {
                name: 0,
                function: FunctionId(0)
            }])],
            vec![Constant::Octet(Box::new([]))]
        )
        .is_err()
    );

    let functions = (0..2000)
        .map(|id| {
            function(vec![FunctionMember {
                name: 0,
                function: FunctionId((id + 1) % 2000),
            }])
        })
        .collect();
    let module = Module::with_constants(functions, vec![name]).unwrap();
    let mut heap = Heap::new();
    let mut vm = Vm::new(&module);
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(1).unwrap()),
        VmExit::Yielded
    ));
    heap.collect(vm.roots());
    let VmExit::Finished(Value::Obj(value)) = vm.run_slice(&mut heap, RunBudget::new(1).unwrap())
    else {
        panic!("expected function")
    };
    let child = heap.intern(&"child".encode_utf16().collect::<Vec<_>>());
    let mut current = Value::Obj(value);
    for _ in 0..2000 {
        let Value::Obj(object) = current else {
            panic!("expected member function")
        };
        current = heap.member(object.object.unwrap(), child).unwrap().unwrap();
    }
    assert!(tjs_core::value::strict_equal(&heap, current, Value::Obj(value)).unwrap());
    drop(vm);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}
