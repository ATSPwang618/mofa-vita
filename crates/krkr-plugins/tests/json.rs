use krkr_engine::{
    Engine, EngineEvent,
    assets::{Limits, Vfs},
};
use std::{fs, num::NonZeroUsize, time::Duration};
use tjs_core::{NativeCallable, NativeCx, NativeError, NativeResult, RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit};

struct Clock;
impl tjs_runtime::clock::Clock for Clock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}
fn enable_missing(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let Some(Value::Obj(reference)) = args.first() else {
        return Err(NativeError::Type("an object"));
    };
    cx.heap_mut().set_call_missing(reference.object.unwrap())?;
    Ok(Value::Void)
}
#[test]
fn json_interface_group_preserves_reference_values_streams_and_callbacks() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("patch")).unwrap();
    fs::create_dir(directory.path().join("moved")).unwrap();
    fs::write(
        directory.path().join("patch/joined.json"),
        b"[1\r\n2, \"a\nb\"]",
    )
    .unwrap();
    fs::write(directory.path().join("bad.json"), b"{broken").unwrap();
    fs::write(directory.path().join("bom.json"), b"\xef\xbb\xbf[]").unwrap();
    fs::File::create(directory.path().join("large.json"))
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::scripts::install(&mut runtime.heap).unwrap();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(directory.path(), Limits::default()).unwrap(),
    )
    .unwrap();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(runtime, Clock, Default::default(), Default::default()).unwrap();
    let global = engine.global();
    let heap = &mut engine.runtime_mut().heap;
    for (name, units) in [("nulString", vec![97, 0, 98]), ("surrogate", vec![0xd800])] {
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
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8(
            "JSON plugin interface group",
            include_str!("fixtures/json.tjs"),
        )
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    let value = loop {
        let result = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match result {
            EngineEvent::Yielded => {}
            EngineEvent::Completed {
                result: RuntimeExit::Finished(value),
                ..
            } => break value,
            other => panic!("JSON group fixture failed: {other:?}"),
        }
    };
    assert_eq!(engine.runtime().heap.display(value).unwrap(), "passed");
    assert_eq!(
        fs::read(directory.path().join("utf8.json")).unwrap(),
        "[\n \"雪\",\n null\n]".as_bytes()
    );
    assert_eq!(
        fs::read(directory.path().join("default.json")).unwrap(),
        b"[\r\n 1,\r\n 2\r\n]"
    );
    assert_eq!(
        fs::read(directory.path().join("prefix.json")).unwrap(),
        b"[\r\n "
    );
    assert_eq!(
        fs::read(directory.path().join("fixed.json")).unwrap(),
        b"[\r\n 3\r\n]"
    );
    assert!(!directory.path().join("moved/fixed.json").exists());
    assert_eq!(
        fs::read(directory.path().join("surrogate.json")).unwrap(),
        [34, 0xed, 0xa0, 0x80, 34]
    );
}
