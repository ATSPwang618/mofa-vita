use tjs_core::{
    Constant, Function, Heap, Instruction, Module, Register, RunBudget, Value, Vm, VmExit,
};

fn entry(constant: u32) -> Function {
    Function::new(
        "entry",
        0,
        1,
        vec![
            Instruction::LoadConstant {
                dst: Register(0),
                constant,
            },
            Instruction::Return { src: Register(0) },
        ],
        vec![None; 2],
        vec![],
    )
    .unwrap()
}

#[test]
fn constants_are_validated_before_loading_and_remain_portable_between_heaps() {
    assert!(
        Module::from_functions(vec![entry(0)])
            .unwrap_err()
            .message
            .contains("constant")
    );
    assert!(Module::with_constants(vec![entry(1)], vec![Constant::Octet(vec![1].into())]).is_err());
    let module =
        Module::with_constants(vec![entry(0)], vec![Constant::Octet(vec![0, 255].into())]).unwrap();
    let mut heap = Heap::new();
    let mut vm = Vm::new(&module);
    let budget = RunBudget::new(10).unwrap();
    let VmExit::Finished(Value::Octet(first)) = vm.run_slice(&mut heap, budget) else {
        panic!()
    };
    vm.reset();
    heap.collect(vm.roots());
    let VmExit::Finished(Value::Octet(second)) = vm.run_slice(&mut heap, budget) else {
        panic!()
    };
    assert_eq!(first, second);
    assert_eq!(heap.octet(second).unwrap(), &[0, 255]);
    assert_eq!(heap.counts().octets, 1);
    drop(vm);
    heap.collect([]);
    assert!(heap.octet(first).is_err());

    // A compiled module contains bytes, so another heap loads its own runtime ID.
    let mut other_heap = Heap::new();
    let mut other_vm = Vm::new(&module);
    let VmExit::Finished(Value::Octet(id)) = other_vm.run_slice(&mut other_heap, budget) else {
        panic!()
    };
    assert_eq!(other_heap.octet(id).unwrap(), &[0, 255]);
}

#[test]
fn container_edges_are_validated_and_large_graphs_load_and_collect_iteratively() {
    use tjs_core::{ConstantValue, HeapCounts};
    for index in [0, 1, u32::MAX] {
        assert!(
            Module::with_constants(
                vec![entry(0)],
                vec![Constant::Array(
                    vec![ConstantValue::Reference(index)].into()
                )]
            )
            .is_err()
        );
    }
    let mut constants = vec![Constant::Array(vec![ConstantValue::Int(7)].into())];
    for index in 0..6000 {
        constants.push(Constant::Array(
            vec![ConstantValue::Reference(index)].into(),
        ));
    }
    let module = Module::with_constants(vec![entry(6000)], constants).unwrap();
    let mut heap = Heap::new();
    let mut vm = Vm::new(&module);
    let VmExit::Finished(mut value) = vm.run_slice(&mut heap, RunBudget::new(10).unwrap()) else {
        panic!()
    };
    heap.collect(vm.roots());
    for _ in 0..6001 {
        let Value::Obj(object) = value else { panic!() };
        value = heap.array(object.object.unwrap()).unwrap()[0];
    }
    assert!(matches!(value, Value::Int(7)));
    drop(vm);
    drop(module);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}
