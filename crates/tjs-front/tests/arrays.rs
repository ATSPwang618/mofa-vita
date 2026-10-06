use tjs_core::{Heap, Module, Phase, RunBudget, SourceMap, Value, Vm, VmExit, VmLimits};
use tjs_front::compile;

fn module(script: &str) -> Module {
    let mut sources = SourceMap::new();
    let id = sources.add_utf8("arrays", script).unwrap();
    compile(&sources, id).unwrap()
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
    let module = module(script);
    for slice in [1, 10_000] {
        let mut heap = tjs_bind::new_heap();
        let baseline = heap.collect([]).after;
        let mut vm = Vm::new(&module);
        let exit = run(&mut vm, &mut heap, slice);
        let VmExit::Finished(value) = exit else {
            panic!("{script}: {exit:?}")
        };
        assert_eq!(heap.display(value).unwrap(), expected, "{script}");
        drop(vm);
        assert_eq!(heap.collect([]).after, baseline);
    }
}

#[test]
fn array_literals_indices_lengths_and_deletion_follow_tjs_semantics() {
    for (script, expected) in [
        ("[].count;", "0"),
        ("[,].count;", "2"),
        ("[1,].length;", "2"),
        ("[, 2,,].count;", "4"),
        ("var a = [1, [2, 3], %[x: 4]]; a[1][-1] + a[2].x;", "7"),
        ("var a = [1, 2]; a[-3];", "void"),
        ("var a = [1, 2]; a[99];", "void"),
        ("var a = []; a[3] = 7; a.count * 10 + a[-1];", "47"),
        ("var a = []; a[3] = 7; a[1];", "void"),
        (
            "var a = [1, 2, 3]; a[-1] = 9; delete a[0]; a[0] * 100 + a[1] * 10 + a.count;",
            "292",
        ),
        ("var a = [1]; (delete a[-2]) + (delete a[9]);", "0"),
        (
            "var a = [1, 2, 3]; a.length = 1; a.count = 3; a[1];",
            "void",
        ),
        (
            "var a = [1, 2]; delete a.count; a.count = 9; a.count * 10 + a.length;",
            "92",
        ),
        (
            "var a = []; (delete a.length) * 10 + (delete a.length);",
            "10",
        ),
        ("var a = [4, 7]; a[\" + 01.9.. \"];", "7"),
        ("var a = [4, 7]; a[\" - 1 \"];", "7"),
        ("var a = [4, 7]; a[4294967296];", "4"),
        (
            "var a = [4]; a.name = \"array\"; a[\"1e0\"] = 7; a.name + a[\"1e0\"];",
            "array7",
        ),
        ("var a = [1]; a = [a]; a[0][0];", "1"),
        (
            "function f() { var x = 1; var a = [x, x = 7]; return a[0] * 10 + a[1]; } f();",
            "17",
        ),
        (
            // Native property closures bind to a at construction; indexing uses b.
            "var a = [1]; var b = [7, 8]; (a incontextof b)[0] * 10 + (a incontextof b).count;",
            "71",
        ),
        (
            "var a = [1]; var b = [7]; (a incontextof b)[0] = 9; a[0] * 10 + b[0];",
            "19",
        ),
        (
            "function f() { return this.tag; } var a = [f incontextof null]; a.tag = 7; a[0]();",
            "7",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn named_rest_arrays_spreads_and_unnamed_forwarding_preserve_raw_arguments() {
    for (script, expected) in [
        (
            "function f(head, tail*) { return tail.count * 100 + tail[0] * 10 + tail[1]; } f(0, 2, 3);",
            "223",
        ),
        ("function f(head, tail*) { return tail.count; } f();", "0"),
        ("function f(tail*) { return tail; } f(1,,3)[1];", "void"),
        (
            "function pack(a, b) { return a * 10 + b; } pack([2, 3]*);",
            "23",
        ),
        (
            "function pack(a, b) { return a * 10 + b; } pack(2 * 3, [7]*);",
            "67",
        ),
        (
            "function f(args*) { return args[0]; } var a = []; f((a = [7])*);",
            "7",
        ),
        (
            "function f(values*) { var i = 0; var sum = 0; while (i < values.count) { sum = sum + values[i]; i = i + 1; } return sum; } f([1,2]*, 3, [4,5]*);",
            "15",
        ),
        (
            "function sink(args*) { return args; } function relay(head = 9, tail*) { head = 8; tail[0] = 99; return sink(...); } relay(void, 2)[1];",
            "2",
        ),
        (
            "function sink(args*) { return args; } function relay(head, *) { head = 99; return sink(7, *, 8); } relay(1, 2, 3)[2];",
            "3",
        ),
        (
            "function sink(args*) { return args.count; } function relay(head, *) { return sink(*); } relay();",
            "0",
        ),
        (
            "function sink(args*) { return args.count; } function relay(head, tail*) { return sink(*); } relay(1, 2, 3);",
            "3",
        ),
        (
            "function sink(args*) { return args.count; } sink([]*, []*);",
            "0",
        ),
        (
            "var rest = [7,8]; function f(a = rest.count, rest*) { return a * 10 + rest.count; } f();",
            "20",
        ),
        (
            "function f(args*) { return args[0]; } var a = [1]; var b = [7]; f((a incontextof b)*);",
            "1",
        ),
        (
            "function pack(a, b) { return a * 10 + b; } var a = [1]; pack(a*, a[0] = 9);",
            "99",
        ),
        (
            "function pack(a, b) { return a * 10 + b; } function test() { var a = [1]; return pack(a*, (a = [7])[0]); } test();",
            "77",
        ),
        (
            "function pack(a, b) { return a * 10 + b; } var a = [1]; pack(a*, (a = [7])[0]);",
            "17",
        ),
        (
            "var a = [1]; function sink(x) { return x; } function factory() { a[0] = 9; return sink; } factory()(a*);",
            "9",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn arrays_survive_recursive_variadic_calls_unwind_and_collection() {
    check(
        r#"
        function relay(n, values*) {
            if (n == 0) { values[1] = values; throw values; }
            return relay(n - 1, values*);
        }
        try { relay(20, %[text: "kept"]); }
        catch (e) { var saved = e[1][0]; e.length = 0; saved.text; }
    "#,
        "kept",
    );
    let values = (0..500)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let script = format!("var a = [{values}]; a.count + a[-1];");
    assert!(module(&script).register_count() < 12);
    check(&script, "999");
}

#[test]
fn invalid_arrays_and_expansion_limits_fail_without_partial_call_frames() {
    for script in [
        "var a = []; a[-1] = 1;",
        "[].unknown;",
        "var a = []; a.length = %[];",
        "([1] incontextof null)[0];",
        "function f() {} f(7*);",
        "void.m(7*);",
    ] {
        let module = module(script);
        let mut heap = tjs_bind::new_heap();
        let baseline = heap.collect([]).after;
        let mut vm = Vm::new(&module);
        let VmExit::Fault(error) = run(&mut vm, &mut heap, 1) else {
            panic!("{script}")
        };
        assert_eq!(error.phase, Phase::Runtime);
        drop(vm);
        assert_eq!(heap.collect([]).after, baseline);
    }
    let module = module("function f(args*) {} var a = []; a.count = 100; f(a*, a*);");
    let limits = VmLimits {
        max_call_depth: 8,
        max_stack_values: 150,
    };
    let mut vm = Vm::with_limits(&module, limits).unwrap();
    let mut heap = tjs_bind::new_heap();
    let VmExit::Fault(error) = run(&mut vm, &mut heap, 1) else {
        panic!()
    };
    assert!(error.message.contains("value stack"));
    assert_eq!(vm.call_depth(), 1);
    assert!(vm.stack_value_count() <= limits.max_stack_values);
    assert!(!matches!(vm.registers()[0], Value::Obj(_)));
}
