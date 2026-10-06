use tjs_core::{Heap, HeapCounts, Module, Phase, RunBudget, SourceMap, Value, Vm, VmExit};

fn compile(sources: &mut SourceMap, script: &str) -> Module {
    let source = sources.add_utf8("function forms", script).unwrap();
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
        let VmExit::Finished(value) = run(&mut vm, &mut heap, slice) else {
            panic!("{script}: {:?}", vm.diagnostic("failed"))
        };
        assert_eq!(heap.display(value).unwrap(), expected, "{script}");
        drop(vm);
        assert_eq!(heap.collect([]).after, HeapCounts::default());
    }
}

#[test]
fn expressions_optional_parentheses_annotations_and_arguments_execute_together() {
    for (script, expected) in [
        (
            "var twice = function(x) { return x * 2; }; twice(21);",
            "42",
        ),
        ("(function { return 7; })();", "7"),
        ("function f { return 8; } f();", "8"),
        ("function(x) { return x + 1; }(3);", "4"),
        (
            "var d = %[f: function(a = 5, tail*) { return a + tail[0]; }]; d.f(void, 7);",
            "12",
        ),
        (
            "function apply(f = function(x) { return x * 2; }, n = 6) { return f(n); } apply();",
            "12",
        ),
        (
            "var relay = function(head, *) { return (function(a,b) { return a * 10 + b; })(*); }; relay(9, 2, 3);",
            "23",
        ),
        (
            "var f = function(a = 9) { return (function(b = 4) { return b; })(...); }; f();",
            "4",
        ),
        (
            r#"var f: Callable = function(x: int): string { return x; }; f("kept");"#,
            "kept",
        ),
        (
            "function f: int { var a: octet, b: void, c: real = 3; return c; } f();",
            "3",
        ),
        ("(function() { 42; })();", "void"),
        ("function f(a,a,b) { return a * 10 + b; } f(1,2,3);", "12"),
        (
            "function f(a = 1, a = 2, b = 3) { return a * 10 + b; } f();",
            "12",
        ),
        ("function f(a, a*) { return a; } f(4,5,6);", "4"),
    ] {
        check(script, expected);
    }
}

#[test]
fn declarations_follow_context_registration_and_source_order() {
    for (script, expected) in [
        ("var n = f(); if(0) { function f() { return 7; } } n;", "7"),
        (
            "function f() { return 1; } { function f() { return 2; } } f();",
            "2",
        ),
        (
            "function outer() { function inner(x) { return x + 1; } return inner(4); } outer();",
            "5",
        ),
        (
            "function outer() { if(0) { function inner() { return 7; } } } outer.inner();",
            "7",
        ),
        (
            "function outer() { function inner() { return 1; } var a = inner(); function inner() { return 2; } return a * 10 + inner(); } outer() * 10 + outer.inner();",
            "122",
        ),
        (
            "function outer() { function middle() { function leaf() { return 7; } return leaf(); } return middle; } outer().leaf() + outer.middle();",
            "14",
        ),
        (
            "function f() { function g() { return 2; } var before = g(); { function g() { return 7; } before += g(); } return before + g(); } f();",
            "11",
        ),
        // The reference does not register named declarations under ctExprFunction.
        (
            "function g() { return 9; } (function() { function g() { return 2; } return g(); })();",
            "9",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn functions_use_call_context_without_capturing_outer_locals_or_with() {
    for (script, expected) in [
        (
            "var x = 7; function make() { var x = 2; return function() { return x; }; } make()();",
            "7",
        ),
        (
            "var x = 7; function make() { var x = 2; function inner() { return x; } return inner; } make()();",
            "7",
        ),
        (
            "var x = 7, f; with(%[x: 2]) { f = function { return .x; }; } f();",
            "7",
        ),
        (
            "var x = 1; var d = %[x: 3, f: function { return this.x; }]; d.f();",
            "3",
        ),
        (
            "var x = 1; var d = %[x: 3, f: function { return this.x; }]; var f = d.f; f();",
            "1",
        ),
        (
            "function make(n) { return function { return this.value; } incontextof %[value: n]; } var a = make(2), b = make(7); a() * 10 + b();",
            "27",
        ),
        (
            "function make() { function inner() { return this.x; } return inner; } var d = %[x: 4]; var f = (make incontextof d)(); (f incontextof %[x: 8])();",
            "8",
        ),
        (
            "var fact = function(self,n) { if(n < 2) return 1; return n * self(self,n-1); }; fact(fact,6);",
            "720",
        ),
        (
            "var n = 0; try { (function { throw %[x: 7]; })(); } catch(e) { n = e.x; } n;",
            "7",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn bare_declarations_reserve_slots_without_hoisting_name_visibility() {
    for (script, expected) in [
        ("if(1) var x = 7; x;", "7"),
        ("function f() { if(0) var x = 7; return x; } f();", "void"),
        (
            "var x = 7; function f() { var before = x; if(0) var x = 2; return before * 10 + (x === void); } f();",
            "71",
        ),
        (
            "function f(flag) { if(flag) var x = 2; else x = 7; return x; } f(1) * 10 + f(0);",
            "27",
        ),
        (
            "function f() { var x = 9; if(0) var x = 2; return x; } f();",
            "9",
        ),
        (
            "function f() { while(0) var x = 2; return x; } f();",
            "void",
        ),
        (
            "function f() { var i = 0; while(i++ < 3) var x = i; return x; } f();",
            "3",
        ),
        (
            "function f() { do var x = 7; while(0); return x; } f();",
            "7",
        ),
        (
            "function f() { var n = 0; for(var i = 0; i < 3; i++, n += x) var x = i; return n; } f();",
            "3",
        ),
        (
            "function f(flag) { if(flag) function inner() { return 7; } if(flag) return inner(); return inner; } f(1);",
            "7",
        ),
        (
            "function f(flag) { if(flag) function inner() { return 7; } return inner; } f(0);",
            "void",
        ),
        ("var x = 9; with(null) if(1) var x = 2; x;", "9"),
        (
            "var x = 9, n = 0; switch(1) { case 1: if(0) var x = 2; n += (x === void); case 2: n += x; } n;",
            "10",
        ),
        (
            "function fail() { throw 7; } function f() { try var x = fail(); catch(e) {} return x; } f();",
            "void",
        ),
        ("var x = 9; try throw 7; catch(e) var x = e; x;", "9"),
        (
            "var n = 0; for(var i = 0; i < 3; i++) { if(i == 0) var x = 7; n += (x === void); } n;",
            "2",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn escaped_literals_keep_identity_members_code_and_bound_objects_across_vms() {
    let mut sources = SourceMap::new();
    let mut heap = Heap::new();
    let module = compile(
        &mut sources,
        "function factory() { return function { return this.value; }; } var f = factory(); f.tag = 7; %[factory: factory, original: f, bound: f incontextof %[value: 23]];",
    );
    let mut creator = Vm::new(&module);
    let VmExit::Finished(exports) = run(&mut creator, &mut heap, 1) else {
        panic!("creator failed")
    };
    let root = heap.root(exports);
    drop(creator);
    drop(module);
    heap.collect([]);
    let global = heap.alloc_global();
    let name = heap.intern(&"imported".encode_utf16().collect::<Vec<_>>());
    heap.set_member(global, name, exports).unwrap();
    heap.release_root(root);
    let module = compile(
        &mut sources,
        "var f = imported.factory(); (f === imported.original) * 100 + f.tag * 10 + imported.bound();",
    );
    let mut caller = Vm::with_global(&module, global);
    assert!(matches!(
        run(&mut caller, &mut heap, 1),
        VmExit::Finished(Value::Int(193))
    ));
    drop(caller);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn each_function_has_independent_control_flow_and_parser_nesting_is_bounded() {
    for script in [
        "while(1) { var f = function { break; }; break; }",
        "switch(1) { case 1: function f() { continue; } }",
        "with(null) { var f = function { case 1: return 1; }; }",
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("invalid context", script).unwrap();
        assert_eq!(
            tjs_front::compile(&sources, source).unwrap_err().phase,
            Phase::Compile
        );
    }
    for script in [
        format!("{}1;{}", "function f() {".repeat(1000), "}".repeat(1000)),
        format!(
            "{}1;{}",
            "var f = function {".repeat(1000),
            "};".repeat(1000)
        ),
        format!(
            "{}1;{}",
            "var f = function(a = function {".repeat(1000),
            "}) {};".repeat(1000)
        ),
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("deep functions", &script).unwrap();
        assert_eq!(
            tjs_front::compile(&sources, source).unwrap_err().phase,
            Phase::Parse
        );
    }
    // A local declaration has no binding in the surrounding script after the block.
    let mut sources = SourceMap::new();
    let module = compile(
        &mut sources,
        "function f() { { function inner() {} } return inner; } f();",
    );
    assert!(matches!(
        run(&mut Vm::new(&module), &mut Heap::new(), 1),
        VmExit::Fault(_)
    ));
}
