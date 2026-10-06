#[path = "../../krkr-assets/tests/support/mod.rs"]
mod support;
use krkr_assets::{Limits, Vfs, name::units, text};
use std::fs;
use tjs_core::{RunBudget, Vm};
use tjs_runtime::{Runtime, RuntimeExit};

fn run(runtime: &mut Runtime, source: &str) -> String {
    let id = runtime
        .sources
        .add_utf8("storage integration", source)
        .unwrap();
    let module = tjs_front::compile(&runtime.sources, id).unwrap();
    let mut vm = Vm::new(&module);
    for _ in 0..30000 {
        let result = runtime.run_slice(&mut vm, RunBudget::new(1).unwrap());
        runtime.collect(vm.roots());
        match result {
            RuntimeExit::Yielded => {}
            RuntimeExit::Finished(value) => return runtime.heap.display(value).unwrap(),
            other => panic!("{source}: {other:?}"),
        }
    }
    panic!("script did not finish");
}
#[test]
fn storages_scripts_and_containers_share_files_archives_and_text_modes() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("Patch")).unwrap();
    fs::write(
        directory.path().join("Data.xp3"),
        support::archive("Scenario/Answer.TJS", b"6*7;", true, true),
    )
    .unwrap();
    fs::write(directory.path().join("Patch/Answer.TJS"), b"40+3;").unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::scripts::install(&mut runtime.heap).unwrap();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(directory.path(), Limits::default()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        run(
            &mut runtime,
            r#"
        Storages.addAutoPath('data.xp3>scenario/');
        var a=Scripts.evalStorage('answer.tjs');
        Storages.addAutoPath('patch/');
        var b=Scripts.evalStorage('answer.tjs');
        Storages.removeAutoPath('patch/');
        Storages.clearArchiveCache();
        var c=Scripts.evalStorage('answer.tjs');
        a==42 && b==43 && c==42 && Storages.isExistentStorage('missing/answer.tjs') && Storages.getPlacedPath('absent')=='';
    "#
        ),
        "1"
    );
    assert_eq!(
        run(
            &mut runtime,
            r#"
        var p=Storages.getFullPath('DATA.xp3>Scenario/../Scenario/Answer.TJS');
        Storages.extractStorageName(p)=='answer.tjs' && Storages.extractStorageExt(p)=='.tjs' &&
        Storages.chopStorageExt('DIR/a.xp3>START.TJS')=='DIR/a.xp3>START' &&
        Storages.extractStoragePath('a.xp3>test')=='a.xp3>' && Storages.getLocalName('save.txt').length>0;
    "#
        ),
        "1"
    );
    assert_eq!(
        run(
            &mut runtime,
            r#"
        var a=['你好','abc']; a.save('saved.txt','z9');
        var b=new Array; b.load('saved.txt');
        a.save('cipher.txt','c1'); var c=new Array; c.load('cipher.txt');
        var d=[1,%[ok:'yes']]; d.saveStruct('save.bin','b'); var e=new Array; e.loadStruct('save.bin');
        b[0]==a[0] && b[1]==a[1] && c[0]==a[0] && e[1].ok=='yes';
    "#
        ),
        "1"
    );
    assert_eq!(
        run(
            &mut runtime,
            r#"
        Scripts.compileStorage('answer.tjs','answer.krr',true,true,true);
        Scripts.evalStorage('answer.krr');
    "#
        ),
        "42"
    );
    fs::write(
        directory.path().join("script.tjs"),
        text::encode(&units("21*2;"), &units("z9"), 4096).unwrap(),
    )
    .unwrap();
    assert_eq!(
        run(&mut runtime, "Scripts.evalStorage('script.tjs');"),
        "42"
    );
    let encoded = text::encode(&units("7*6;"), &units("c"), 4096).unwrap();
    fs::write(
        directory.path().join("offset.tjs"),
        [b"padding".as_slice(), encoded.as_slice()].concat(),
    )
    .unwrap();
    assert_eq!(
        run(&mut runtime, "Scripts.evalStorage('offset.tjs','o7');"),
        "42"
    );
}

#[test]
fn storage_failures_are_catchable_and_limits_cover_compressed_scripts() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("big.tjs"),
        text::encode(&vec![32; 2000], &units("z"), 10000).unwrap(),
    )
    .unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::scripts::install(&mut runtime.heap).unwrap();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(
            directory.path(),
            Limits {
                max_read_bytes: 256,
                ..Limits::default()
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        run(
            &mut runtime,
            r#"
        var count=0;
        try {Scripts.execStorage('missing');} catch(e){count++;}
        try {Scripts.execStorage('big.tjs');} catch(e){count++;}
        try {Storages.addAutoPath('no-delimiter');} catch(e){count++;}
        try {Storages.getFullPath('data.xp3>../escape');} catch(e){count++;}
        try {Storages.getLocalName('data.xp3>entry');} catch(e){count++;}
        count;
    "#
        ),
        "5"
    );
}
