use krkr_engine::{
    Engine, EngineEvent,
    assets::{Limits, Vfs},
};
use std::{
    fs,
    num::NonZeroUsize,
    time::{Duration, Instant},
};
use tjs_core::{NativeCallable, NativeCx, NativeError, NativeResult, RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};
mod support;
type Runner = Engine<MonotonicClock>;
fn submit(engine: &mut Runner, text: &str) -> tjs_runtime::ContextId {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("saveStruct scenario", text)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"))
}
fn run(engine: &mut Runner, text: &str) -> String {
    let id = submit(engine, text);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "saveStruct scenario timeout");
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Yielded => {}
            EngineEvent::Waiting { .. } => std::thread::sleep(Duration::from_millis(1)),
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } if context == id => {
                let result = engine.runtime().heap.display(value).unwrap();
                engine.take_result(id);
                return result;
            }
            other => panic!("saveStruct scenario failed: {other:?}"),
        }
    }
}
fn enable_missing(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let Some(Value::Obj(reference)) = args.first() else {
        return Err(NativeError::Type("an object"));
    };
    cx.heap_mut().set_call_missing(reference.object.unwrap())?;
    Ok(Value::Void)
}
fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[test]
fn save_struct_complete_surface_streams_captures_and_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("moved")).unwrap();
    fs::write(directory.path().join("truncate.txt"), "previous content").unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, support::Log).unwrap();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(directory.path(), Limits::default()).unwrap(),
    )
    .unwrap();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(
        runtime,
        MonotonicClock::default(),
        Default::default(),
        Default::default(),
    )
    .unwrap();
    let global = engine.global();
    let heap = &mut engine.runtime_mut().heap;
    for (name, units) in [
        ("nulString", vec![97, 0, 98]),
        ("surrogate", vec![0xd800]),
        ("longText", vec![120; 150000]),
    ] {
        let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
        let value = Value::Str(heap.alloc_string(units));
        heap.set_member(global, key, value).unwrap();
    }
    let object = heap.alloc_dictionary();
    for (name, hidden) in [("visible", false), ("hidden", true)] {
        let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
        heap.set_member_flags(object, key, Value::Int(7), hidden, false)
            .unwrap();
    }
    let key = heap.intern(&"flagged".encode_utf16().collect::<Vec<_>>());
    heap.set_member(global, key, Value::Obj(object.into()))
        .unwrap();
    let function = heap.alloc_native_function(NativeCallable::Leaf(enable_missing));
    let key = heap.intern(&"enableMissing".encode_utf16().collect::<Vec<_>>());
    heap.set_member(global, key, Value::Obj(function.into()))
        .unwrap();
    assert_eq!(
        run(&mut engine, include_str!("fixtures/save_struct.tjs")),
        "passed"
    );
    for (name, expected) in [
        ("lines.txt", "雪\na\n\n"),
        ("array.tjs", "[\"雪\",null]"),
        ("dictionary.tjs", "%[\"a\"=>int 1]"),
        ("partial.txt", "first\r\n"),
        ("void.txt", ""),
        ("truncate.txt", ""),
        ("123", "[]"),
        ("prefix.tjs", "["),
        ("fixed.tjs", "[int 3]"),
    ] {
        assert_eq!(
            fs::read(directory.path().join(name)).unwrap(),
            utf16(expected),
            "{name}"
        );
    }
    assert_eq!(
        fs::read(directory.path().join("surrogate.tjs")).unwrap(),
        [91u16, 34, 0xd800, 34, 93]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        fs::read(directory.path().join("flushed.tjs")).unwrap(),
        utf16(&format!("[\"{}\",[]]", "x".repeat(150000)))
    );
    assert!(!directory.path().join("moved/fixed.tjs").exists());
    // Dictionary methods carry the reference's static flag; Array's do not.
    let heap = &mut engine.runtime_mut().heap;
    let key = heap.intern(&"toStructString".encode_utf16().collect::<Vec<_>>());
    for (name, static_member) in [("Array", false), ("Dictionary", true)] {
        let class = heap.registered_class(name).unwrap();
        assert_eq!(
            heap.member_with_flags(class, key).unwrap().unwrap().2,
            static_member
        );
    }
    run(&mut engine, "var entered=false,resumed=false;mode='wait';");
    let id = submit(&mut engine, "[1].saveStruct2('cancel.tjs');");
    loop {
        let event = engine.poll(RunBudget::new(64).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Yielded => {}
            EngineEvent::Waiting { .. } => break,
            other => panic!("count getter failed to suspend: {other:?}"),
        }
    }
    assert_eq!(
        fs::read(directory.path().join("cancel.tjs")).unwrap(),
        utf16("[")
    );
    engine.cancel(id);
    engine.collect([]);
    fs::rename(
        directory.path().join("cancel.tjs"),
        directory.path().join("closed.tjs"),
    )
    .unwrap();
    assert_eq!(
        run(
            &mut engine,
            "mode='normal'; entered && !resumed && [2].toStructString()=='[int 2]';"
        ),
        "1"
    );
    assert_eq!(engine.pending_operations(), 0);
    // Long scalar work yields with a flushed prefix; cancellation releases it.
    let id = submit(&mut engine, "[longText].saveStruct2('long-cancel.tjs');");
    loop {
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(1).unwrap());
        engine.collect([]);
        assert!(matches!(event, EngineEvent::Yielded));
        if fs::metadata(directory.path().join("long-cancel.tjs")).is_ok_and(|m| m.len() > 10000) {
            break;
        }
    }
    engine.cancel(id);
    engine.collect([]);
    let bytes = fs::read(directory.path().join("long-cancel.tjs")).unwrap();
    assert!(bytes.len() < 300008 && bytes.starts_with(&utf16("[\"xxx")));
    assert_eq!(
        run(
            &mut engine,
            "check(Plugins.unlink('saveStruct.dll'),'final unload');'passed';"
        ),
        "passed"
    );
}
