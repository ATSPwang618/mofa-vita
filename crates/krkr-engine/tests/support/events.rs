use krkr_engine::{Engine, EngineEvent, TimerLimits};
use std::{cell::Cell, num::NonZeroUsize, rc::Rc, time::Duration};
use tjs_core::RunBudget;
use tjs_runtime::{Runtime, RuntimeExit, clock::Clock};

#[derive(Clone, Default)]
pub struct Manual(pub Rc<Cell<Duration>>);
impl Clock for Manual {
    fn now(&self) -> Duration {
        self.0.get()
    }
}
pub fn setup(runtime: Runtime, limits: TimerLimits) -> (Engine<Manual>, Manual) {
    let now = Manual::default();
    let engine = Engine::new(runtime, now.clone(), Default::default(), limits).unwrap();
    (engine, now)
}
pub fn step(engine: &mut Engine<Manual>) -> EngineEvent {
    let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
    engine.collect([]);
    event
}
pub fn next(engine: &mut Engine<Manual>) -> EngineEvent {
    for _ in 0..10000 {
        let event = step(engine);
        if !matches!(event, EngineEvent::Yielded) {
            return event;
        }
    }
    panic!("event did not finish");
}
pub fn run(engine: &mut Engine<Manual>, script: &str) -> String {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("event test", script)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source)
        .unwrap_or_else(|e| panic!("{script}: {e}"));
    let id = engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    let event = next(engine);
    let EngineEvent::Completed {
        context,
        result: RuntimeExit::Finished(value),
    } = event
    else {
        panic!("{script}: {event:?}");
    };
    assert_eq!(context, id);
    let result = engine.runtime().heap.display(value).unwrap();
    engine.take_result(id);
    result
}
