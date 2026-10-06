use krkr_engine::{
    Engine, EngineEvent,
    assets::{Limits, Vfs, text},
};
use std::{
    fs,
    num::NonZeroUsize,
    time::{Duration, Instant},
};
use tjs_core::{RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};
mod support;
type Runner = Engine<MonotonicClock>;
fn submit(engine: &mut Runner, text: &str) -> tjs_runtime::ContextId {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("ScriptsEx scenario", text)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"))
}
fn run(engine: &mut Runner, text: &str) -> String {
    let id = submit(engine, text);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "ScriptsEx timeout");
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Yielded => {}
            EngineEvent::Waiting { .. } => std::thread::sleep(Duration::from_millis(1)),
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } if context == id => {
                let text = engine.runtime().heap.display(value).unwrap();
                engine.take_result(id);
                return text;
            }
            EngineEvent::Completed {
                result: RuntimeExit::Thrown(exception),
                ..
            } => {
                let heap = &mut engine.runtime_mut().heap;
                let message = if let Value::Obj(r) = exception.value {
                    let key = heap.intern(&"message".encode_utf16().collect::<Vec<_>>());
                    heap.member(r.object.unwrap(), key)
                        .unwrap()
                        .unwrap_or(exception.value)
                } else {
                    exception.value
                };
                panic!(
                    "ScriptsEx threw {}: {:?}",
                    heap.display(message).unwrap(),
                    exception.diagnostic
                );
            }
            other => panic!("ScriptsEx failed: {other:?}"),
        }
    }
}
#[test]
fn scripts_ex_whole_plugin_objects_flags_storage_and_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let source = "(const)%[\"name\"=>\"雪\",\"values\"=>(const)[1,2]]";
    for (name, data) in [
        ("data.txt", source),
        ("offset.txt", &format!("skip{source}")),
        ("empty.txt", ""),
        ("multiple.txt", "1,2"),
        ("code.txt", "(global.executed=true)"),
        ("escape.txt", "1];global.executed=true;//"),
    ] {
        fs::write(directory.path().join(name), data).unwrap();
    }
    for (name, mode) in [
        ("wide.txt", ""),
        ("cipher.txt", "c1"),
        ("compressed.txt", "z"),
    ] {
        fs::write(
            directory.path().join(name),
            text::encode(
                &source.encode_utf16().collect::<Vec<_>>(),
                &mode.encode_utf16().collect::<Vec<_>>(),
                4096,
            )
            .unwrap(),
        )
        .unwrap();
    }
    fs::write(directory.path().join("sjis.txt"), [34, 0x82, 0xa0, 34]).unwrap();
    fs::write(
        directory.path().join("data.xp3"),
        archive(source.as_bytes()),
    )
    .unwrap();
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
    assert_eq!(
        run(&mut engine, include_str!("fixtures/scripts_ex.tjs")),
        "passed"
    );
    // Native flags are retained even when a setter throws, and cloned slots
    // retain both bits. These cannot be inferred from value equality alone.
    let global = engine.global();
    let heap = &mut engine.runtime_mut().heap;
    for name in ["flagsDict", "cloneFlags"] {
        let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
        let Value::Obj(object) = heap.member(global, key).unwrap().unwrap() else {
            panic!("dictionary")
        };
        let key = heap.intern(&"both".encode_utf16().collect::<Vec<_>>());
        let (_, hidden, static_) = heap
            .member_with_flags(object.object.unwrap(), key)
            .unwrap()
            .unwrap();
        assert!(hidden && static_);
    }
    run(
        &mut engine,
        "var entered=false,resumed=false;function pause(k,v){global.entered=true;global.System.wait(60000);global.resumed=true;}",
    );
    let id = submit(&mut engine, "S.foreach([1,2],pause);");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline);
        let event = engine.poll(RunBudget::new(64).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Yielded => {}
            EngineEvent::Waiting { .. } => break,
            other => panic!("callback did not pause: {other:?}"),
        }
    }
    engine.cancel(id);
    engine.collect([]);
    assert_eq!(
        run(&mut engine, "entered && !resumed && S.clone([7])[0]==7;"),
        "1"
    );
    assert_eq!(engine.pending_operations(), 0);
    // A required getter suspends on the same VM; adding raw bypasses it.
    run(
        &mut engine,
        "entered=false;resumed=false;property pending{getter(){global.entered=true;global.System.wait(60000);global.resumed=true;return 3;}}var waiting=%[x:&pending];",
    );
    let id = submit(&mut engine, "S.propGet(waiting,'x',S.pfMemberMustExist);");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline);
        let event = engine.poll(RunBudget::new(64).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Yielded => {}
            EngineEvent::Waiting { .. } => break,
            other => panic!("getter did not pause: {other:?}"),
        }
    }
    engine.cancel(id);
    engine.collect([]);
    assert_eq!(
        run(
            &mut engine,
            "entered && !resumed && (S.propGet(waiting,'x',S.pfIgnoreProp|S.pfMemberMustExist) instanceof 'Property');"
        ),
        "1"
    );
    assert_eq!(engine.pending_operations(), 0);
    assert_eq!(
        run(&mut engine, "Plugins.unlink('ScriptsEx.tpm');'passed';"),
        "passed"
    );
}
// Small independently encoded XP3 entry, exercising the actual storage reader.
fn archive(payload: &[u8]) -> Vec<u8> {
    fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
        [tag.as_slice(), &(data.len() as u64).to_le_bytes(), data].concat()
    }
    let name: Vec<_> = "data.txt".encode_utf16().collect();
    let mut info = 0u32.to_le_bytes().to_vec();
    info.extend((payload.len() as u64).to_le_bytes());
    info.extend((payload.len() as u64).to_le_bytes());
    info.extend((name.len() as u16).to_le_bytes());
    info.extend(name.iter().flat_map(|u| u.to_le_bytes()));
    let mut segment = 0u32.to_le_bytes().to_vec();
    for n in [19, payload.len() as u64, payload.len() as u64] {
        segment.extend(n.to_le_bytes());
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in payload {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    let index = chunk(
        b"File",
        &[
            chunk(b"info", &info),
            chunk(b"segm", &segment),
            chunk(b"adlr", &((b << 16) | a).to_le_bytes()),
        ]
        .concat(),
    );
    let mut bytes = b"XP3\r\n \n\x1a\x8bg\x01".to_vec();
    bytes.extend((19 + payload.len() as u64).to_le_bytes());
    bytes.extend(payload);
    bytes.push(0);
    bytes.extend((index.len() as u64).to_le_bytes());
    bytes.extend(index);
    bytes
}
