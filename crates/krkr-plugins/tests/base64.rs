use krkr_engine::{
    Engine, EngineEvent,
    assets::{Limits, Vfs},
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
        .add_utf8("Base64 scenario", text)
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
        assert!(Instant::now() < deadline, "Base64 timeout");
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
                    "Base64 threw {}: {:?}",
                    heap.display(message).unwrap(),
                    exception.diagnostic
                );
            }
            other => panic!("Base64 failed: {other:?}"),
        }
    }
}
#[test]
fn base64_whole_plugin_files_archives_decoding_and_lifetime() {
    let directory = tempfile::tempdir().unwrap();
    let bytes: Vec<u8> = (0..40963).map(|i| (i % 256) as u8).collect();
    fs::write(directory.path().join("binary"), &bytes).unwrap();
    fs::write(directory.path().join("empty"), []).unwrap();
    fs::write(directory.path().join("data.xp3"), archive(b"Man")).unwrap();
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
    let key = heap.intern(&"nulData".encode_utf16().collect::<Vec<_>>());
    let value = Value::Str(heap.alloc_string(vec![84, 0, 81, 61]));
    heap.set_member(global, key, value).unwrap();
    assert_eq!(
        run(
            &mut engine,
            r#"
        Plugins.link("base64.dll");
        function check(ok) { if (!ok) throw "base64 assertion"; }
        function rejects(f) { try { f(); } catch(e) { return true; } return false; }
        check(rejects(function() { new Base64(); }));
        check(rejects(function() { Base64.encode(); }));
        check(rejects(function() { Base64.decode("TQ=="); }));
        Base64.encode(%[]); // No result: must skip conversion and IO.
        check(Base64.encode("missing") === void);
        check(Base64.encode("empty") === "");
        check(Base64.encode("data.xp3>data.txt") === "TWFu");
        check(Base64.decode("TWFu", "man") === "627661c621eab1b7b298abc47d1a250d");
        check(Base64.decode("", "zero") === "d41d8cd98f00b204e9800998ecf8427e");
        Base64.decode("TQ==", "one");
        Base64.decode("TWE=", "two");
        Base64.decode("TWE", "tail"); // Last implicit C-string terminator.
        Base64.decode("T Q=", "space");
        Base64.decode(nulData, "embedded-nul.bin");
        check(rejects(function() { Base64.decode(1, "bad"); }));
        check(rejects(function() { Base64.decode("TQ==", void); }));
        check(rejects(function() { Base64.decode("TQ", "bad"); }));
        check(rejects(function() { Base64.decode("ĀAAA", "bad"); }));
        var encoded = Base64.encode("binary");
        check(encoded.length === 54620);
        Base64.decode(encoded, "roundtrip");
        var oldEncode = Base64.encode;
        check(Plugins.unlink("base64.dll"));
        check(typeof global.Base64 === "undefined");
        check(oldEncode("one") === "TQ==");
        Plugins.link("base64.tpm");
        check(Base64.encode("two") === "TWE=");
        "passed";
    "#
        ),
        "passed"
    );
    for (name, expected) in [
        ("man", b"Man".as_slice()),
        ("zero", b""),
        ("one", b"M"),
        ("two", b"Ma"),
        ("tail", b"Ma\0"),
        ("space", &[76, 4]),
        ("embedded-nul.bin", &[76, 4]),
        ("roundtrip", bytes.as_slice()),
    ] {
        assert_eq!(
            fs::read(directory.path().join(name)).unwrap(),
            expected,
            "{name}"
        );
    }
    // Cancellation closes the actual writer; already written prefixes remain.
    let id = submit(
        &mut engine,
        r#"Base64.decode("TWFu".repeat(20000), "cancelled"); throw "resumed after cancellation";"#,
    );
    let path = directory.path().join("cancelled");
    for _ in 0..10000 {
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        assert!(matches!(event, EngineEvent::Yielded));
        if fs::metadata(&path).is_ok_and(|m| m.len() > 0) {
            break;
        }
    }
    assert!(fs::metadata(&path).unwrap().len() < 60000);
    engine.cancel(id);
    engine.collect([]);
    fs::remove_file(path).unwrap();
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
