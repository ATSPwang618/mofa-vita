use tjs_core::{Heap, HeapCounts, Module, Phase, RunBudget, SourceMap, Value, Vm, VmExit};
use tjs_front::compile;

fn module(script: &str) -> Module {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("constant containers", script).unwrap();
    compile(&sources, source).unwrap_or_else(|error| panic!("{script}: {error}"))
}

fn run(vm: &mut Vm, heap: &mut Heap) -> Value {
    loop {
        let exit = vm.run_slice(heap, RunBudget::new(1).unwrap());
        heap.collect(vm.roots());
        match exit {
            VmExit::Finished(value) => return value,
            VmExit::Yielded => assert!(vm.work_executed() < 100_000),
            exit => panic!("{exit:?}"),
        }
    }
}

#[test]
fn constant_literals_share_mutable_identity_and_support_native_methods() {
    for (script, expected) in [
        (
            "function a() { return (const)[1]; } var x = a(); x[0]++; (a() === x) * 10 + a()[0];",
            "12",
        ),
        ("var a = (const)[1]; var b = (const)[1]; a === b;", "0"),
        (
            "function a() { return (const)[(const)%['n'=>2], (const)[]]; } a()[0].n = 7; a()[1].push(9); a()[0].n * 10 + a()[1][0];",
            "79",
        ),
        (
            "var a = (const)[void, null, -'2', +'3.5', <%41 ff%>, true]; a[4][1] + a[2] + a[3] + a[5];",
            "257.5",
        ),
        (
            "var d = (const)%[,1=>2,'1'=>3,1.5=>4]; d['1'] * 10 + d['1.5'];",
            "34",
        ),
        (
            "var d = (const)%[]; d.n = 8; (Dictionary.clear incontextof d)(); typeof d.n;",
            "undefined",
        ),
        (
            "function a() { return (const)[1]; } var x = a(); x[0] = x; a()[0] === x;",
            "1",
        ),
    ] {
        let module = module(script);
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&module);
        let value = run(&mut vm, &mut heap);
        assert_eq!(heap.display(value).unwrap(), expected, "{script}");
    }
}

#[test]
fn constant_graph_survives_export_gc_reset_and_cross_vm_calls() {
    let mut heap = Heap::new();
    let global = heap.alloc_global();
    let root = heap.root(Value::Obj(global.into()));
    let compiled = module("function data() { return (const)[(const)%['n'=>0]]; } data()[0].n = 7;");
    let mut creator = Vm::with_global(&compiled, global);
    run(&mut creator, &mut heap);
    drop(creator);
    drop(compiled);
    heap.collect([]);
    let increment = module("++data()[0].n;");
    for expected in [8, 9, 10] {
        let mut caller = Vm::with_global(&increment, global);
        assert!(matches!(run(&mut caller, &mut heap), Value::Int(n) if n == expected));
        drop(caller);
        heap.collect([]);
    }
    heap.release_root(root);
    assert_eq!(heap.collect([]).after, HeapCounts::default());

    let compiled = module("var a = (const)[0]; ++a[0];");
    let mut first = Vm::new(&compiled);
    assert!(matches!(run(&mut first, &mut heap), Value::Int(1)));
    first.reset();
    assert!(matches!(run(&mut first, &mut heap), Value::Int(2)));
    let mut other = Heap::new();
    assert!(matches!(
        run(&mut Vm::new(&compiled), &mut other),
        Value::Int(1)
    ));
    drop(first);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn constant_grammar_and_compile_time_conversions_are_enforced() {
    for script in [
        "(const)[,];",
        "(const)[1,];",
        "(const)[1 + 2];",
        "(const)[name];",
        "(const)[[1]];",
        "(const)[function() {}];",
        "(const)[--1];",
        "(const)[+void];",
        "(const)%['x'=>1,];",
        "(const)%[x:1];",
        "(const)%[void=>1];",
        "(const)%[-1=>2];",
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("invalid constant", script).unwrap();
        assert_eq!(
            compile(&sources, source).unwrap_err().phase,
            Phase::Parse,
            "{script}"
        );
    }
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8("invalid conversion", "(const)[+<%41%>];")
        .unwrap();
    assert_eq!(compile(&sources, source).unwrap_err().phase, Phase::Compile);
    let script = format!("{}0{};", "(const)[".repeat(1000), "]".repeat(1000));
    let source = sources.add_utf8("deep constants", &script).unwrap();
    assert_eq!(compile(&sources, source).unwrap_err().phase, Phase::Parse);
    let compiled = module("function a() { return (const)[(const)%['n'=>1]]; } a();");
    let code = compiled.disassemble();
    assert!(code.contains("constant c") && code.contains("dictionary") && code.contains("array"));
    assert!(!code.contains("new_array") && !code.contains("new_dictionary"));
}
