use krkr_engine::{
    Engine, EngineEvent,
    assets::{Vfs, name},
    protocol::{
        self, graphics,
        window::{Command, Geometry, Response},
    },
};
use std::{
    io::{Read, Seek, SeekFrom},
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};
use tjs_core::RunBudget;
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};
#[path = "../../krkr-image/tests/support/psd_fixture.rs"]
mod fixture;
mod support;
type Runner = Engine<MonotonicClock>;
fn submit(engine: &mut Runner, script: &str) -> tjs_runtime::ContextId {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("PSD plugin scenario", script)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("submit PSD scenario"))
}
#[test]
fn psd_plugin_xp3_media_gc_errors_and_cancellation() {
    let temp = tempfile::tempdir().unwrap();
    let data = fixture::layered(8, 1);
    std::fs::write(temp.path().join("data.xp3"), archive(&data)).unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, support::Log).unwrap();
    let vfs = krkr_engine::storages::install(
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
    let (client, host) = protocol::window::channel(Default::default(), Arc::new(|| {}));
    let budget = client.staging_budget();
    engine.attach_windows(client).unwrap();
    let mut uploads = 0;
    let mut drive = |engine: &mut Runner, script: &str, fail_upload: bool, cancel_upload: bool| {
        let id = submit(engine, script);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(Instant::now() < deadline, "PSD VM work timed out");
            let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
            engine.collect([]);
            while let Some(request) = host.next_request() {
                if let Command::Graphics(graphics::Command::AssignBitmap { pixels, .. }) =
                    &request.command
                {
                    uploads += 1;
                    assert_eq!(pixels.main.as_ref().unwrap().as_slice(), fixture::RGBA);
                    if cancel_upload {
                        engine.cancel(id);
                        engine.collect([]);
                        assert!(request.cancelled());
                        request.respond(Ok(Response::Done));
                        host.take_scenes(u64::MAX);
                        assert_eq!(engine.pending_operations(), 0);
                        return;
                    }
                    request.respond(if fail_upload {
                        Err("injected PSD upload failure".into())
                    } else {
                        Ok(Response::Done)
                    });
                } else if matches!(request.command, Command::Graphics(_)) {
                    request.respond(Ok(Response::Done));
                } else {
                    request.complete(Ok(Geometry {
                        width: 64,
                        height: 64,
                        inner_width: 64,
                        inner_height: 64,
                        ..Default::default()
                    }));
                }
            }
            host.take_scenes(u64::MAX);
            match event {
                EngineEvent::Yielded | EngineEvent::Waiting { .. } | EngineEvent::Idle => {}
                EngineEvent::Completed {
                    context,
                    result: RuntimeExit::Finished(_),
                } if context == id => {
                    engine.take_result(id);
                    break;
                }
                other => panic!("PSD scenario failed: {other:?}"),
            }
        }
    };
    drive(
        &mut engine,
        r#"
        Plugins.link('psd.dll'); System.exitOnWindowClose=false;
        Storages.addAutoPath('data.xp3>');
        var p=new PSD(); if(!p.load('data.xp3>packed.psd')) throw 'XP3 PSD load';
        var w=new Window(), root=new Layer(w,null), target=new Layer(w,root);
        p.getLayerDataRaw(target,2);
        if(target.name!='彩/色'||target.fill_opacity!=170||p.getLayerInfo(2).layer_comp[9].offset_y!=-3) throw 'metadata';
    "#,
        false,
        false,
    );
    let url = name::units("PSD://PACKED.PSD/ROOT/GROUP/INNER/彩_色.BMP");
    let plan = vfs.borrow_mut().plan(&url).unwrap();
    assert_eq!(plan.bytes, 78);
    assert!(plan.open_interruptible(&|| true).is_err());
    let mut first = plan.open().unwrap();
    let mut second = plan.open().unwrap();
    let mut a = [0; 2];
    first.read_exact(&mut a).unwrap();
    assert_eq!(&a, b"BM");
    first.seek(SeekFrom::Start(54)).unwrap();
    second.read_exact(&mut a).unwrap();
    assert_eq!(&a, b"BM");
    vfs.borrow_mut()
        .set_directory(&name::units("psd://packed.psd/root/group/inner/"))
        .unwrap();
    assert_eq!(
        vfs.borrow().full_path(&name::units("./彩_色.bmp")).unwrap(),
        plan.name
    );
    assert!(
        vfs.borrow()
            .full_path(&name::units("../../../../outside.bmp"))
            .is_err()
    );
    assert!(vfs.borrow().write_plan(&url).is_err());
    assert!(vfs.borrow_mut().write(&url, None, b"bad").is_err());
    vfs.borrow_mut()
        .set_directory(&krkr_engine::assets::local::directory(temp.path()).unwrap())
        .unwrap();
    drop(first);
    drop(second);
    drive(
        &mut engine,
        "var caught=false;try{p.getLayerDataRaw(target,2);}catch(e){caught=true;}if(!caught)throw 'GPU failure lost';",
        true,
        false,
    );
    drive(
        &mut engine,
        "var resumed=false;p.getLayerDataRaw(target,2);resumed=true;",
        false,
        true,
    );
    assert_eq!(
        budget.used(),
        0,
        "cancelled transfer retained staging bytes"
    );
    drive(
        &mut engine,
        r#"
        if(resumed)throw 'cancel resumed';
        PSD.clearStorageCache(); invalidate p;
        if(!Storages.isExistentStorage('psd://packed.psd/id/42.bmp')) throw 'XP3 auto-load';
        Plugins.unlink('psd.dll');
        if(typeof global.PSD!='undefined')throw 'export leaked';
    "#,
        false,
        false,
    );
    assert!(vfs.borrow_mut().plan(&url).is_err());
    // An already published plan retains its own source and remains seekable
    // after unlink; new names resolve only after a provider is installed again.
    assert_eq!(&plan.read(0).unwrap()[..2], b"BM");
    drive(
        &mut engine,
        "Plugins.link('psd.tpm');var p=new PSD();if(!p.load('packed.psd'))throw 'reload';invalidate w;",
        false,
        false,
    );
    assert!(vfs.borrow_mut().plan(&url).is_ok());
    assert_eq!(uploads, 3);
}
fn archive(payload: &[u8]) -> Vec<u8> {
    fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
        [tag.as_slice(), &(data.len() as u64).to_le_bytes(), data].concat()
    }
    let name: Vec<_> = "packed.psd".encode_utf16().collect();
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
