use tjs_core::{RunBudget, SourceMap, Vm, VmExit};
use tjs_front::compile;

#[test]
fn runtime_trace_points_at_each_call_and_survives_module_drop() {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("trace.tjs", "function fail() { return 1 % 0; }\nfunction middle(a = fail()) { return a; }\nfunction outer() { return middle(); }\nouter();").unwrap();
    let error = {
        let module = compile(&sources, source).unwrap();
        let mut heap = tjs_core::Heap::new();
        let mut vm = Vm::new(&module);
        loop {
            match vm.run_slice(&mut heap, RunBudget::new(1).unwrap()) {
                VmExit::Fault(error) => break error,
                VmExit::Thrown(exception) => panic!("unexpected script exception: {exception:?}"),
                VmExit::Inspecting(request) => panic!("unexpected inspection: {request:?}"),
                VmExit::Waiting(request) => panic!("unexpected native wait: {request:?}"),
                VmExit::CompileRequest(request) => {
                    panic!("unexpected compile request: {request:?}")
                }
                VmExit::Yielded => {}
                VmExit::Finished(_) => panic!("division by zero expected"),
            }
        }
    };
    assert_eq!(
        error
            .trace
            .iter()
            .map(|frame| frame.function.as_str())
            .collect::<Vec<_>>(),
        ["fail", "middle", "outer", "<script>"]
    );
    let spans: Vec<_> = error
        .trace
        .iter()
        .map(|frame| String::from_utf16(sources.slice(frame.span.unwrap()).unwrap()).unwrap())
        .collect();
    assert_eq!(spans, ["1 % 0", "fail()", "middle()", "outer()"]);
}

#[test]
fn explicit_host_stop_captures_a_trace_without_faulting_the_vm() {
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8("paused", "function f() { while (1) {} } f();")
        .unwrap();
    let module = compile(&sources, source).unwrap();
    let mut heap = tjs_core::Heap::new();
    let mut vm = Vm::new(&module);
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(20).unwrap()),
        VmExit::Yielded
    ));
    let error = vm.diagnostic("host stopped this slice");
    assert_eq!(error.trace[0].function, "f");
    assert_eq!(error.trace[1].function, "<script>");
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(1).unwrap()),
        VmExit::Yielded
    ));
    assert_eq!(vm.instructions_executed(), 21);
}
