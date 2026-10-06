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
        .add_utf8("CSV plugin scenario", text)
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
        assert!(Instant::now() < deadline, "CSV scenario timed out");
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
            other => panic!("CSV scenario failed: {other:?}"),
        }
    }
}

#[test]
fn csv_plugin_streams_rows_callbacks_and_lifecycle() {
    let directory = tempfile::tempdir().unwrap();
    let source = "name,雪\r\nx,\"line\nbreak\"\r\n";
    fs::write(directory.path().join("utf8.csv"), source).unwrap();
    for (name, mode) in [
        ("utf16.csv", ""),
        ("cipher.csv", "c1"),
        ("compressed.csv", "z"),
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
    fs::write(
        directory.path().join("raw.csv"),
        b"\xef\xbb\xbfA,B\0ignored,C",
    )
    .unwrap();
    fs::write(directory.path().join("invalid.csv"), [0xff, 0x80]).unwrap();
    fs::write(
        directory.path().join("data.xp3"),
        archive(source.as_bytes()),
    )
    .unwrap();
    fs::write(
        directory.path().join("large.csv"),
        format!("{}\nend", "x".repeat(150000)),
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
    let global = engine.global();
    let heap = &mut engine.runtime_mut().heap;
    let key = heap.intern(&"nulCsv".encode_utf16().collect::<Vec<_>>());
    let value = Value::Str(heap.alloc_string(vec![0, 10, 34, 97, 34, 0, 34, 98, 34, 44, 0, 99]));
    heap.set_member(global, key, value).unwrap();
    assert_eq!(run(&mut engine, include_str!("fixtures/csv.tjs")), "passed");

    // Cancel inside a real VM callback. The delivered row remains consumed,
    // the suspended callback cannot resume, and the next row remains readable.
    run(
        &mut engine,
        "var entered=false, resumed=false; p=new CSVParser; p.doLine=function(row,n){global.entered=true;System.wait(60000);global.resumed=true;};p.init('first\nsecond');",
    );
    let id = submit(&mut engine, "p.parse();");
    loop {
        let event = engine.poll(RunBudget::new(64).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Yielded => {}
            EngineEvent::Waiting { .. } => break,
            other => panic!("CSV callback did not suspend: {other:?}"),
        }
    }
    engine.cancel(id);
    assert_eq!(
        run(
            &mut engine,
            "entered && !resumed && p.currentLineNumber==1 && p.getNextLine()[0]=='second';"
        ),
        "1"
    );
    assert_eq!(engine.pending_operations(), 0);
    // A cancelled partial record leaves its input position uncommitted.
    let global = engine.global();
    let heap = &mut engine.runtime_mut().heap;
    let key = heap.intern(&"longCsv".encode_utf16().collect::<Vec<_>>());
    let value = Value::Str(heap.alloc_string(vec![120; 150000]));
    heap.set_member(global, key, value).unwrap();
    run(&mut engine, "p.init(longCsv);");
    let id = submit(&mut engine, "p.getNextLine();");
    assert!(matches!(
        engine.poll(RunBudget::new(16).unwrap(), NonZeroUsize::new(1).unwrap()),
        EngineEvent::Yielded
    ));
    engine.cancel(id);
    engine.collect([]);
    assert_eq!(
        run(
            &mut engine,
            "p.currentLineNumber==0 && p.getNextLine()[0].length==150000 && p.currentLineNumber==1;"
        ),
        "1"
    );
    assert_eq!(
        run(
            &mut engine,
            "invalidate p;check(Plugins.unlink('csvParser.tpm'),'final unlink');'done';"
        ),
        "done"
    );
}

// An independent, single-file uncompressed XP3 fixture exercises the real VFS.
fn archive(payload: &[u8]) -> Vec<u8> {
    fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
        [tag.as_slice(), &(data.len() as u64).to_le_bytes(), data].concat()
    }
    let name: Vec<_> = "table.csv".encode_utf16().collect();
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
