//! End-to-end allocation/mutation workload: gc_workload [auto|full|incremental].
use std::time::{Duration, Instant};
use tjs_core::{RunBudget, Value, Vm};
use tjs_runtime::{Runtime, RuntimeExit};
fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "auto".into());
    let mut times = Vec::new();
    let mut pauses = Vec::new();
    let mut total_gc = Duration::ZERO;
    let mut peak_objects = 0;
    for round in 0..12 {
        let mut runtime = Runtime::new();
        let source = runtime
            .sources
            .add_utf8(
                "gc workload",
                r#"
            var keep=[];
            for(var i=0;i<2000;i++) keep.add([i]);
            var total=0;
            for(var i=0;i<100000;i++) {
                var a=[i,i+1];
                keep[i%2000]=a;
                total+=a[0];
            }
            total;
        "#,
            )
            .unwrap();
        let module = tjs_front::compile(&runtime.sources, source).unwrap();
        let mut vm = Vm::new(&module);
        let started = Instant::now();
        loop {
            let exit = runtime.run_slice(&mut vm, RunBudget::new(2000).unwrap());
            peak_objects = peak_objects.max(runtime.heap.counts().objects);
            let done = match exit {
                RuntimeExit::Finished(value) => {
                    assert!(matches!(value, Value::Int(4_999_950_000)));
                    true
                }
                RuntimeExit::Yielded => false,
                other => panic!("{other:?}"),
            };
            if done || runtime.heap.is_collecting() || runtime.allocation_debt() >= 256 * 1024 {
                let gc_started = Instant::now();
                if done || mode == "full" {
                    runtime.collect(vm.roots());
                } else if mode == "auto" {
                    runtime.collect_auto(vm.roots());
                } else {
                    let budget = 512 + (runtime.allocation_debt() / 256).min(3584);
                    runtime.collect_step(vm.roots(), budget);
                }
                if round >= 2 {
                    let pause = gc_started.elapsed();
                    pauses.push(pause);
                    total_gc += pause;
                }
            }
            if done {
                break;
            }
        }
        if round >= 2 {
            times.push(started.elapsed());
        }
    }
    times.sort();
    pauses.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "mode={} runtime_median_ms={:.3} gc_mean_ms={:.3} pause_p95_ms={:.3} pause_max_ms={:.3} peak_objects={}",
        mode,
        ms(times[times.len() / 2]),
        ms(total_gc) / times.len() as f64,
        ms(pauses[(pauses.len() - 1) * 95 / 100]),
        ms(*pauses.last().unwrap()),
        peak_objects
    );
}
