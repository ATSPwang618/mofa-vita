use tjs_core::{Heap, HeapCounts, Module, RunBudget, SourceMap, Vm, VmExit};

fn compile(sources: &mut SourceMap, script: &str) -> Module {
    let source = sources.add_utf8("property operators", script).unwrap();
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
    let module = compile(&mut SourceMap::new(), script);
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
fn raw_properties_can_be_saved_replaced_and_explicitly_accessed() {
    for (script, expected) in [
        (
            "var n = 3; property p { getter { return n; } setter(v) { n = v; } } function f() { var ref = &p; *ref += 4; return (*ref)++ + n; } f();",
            "15",
        ),
        (
            "class C { var n = 2; property p { getter { return n; } setter(v) { n = v; } } } function f() { var c = new C(); var ref = &c.p; *ref = 8; var d = %[n: 40]; var other = ref incontextof d; *other += 2; return c.n * 100 + d.n; } f();",
            "842",
        ),
        (
            "property p { getter { throw 1; } setter(v) { throw 2; } } var ref = &p; &p = 7; p;",
            "7",
        ),
        (
            "class A { property p { getter { return 3; } } } class B extends A {} &B.p = 9; A.p * 10 + B.p;",
            "39",
        ),
        (
            "var p = 2; class C { function replace() { &p = 8; } } var c = new C(); c.replace(); p * 10 + c.p;",
            "28",
        ),
        (
            "class C { property p { getter { return 6; } } } var c = new C(); function f() { with(c) { var ref = &.p; return *ref; } } f();",
            "6",
        ),
        (
            "var n = 1; property p { getter { return n; } setter(v) { n = v; } } var a = [&p]; a[0] += 4; function f() { var ref = &a[0]; *ref = 9; return a[0]; } f();",
            "9",
        ),
        (
            "property p { getter { return function(n) { return n + 1; }; } } var a = [&p]; a[0](4);",
            "5",
        ),
        (
            "property p { getter { return 5; } } var d = %[]; &d.p = &p; function f() { var ref = &d.p; &d.p = 7; return *ref * 10 + d.p; } f();",
            "57",
        ),
        (
            "property p { getter { return this.n; } } var d=%[n:7]; function f() { var ref = &p incontextof d; return *ref; } f();",
            "7",
        ),
        (
            "property p { getter { throw 99; } } property q { getter { return this.n; } } var d=%[n:8]; &d.p = &q incontextof d; d.p;",
            "8",
        ),
        ("typeof *void;", "void"),
    ] {
        check(script, expected);
    }
}

#[test]
fn call_arguments_distinguish_property_access_from_rest_forwarding() {
    for (script, expected) in [
        (
            "property p { getter { return 7; } } function f(a) { return a; } f(*(&p));",
            "7",
        ),
        (
            "property p { getter { return 7; } } function f(a, b) { return a * 10 + b; } function invoke() { var ref = &p; return f(*ref, *(&p)); } invoke();",
            "77",
        ),
        (
            "class Layer { property width { getter { return this.w; } } property height { getter { return this.h; } } } var layer = %[w: 6, h: 4]; function rect(w, h) { return w * h; } rect(*(&global.Layer.width incontextof layer), *(&global.Layer.height incontextof layer));",
            "24",
        ),
        (
            "var reads = 0; property p { getter { return ++reads; } } function f(a, b, c) { return a * 100 + b * 10 + c; } f(*(&p), *(&p) * 2, -*(&p));",
            "137",
        ),
        (
            "property p { getter { return 7; } } function f(a, b, c) { return a * 100 + b * 10 + c; } function relay(*) { return f(*(&p), *); } relay(2, 3);",
            "723",
        ),
        (
            "property p { getter { return 7; } } function f(a, b, c) { return a * 100 + b * 10 + c; } function relay(*) { return f(*, *(&p)); } relay(2, 3);",
            "237",
        ),
        (
            "property p { getter { return 7; } } class C { var n; function C(v) { n = v; } } var c = new C(*(&p)); c.n;",
            "7",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn update_order_and_getter_throws_survive_suspension() {
    for (script, expected) in [
        (
            "var reads = 0, stored = 0; property p { getter { return ++reads; } setter(v) { stored = v; } } function f() { 1 ? p++ : p--; } f(); reads * 10 + stored;",
            "12",
        ),
        (
            "var reads = 0, stored = 0; property p { getter { return ++reads; } setter(v) { stored = v; } } function f() { var ref = &p; var old = (*ref)++; return old * 100 + reads * 10 + stored; } f();",
            "123",
        ),
        (
            "var reads = 0, stored = 0; property p { getter { return ++reads; } setter(v) { stored = v; } } function f() { var ref = &p; (*ref)++; } f(); reads * 10 + stored;",
            "12",
        ),
        (
            "var reads = 0, stored = 0; property p { getter { return ++reads; } setter(v) { stored = v; } } var old = p++; old * 100 + reads * 10 + stored;",
            "123",
        ),
        (
            "var reads = 0; property p { getter { reads++; throw 7; } } function f() { var ref = &p; var answer; try { answer = *ref; } catch(e) { answer = e; } return answer * 10 + reads; } f();",
            "71",
        ),
        (
            "var n = 0; property p { setter(v) { throw v; } } function f() { var ref = &p; try { *ref = 9; } catch(e) { n = e; } } f(); n;",
            "9",
        ),
        (
            "var log = ''; property p { getter { log += 'g'; return 1; } setter(v) { log += 's'; } } function rhs() { log += 'r'; return 3; } function target() { log += 't'; return &p; } *target() += rhs(); log;",
            "rtgs",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn typeof_distinguishes_missing_members_from_void_and_runs_getters() {
    for (script, expected) in [
        (
            "typeof void + ',' + typeof null + ',' + typeof 1 + ',' + typeof 1.5 + ',' + typeof 'x' + ',' + typeof function {};",
            "void,Object,Integer,Real,String,Object",
        ),
        (
            "var d = %[x: void]; typeof d.x + ',' + typeof d.missing + ',' + typeof d['missing'] + ',' + typeof global.unknown;",
            "void,undefined,undefined,undefined",
        ),
        (
            "var a = [void, 3]; typeof a[0] + ',' + typeof a[2] + ',' + typeof a[-3] + ',' + typeof a[4294967297] + ',' + typeof a['4294967297'];",
            "void,undefined,undefined,Integer,Integer",
        ),
        (
            "var n = 0; property p { getter { n++; return 3; } } typeof global.p + ',' + typeof &p + ',' + n;",
            "Integer,Object,1",
        ),
        (
            "property p { getter { throw 8; } } var n = 0; try { typeof global['p']; } catch(e) { n = e; } n;",
            "8",
        ),
        (
            "class A { property p { getter { return 4; } } } class B extends A {} typeof B.p + ',' + typeof B.missing;",
            "Integer,undefined",
        ),
        ("var d = %['1.5' => 2]; typeof d[1.5];", "Integer"),
    ] {
        check(script, expected);
    }
    for script in [
        "typeof missing;",
        "typeof void.x;",
        "typeof null.x;",
        "property p { setter(v) {} } typeof global.p;",
        "property p { setter(v) {} } typeof global['p'];",
        "property p { setter(v) {} } &global[0] = &p; typeof global[0];",
    ] {
        let module = compile(&mut SourceMap::new(), script);
        let mut heap = Heap::new();
        let mut vm = Vm::new(&module);
        assert!(
            matches!(run(&mut vm, &mut heap, 1), VmExit::Fault(_)),
            "{script}"
        );
    }
}

#[test]
fn instanceof_uses_names_and_records_classes_before_base_initialization() {
    for (script, expected) in [
        (
            "(1 instanceof 'Number') + (1.5 instanceof 'Number') + ('x' instanceof 'String') + (null instanceof 'Object') + ([] instanceof 'Array') + (%[] instanceof 'Dictionary');",
            "6",
        ),
        (
            "(void instanceof 'Object') + (1 instanceof 'Integer') + (1.5 instanceof 'Real') + (null instanceof 'Class') + (1 instanceof void);",
            "0",
        ),
        (
            "class A {} class B extends A {} var b = new B(); (b instanceof 'B') * 100 + (b instanceof 'A') * 10 + (b instanceof 'Object');",
            "111",
        ),
        (
            "class A { var seen = this instanceof 'B'; } class B extends A {} new B().seen;",
            "1",
        ),
        (
            "class A {} class B {} class C extends A, B {} var c = new C(); (c instanceof 'A') + (c instanceof 'B') + (c instanceof 'C');",
            "3",
        ),
        (
            "class C {} var c = new C(); (C instanceof 'Class') * 100 + (C instanceof 'C') * 10 + (c instanceof C);",
            "100",
        ),
        (
            "property p { getter { return 3; } } (typeof &p) + ',' + ((&p) instanceof 'Property') + ',' + (function {} instanceof 'Function');",
            "Object,1,1",
        ),
        ("var n = 1; n instanceof (n = 'Number');", "1"),
        ("!1 instanceof 'Number';", "0"),
        ("1 instanceof 'Number' + 2;", "3"),
        (
            "class A {} var base = A; class C extends base {} var c = new C(); base = void; c instanceof 'A';",
            "1",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn escaped_property_and_class_information_outlive_the_defining_vm() {
    let mut sources = SourceMap::new();
    let first = compile(
        &mut sources,
        "class C { var n = 6; property p { getter { return n; } setter(v) { n = v; } } } var c = new C(); var saved = &c.p; void;",
    );
    let mut heap = Heap::new();
    let mut vm = Vm::new(&first);
    assert!(matches!(run(&mut vm, &mut heap, 1), VmExit::Finished(_)));
    let global = vm.global().unwrap();
    let root = heap.root(tjs_core::Value::Obj(global.into()));
    drop(vm);
    heap.collect([]);
    let next = compile(
        &mut sources,
        "*(&saved) = 12; (c instanceof 'C') * 100 + *(&saved);",
    );
    let mut vm = Vm::with_global(&next, global);
    let VmExit::Finished(value) = run(&mut vm, &mut heap, 1) else {
        panic!("second module")
    };
    assert_eq!(heap.display(value).unwrap(), "112");
    drop(vm);
    heap.release_root(root).unwrap();
    assert_eq!(heap.collect([]).after, HeapCounts::default());

    // An empty instance must retain its class-name symbols even when neither
    // the defining global nor any method/function pool remains reachable.
    let module = compile(&mut sources, "class Empty {} new Empty();");
    let mut vm = Vm::new(&module);
    let VmExit::Finished(instance) = run(&mut vm, &mut heap, 1) else {
        panic!("empty instance")
    };
    let root = heap.root(instance);
    drop(vm);
    heap.collect([]);
    assert_eq!(heap.counts().objects, 1);
    let name = tjs_core::Value::Str(heap.alloc_string("Empty".encode_utf16().collect::<Vec<_>>()));
    assert!(tjs_core::value::instance_of(&mut heap, instance, name).unwrap());
    heap.release_root(root).unwrap();
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn octet_types_and_warmed_type_names_survive_gc_and_reset_without_heap_growth() {
    let module = compile(
        &mut SourceMap::new(),
        "function check() { var t = typeof blob; return (t == 'Octet') + (blob instanceof 'Octet'); } check();",
    );
    let mut heap = Heap::new();
    let global = heap.alloc_global();
    let blob = tjs_core::Value::Octet(heap.alloc_octet(vec![1, 2, 3]));
    let name = heap.intern(&"blob".encode_utf16().collect::<Vec<_>>());
    heap.set_member(global, name, blob).unwrap();
    let mut vm = Vm::with_global(&module, global);
    for iteration in 0..3 {
        let before = (heap.counts(), heap.allocation_debt());
        let VmExit::Finished(value) = run(&mut vm, &mut heap, 1) else {
            panic!("octet type")
        };
        assert_eq!(value.as_integer(), Some(2));
        if iteration > 0 {
            assert_eq!((heap.counts(), heap.allocation_debt()), before);
        }
        vm.reset();
        heap.collect(vm.roots());
    }
    drop(vm);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn invalid_property_targets_fail_before_or_during_execution() {
    for script in [
        "function f() { var x = 1; return &x; }",
        "function f() { var x; &x = 2; }",
        "&3;",
        "property p {} &p += 1;",
        "property p {} (&p)++;",
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("invalid property target", script).unwrap();
        assert!(tjs_front::compile(&sources, source).is_err(), "{script}");
    }
    for script in [
        "*3;",
        "*null;",
        "*[];",
        "*void = 1;",
        "property p { getter { return 1; } } function f() { var ref = &p; *ref = 2; } f();",
    ] {
        let module = compile(&mut SourceMap::new(), script);
        let mut heap = Heap::new();
        let mut vm = Vm::new(&module);
        assert!(
            matches!(run(&mut vm, &mut heap, 1), VmExit::Fault(_)),
            "{script}"
        );
    }
}
