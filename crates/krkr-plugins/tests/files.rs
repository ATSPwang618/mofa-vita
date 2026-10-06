use krkr_engine::{
    Engine, EngineEvent,
    assets::{Limits, Vfs},
};
use std::{fs, num::NonZeroUsize, path::PathBuf, time::Duration};
use tjs_core::{RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit};

#[path = "../../host-desktop/src/system/files.rs"]
mod host_files;
struct Filesystem;
impl krkr_engine::system::SystemHost for Filesystem {
    fn create_app_lock(&mut self, _: &[u16]) -> Result<bool, String> {
        unreachable!()
    }
    fn file_attributes(&mut self, path: &[u16]) -> Result<u32, String> {
        host_files::attributes(path)
    }
    fn change_file_attributes(
        &mut self,
        path: &[u16],
        mask: u32,
        set: bool,
    ) -> Result<bool, String> {
        host_files::change_attributes(path, mask, set)
    }
    fn file_display_name(&mut self, path: &[u16]) -> Result<Vec<u16>, String> {
        host_files::display_name(path)
    }
}

struct Clock;
impl tjs_runtime::clock::Clock for Clock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

#[test]
fn fstat_file_time_and_path_contracts_share_the_vfs() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("patch")).unwrap();
    fs::write(directory.path().join("patch/payload.bin"), b"payload").unwrap();
    fs::write(directory.path().join("local.bin"), b"local").unwrap();
    fs::write(directory.path().join("linked.png"), b"linked bytes").unwrap();
    fs::write(
        directory.path().join("legacy.bmp.krkr-link"),
        krkr_engine::assets::converted::encode_link("linked.png").unwrap(),
    )
    .unwrap();
    fs::write(directory.path().join("million.bin"), vec![b'a'; 1_000_000]).unwrap();
    fs::write(directory.path().join("empty.bin"), []).unwrap();
    fs::write(directory.path().join("data.xp3"), archive_abc()).unwrap();
    fs::create_dir_all(directory.path().join("tree/sub")).unwrap();
    fs::write(directory.path().join("tree/sub/child.bin"), b"child").unwrap();
    fs::write(directory.path().join("tree/top.bin"), b"top").unwrap();
    fs::hard_link(
        directory.path().join("local.bin"),
        directory.path().join("alias.bin"),
    )
    .unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(directory.path(), Limits::default()).unwrap(),
    )
    .unwrap();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut system = krkr_engine::system::SystemConfig::for_process().unwrap();
    system.host = Some(Box::new(Filesystem));
    let mut engine = Engine::with_system(
        runtime,
        Clock,
        Default::default(),
        Default::default(),
        system,
    )
    .unwrap();
    let (client, host) =
        krkr_engine::protocol::window::channel(Default::default(), std::sync::Arc::new(|| {}));
    engine.attach_windows(client).unwrap();
    let mut selections = 0;
    let global = engine.global();
    let key = engine
        .runtime_mut()
        .heap
        .intern(&"supportsCreationTime".encode_utf16().collect::<Vec<_>>());
    engine
        .runtime_mut()
        .heap
        .set_member(global, key, Value::Int(i64::from(cfg!(windows))))
        .unwrap();
    let source = include_str!("fixtures/files.tjs");
    let id = engine
        .runtime_mut()
        .sources
        .add_utf8("fstat storage group", source)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, id).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    let value = loop {
        let result = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        // Dates, dictionary getter values and pending file handles must remain
        // correct across a collection at every continuation boundary.
        engine.collect([]);
        while let Some(request) = host.next_request() {
            use krkr_engine::protocol::window::{Command, Geometry, Response};
            match &request.command {
                Command::Create { .. } => request.complete(Ok(Geometry::default())),
                Command::SelectDirectory(options) => {
                    assert_eq!(
                        String::from_utf16(&options.title).unwrap(),
                        "Choose a folder"
                    );
                    assert_eq!(
                        fs::canonicalize(PathBuf::from(
                            String::from_utf16(&options.initial).unwrap()
                        ))
                        .unwrap(),
                        fs::canonicalize(directory.path().join("tree")).unwrap()
                    );
                    assert_eq!(
                        fs::canonicalize(PathBuf::from(String::from_utf16(&options.root).unwrap()))
                            .unwrap(),
                        fs::canonicalize(directory.path()).unwrap()
                    );
                    assert_ne!(request.window, Default::default());
                    selections += 1;
                    let selected = (selections != 2).then(|| {
                        krkr_engine::assets::local::units(&directory.path().join("tree/sub"))
                            .unwrap()
                    });
                    request.respond(Ok(Response::DirectorySelected(selected)));
                }
                other => panic!("unexpected file dialog host request: {other:?}"),
            }
        }
        match result {
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } => {
                engine.take_result(context);
                break value;
            }
            other => panic!("fstat fixture failed: {other:?}"),
        }
    };
    assert_eq!(engine.runtime().heap.display(value).unwrap(), "passed");
    assert_eq!(selections, 3);
    // Completed VM frames retain their registers until take_result. After
    // releasing those roots, run queued native finalizers at the idle boundary.
    engine.collect([]);
    loop {
        match engine.poll(RunBudget::new(64).unwrap(), NonZeroUsize::new(64).unwrap()) {
            EngineEvent::Idle => break,
            EngineEvent::Yielded => {
                engine.collect([]);
            }
            other => panic!("temporary finalization failed: {other:?}"),
        }
    }
    assert_eq!(
        fs::read(directory.path().join("MixedCase.BIN")).unwrap(),
        b"payload"
    );
    assert!(!directory.path().join("patch/payload.bin").exists());
    assert!(directory.path().join("ExactCase/Nested").is_dir());
    assert!(!directory.path().join("gc.bin").exists());
    assert_eq!(
        fs::read(directory.path().join("owned.bin")).unwrap(),
        b"local"
    );
    assert_eq!(
        directory.path().join("renamed.bin").exists(),
        !cfg!(windows)
    );
    assert!(directory.path().join("shutdown.bin").exists());
    drop(engine);
    assert!(!directory.path().join("shutdown.bin").exists());
}

// Minimal independent XP3 wire fixture: one uncompressed file, "dir/abc.bin".
fn archive_abc() -> Vec<u8> {
    fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
        [tag.as_slice(), &(body.len() as u64).to_le_bytes(), body].concat()
    }
    let filename: Vec<_> = "dir/abc.bin".encode_utf16().collect();
    let mut info = 0u32.to_le_bytes().to_vec();
    info.extend(3u64.to_le_bytes());
    info.extend(3u64.to_le_bytes());
    info.extend((filename.len() as u16).to_le_bytes());
    info.extend(filename.iter().flat_map(|u| u.to_le_bytes()));
    let mut segment = 0u32.to_le_bytes().to_vec();
    for value in [19u64, 3, 3] {
        segment.extend(value.to_le_bytes());
    }
    let index = chunk(
        b"File",
        &[
            chunk(b"info", &info),
            chunk(b"segm", &segment),
            chunk(b"adlr", &0x024d0127u32.to_le_bytes()),
        ]
        .concat(),
    );
    let mut output = b"XP3\r\n \n\x1a\x8bg\x01".to_vec();
    output.extend(22u64.to_le_bytes());
    output.extend(b"abc");
    output.push(0);
    output.extend((index.len() as u64).to_le_bytes());
    output.extend(index);
    output
}
