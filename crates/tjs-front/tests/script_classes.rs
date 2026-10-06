use tjs_core::{Heap, HeapCounts, Module, RunBudget, SourceMap, Value, Vm, VmExit};

fn compile(sources: &mut SourceMap, script: &str) -> Module {
    let source = sources.add_utf8("script classes", script).unwrap();
    tjs_front::compile(sources, source).unwrap_or_else(|error| panic!("{script}: {error}"))
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

fn check(script: &str, expected: &str) {
    let mut sources = SourceMap::new();
    let module = compile(&mut sources, script);
    for slice in [1, 10_000] {
        let mut heap = Heap::new();
        let mut vm = Vm::new(&module);
        let exit = run(&mut vm, &mut heap, slice);
        let VmExit::Finished(value) = exit else {
            panic!("{script}: {exit:?}")
        };
        assert_eq!(heap.display(value).unwrap(), expected, "{script}");
        drop(vm);
        assert_eq!(heap.collect([]).after, HeapCounts::default());
    }
}

#[test]
fn construction_fields_methods_and_inheritance_follow_registration_order() {
    for (script, expected) in [
        (
            "class C { var x = 3; function C(n) { x += n; return 99; } function get() { return x; } } var c = new C(4); c.get();",
            "7",
        ),
        (
            "class C { var x = 1; function add() { return ++x; } } var a = new C(); var b = new C(); var f = a.add; f() * 10 + b.x;",
            "21",
        ),
        (
            "class A { var x = 2; function A(n) { x += n; } function get() { return x; } } class B extends A { var y = x + 1; function B(n) { super.A(n); } function get() { return super.get() + y; } } var b = new B(4); b.get();",
            "9",
        ),
        (
            "class A { var x = 1; function f() { return 1; } } class B { var x = 2; function f() { return 2; } } class C extends A, B {} var c = new C(); c.x * 10 + c.f();",
            "22",
        ),
        (
            "class A { function A() { global.n += 10; } } class B extends A {} var n = 0; var b = new B(); n;",
            "0",
        ),
        (
            "class C { var x = 3; { var x = 8; } function get() { return x; } } new C().get();",
            "3",
        ),
        ("var c = new C(); if(0) class C { var x = 5; } c.x;", "5"),
        (
            "class C { var x = 2; function f() { return x; } } var d = %[x: 9]; (C.f incontextof d)();",
            "9",
        ),
        (
            "class C { function f() { return 2; } } class D extends C {} D.f();",
            "2",
        ),
        (
            "class C {} class D extends C {} C.k = 3; D.k = 8; C.k * 10 + D.k;",
            "88",
        ),
        (
            "class A { function f() { return 1; } } class B { function f() { return 2; } } var base = A; class C extends base { function f() { return super.f(); } } var c = new C(); base = B; c.f();",
            "2",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn properties_bind_context_and_suspend_on_reads_writes_and_calls() {
    for (script, expected) in [
        (
            "var x = 4; property p { getter { return x; } setter(v) { x = v; } } p += 3; p;",
            "7",
        ),
        (
            "class C { var x = 1; property p { getter(): int { return x * 2; } setter(v: int) { x = v; } } } var c = new C(); c.p = 3; c.p++; c.x * 10 + c.p;",
            "84",
        ),
        (
            "class C { var x = 6; property callback { getter { return function(v) { return x + v; }; } } } var c = new C(); c.callback(4);",
            "10",
        ),
        (
            "class A { var x = 1; property p { getter { return x; } setter(v) { x = v; } } } class B extends A { property p { getter { return super.p + 1; } setter(v) { super.p = v + 1; } } } var b = new B(); b.p = 5; b.p;",
            "7",
        ),
        (
            "var x = 1; property p { setter(v) { x = v; } getter { return x; } } function f() { return p++; } f() * 10 + p;",
            "12",
        ),
        (
            "class C { property p { getter { return 4; } } var p = 9; } new C().p;",
            "9",
        ),
        (
            "function f() { property p { getter { return 8; } } } f.p;",
            "8",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn throws_unwind_accessors_initializers_constructors_and_resumed_calls() {
    for (script, expected) in [
        (
            "property p { getter { throw 3; } } var n; try { n = p; } catch(e) { n = e; } n;",
            "3",
        ),
        (
            "class C { property p { setter(v) { throw v; } } } var c = new C(); var n; try { c.p = 7; } catch(e) { n = e; } n;",
            "7",
        ),
        (
            "class C { function C() { throw 8; } } var n; try { new C(); } catch(e) { n = e; } n;",
            "8",
        ),
        (
            "class C { var x = boom(); } function boom() { throw 9; } var n; try { new C(); } catch(e) { n = e; } n;",
            "9",
        ),
        (
            "function base() { throw 5; } class C extends base() {} var n; try { C.nope; } catch(e) { n = e; } n;",
            "5",
        ),
        (
            "class C { property f { getter { return function() { throw 6; }; } } } var n; try { new C().f(); } catch(e) { n = e; } n;",
            "6",
        ),
        (
            "class C { function C(v) { try { throw v; } catch(e) { this.x = e; } } } new C(4).x;",
            "4",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn class_pools_and_pending_operations_survive_across_vms() {
    let mut sources = SourceMap::new();
    let mut heap = Heap::new();
    let global = heap.alloc_object();
    let root = heap.root(Value::Obj(global.into()));
    for (script, expected) in [
        (
            "class C { var x = 2; property p { getter { return x; } setter(v) { x = v; } } } var c = new C(); c.p;",
            "2",
        ),
        (
            "c.p = 9; class D extends C { function get() { return super.p; } } var d = new D(); c.p * 10 + d.get();",
            "92",
        ),
    ] {
        let module = compile(&mut sources, script);
        let mut vm = Vm::with_global(&module, global);
        let VmExit::Finished(value) = run(&mut vm, &mut heap, 1) else {
            panic!("cross-module class")
        };
        assert_eq!(heap.display(value).unwrap(), expected);
    }
    heap.release_root(root).unwrap();
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn class_lookup_is_dynamic_but_instance_members_are_copied() {
    for (script, expected) in [
        (
            "class C { var x = 3; property C { setter(v) { throw 1; } } } new C().x;",
            "3",
        ),
        (
            "var f = function() { return this.x; }; var x = 1; class C { var x = 7; function call() { return f(); } } new C().call();",
            "7",
        ),
        (
            "class A { var x = f(); function f() { return 1; } } class B extends A { var y = f(); function f() { return 2; } } var b = new B(); b.x * 10 + b.y;",
            "12",
        ),
        (
            "class A { function f() { return 1; } } class B {} class C extends A, B {} C.f();",
            "1",
        ),
        (
            "class A {} class B extends A {} A.x = 4; var n = delete B.x; n * 10 + (delete A.x);",
            "10",
        ),
        ("class C {} C.C = 1; var c = new C(); c.x = 7; c.x;", "7"),
        (
            "class C { var C = 1; function C() { this.x = 7; } } new C().x;",
            "7",
        ),
        (
            "class A { function f() { return 3; } } var n = 0; function base() { n++; return A; } class B extends base() { function B() {} } new B(); B.f(); n;",
            "2",
        ),
        (
            "class C { function f() { return 1; } } var a = new C(); C.f = function() { return 2; }; var b = new C(); a.f() * 10 + b.f();",
            "12",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn recursive_properties_and_cyclic_bases_obey_vm_depth_and_work_limits() {
    for script in [
        "property p { getter { return p; } } p;",
        "class C extends C {} new C();",
        "class C extends C {} C.absent;",
    ] {
        let mut sources = SourceMap::new();
        let module = compile(&mut sources, script);
        let mut vm = Vm::with_limits(
            &module,
            tjs_core::VmLimits {
                max_call_depth: 8,
                max_stack_values: 4096,
            },
        )
        .unwrap();
        let mut heap = Heap::new();
        let VmExit::Fault(error) = run(&mut vm, &mut heap, 1) else {
            panic!("depth limit: {script}")
        };
        assert!(error.message.contains("depth"), "{error}");
        assert!(vm.work_executed() < 200);
        assert!(vm.call_depth() <= 8);
        vm.reset();
        assert!(matches!(run(&mut vm, &mut heap, 1), VmExit::Fault(_)));
    }
}

#[test]
fn invalid_definitions_and_accessor_permissions_report_errors() {
    for script in [
        "class C extends A, B { function f() { return super.f(); } }",
        "function f() { return super.x; }",
        "property p {}",
        "property p { getter {} getter {} }",
        "property p { setter() {} }",
        "property p { getter(x) {} }",
        "class C extends {}",
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("invalid definition", script).unwrap();
        assert!(tjs_front::compile(&sources, source).is_err(), "{script}");
    }
    for script in [
        "property p { getter { return 1; } } p = 2;",
        "property p { setter(v) {} } p;",
        "class C {} var c = new C(); c.missing;",
    ] {
        let mut sources = SourceMap::new();
        let module = compile(&mut sources, script);
        assert!(
            matches!(
                run(&mut Vm::new(&module), &mut Heap::new(), 1),
                VmExit::Fault(_)
            ),
            "{script}"
        );
    }
    for script in [
        format!("{}{}", "class C {".repeat(1000), "}".repeat(1000)),
        format!(
            "{}{}",
            "property p { getter {".repeat(1000),
            "}}".repeat(1000)
        ),
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("deep definition", &script).unwrap();
        assert!(tjs_front::compile(&sources, source).is_err());
    }
}
