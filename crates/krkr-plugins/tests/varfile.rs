mod support;
use krkr_engine::{Engine, EngineEvent, assets::Vfs};
use std::{
    num::NonZeroUsize,
    time::{Duration, Instant},
};
use tjs_core::{RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};
fn run(engine: &mut Engine<MonotonicClock>, source: &str) {
    let id = engine
        .runtime_mut()
        .sources
        .add_utf8("varfile", source)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, id).unwrap();
    let root = engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("capacity"));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "fixture timed out");
        match engine.poll(
            RunBudget::new(1000).unwrap(),
            NonZeroUsize::new(64).unwrap(),
        ) {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(Value::Int(42)),
            } if context == root => {
                engine.take_result(root);
                break;
            }
            EngineEvent::Waiting { .. } => std::thread::yield_now(),
            EngineEvent::Yielded => {}
            other => panic!("fixture failed: {other:?}"),
        }
        engine.collect([]);
    }
}
#[test]
fn live_storage_consumers() {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("fixture.db");
    let db = rusqlite::Connection::open(&database_path).unwrap();
    db.execute_batch("CREATE TABLE sample(n INTEGER); INSERT INTO sample VALUES(42);")
        .unwrap();
    drop(db);
    let database = std::fs::read(database_path)
        .unwrap()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, support::Log).unwrap();
    let _vfs = krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(temp.path(), Default::default()).unwrap(),
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
    let source = r#"
        function check(v, label) { if(!v) throw label; }
        Plugins.link('sqlite3.dll');
        global.DatabaseBytes = <% DATABASE_BYTES %>;
        Plugins.link('varfile.dll'); Plugins.link('base64.dll'); Plugins.link('fstat.dll');
        global.Data = %['Code.tjs' => <% 34 32 3b %>, 'Text' => <% 61 0a 62 %>];
        check(varfileLoaded == 1, 'loaded');
        var db = new Sqlite('var://./DatabaseBytes', true);
        check(db.errorCode == 0 && db.execValue('SELECT n FROM sample') == 42, 'managed sqlite');
        invalidate db;
        check(Scripts.evalStorage('var://./Data/Code.tjs') == 42, 'script');
        check(!Storages.isExistentStorage('var://./data/Code.tjs'), 'case');
        check(Base64.encode('var://./Data/Text') == 'YQpi', 'base64');
        var lines = []; lines.load('var://./Data/Text');
        check(lines.count == 2 && lines[1] == 'b', 'array load');
        lines.save('var://./Data/Text');
        check(Base64.encode('var://./Data/Text') == 'YQpi', 'discard write');
        global.calls = 0;
        class Source {
            property File { getter { global.calls++; return global.Data['Code.tjs']; } }
        }
        global.Live = new Source();
        check(Scripts.evalStorage('var://./Live/File') == 42 && calls == 1, 'getter once');
        Data['Code.tjs'] = <% 34 33 3b %>;
        check(Scripts.evalStorage('var://./Live/File') == 43 && calls == 2, 'live replacement');
        lines.save('var://./Live/File', 'o0');
        check(calls == 3, 'update getter');
        global.Paths = [Data];
        check(Scripts.evalStorage('var://./Paths/0/Code.tjs') == 43, 'array directory');
        Data['code.tjs'] = Data['Code.tjs']; Data['text'] = Data['Text'];
        Storages.addAutoPath('var://./Data/');
        check(Scripts.evalStorage('Code.tjs') == 43, 'auto script');
        check(Storages.getPlacedPath('Code.tjs') == 'var://./Data/code.tjs', 'auto placed');
        check(Storages.isExistentStorage('Text'), 'auto exists');
        lines.load('Text'); check(lines[0] == 'a', 'auto array');
        Storages.exportFile('var://./Data/Text', 'export.txt');
        check(Base64.encode('export.txt') == 'YQpi', 'export file');
        var entries = getDirList('var://./Data/');
        check(entries.count == 4, 'directory entries');
        var caught = false;
        try { Scripts.evalStorage('var://other/Data/Code.tjs'); } catch(e) { caught = true; }
        check(caught, 'domain');
        Storages.removeAutoPath('var://./Data/');
        Plugins.unlink('varfile.dll'); check(varfileLoaded == 1, 'unlink flag');
        42;
"#
    .replace("DATABASE_BYTES", &database);
    run(&mut engine, &source);
}
