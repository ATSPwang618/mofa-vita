use tjs_core::{
    Heap, NativeContinuation, NativeCx, NativeResult, NativeStep, RunBudget, Trace, Value, Vm,
    VmExit,
};

struct Return(Value);
impl Trace for Return {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(self.0);
    }
}
impl NativeContinuation for Return {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(self.0))
    }
}

fn finish(vm: &mut Vm, heap: &mut Heap) -> Value {
    loop {
        match vm.run_slice(heap, RunBudget::new(1).unwrap()) {
            VmExit::Yielded => {
                heap.collect(vm.roots());
            }
            VmExit::Finished(value) => return value,
            _ => panic!("task should finish"),
        }
    }
}

#[test]
fn idle_task_releases_the_previous_heap_and_can_restart_in_a_new_heap() {
    let mut heap = Heap::new();
    let global = heap.alloc_global();
    let text = Value::Str(heap.alloc_string(vec![65; 1024]));
    let mut vm = Vm::task(global, Box::new(Return(text)));
    let value = finish(&mut vm, &mut heap);
    assert_eq!(heap.display(value).unwrap().len(), 1024);
    let mut vm = vm.into_idle_task().unwrap();
    assert_eq!(heap.collect(vm.roots()).after, Default::default());
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(1).unwrap()),
        VmExit::Finished(Value::Void)
    ));

    let mut next = Heap::new();
    let global = next.alloc_global();
    vm.restart_task(global, Box::new(Return(Value::Int(73))));
    assert_eq!(finish(&mut vm, &mut next).as_integer(), Some(73));
    assert_eq!(vm.work_executed(), 2);
}
