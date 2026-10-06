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
fn discarded(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    if cx.result_needed() {
        return Err(NativeError::Message("each callback requested a result"));
    }
    Ok(Value::Void)
}
#[test]
fn collection_and_geometry_source_contracts_survive_gc() {
    let mut runtime = Runtime::new();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(runtime, Clock, Default::default(), Default::default()).unwrap();
    let global = engine.global();
    for (name, call) in [
        ("armMissing", arm_missing as tjs_core::NativeThunk),
        ("discarded", discarded),
    ] {
        let heap = &mut engine.runtime_mut().heap;
        let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
        let function = heap.alloc_native_function(NativeCallable::Leaf(call));
        heap.set_member(global, key, Value::Obj(function.into()))
            .unwrap();
    }
    let heap = &mut engine.runtime_mut().heap;
    let flagged = heap.alloc_dictionary();
    for (name, value, hidden, static_) in [
        ("visible", 1, false, false),
        ("hidden", 2, true, false),
        ("both", 3, true, true),
    ] {
        let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
        heap.set_member_flags(flagged, key, Value::Int(value), hidden, static_)
            .unwrap();
    }
    for (name, value) in [
        ("flagged", Value::Obj(tjs_core::ObjRef::bound(flagged))),
        ("realNan", Value::Real(f64::NAN)),
        ("minusZero", Value::Real(-0.0)),
    ] {
        let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
        heap.set_member(global, key, value).unwrap();
    }
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8(
            "ksupport collection and geometry group",
            include_str!("fixtures/ksupport.tjs"),
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
            other => panic!("ksupport fixture failed: {other:?}"),
        }
    };
    assert_eq!(engine.runtime().heap.display(value).unwrap(), "passed");
}
