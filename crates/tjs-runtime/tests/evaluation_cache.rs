use std::time::{Duration, Instant};
use tjs_core::{RunBudget, Value, Vm};
use tjs_runtime::{Runtime, RuntimeExit};

fn execute(runtime: &mut Runtime, text: &str, slice: u32, collect_every_slice: bool) -> Value {
    let source = runtime.sources.add_utf8("driver.tjs", text).unwrap();
    let module = tjs_front::compile(&runtime.sources, source).unwrap();
    let mut vm = Vm::new(&module);
    loop {
        match runtime.run_slice(&mut vm, RunBudget::new(slice).unwrap()) {
            RuntimeExit::Finished(value) => return value,
            RuntimeExit::Yielded => {
                if collect_every_slice {
                    runtime.collect(vm.roots());
                } else if runtime.allocation_debt() > 128 * 1024 || runtime.heap.is_collecting() {
                    runtime.collect_auto(vm.roots());
                }
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn cached_code_keeps_live_contexts_fresh_pools_modes_and_exceptions() {
    let text = r#"
        var globalValue=5,ok=true;
        class Reader {
            var value=0;
            function Reader(v) {value=v;}
            function read() {return 'value+globalValue'!;}
            function makeClosure() {return ('function(){return value+globalValue;}'!) incontextof this;}
        }
        var a=new Reader(3),b=new Reader(9),fa=a.makeClosure(),fb=b.makeClosure();
        var x=0;
        for(var i=0;i<12;i++) {
            globalValue++;
            a.value++; b.value+=2;
            ok=ok && a.read()==a.value+globalValue && b.read()==b.value+globalValue;
            ok=ok && fa()==a.value+globalValue && fb()==b.value+globalValue;
            var fc=a.makeClosure();
            ok=ok && fc()==a.value+globalValue;
            var p='(const)[(const)%["n"=>0]]'!;
            ok=ok && p[0].n==0; p[0].n=100;
            var r='x=4; x=99'!;
            ok=ok && r==4 && x==4;
            'x=4; x=99'!;
            ok=ok && x==99;
            try {'throw x'!;} catch(e) {ok=ok && e==99;}
        }
        ok;
    "#;
    for limit in [0, 512 * 1024] {
        for slice in [1, 10000] {
            let mut runtime = Runtime::new();
            runtime.set_compile_cache_limit(limit);
            assert_eq!(
                execute(&mut runtime, text, slice, true).as_integer(),
                Some(1)
            );
            assert!(runtime.compile_cache_bytes() <= limit);
            if limit != 0 {
                assert!(runtime.compile_cache_bytes() > 0);
            }
        }
    }
}

#[test]
fn preprocessor_mutations_and_replacement_cannot_reuse_stale_code() {
    let mut runtime = Runtime::new();
    let text = r#"
        var n=0;
        for(var i=0;i<8;i++) {
            '@if(mode==1) n+=3; @endif @if(mode==2) n+=7; @endif'!;
        }
        n;
    "#;
    for mode in [1, 2, 1] {
        runtime.preprocessor.set("mode", mode);
        assert_eq!(
            execute(&mut runtime, text, 1, true).as_integer(),
            Some(if mode == 1 { 24 } else { 56 })
        );
    }
    let mut replacement = tjs_front::Preprocessor::default();
    replacement.set("mode", 2);
    runtime.preprocessor = replacement;
    assert_eq!(execute(&mut runtime, text, 1, true).as_integer(), Some(56));

    let mut runtime = Runtime::new();
    let text = r#"
        for(var i=0;i<8;i++) {'@set(tick=tick+1) var n=0;'!;}
        1;
    "#;
    assert_eq!(execute(&mut runtime, text, 1, true).as_integer(), Some(1));
    assert_eq!(runtime.preprocessor.get("tick"), 8);
    assert_eq!(runtime.compile_cache_bytes(), 0);

    let revision = runtime.preprocessor.revision();
    execute(
        &mut runtime,
        r#"
        for(var i=0;i<8;i++) {'@set(tick=tick+1) @set(tick=tick-1) var n=0;'!;}
    "#,
        1,
        true,
    );
    assert_eq!(runtime.preprocessor.get("tick"), 8);
    assert_ne!(runtime.preprocessor.revision(), revision);
    assert_eq!(runtime.compile_cache_bytes(), 0);
}

#[test]
#[ignore = "manual repeated scenario expression throughput measurement"]
fn repeated_scenario_expressions_workload() {
    for limit in [0, 512 * 1024] {
        scenario_expressions_workload(limit);
    }
}

fn scenario_expressions_workload(limit: usize) {
    let mut times = Vec::<Duration>::new();
    for round in 0..7 {
        let mut runtime = Runtime::new();
        runtime.set_compile_cache_limit(limit);
        let started = Instant::now();
        let result = execute(
            &mut runtime,
            r#"
            var f=%["ready"=>1,"value"=>7],total=0;
            for(var i=0;i<6000;i++) {
                total+='f.ready ? f.value+3 : 0'!;
                'f.value=(f.value+1)%31'!;
            }
            total;
        "#,
            10000,
            false,
        );
        let elapsed = started.elapsed();
        let expected: i64 = (0..6000).map(|i| (7 + i) % 31 + 3).sum();
        assert_eq!(result.as_integer(), Some(expected));
        if round >= 2 {
            times.push(elapsed);
        }
    }
    times.sort();
    println!(
        "cache_limit={limit} scenario_eval_median_ms={:.3}",
        times[2].as_secs_f64() * 1000.
    );
}
