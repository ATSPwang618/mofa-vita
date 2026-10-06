use tjs_core::{Heap, Module, ObjId, RunBudget, SourceMap, Value, Vm, VmExit};

fn compile(script: &str) -> Module {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("lifecycle", script).unwrap();
    tjs_front::compile(&sources, source).unwrap_or_else(|error| panic!("{script}: {error}"))
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

fn evaluate(heap: &mut Heap, global: ObjId, script: &str) -> Value {
    let mut vm = Vm::with_global(&compile(script), global);
    let exit = run(&mut vm, heap, 1);
    let VmExit::Finished(value) = exit else {
        panic!("{script}: {exit:?}")
    };
    value
}

fn check(script: &str, expected: &str) {
    let module = compile(script);
    for slice in [1, 10_000] {
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&module);
        let exit = run(&mut vm, &mut heap, slice);
        let VmExit::Finished(value) = exit else {
            panic!("{script}: {exit:?}")
        };
        assert_eq!(heap.display(value).unwrap(), expected, "{script}");
    }
}

#[test]
fn validity_operators_follow_value_and_object_rules() {
    for (script, expected) in [
        (
            "isvalid void + isvalid 2 + ('x' isvalid) + (1.5 isvalid);",
            "4",
        ),
        (
            "(invalidate void) + (invalidate 2) + (invalidate 'x');",
            "0",
        ),
        ("var d = %[]; invalidate d isvalid; isvalid d;", "1"),
        (
            "var d = %[]; (invalidate d) * 100 + (isvalid d) * 10 + (invalidate d);",
            "100",
        ),
        (
            "var d = %[]; invalidate (d incontextof %[]); d isvalid;",
            "0",
        ),
        (
            "var a = []; (invalidate a.push) * 10 + isvalid a.push;",
            "1",
        ),
        (
            "var a = []; var p = &a.count; (invalidate &p) * 10 + isvalid &p;",
            "1",
        ),
        (
            "var reads = 0; property p { getter { reads++; return %[]; } } (invalidate p) * 10 + reads;",
            "11",
        ),
    ] {
        check(script, expected);
    }
    for script in ["isvalid null;", "null isvalid;", "invalidate null;"] {
        let mut heap = Heap::new();
        let mut vm = Vm::new(&compile(script));
        let VmExit::Fault(error) = run(&mut vm, &mut heap, 1) else {
            panic!("{script}")
        };
        assert!(error.message.contains("null"), "{error}");
    }
}

#[test]
fn explicit_finalize_runs_before_invalidation_and_allows_reentry() {
    for (script, expected) in [
        (
            "var log = ''; class C { var n = 7; function finalize() { global.log += n + ':' + (isvalid this) + ':' + (invalidate this); n = 9; } } var c = new C(); invalidate c; log + ':' + (isvalid c) + ':' + (invalidate c);",
            "7:1:1:0:0",
        ),
        (
            "var log = 0; class C { var n = 8; function finalize() { global.log = n; } } var c = new C(); invalidate (c incontextof %[n: 2]); log;",
            "8",
        ),
        (
            "var log = ''; class C { property finalize { getter { global.log += 'g'; return function() { global.log += 'f'; }; } } } var c = new C(); invalidate c; log + (isvalid c);",
            "gf0",
        ),
        (
            "class C { property finalize { setter(v) {} } } var c = new C(); invalidate c; isvalid c;",
            "0",
        ),
        (
            "class C { var finalize = 3; } var c = new C(); invalidate c; isvalid c;",
            "0",
        ),
        (
            "class C { var finalize = %[]; } var c = new C(); invalidate c; isvalid c;",
            "0",
        ),
        (
            "class C { var finalize = null; } var c = new C(); invalidate c; isvalid c;",
            "0",
        ),
        (
            "class C {} function f() { throw 1; } invalidate f; var c = new C(); c.finalize = f; invalidate c; isvalid c;",
            "0",
        ),
        (
            "class C { function finalize() { try { throw 3; } catch(e) { this.n = e; } } } var c = new C(); invalidate c; isvalid c;",
            "0",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn throws_leave_objects_valid_and_explicit_finalization_can_retry() {
    for (script, expected) in [
        (
            "var calls = 0, caught = 0; class C { var n = 7; function finalize() { global.calls++; if (global.calls == 1) throw n; } } var c = new C(); try { invalidate c; } catch(e) { caught = e; } var live = isvalid c; invalidate c; caught * 100 + live * 10 + calls;",
            "712",
        ),
        (
            "var caught = 0; class C { property finalize { getter { throw 4; } } } var c = new C(); try { invalidate c; } catch(e) { caught = e; } &c.finalize = function() {}; invalidate c; caught * 10 + (isvalid c);",
            "40",
        ),
        (
            "var log = 0; class C { function finalize() { global.log++; throw 1; } } var c = new C(); try { invalidate c; } catch(e) {} try { invalidate c; } catch(e) {} log * 10 + (isvalid c);",
            "21",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn special_objects_skip_script_finalizers_and_invalid_properties_become_values() {
    for (script, expected) in [
        (
            "var n = 0; function f() {} class C {} var a = [], d = %[]; a.finalize = d.finalize = C.finalize = f.finalize = Array.finalize = function() { global.n++; }; invalidate a; invalidate d; invalidate C; invalidate f; invalidate Array; n + (isvalid a) + (isvalid d) + (isvalid C) + (isvalid f) + (isvalid Array);",
            "0",
        ),
        (
            "function f() { invalidate f; return 8; } f() * 10 + (isvalid f);",
            "80",
        ),
        (
            "property p { getter { throw 1; } setter(v) { throw 2; } } var ref = &p; invalidate &ref; var d = %[]; &d.p = ref; var t = typeof d.p; d.p = 8; t + ':' + d.p;",
            "Object:8",
        ),
        (
            "var d = %[]; invalidate d; (delete d.x) + ':' + (d instanceof 'Object');",
            "0:1",
        ),
    ] {
        check(script, expected);
    }
    for script in [
        "var a = []; var push = a.push; invalidate a; push(1);",
        "var d = %[]; var clear = Dictionary.clear incontextof d; invalidate d; clear();",
        "function f() {} invalidate f; f();",
        "class C {} invalidate C; new C();",
        "var d = %[]; invalidate d; d.x;",
        "var d = %[]; invalidate d; d.x = 3;",
        "var d = %[]; invalidate d; d.f();",
        "var d = %[]; invalidate d; typeof d.x;",
        "var d = %[]; invalidate d; typeof d['x'];",
        "var d = %[]; invalidate d; typeof d[0];",
        "var d = %[]; invalidate d; d instanceof 'Dictionary';",
        "property p { getter { return 1; } } var ref = &p; invalidate &ref; *ref;",
    ] {
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&compile(script));
        let VmExit::Fault(error) = run(&mut vm, &mut heap, 1) else {
            panic!("{script}")
        };
        assert!(error.message.contains("invalidated"), "{script}: {error}");
    }
}

#[test]
fn cancelled_or_failed_finalizers_release_the_reentry_guard() {
    for cancel in ["drop", "reset", "throw", "fault"] {
        let mut heap = Heap::new();
        let global = heap.alloc_global();
        let global_root = heap.root(Value::Obj(global.into()));
        evaluate(
            &mut heap,
            global,
            "var entered = 0; class C { function finalize() { global.entered++; while (1) {} } } var c = new C();",
        );
        if cancel == "throw" {
            evaluate(&mut heap, global, "c.finalize = function() { throw 9; };");
        }
        if cancel == "fault" {
            evaluate(&mut heap, global, "c.finalize = function() { missing(); };");
        }
        let mut vm = Vm::with_global(&compile("invalidate c;"), global);
        let mut held = Vec::new();
        if matches!(cancel, "drop" | "reset") {
            for _ in 0..100 {
                assert!(matches!(
                    vm.run_slice(&mut heap, RunBudget::new(1).unwrap()),
                    VmExit::Yielded
                ));
                heap.collect(vm.roots());
            }
            held.extend(vm.roots().map(|value| heap.root(value)));
            assert_eq!(
                evaluate(&mut heap, global, "entered;").as_integer(),
                Some(1)
            );
            // Another VM sees the same in-progress object but cannot call it twice.
            assert_eq!(
                evaluate(&mut heap, global, "invalidate c;").as_integer(),
                Some(1)
            );
            if cancel == "reset" {
                vm.reset();
            }
        } else {
            assert!(matches!(
                run(&mut vm, &mut heap, 1),
                VmExit::Fault(_) | VmExit::Thrown(_)
            ));
        }
        if cancel == "drop" {
            drop(vm);
        } else {
            held.extend(vm.roots().map(|value| heap.root(value)));
            // Keep the terminal/reset VM alive to prove cancellation is immediate.
            assert_eq!(evaluate(&mut heap, global, "c.finalize = function() { global.entered = 7; }; invalidate c; entered * 10 + (isvalid c);").as_integer(), Some(70));
            drop(vm);
            for root in held {
                heap.release_root(root);
            }
            heap.release_root(global_root);
            continue;
        }
        assert_eq!(evaluate(&mut heap, global, "c.finalize = function() { global.entered = 7; }; invalidate c; entered * 10 + (isvalid c);").as_integer(), Some(70));
        for root in held {
            heap.release_root(root);
        }
        heap.release_root(global_root);
    }
}

#[test]
fn gc_queues_entire_cycles_retains_edges_and_marks_resurrection() {
    let mut heap = Heap::new();
    let global = heap.alloc_global();
    let root = heap.root(Value::Obj(global.into()));
    evaluate(
        &mut heap,
        global,
        "var count = 0, total = 0, saved; class C { var n; function C(v) { n = v; } function finalize() { global.count++; global.total += n; global.saved = this; } } function make() { var a = new C(3), b = new C(8); a.peer = b; b.peer = a; } make();",
    );
    heap.collect([Value::Obj(global.into())]);
    assert_eq!(heap.pending_finalizers(), 2);
    assert_eq!(evaluate(&mut heap, global, "count;").as_integer(), Some(0));
    // Queue roots keep both objects and both callbacks alive across repeated GC.
    heap.collect([]);
    let mut attempted = 0;
    while let Some(mut vm) = Vm::take_finalizer(&mut heap) {
        assert!(matches!(
            run(&mut vm, &mut heap, 1),
            VmExit::Finished(Value::Int(1))
        ));
        attempted += 1;
    }
    assert_eq!(attempted, 2);
    heap.collect([]);
    assert_eq!(
        evaluate(
            &mut heap,
            global,
            "count * 100 + total * 10 + (isvalid saved);"
        )
        .as_integer(),
        Some(310)
    );
    heap.release_root(root);
    assert_eq!(heap.collect([]).after, tjs_core::HeapCounts::default());
}

#[test]
fn automatic_attempts_do_not_repeat_after_throw_or_cancel_but_explicit_retry_works() {
    for cancel in [false, true] {
        let mut heap = Heap::new();
        let global = heap.alloc_global();
        let root = heap.root(Value::Obj(global.into()));
        evaluate(
            &mut heap,
            global,
            "var saved, entered = 0; class C { function finalize() { global.saved = this; global.entered++; throw 7; } } function make() { new C(); } make();",
        );
        heap.collect([]);
        assert_eq!(heap.pending_finalizers(), 1);
        let mut vm = Vm::take_finalizer(&mut heap).unwrap();
        if cancel {
            // Stop after saved is installed but before the throw.
            let saved = heap.intern(&"saved".encode_utf16().collect::<Vec<_>>());
            while matches!(heap.member(global, saved).unwrap(), Some(Value::Void)) {
                assert!(matches!(
                    vm.run_slice(&mut heap, RunBudget::new(1).unwrap()),
                    VmExit::Yielded
                ));
                heap.collect(vm.roots());
            }
        } else {
            assert!(matches!(run(&mut vm, &mut heap, 1), VmExit::Thrown(_)));
        }
        drop(vm);
        heap.collect([]);
        assert_eq!(heap.pending_finalizers(), 0);
        assert_eq!(
            evaluate(&mut heap, global, "isvalid saved;").as_integer(),
            Some(1)
        );
        assert_eq!(
            evaluate(
                &mut heap,
                global,
                "saved.finalize = function() {}; invalidate saved; isvalid saved;"
            )
            .as_integer(),
            Some(0)
        );
        heap.release_root(root);
        assert_eq!(heap.collect([]).after, tjs_core::HeapCounts::default());
    }
}
