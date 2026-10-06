use krkr_engine::{Engine, EngineEvent};
use std::{num::NonZeroUsize, time::Duration};
use tjs_core::{NativeCallable, NativeCx, NativeError, NativeResult, RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit};

struct Clock;
impl tjs_runtime::clock::Clock for Clock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

fn arm_missing(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let Value::Obj(reference) = args[0] else {
        return Err(NativeError::Type("object"));
    };
    cx.heap_mut().set_call_missing(reference.object.unwrap())?;
    Ok(Value::Void)
}

fn measured_result(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    if !cx.result_needed() {
        return Err(NativeError::Message("table measurement requires a result"));
    }
    let Value::Str(text) = args[0] else {
        return Err(NativeError::Type("converted text"));
    };
    Ok(Value::Int(cx.heap().string(text)?.len() as i64 * 10))
}

#[test]
fn table_source_contracts_survive_callbacks_and_gc() {
    let mut runtime = Runtime::new();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(runtime, Clock, Default::default(), Default::default()).unwrap();
    let global = engine.global();
    let heap = &mut engine.runtime_mut().heap;
    for (name, call) in [
        ("armMissing", arm_missing as tjs_core::NativeThunk),
        ("measuredResult", measured_result),
    ] {
        let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
        let function = heap.alloc_native_function(NativeCallable::Leaf(call));
        heap.set_member(global, key, Value::Obj(function.into()))
            .unwrap();
    }
    for (name, units) in [
        ("nulText", vec![65, 0, 66, 67, 68, 69]),
        ("surrogateText", vec![65, 0xd83d, 0xde00, 0xd800, 66]),
    ] {
        let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
        let text = heap.alloc_string(units);
        heap.set_member(global, key, Value::Str(text)).unwrap();
    }
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8(
            "ksupport table source contracts",
            include_str!("fixtures/table.tjs"),
        )
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    let value = loop {
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Yielded => {}
            EngineEvent::Completed {
                result: RuntimeExit::Finished(value),
                ..
            } => break value,
            other => panic!("table fixture failed: {other:?}"),
        }
    };
    assert_eq!(engine.runtime().heap.display(value).unwrap(), "passed");
}
