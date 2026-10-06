use tjs_core::{Heap, HeapCounts, Phase, RunBudget, SourceMap, Value, Vm, VmExit};
use tjs_front::compile;

fn evaluate(script: &str, slice: u32) -> String {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("members", script).unwrap();
    let module = compile(&sources, source).unwrap();
    let mut heap = Heap::new();
    // Runtime conversion errors install this static class. Its registered
    // functions remain roots after script teardown by the native API contract.
    tjs_core::exception::install(&mut heap).unwrap();
    let baseline = heap.collect([]).after;
    let mut vm = Vm::new(&module);
    let result = loop {
        let exit = vm.run_slice(&mut heap, RunBudget::new(slice).unwrap());
        heap.collect(vm.roots());
        match exit {
            VmExit::Yielded => assert!(vm.work_executed() < 100_000),
            VmExit::Finished(value) => break heap.display(value).unwrap(),
            exit => panic!("{script}: {exit:?}"),
        }
    };
    drop(vm);
    heap.collect([]);
    while let Some(mut finalizer) = Vm::take_finalizer(&mut heap) {
        loop {
            let exit = finalizer.run_slice(&mut heap, RunBudget::new(slice).unwrap());
            heap.collect(finalizer.roots());
            assert!(finalizer.work_executed() < 100_000, "{script}");
            match exit {
                VmExit::Yielded => {}
                VmExit::Finished(_) => break,
                exit => panic!("finalizer for {script}: {exit:?}"),
            }
        }
        drop(finalizer);
        heap.collect([]);
    }
    assert_eq!(heap.counts(), baseline, "{script}");
    result
}

#[test]
fn literals_member_reads_writes_and_deletes_survive_all_slice_boundaries() {
    for (script, expected) in [
        ("%[].missing;", "void"),
        ("%[,].missing;", "void"),
        (
            r#"var d = %[a: 1, "b" => 2, "c", 3,]; d.a + d["b"] + d.c;"#,
            "6",
        ),
        (r#"var d = %["你好😀" => "value"]; d["你好😀"];"#, "value"),
        (r#"var d = %[a: 1, a: 2]; d.a;"#, "2"),
        (
            "var d = %[child: %[value: 3]]; d.child.value = 42; d.child.value;",
            "42",
        ),
        ("var d = %[]; d.x = d.y = 7; d.x + d.y;", "14"),
        (
            "var d = %[]; d.function = 3; d.class = 4; d.null = 5; d.function + d.class + d.null;",
            "12",
        ),
        (r#"var d = %[]; d["a" + "b"] = 9; d.ab;"#, "9"),
        (
            "var d = %[x: void]; var first = delete d.x; first * 10 + (delete d.x);",
            "10",
        ),
        ("var d = %[x: 1]; delete d.x; d.x;", "void"),
        ("var d = %[x: 1]; var alias = d; alias.x = 7; d.x;", "7"),
        ("var d = %[]; d.self = d; d.self == d;", "1"),
        ("%[] == %[];", "0"),
        (r#"var d = %[]; d[-2] = 7; d["-2"];"#, "7"),
        // Numeric get/set use signed 32-bit dispatch; delete stringifies i64.
        (
            r#"var d = %[]; d[4294967296] = 7; (delete d[4294967296]) + d["0"];"#,
            "7",
        ),
        (
            r#"var d = %["4294967296" => 8]; delete d[4294967296];"#,
            "1",
        ),
        (
            r#"var d = %[]; var i = 0; while (i < 25) { d["k" + i] = i; i = i + 1; } d.k24;"#,
            "24",
        ),
    ] {
        for slice in [1, 2, 7, 10_000] {
            assert_eq!(evaluate(script, slice), expected, "slice={slice}: {script}");
        }
    }
}

#[test]
fn assignment_evaluates_rhs_then_receiver_then_key_once() {
    let record = "function touch(trace, digit, value) { trace.order = trace.order * 10 + digit; return value; }";
    for (body, expected) in [
        (
            r#"touch(t, 1, d)[touch(t, 2, "x")] = touch(t, 3, 42); t.order * 100 + d.x;"#,
            "31242",
        ),
        (r#"touch(t, 1, d)[touch(t, 2, "x")]; t.order;"#, "12"),
        (r#"delete touch(t, 1, d)[touch(t, 2, "x")]; t.order;"#, "12"),
        (
            r#"function fail(t) { t.order = 3; throw "stop"; } try { touch(t, 1, d)[touch(t, 2, "x")] = fail(t); } catch (e) {} t.order;"#,
            "3",
        ),
    ] {
        let script = format!("{record} var t = %[order: 0]; var d = %[x: 1]; {body}");
        for slice in [1, 7, 10_000] {
            assert_eq!(evaluate(&script, slice), expected);
        }
    }
}

#[test]
fn converted_keys_default_members_and_dispatch_errors_follow_reference() {
    // tjsInterCodeExec Prop*Indirect / CallFunctionIndirect / DeleteMemberIndirect
    // and tjsVariant::AsString. Collect at every slice, including accessor calls
    // and class base resolution; expected results come from those dispatch paths.
    for (script, expected) in [
        ("var d=%[1.5 => 2]; d[1.5]+=3; d[1.5]++; d['1.5'];", "6"),
        (
            "var d=%[]; d[4294967296]=1; d[4294967296.0]=2; d[0]+d['4294967296'];",
            "3",
        ),
        (
            "var a=[1,2,3]; a[1.9]=8; var n=a[1]; delete a[1.9]; n*10+a[1];",
            "83",
        ),
        (
            "var d=%[]; d[-0.0]=3; d[0.0]=4; d['-0.0']*10+d['+0.0'];",
            "34",
        ),
        (
            "var d=%[]; d[NaN]=1; d[Infinity]=2; d[-Infinity]=4; d['NaN']+d['+Infinity']+d['-Infinity'];",
            "7",
        ),
        (
            "var d=%[]; var k=%[]; k.toString=function(){throw 99;}; d[k]=8; d[null]=9; d[string k]*10+d[string null];",
            "89",
        ),
        (
            "var d=%[]; d[1.5]=function(x){return x+1;}; d[1.5](6);",
            "7",
        ),
        ("var d=%[]; d[null]=function(){return 9;}; d[null]();", "9"),
        (
            "var d=%[]; d[1.5]=4; var n=delete d[1.5]; n+','+typeof d[1.5];",
            "1,undefined",
        ),
        ("var d=%[]; (delete d[void])+','+(delete d['']);", "0,0"),
        (
            "var n=0; property p { getter { n++; return n; } setter(v) { n=v; } } var q=&p; (&q)[void]=8; (&q)['']+','+typeof (&q)[void]+','+n;",
            "9,Integer,10",
        ),
        (
            "var n=0; property p { getter { return n; } setter(v) { n=v; } } var q=&p; &(&q)[void]=7; &(&q)[''];",
            "7",
        ),
        (
            "property p { getter { throw 42; } } var q=&p; try { (&q)[void]; } catch(e) { e; }",
            "42",
        ),
        (
            "var n=1; property p { getter { return n; } setter(v) { n=v; } } var d=%[]; &d[1.5]=&p; d[1.5]+=3; typeof d[1.5]+','+n;",
            "Integer,4",
        ),
        (
            "class A {} function base(){return A;} class B extends base() {} &A[1.5]=function(){return 7;}; B[1.5]();",
            "7",
        ),
        (
            "class A {} var calls=0; function base(){calls++; return A;} class B extends base() {} calls=0; delete B[void]; calls;",
            "0",
        ),
        ("class A {} var a=new A; invalidate a; delete a[1.5];", "0"),
        // A failed conversion must happen before dispatch, so it must not write
        // the property or enter the getter/target function.
        (
            "var n=0; var d=%[]; try { d[<%01%>]=n=3; } catch(e) {} n;",
            "3",
        ),
        (
            "var d=%[]; d[1.5]=function(){throw 88;}; try { d[1.5](); } catch(e) { e; }",
            "88",
        ),
        ("'abc'[1.9]+','+<%102030%>[1.9];", "b,32"),
    ] {
        for slice in [1, 7, 10_000] {
            assert_eq!(evaluate(script, slice), expected, "slice={slice}: {script}");
        }
    }
    for operation in [
        "d[void]",
        "d['']",
        "d[void]=1",
        "typeof d[void]",
        "d[<%01%>]",
        "d[<%01%>]=1",
        "delete d[<%01%>]",
        "typeof d[<%01%>]",
        "d[<%01%>]()",
        "d[void]()",
        "d['']()",
        "'abc'[void]",
        "'abc'[null]",
        "'abc'[<%01%>]",
        "<%01%>[void]",
        "typeof 'abc'[void]",
    ] {
        let script = format!(
            "var d=%[]; var caught=0; try {{ {operation}; }} catch(e) {{ caught=1; }} caught;"
        );
        assert_eq!(evaluate(&script, 1), "1", "{operation}");
    }
    for key in ["'p'", "1.5", "null"] {
        let script = format!(
            "property p {{ setter(v) {{}} }} var d=%[]; &d[{key}]=&p; var caught=0; try {{ typeof d[{key}]; }} catch(e) {{ caught=1; }} caught;"
        );
        assert_eq!(evaluate(&script, 1), "1", "{key}");
    }
}

#[test]
fn local_addresses_and_top_level_snapshots_follow_reference_codegen() {
    for (body, local, top_level) in [
        (
            r#"var key = "before"; var d = %[key => key = "after"]; d.after;"#,
            "after",
            "void",
        ),
        (
            r#"var a = %[x: 1]; var b = %[x: 2]; a[(a = b).missing + "x"];"#,
            "2",
            "1",
        ),
        (
            r#"var x = 1; var d = %[]; var out = (d[(x = 7) + ""] = x); out * 100 + d["7"] * 10 + x;"#,
            "777",
            "117",
        ),
        // Literal construction must finish before publishing its destination.
        (
            "var d = %[old: 9]; d = %[previous: d]; d.previous.old;",
            "9",
            "9",
        ),
    ] {
        let last = body.rfind(';').unwrap();
        let start = body[..last].rfind(';').map_or(0, |p| p + 1);
        let local_script = format!(
            "function f() {{ {} return {}; }} f();",
            &body[..start],
            &body[start..last]
        );
        for slice in [1, 7, 10_000] {
            assert_eq!(evaluate(&local_script, slice), local, "{local_script}");
            assert_eq!(evaluate(body, slice), top_level, "{body}");
        }
    }
}

#[test]
fn dictionary_results_and_cycles_survive_calls_forwarding_and_unwind() {
    let script = r#"
        function make(n, payload = %[message: "hello"]) {
            if (n == 0) { payload.self = payload; throw payload; }
            return make(n - 1, payload);
        }
        function relay(n) { return make(...); }
        try { relay(30); } catch (e) { e.self.message + "!"; }
    "#;
    for slice in [1, 2, 13, 10_000] {
        assert_eq!(evaluate(script, slice), "hello!");
    }

    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8(
            "escaping",
            "var d = %[message: \"kept\"]; d.self = d; throw d;",
        )
        .unwrap();
    let module = compile(&sources, source).unwrap();
    let mut heap = Heap::new();
    let mut vm = Vm::new(&module);
    let value = loop {
        let exit = vm.run_slice(&mut heap, RunBudget::new(1).unwrap());
        heap.collect(vm.roots());
        match exit {
            VmExit::Yielded => {}
            VmExit::Thrown(exception) => break exception.value,
            exit => panic!("{exit:?}"),
        }
    };
    let root = heap.root(value);
    drop(vm);
    heap.collect([]);
    let key = Value::Str(heap.alloc_string("message".encode_utf16().collect::<Vec<_>>()));
    let message = tjs_core::member::get(&mut heap, value, key).unwrap();
    assert_eq!(heap.display(message).unwrap(), "kept");
    heap.release_root(root);
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}

#[test]
fn malformed_and_unsupported_member_operations_report_source_locations() {
    for script in [
        "%[x:];",
        "%[\"x\"];",
        "%[x: 1,,];",
        "%[1: 2];",
        "var d = %[]; d.;",
        "var d = %[]; d[];",
        "(%[] + 1) = 3;",
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("invalid", script).unwrap();
        let error = compile(&sources, source).unwrap_err();
        assert_eq!(error.phase, Phase::Parse, "{script}");
        assert!(error.span.is_some());
    }
    for (script, message) in [
        ("null.x;", "null"),
        ("delete null.x;", "null"),
        ("(7).x;", "requires an object"),
        ("%[][<%01%>];", "not implemented"),
        ("var d=%[]; d[<%01%>](7*);", "not implemented"),
        ("%[][void];", "not a property"),
        ("%[][''];", "not a property"),
        (r#"var d = %["" => 8];"#, "not a property"),
        ("void.x = 1;", "requires an object"),
        ("void.any.more;", "requires an object"),
    ] {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("fault", script).unwrap();
        let module = compile(&sources, source).unwrap();
        let mut heap = Heap::new();
        let mut vm = Vm::new(&module);
        let VmExit::Fault(error) = vm.run_slice(&mut heap, RunBudget::new(1000).unwrap()) else {
            panic!("{script}")
        };
        assert!(error.message.contains(message), "{error}");
        assert!(error.span.is_some());
    }
}

#[test]
fn wide_literals_reuse_temporaries_and_deep_member_chains_are_bounded() {
    let script = format!(
        "var d = %[{}]; d.x;",
        (0..500)
            .map(|n| format!("x: {n}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("wide", &script).unwrap();
    let module = compile(&sources, source).unwrap();
    assert!(module.register_count() < 10);
    assert_eq!(module.constants().len(), 2); // Global "d" and one shared member name "x".
    assert_eq!(evaluate(&script, 1), "499");
    let source = sources
        .add_utf8("deep", &format!("%[]{};", ".x".repeat(200)))
        .unwrap();
    let error = compile(&sources, source).unwrap_err();
    assert_eq!(error.phase, Phase::Parse);
    assert!(error.message.contains("complexity"));
}
