//! Run with --release: gc_profile [full|incremental] [objects] [rounds] [budget].
//! Synthetic ring + short-lived containers; timings are diagnostic, not tests.
use std::{
    cell::RefCell,
    hint::black_box,
    rc::Rc,
    time::{Duration, Instant},
};
use tjs_core::{Heap, ObjId, Trace, Value};

struct Edges(Rc<RefCell<Vec<Value>>>);
impl Trace for Edges {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for &value in self.0.borrow().iter() {
            visit(value);
        }
    }
}
fn object(id: ObjId) -> Value {
    Value::Obj(id.into())
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let incremental = args.get(1).is_none_or(|v| v == "incremental");
    let count = args
        .get(2)
        .map_or(20_000, |v| v.parse::<usize>().unwrap())
        .max(2);
    let rounds = args
        .get(3)
        .map_or(30, |v| v.parse::<usize>().unwrap())
        .max(1);
    let budget = args
        .get(4)
        .map_or(512, |v| v.parse::<usize>().unwrap())
        .max(1);
    let mut heap = Heap::new();
    let live: Vec<_> = (0..count).map(|_| heap.alloc_array()).collect();
    for (i, &id) in live.iter().enumerate() {
        heap.array_push(id, object(live[(i + 1) % count])).unwrap();
        let text = heap.alloc_string(vec![65; 16]);
        heap.array_push(id, Value::Str(text)).unwrap();
        if i % 10 == 0 {
            heap.initialize_native_state(
                id,
                Edges(Rc::new(RefCell::new(vec![object(live[(i + 7) % count])]))),
            )
            .unwrap();
        }
    }
    let roots = [object(live[0])];
    let mut pauses = Vec::new();
    let mut cycles = Vec::new();
    let mut mutation_time = Duration::ZERO;
    for round in 0..rounds + 5 {
        let started = Instant::now();
        for i in 0..count {
            let dead = heap.alloc_array();
            let text = heap.alloc_string(vec![66; 16]);
            heap.array_push(dead, Value::Str(text)).unwrap();
            heap.array_set(live[i], 0, object(live[(i + 1) % count]))
                .unwrap();
        }
        if round >= 5 {
            mutation_time += started.elapsed();
        }
        let mut elapsed = Duration::ZERO;
        loop {
            let started = Instant::now();
            let completed = if incremental {
                heap.collect_step(roots, budget).completed
            } else {
                Some(heap.collect(roots))
            };
            let pause = started.elapsed();
            elapsed += pause;
            if round >= 5 {
                pauses.push(pause);
            }
            if let Some(stats) = completed {
                assert_eq!(stats.after.objects, count);
                assert_eq!(stats.after.strings, count);
                black_box(stats);
                break;
            }
        }
        if round >= 5 {
            cycles.push(elapsed);
        }
    }
    pauses.sort();
    cycles.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "mode={} objects={} rounds={} budget={} slices={} pause_median_ms={:.3} pause_p95_ms={:.3} pause_max_ms={:.3} cycle_median_ms={:.3} mutation_mean_ms={:.3}",
        if incremental { "incremental" } else { "full" },
        count,
        rounds,
        budget,
        pauses.len(),
        ms(pauses[pauses.len() / 2]),
        ms(pauses[(pauses.len() - 1) * 95 / 100]),
        ms(*pauses.last().unwrap()),
        ms(cycles[cycles.len() / 2]),
        ms(mutation_time) / rounds as f64
    );
}
