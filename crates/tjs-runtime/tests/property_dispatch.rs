use tjs_core::{RunBudget, Value, Vm};
use tjs_runtime::{Runtime, RuntimeExit};

fn run(text: &str, slice: u32, collect: bool) -> Value {
    let mut runtime = Runtime::new();
    let source = runtime.sources.add_utf8("properties.tjs", text).unwrap();
    let module = tjs_front::compile(&runtime.sources, source).unwrap();
    let mut vm = Vm::new(&module);
    loop {
        match runtime.run_slice(&mut vm, RunBudget::new(slice).unwrap()) {
            RuntimeExit::Finished(value) => return value,
            RuntimeExit::Yielded => {
                if collect {
                    runtime.collect(vm.roots());
                }
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn resolved_properties_keep_context_callbacks_and_exceptions_across_gc() {
    for slice in [1, 10000] {
        let result = run(
            r#"
            var gets=0,sets=0;
            class Base {
                var stored=3;
                property value {
                    getter {gets++;return stored;}
                    setter(v) {sets++;stored=v;}
                }
                property callable {
                    getter {gets++;return function(n){return this.stored+n;};}
                }
                property failure {
                    getter {gets++;throw 17;}
                    setter(v) {sets++;throw v;}
                }
            }
            class Derived extends Base {}
            function check() {
            var a=new Derived(),b=new Derived();
            a.value=9;
            var p=(&a.value) incontextof b;
            *p=5;
            var ok=a.value==9 && b.value==5 && typeof a.value=='Integer';
            ok=ok && a.callable(2)==11;
            var caught=0;
            try {a.failure;} catch(e) {caught+=e;}
            try {a.failure=23;} catch(e) {caught+=e;}
            return ok && caught==40 && gets==5 && sets==3;
            }
            check();
            "#,
            slice,
            true,
        );
        assert_eq!(result.as_integer(), Some(1), "slice={slice}");
    }
}

#[test]
#[ignore = "manual property dispatch throughput measurement"]
fn property_access_workload() {
    let mut elapsed = Vec::new();
    for round in 0..7 {
        let started = std::time::Instant::now();
        let result = run(
            r#"
            class Cell {
                var stored=0;
                property value {
                    getter {return stored;}
                    setter(v) {stored=v;}
                }
            }
            var c=new Cell(),total=0;
            for(var i=0;i<100000;i++) {c.value=i;total+=c.value;}
            total;
            "#,
            100000,
            false,
        );
        assert_eq!(result.as_integer(), Some(4_999_950_000));
        if round >= 2 {
            elapsed.push(started.elapsed());
        }
    }
    elapsed.sort();
    println!(
        "property_workload_median_ms={:.3}",
        elapsed[elapsed.len() / 2].as_secs_f64() * 1000.
    );
}
