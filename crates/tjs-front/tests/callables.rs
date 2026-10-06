use tjs_core::{Heap, HeapCounts, Module, ObjRef, Phase, RunBudget, SourceMap, Value, Vm, VmExit};
use tjs_front::compile;

fn compile_module(sources: &mut SourceMap, name: &str, script: &str) -> Module {
    let source = sources.add_utf8(name, script).unwrap();
    compile(sources, source).unwrap()
}

fn run(vm: &mut Vm, heap: &mut Heap, slice: u32) -> VmExit {
    loop {
        let exit = vm.run_slice(heap, RunBudget::new(slice).unwrap());
        heap.collect(vm.roots());
        if !matches!(exit, VmExit::Yielded) {
            return exit;
        }
        assert!(vm.work_executed() < 100_000);
    }
}

fn evaluate(script: &str, slice: u32) -> String {
    let mut sources = SourceMap::new();
    let module = compile_module(&mut sources, "callables", script);
    let mut heap = Heap::new();
    tjs_core::exception::install(&mut heap).unwrap();
    let baseline = heap.collect([]).after;
    let mut vm = Vm::new(&module);
    let VmExit::Finished(value) = run(&mut vm, &mut heap, slice) else {
        panic!("{script}: {:?}", vm.diagnostic("failed"))
    };
    let text = heap.display(value).unwrap();
    drop(vm);
    assert_eq!(heap.collect([]).after, baseline);
    text
}

#[test]
fn functions_are_values_and_globals_are_shared_with_function_bodies() {
    for (script, expected) in [
        (
            "function add(a, b) { return a + b; } var f = add; f(20, 22);",
            "42",
        ),
        (
            "function f() { return 7; } function factory() { return f; } factory()();",
            "7",
        ),
        (
            "function f(a) { return a; } function apply(fn, value) { return fn(value); } apply(f, 42);",
            "42",
        ),
        (
            "function f(a, b) { return b; } function relay(fn, x) { return fn(...); } relay(f, 42);",
            "42",
        ),
        (
            "var x = 3; function f() { x = x + 4; return x; } f(); global.x;",
            "7",
        ),
        (
            "function f() { return 1; } function g() { return 2; } var old = f; f = g; old() * 10 + f();",
            "12",
        ),
        ("function f() {} var f = 1; f;", "1"),
        (
            "function f() { return 1; } function g() { return 2; } { var f = g; f(); }",
            "2",
        ),
        ("function f() {} var x = f; x == f;", "1"),
        ("function f() {} f.tag = 9; var alias = f; alias.tag;", "9"),
        (
            "var b = 11; function f(a = b, b = 7) { return a * 10 + b; } f();",
            "117",
        ),
        ("function f() { return 42; } global.f();", "42"),
        ("this == global;", "1"),
        (
            "var x = 7; function f() { var x = x + 1; return x; } f();",
            "8",
        ),
        ("var x = 7; { var x = x + 2; x; }", "9"),
        ("function f() { return 7; } var f = f(); f;", "7"),
    ] {
        for slice in [1, 2, 7, 10_000] {
            assert_eq!(evaluate(script, slice), expected, "{script}");
        }
    }
}

#[test]
fn explicit_bindings_member_receivers_and_unbound_calls_choose_the_right_this() {
    let prelude =
        "var x = 1; function probe() { return this.x; } var d = %[x: 2]; var other = %[x: 3];";
    for (body, expected) in [
        ("d.m = probe; d.m();", "1"), // Named functions are initially bound to global.
        ("d.m = probe incontextof null; d.m();", "2"),
        ("d.m = probe incontextof null; var f = d.m; f();", "1"),
        ("other.m = probe incontextof d; other.m();", "2"),
        (
            "d.m = probe incontextof null; (d incontextof other).m();",
            "3",
        ),
        (
            "function apply(fn) { return fn(); } (apply incontextof d)(probe incontextof null);",
            "2",
        ),
        (
            "function outer(fn) { var a = this.x; var b = fn(); return a * 100 + b * 10 + this.x; } (outer incontextof d)(probe incontextof other);",
            "232",
        ),
        ("(probe incontextof d) == (probe incontextof other);", "1"),
        (
            "var f = probe incontextof d; var g = f incontextof other; f() * 10 + g();",
            "23",
        ),
        ("function read() { return x; } (read incontextof d)();", "2"),
        (
            "var missing = 9; function read() { return missing; } (read incontextof d)();",
            "void",
        ), // Dictionary's successful void read stops fallback.
        (
            "function call() { return probe(); } (call incontextof d)();",
            "1",
        ), // Missing method falls back to global.
        (
            "function write() { x = 4; return global.x * 10 + this.x; } (write incontextof d)();",
            "14",
        ),
        (
            "var y = 8; function write() { y = 9; return global.y; } (write incontextof d)();",
            "9",
        ),
    ] {
        let script = format!("{prelude} {body}");
        for slice in [1, 7, 10_000] {
            assert_eq!(evaluate(&script, slice), expected, "{body}");
        }
    }
}

#[test]
fn arguments_precede_callee_evaluation_and_local_arguments_remain_live_addresses() {
    for (script, expected) in [
        (
            "var order = 0; function mark(n, value) { order = order * 10 + n; return value; } function f(x) { order = order * 10 + 4; return x; } var d = %[m: f]; mark(2, d)[mark(3, \"m\")](mark(1, 9)); order;",
            "1234",
        ),
        (
            "function f(a) { return 1; } function g(a) { return 2; } f(f = g);",
            "2",
        ),
        (
            "function identity(x) { return x; } function test() { var x = 1; var d = %[\"7\" => identity]; return d[(x = 7) + \"\"](x); } test();",
            "7",
        ),
        (
            "function identity(x) { return x; } var x = 1; var d = %[\"7\" => identity]; d[(x = 7) + \"\"](x);",
            "1",
        ),
        (
            "var order = 0; function fail() { order = 1; throw 9; } function factory() { order = 2; return factory; } try { factory()(fail()); } catch (e) {} order;",
            "1",
        ),
        (
            "var n = 0; function arg() { n = n + 1; return 7; } try {void.m(arg());} catch(e) {n+=10;} n;",
            "11",
        ),
        (
            "function f() { return 7; } var d = %[\"4294967296\" => f]; d[4294967296]();",
            "7",
        ),
    ] {
        for slice in [1, 7, 10_000] {
            assert_eq!(evaluate(script, slice), expected, "{script}");
        }
    }
}

#[test]
fn bound_context_survives_deleted_bindings_recursion_and_exception_unwind() {
    let script = r#"
        function method(n) {
            if (n == 0) throw this;
            return this.again(n - 1);
        }
        var owner = %[message: "kept"];
        owner.again = method incontextof owner;
        var escaped = owner.again;
        owner = void;
        delete global.method;
        try { escaped(30); } catch (e) { e.message; }
    "#;
    for slice in [1, 7, 10_000] {
        assert_eq!(evaluate(script, slice), "kept");
    }
}

#[test]
fn escaping_functions_own_code_and_execute_across_modules_after_the_creator_is_dropped() {
    let mut sources = SourceMap::new();
    let mut heap = Heap::new();
    let exported = {
        let module = compile_module(
            &mut sources,
            "library",
            r#"var prefix = "lib:"; function f(n) { if (n == 0) throw prefix; return prefix + n; } f;"#,
        );
        let mut vm = Vm::new(&module);
        let VmExit::Finished(value) = run(&mut vm, &mut heap, 1) else {
            panic!()
        };
        heap.root(value)
    }; // Both Module and the creating VM are dropped here.
    heap.collect([]);
    let global = heap.alloc_object();
    let name = heap.intern(&"imported".encode_utf16().collect::<Vec<_>>());
    heap.set_member(global, name, heap.rooted(exported).unwrap())
        .unwrap();
    heap.release_root(exported);
    let module = compile_module(
        &mut sources,
        "consumer",
        r#"var prefix = "consumer:"; try { imported(0); } catch (e) { imported(2) + e; }"#,
    );
    let mut vm = Vm::with_global(&module, global);
    drop(module); // The executing VM also owns its immutable code.
    let VmExit::Finished(value) = run(&mut vm, &mut heap, 1) else {
        panic!()
    };
    assert_eq!(heap.display(value).unwrap(), "lib:2lib:");
    vm.reset();
    let VmExit::Finished(value) = run(&mut vm, &mut heap, 2) else {
        panic!()
    };
    assert_eq!(heap.display(value).unwrap(), "lib:2lib:");
    drop(vm);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn reset_uses_fresh_owned_globals_without_invalidating_exported_functions() {
    let mut sources = SourceMap::new();
    let module = compile_module(
        &mut sources,
        "counter",
        "var n = 0; function tick() { n = n + 1; return n; } tick;",
    );
    let mut heap = Heap::new();
    let mut vm = Vm::new(&module);
    let VmExit::Finished(first) = run(&mut vm, &mut heap, 1) else {
        panic!()
    };
    let root = heap.root(first);
    let first_global = vm.global().unwrap();
    vm.reset();
    let VmExit::Finished(second) = run(&mut vm, &mut heap, 1) else {
        panic!()
    };
    assert_ne!(first_global, vm.global().unwrap());
    assert!(!tjs_core::value::equal(&heap, first, second).unwrap());
    let current_global = vm.global().unwrap();
    let old = heap.intern(&"old".encode_utf16().collect::<Vec<_>>());
    heap.set_member(current_global, old, first).unwrap();
    let new = heap.intern(&"newTick".encode_utf16().collect::<Vec<_>>());
    heap.set_member(current_global, new, second).unwrap();
    heap.release_root(root);
    drop(vm);
    let module = compile_module(
        &mut sources,
        "use-counters",
        "old(); old() * 10 + newTick();",
    );
    let mut vm = Vm::with_global(&module, current_global);
    let VmExit::Finished(value) = run(&mut vm, &mut heap, 1) else {
        panic!()
    };
    assert_eq!(value.as_integer(), Some(21));
    drop(vm);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn dynamic_lookup_and_invalid_calls_fail_at_runtime_without_leaking_local_bindings() {
    for script in [
        "unknown();",
        "var a = 1; b;",
        "{ var a = 1; } a;",
        "try { throw 7; } catch (e) {} e;",
        "function f(a = b, b = 7) {} f();",
        "function f() {} { var f = 1; f(); }",
        "null();",
        "%[]();",
        "var d = %[]; d.m();",
        "void[\"m\"]();",
        "(7 incontextof null);",
        "function f() {} (f incontextof 7);",
        "missing = 1;",
        "var d = %[initial: d];",
        "function f() { var d = %[initial: d]; } f();",
    ] {
        let mut sources = SourceMap::new();
        let module = compile_module(&mut sources, "fault", script);
        let mut heap = Heap::new();
        let mut vm = Vm::new(&module);
        let VmExit::Fault(error) = run(&mut vm, &mut heap, 1) else {
            panic!("{script}")
        };
        assert_eq!(error.phase, Phase::Runtime);
        assert!(error.span.is_some());
        let work = vm.work_executed();
        assert!(matches!(run(&mut vm, &mut heap, 1), VmExit::Fault(_)));
        assert_eq!(vm.work_executed(), work);
        drop(vm);
        assert_eq!(heap.collect([]).after, HeapCounts::default());
    }
}

#[test]
fn unbound_functions_fall_back_to_plain_this_then_global_for_missing_names() {
    let mut sources = SourceMap::new();
    let module = compile_module(
        &mut sources,
        "fallback",
        "var x = 7; function f() { return x; } (f incontextof receiver)();",
    );
    let mut heap = Heap::new();
    let global = heap.alloc_object();
    let receiver = heap.alloc_object();
    let key = heap.intern(&"receiver".encode_utf16().collect::<Vec<_>>());
    heap.set_member(global, key, Value::Obj(ObjRef::bound(receiver)))
        .unwrap();
    let mut vm = Vm::with_global(&module, global);
    let VmExit::Finished(value) = run(&mut vm, &mut heap, 1) else {
        panic!()
    };
    assert_eq!(value.as_integer(), Some(7));
    drop(vm);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn host_globals_and_cached_function_identity_survive_reset_without_new_heap_allocations() {
    let mut sources = SourceMap::new();
    let module = compile_module(
        &mut sources,
        "host-counter",
        "var n = n + 1; function tick() { n = n + 1; return n; } tick(); tick;",
    );
    let mut heap = Heap::new();
    let global = heap.alloc_object();
    let n = heap.intern(&[b'n' as u16]);
    heap.set_member(global, n, Value::Int(0)).unwrap();
    let mut vm = Vm::with_global(&module, global);
    let VmExit::Finished(first) = run(&mut vm, &mut heap, 1) else {
        panic!()
    };
    let counts = heap.counts();
    for iteration in 1..=100 {
        vm.reset();
        let VmExit::Finished(value) = vm.run_slice(&mut heap, RunBudget::new(10_000).unwrap())
        else {
            panic!()
        };
        assert!(tjs_core::value::equal(&heap, first, value).unwrap());
        assert_eq!(
            heap.member(global, n).unwrap().unwrap().as_integer(),
            Some((iteration + 1) * 2)
        );
        assert_eq!(heap.counts(), counts);
        assert_eq!(heap.allocation_debt(), 0);
    }
    drop(vm);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn uncaught_cross_module_exceptions_keep_source_locations_and_original_callers() {
    let mut sources = SourceMap::new();
    let library_source = sources
        .add_utf8("library", "function fail() { throw this; } fail;")
        .unwrap();
    let module = compile(&sources, library_source).unwrap();
    let mut heap = Heap::new();
    let mut producer = Vm::new(&module);
    let VmExit::Finished(exported) = run(&mut producer, &mut heap, 1) else {
        panic!()
    };
    let root = heap.root(exported);
    drop(producer);
    drop(module);
    let global = heap.alloc_object();
    let name = heap.intern(&"imported".encode_utf16().collect::<Vec<_>>());
    heap.set_member(global, name, exported).unwrap();
    heap.release_root(root);
    let caller_source = sources
        .add_utf8("caller", "function outer() { return imported(); } outer();")
        .unwrap();
    let module = compile(&sources, caller_source).unwrap();
    let mut vm = Vm::with_global(&module, global);
    let VmExit::Thrown(exception) = run(&mut vm, &mut heap, 1) else {
        panic!()
    };
    let trace = &exception.diagnostic.trace;
    assert_eq!(
        trace
            .iter()
            .map(|frame| frame.function.as_str())
            .collect::<Vec<_>>(),
        ["fail", "outer", "<script>"]
    );
    assert_eq!(trace[0].span.unwrap().source(), library_source);
    assert_eq!(trace[1].span.unwrap().source(), caller_source);
    assert_eq!(trace[2].span.unwrap().source(), caller_source);
    assert!(matches!(run(&mut vm, &mut heap, 1), VmExit::Thrown(_)));
    drop(vm);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}
