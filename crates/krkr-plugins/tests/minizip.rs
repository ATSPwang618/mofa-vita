mod support;
use krkr_engine::{
    Engine, EngineEvent,
    assets::{Vfs, name},
    protocol::budget::Budget,
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    num::NonZeroUsize,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use tjs_core::{RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};

fn run(engine: &mut Engine<MonotonicClock>, source: &str) {
    let id = engine
        .runtime_mut()
        .sources
        .add_utf8("Minizip storage", source)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, id).unwrap();
    let root = engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("capacity"));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "fixture timed out");
        match engine.poll(RunBudget::new(32).unwrap(), NonZeroUsize::new(64).unwrap()) {
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
fn minizip_archive_and_image_paths() {
    let temp = tempfile::tempdir().unwrap();
    use base64::Engine as _;
    // FFmpeg/libwebp encoded white 18x2: exercises VP8's padded luma stride.
    std::fs::write(
        temp.path().join("lossy.webp"),
        base64::engine::general_purpose::STANDARD
            .decode("UklGRjAAAABXRUJQVlA4ICQAAACwAgCdASoSAAIAPpE6l0eloyIhMAgAsBIJaQAAeyAA/vhNAAA=")
            .unwrap(),
    )
    .unwrap();
    std::fs::write(temp.path().join("source.bin"), b"portable ZIP content").unwrap();
    std::fs::write(temp.path().join("large.bin"), vec![42u8; 500_000]).unwrap();
    let data = b"'ARCHIVE_OK';";
    let mut webp = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
        .encode(
            &[255, 0, 0, 255, 0, 255, 0, 128],
            2,
            1,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    let mut zip = zip::ZipWriter::new(File::create(temp.path().join("pack.zip")).unwrap());
    for (file, bytes, compression) in [
        (
            "Scripts/Test.tjs",
            data.as_slice(),
            zip::CompressionMethod::Deflated,
        ),
        (
            "image.webp",
            webp.as_slice(),
            zip::CompressionMethod::Stored,
        ),
    ] {
        zip.start_file(
            file,
            zip::write::SimpleFileOptions::default().compression_method(compression),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
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
    run(
        &mut engine,
        r#"
        function check(v) { if(!v) throw 'Minizip contract'; }
        var removed = false;
        try { Plugins.link('kirikiroid2.dll'); } catch(e) { removed = true; }
        check(removed);
        Plugins.link('minizip.dll');
        check(Storages.mountZip('Game', 'pack.zip'));
        check(Scripts.evalStorage('zip://Game/SCRIPTS/TEST.TJS') == 'ARCHIVE_OK');
        var z = new Zip();
        var closed = false;
        try { z.add('source.bin', 'closed'); } catch(e) { closed = true; }
        check(closed);
        z.close(); z.open('written.zip');
        check(z.add('source.bin', 'Stored.bin', 0));
        check(z.add('large.bin', '日本語.bin', 9, '秘密'));
        check(z.add('source.bin', 'EmptyPassword.bin', -1, ''));
        check(z.add('source.bin', 'IgnoredPassword.bin', 1, 123));
        check(!z.add('source.bin', 'Invalid.bin', 10));
        z.close(); z.close();
        var exists = false;
        try { z.open('written.zip'); } catch(e) { exists = true; }
        check(exists);
        z.open('written.zip', 2);
        check(z.add('zip://Game/Scripts/Test.tjs', 'Appended.tjs'));
        z.close();
        var u = new Unzip(); u.open('written.zip');
        var files = u.list();
        check(files.count == 5);
        check(files[0].filename == 'Stored.bin' && files[0].deflated == 0);
        check(files[1].filename == '日本語.bin' && files[1].crypted == 1);
        check(files[1].uncompressed_size == 500000 && files[1].deflateLevel == 1);
        check(files[1].date instanceof "Date");
        check(files[2].crypted == 0 && files[3].crypted == 0 && files[3].deflateLevel == 3);
        check(u.extract('stored.BIN', 'plain.out'));
        check(!u.extract('日本語.bin', 'wrong.out', 'wrong'));
        check(!u.extract('日本語.bin', 'wrong.out'));
        check(u.extract('日本語.bin', 'secret.out', '秘密'));
        check(!u.extract('missing', 'missing.out'));
        u.close(); u.close();
        check(Storages.mountZip('Written', 'written.zip'));
        check(Scripts.evalStorage('zip://Written/Appended.tjs') == 'ARCHIVE_OK');
        check(!Storages.unmountZip('written'));
        check(Storages.unmountZip('Written') && !Storages.unmountZip('Written'));
        check(Storages.mountZip('Written', 'written.zip'));
        check(!Storages.mountZip('Written', 'missing.zip'));
        check(!Storages.unmountZip('Written'));
        z.open('new-append.zip', 2); z.close();

        42;
    "#,
    );
    {
        let plan = vfs
            .borrow_mut()
            .plan(&name::units("zip://Game/scripts/test.tjs"))
            .unwrap();
        let mut stream = plan.open().unwrap();
        stream.seek(SeekFrom::Start(1)).unwrap();
        let mut text = [0; 7];
        stream.read_exact(&mut text).unwrap();
        assert_eq!(&text, b"ARCHIVE");
        assert!(plan.open_interruptible(&|| true).is_err());
    }
    assert_eq!(
        std::fs::read(temp.path().join("plain.out")).unwrap(),
        b"portable ZIP content"
    );
    assert_eq!(
        std::fs::read(temp.path().join("secret.out")).unwrap(),
        vec![42u8; 500_000]
    );
    assert!(!temp.path().join("wrong.out").exists());
    assert!(!temp.path().join("missing.out").exists());
    let mut written =
        zip::ZipArchive::new(File::open(temp.path().join("written.zip")).unwrap()).unwrap();
    assert_eq!(written.len(), 5);
    let mut decrypted = Vec::new();
    let password = encoding_rs::SHIFT_JIS.encode("秘密").0.into_owned();
    written
        .by_name_decrypt("日本語.bin", &password)
        .unwrap()
        .read_to_end(&mut decrypted)
        .unwrap();
    assert_eq!(decrypted, vec![42u8; 500_000]);
    drop(written);
    run(
        &mut engine,
        "z.open('written.zip', 1); z.close(); u.open('written.zip'); check(u.list().count == 0); u.close(); 42;",
    );
    let cancelled = AtomicBool::new(false);
    let budget = Budget::new(16 * 1024 * 1024);
    let image = krkr_image::resolve::request(
        &mut vfs.borrow_mut(),
        &name::units("zip://Game/image"),
        0x02ffffff,
        None,
        budget.clone(),
    )
    .unwrap()
    .probe(&cancelled)
    .unwrap()
    .decode(&cancelled)
    .unwrap();
    assert_eq!(
        image.pixels.main.unwrap().as_slice(),
        &[255, 0, 0, 255, 0, 255, 0, 128]
    );
    let mask = krkr_image::resolve::grayscale(
        &mut vfs.borrow_mut(),
        &name::units("zip://Game/image.webp"),
        krkr_engine::protocol::graphics::Size {
            width: 2,
            height: 1,
        },
        budget.clone(),
    )
    .unwrap()
    .probe(&cancelled)
    .unwrap()
    .decode(&cancelled)
    .unwrap();
    assert_eq!(mask.pixels.province.unwrap().as_slice(), &[82, 145]);
    let lossy = krkr_image::resolve::grayscale(
        &mut vfs.borrow_mut(),
        &name::units("lossy.webp"),
        krkr_engine::protocol::graphics::Size {
            width: 18,
            height: 2,
        },
        budget,
    )
    .unwrap()
    .probe(&cancelled)
    .unwrap()
    .decode(&cancelled)
    .unwrap();
    assert_eq!(lossy.pixels.province.unwrap().as_slice(), &[235; 36]);
    let retained = vfs
        .borrow_mut()
        .plan(&name::units("zip://Game/scripts/test.tjs"))
        .unwrap();
    let mut left = retained.open().unwrap();
    let mut right = retained.open().unwrap();
    left.seek(SeekFrom::End(-2)).unwrap();
    let mut prefix = [0; 8];
    right.read_exact(&mut prefix).unwrap();
    assert_eq!(&prefix, b"'ARCHIVE");
    left.rewind().unwrap();
    let mut all = Vec::new();
    left.read_to_end(&mut all).unwrap();
    assert_eq!(all, data);
    run(
        &mut engine,
        "var held = new Unzip(); held.open('pack.zip'); var oldMount = Storages.mountZip; Plugins.unlink('minizip.dll'); check(!oldMount('Old', 'pack.zip')); check(held.list().count == 2); held.close(); 42;",
    );
    assert!(
        vfs.borrow_mut()
            .plan(&name::units("zip://Game/scripts/test.tjs"))
            .is_err()
    );
    assert_eq!(retained.read(0).unwrap(), data);
    run(
        &mut engine,
        "Plugins.link('minizip.tpm'); check(!oldMount('Old', 'pack.zip')); check(Storages.mountZip('New', 'pack.zip')); Plugins.unlink('minizip.tpm'); 42;",
    );
}
