#[path = "support/subtree_readback.rs"]
mod subtree_readback;

use krkr_engine::{
    Engine, EngineEvent,
    assets::Vfs,
    protocol::{
        graphics::{Command as Draw, ImageId, Size},
        window::{self, Command, Host, Response},
    },
};
use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
use tjs_core::RunBudget;
use tjs_runtime::{ContextId, Runtime, RuntimeExit, clock::MonotonicClock};

struct Image {
    size: Size,
    lifetime: Weak<krkr_protocol::graphics::ImageLifetime>,
    uploaded: bool,
    main: Vec<u8>,
    province: Vec<u8>,
}
struct Harness {
    engine: Engine<MonotonicClock>,
    client: window::Client,
    host: Host,
    images: HashMap<ImageId, Image>,
    reject: Option<&'static str>,
    hit_reads: usize,
    pixel_reads: usize,
    compressed_loads: usize,
    province_uploads: usize,
    compressed_fallback: bool,
    slice: u32,
    _directory: tempfile::TempDir,
}
impl Harness {
    fn new(slice: u32) -> Self {
        let directory = tempfile::tempdir().unwrap();
        for (name, data) in [
            (
                "art.png",
                include_bytes!("fixtures/images/gradient.png").as_slice(),
            ),
            (
                "art_m.png",
                include_bytes!("fixtures/images/gradient_m.png").as_slice(),
            ),
            (
                "art_p.png",
                include_bytes!("fixtures/images/gradient_p.png").as_slice(),
            ),
            (
                "plain.png",
                include_bytes!("fixtures/images/gradient.png").as_slice(),
            ),
            (
                "broken.png",
                &include_bytes!("fixtures/images/gradient.png")[..40],
            ),
        ] {
            std::fs::write(directory.path().join(name), data).unwrap();
        }
        let mut runtime = Runtime::new();
        krkr_engine::storages::install(
            &mut runtime.heap,
            Vfs::new(directory.path(), Default::default()).unwrap(),
        )
        .unwrap();
        let mut engine = Engine::new(
            runtime,
            MonotonicClock::default(),
            Default::default(),
            Default::default(),
        )
        .unwrap();
        let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
        engine.attach_windows(client.clone()).unwrap();
        Self {
            engine,
            client,
            host,
            images: HashMap::new(),
            reject: None,
            hit_reads: 0,
            pixel_reads: 0,
            compressed_loads: 0,
            province_uploads: 0,
            compressed_fallback: false,
            slice,
            _directory: directory,
        }
    }
    fn submit(&mut self, script: &str) -> ContextId {
        let id = self
            .engine
            .runtime_mut()
            .sources
            .add_utf8("image loading", script)
            .unwrap();
        let module = tjs_front::compile(&self.engine.runtime().sources, id).unwrap();
        self.engine
            .submit(&module)
            .unwrap_or_else(|_| panic!("capacity"))
    }
    fn step(&mut self) -> EngineEvent {
        let event = self.engine.poll(
            RunBudget::new(self.slice).unwrap(),
            NonZeroUsize::new(64).unwrap(),
        );
        self.engine.collect([]);
        event
    }
    // This protocol host checks phase ordering and payload ownership. Actual
    // texture uploads and readback are tested in render-wgpu/tests/pixels.rs.
    fn respond(&mut self, request: window::Request) {
        if request.cancelled() {
            return;
        }
        let response = match &request.command {
            Command::Graphics(Draw::LoadCompressed {
                image,
                texture,
                logical_size,
            }) => {
                self.compressed_loads += 1;
                if self.reject == Some("compressed") {
                    self.reject = None;
                    request.respond(Err("compressed allocation rejected".into()));
                    return;
                }
                assert!(
                    !self.images.contains_key(&image.id),
                    "compressed upload reserved RGBA first"
                );
                let budget = self.host.staging_budget();
                let decoded = krkr_image::compressed::decode(
                    texture,
                    &budget,
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .unwrap();
                let main = krkr_image::scale::expand(
                    decoded.main.as_ref().unwrap(),
                    texture.size,
                    *logical_size,
                    &budget,
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .unwrap();
                self.images.insert(
                    image.id,
                    Image {
                        size: *logical_size,
                        lifetime: Arc::downgrade(&image.lifetime),
                        uploaded: true,
                        main: main.as_slice().to_vec(),
                        province: Vec::new(),
                    },
                );
                // Model a native compressed backend; CPU pixels are only the
                // oracle for script reads in this harness.
                Response::ImageStorage(if self.compressed_fallback {
                    texture.size.rgba_bytes().unwrap()
                } else {
                    texture.data().len()
                })
            }
            Command::Graphics(Draw::Assign { image, source }) => {
                if self.reject == Some("assign") {
                    self.reject = None;
                    request.respond(Err("assignment rejected".into()));
                    return;
                }
                let src = &self.images[&source.id];
                self.images.insert(
                    image.id,
                    Image {
                        size: src.size,
                        lifetime: Arc::downgrade(&image.lifetime),
                        uploaded: true,
                        main: src.main.clone(),
                        province: src.province.clone(),
                    },
                );
                Response::Done
            }
            Command::Graphics(Draw::EnableImage {
                image,
                source,
                size,
                color,
            }) => {
                if self.reject == Some("enable") {
                    self.reject = None;
                    request.respond(Err("image allocation rejected".into()));
                    return;
                }
                assert!(
                    source.is_none(),
                    "this metadata fixture only enables an empty layer"
                );
                let p = color.to_be_bytes();
                self.images.insert(
                    image.id,
                    Image {
                        size: *size,
                        lifetime: Arc::downgrade(&image.lifetime),
                        uploaded: true,
                        main: [p[1], p[2], p[3], p[0]]
                            .repeat(size.width as usize * size.height as usize),
                        province: Vec::new(),
                    },
                );
                Response::Done
            }
            Command::Create { .. } => Response::Geometry(Default::default()),
            Command::Graphics(Draw::Create {
                image,
                lifetime,
                size,
                color,
            }) => {
                let p = color.to_be_bytes();
                self.images.insert(
                    *image,
                    Image {
                        size: *size,
                        lifetime: lifetime.clone(),
                        uploaded: true,
                        main: [p[1], p[2], p[3], p[0]]
                            .repeat(size.width as usize * size.height as usize),
                        province: Vec::new(),
                    },
                );
                Response::Done
            }
            Command::Graphics(Draw::CreateProvince {
                image,
                size,
                operation,
            }) => {
                assert!(matches!(
                    operation,
                    krkr_protocol::graphics::ProvinceOperation::Reserve
                ));
                self.images.insert(
                    image.id,
                    Image {
                        size: *size,
                        lifetime: Arc::downgrade(&image.lifetime),
                        uploaded: false,
                        main: Vec::new(),
                        province: Vec::new(),
                    },
                );
                Response::Done
            }
            Command::Graphics(Draw::BeginUpload {
                image,
                source,
                size,
                main,
                province,
                ..
            }) => {
                let source = source.as_ref().map(|source| &self.images[&source.id]);
                if self.reject == Some("prepare") {
                    self.reject = None;
                    request.respond(Err("resident budget exhausted".into()));
                    return;
                }
                self.images.insert(
                    image.id,
                    Image {
                        size: *size,
                        lifetime: Arc::downgrade(&image.lifetime),
                        uploaded: false,
                        main: if *main {
                            Vec::new()
                        } else {
                            source.map_or_else(Vec::new, |source| source.main.clone())
                        },
                        province: if *province {
                            vec![0; size.width as usize * size.height as usize]
                        } else {
                            Vec::new()
                        },
                    },
                );
                Response::Done
            }
            Command::Graphics(Draw::UploadScaled {
                image,
                pixels,
                logical_size,
            }) => {
                let target = self
                    .images
                    .get_mut(&image.id)
                    .expect("compact destination reserved before decode");
                assert_eq!(pixels.size, target.size);
                assert_ne!(*logical_size, pixels.size);
                assert!(pixels.province.is_none());
                let expanded = krkr_image::scale::expand(
                    pixels.main.as_ref().unwrap(),
                    pixels.size,
                    *logical_size,
                    &self.host.staging_budget(),
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .unwrap();
                target.main = expanded.as_slice().to_vec();
                target.size = *logical_size;
                target.uploaded = true;
                Response::Done
            }
            Command::Graphics(Draw::Upload { image, pixels }) => {
                if self.reject == Some("upload") {
                    self.reject = None;
                    request.respond(Err("staging budget exhausted".into()));
                    return;
                }
                let target = self
                    .images
                    .get_mut(&image.id)
                    .expect("destination reserved before decode");
                assert_eq!(pixels.size, target.size);
                if let Some(main) = &pixels.main {
                    target.main = main.as_slice().to_vec();
                }
                if let Some(province) = &pixels.province {
                    self.province_uploads += 1;
                    target.province = province.as_slice().to_vec();
                }
                target.uploaded = true;
                Response::Done
            }
            Command::Graphics(Draw::ReadHitPlane { image, province }) => {
                self.hit_reads += 1;
                let image = &self.images[&image.id];
                let data = if *province {
                    &image.province
                } else {
                    &image.main
                };
                let plane = if data.is_empty() {
                    krkr_protocol::hit::Plane {
                        size: image.size,
                        data: krkr_protocol::hit::Data::Empty,
                    }
                } else {
                    let budget = self.host.staging_budget();
                    let mut bytes =
                        krkr_protocol::pixels::Bytes::zeroed(data.len(), &budget).unwrap();
                    bytes.as_mut_slice().copy_from_slice(data);
                    krkr_protocol::hit::Plane::from_pixels(
                        image.size,
                        bytes,
                        if *province { 1 } else { 4 },
                        &budget,
                    )
                    .unwrap()
                };
                Response::HitPlane(plane)
            }
            Command::Graphics(Draw::Fill { image, fills }) => {
                let target = self.images.get_mut(&image.id).unwrap();
                for fill in fills {
                    assert_eq!(fill.face, krkr_protocol::graphics::DrawFace::Alpha);
                    assert!(!fill.hold_alpha);
                    let Some(rect) = fill.rectangle.intersection(target.size.rect()) else {
                        continue;
                    };
                    let [a, r, g, b] = fill.color.to_be_bytes();
                    for y in rect.top as usize..rect.top as usize + rect.height as usize {
                        for x in rect.left as usize..rect.left as usize + rect.width as usize {
                            let offset = (y * target.size.width as usize + x) * 4;
                            target.main[offset..offset + 4].copy_from_slice(&[r, g, b, a]);
                        }
                    }
                }
                Response::Done
            }
            Command::Graphics(Draw::ReadImage { image }) => {
                if self.reject == Some("readback") {
                    self.reject = None;
                    request.respond(Err("readback rejected".into()));
                    return;
                }
                let image = &self.images[&image.id];
                let mut main = krkr_protocol::pixels::Bytes::zeroed(
                    image.main.len(),
                    &self.host.staging_budget(),
                )
                .unwrap();
                main.as_mut_slice().copy_from_slice(&image.main);
                Response::Image(krkr_protocol::pixels::Pixels {
                    size: image.size,
                    main: Some(main),
                    province: None,
                })
            }
            Command::Graphics(Draw::Pixel {
                image,
                x,
                y,
                province,
            }) => {
                self.pixel_reads += 1;
                let image = &self.images[&image.id];
                let i = *y as usize * image.size.width as usize + *x as usize;
                Response::Pixel(if *province {
                    image.province.get(i).copied().unwrap_or(0) as u32
                } else {
                    let p = &image.main[4 * i..4 * i + 4];
                    u32::from_be_bytes([p[3], p[0], p[1], p[2]])
                })
            }
            command => panic!("unexpected command {command:?}"),
        };
        request.respond(Ok(response));
    }
    fn scenes(&mut self) {
        for (_, scene) in self.host.take_scenes(u64::MAX) {
            for node in scene.nodes {
                assert!(
                    node.image
                        .as_ref()
                        .is_none_or(|image| self.images[&image.id].uploaded),
                    "uncommitted upload entered a scene"
                );
            }
        }
        self.images
            .retain(|_, image| image.lifetime.upgrade().is_some());
    }
    fn run(&mut self, script: &str) -> String {
        let id = self.submit(script);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            while let Some(request) = self.host.next_request() {
                self.respond(request);
            }
            match self.step() {
                EngineEvent::Completed {
                    context,
                    result: RuntimeExit::Finished(value),
                } if context == id => {
                    let text = self.engine.runtime().heap.display(value).unwrap();
                    self.engine.take_result(id);
                    self.scenes();
                    return text;
                }
                EngineEvent::Yielded => {}
                EngineEvent::Waiting { .. } => std::thread::park_timeout(Duration::from_millis(1)),
                event => panic!("{event:?}"),
            }
            self.scenes();
        }
        panic!("image task did not finish");
    }
    fn reach(&mut self, phase: &str) -> window::Request {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            while let Some(request) = self.host.next_request() {
                if matches!(
                    (&request.command, phase),
                    (Command::Graphics(Draw::Assign { .. }), "assign")
                        | (Command::Graphics(Draw::ReadImage { .. }), "readback")
                        | (Command::Graphics(Draw::ReadHitPlane { .. }), "hit")
                        | (Command::Graphics(Draw::BeginUpload { .. }), "prepare")
                        | (Command::Graphics(Draw::Upload { .. }), "upload")
                        | (Command::Graphics(Draw::LoadCompressed { .. }), "compressed")
                ) {
                    return request;
                }
                self.respond(request);
            }
            assert!(matches!(
                self.step(),
                EngineEvent::Yielded | EngineEvent::Waiting { .. }
            ));
            self.scenes();
            std::thread::park_timeout(Duration::from_millis(1));
        }
        panic!("image phase did not arrive");
    }
}
const SETUP: &str =
    "var w=new Window(), root=new Layer(w,null), art=new Layer(w,root); art.visible=true;";
const UNCHANGED: &str = "art.imageWidth==512 && art.clipWidth==512 && art.getMainPixel(256,90)==0x005a5a && art.getMaskPixel(256,90)==128 && art.getProvincePixel(5,3)==12;";

#[test]
fn repeated_script_mask_reads_share_a_plane_and_reload_invalidates_it() {
    let mut h = Harness::new(1);
    h.run(SETUP);
    h.run("art.loadImages('art');");
    assert_eq!(
        h.run("var ok=true; for(var y=0;y<128;y++) ok=ok && art.getMaskPixel(256,y)==128; ok;"),
        "1"
    );
    assert_eq!(
        h.pixel_reads, 3,
        "only the first three samples cross the command queue"
    );
    assert_eq!(h.hit_reads, 1);
    assert_eq!(
        h.run("art.getMainPixel(256,90)==0x005a5a;"),
        "1",
        "RGB must not use the alpha cache"
    );
    assert_eq!(
        h.run("var ok=true; for(var i=0;i<20;i++) ok=ok && art.getProvincePixel(5,3)==12; ok;"),
        "1"
    );
    assert_eq!(h.hit_reads, 2, "province has its own cached plane");
    h.run("art.loadImages('plain');");
    let previous = h.pixel_reads;
    assert_eq!(
        h.run("var ok=true; for(var y=0;y<128;y++) ok=ok && art.getMaskPixel(256,y)==255; ok;"),
        "1"
    );
    assert_eq!(h.hit_reads, 3);
    assert_eq!(
        h.pixel_reads - previous,
        3,
        "large images learn each new revision"
    );
    h.engine.reset();
    assert_eq!(h.host.staging_budget().used(), 0);
}

#[test]
fn animated_small_masks_keep_the_read_preference_but_refresh_the_pixels() {
    for slice in [1, 10000] {
        let mut h = Harness::new(slice);
        h.run(SETUP);
        h.run("art.face=dfAlpha;");
        for alpha in [17, 77, 139, 255, 0] {
            assert_eq!(
                h.run(&format!(
                    "art.fillRect(0,0,32,32,{}); var ok=true; for(var x=0;x<16;x++) ok=ok && art.getMaskPixel(x,0)=={alpha}; ok;",
                    ((alpha as u32) << 24) | 0x010203
                )),
                "1"
            );
        }
        assert_eq!(h.hit_reads, 5, "each changed mask needs fresh pixels");
        assert_eq!(
            h.pixel_reads, 3,
            "only the first version needs individual samples"
        );
        assert_eq!(h.run("art.getMainPixel(0,0)==0x010203;"), "1");
        assert_eq!(h.pixel_reads, 4, "RGB reads do not use an alpha plane");
        h.engine.reset();
        assert_eq!(h.host.staging_budget().used(), 0);
    }
}

#[test]
fn cancelled_mask_read_cannot_populate_the_script_cache() {
    let mut h = Harness::new(1);
    h.run(SETUP);
    h.run("art.loadImages('art');");
    let context = h.submit("for(var i=0;i<10;i++) art.getMaskPixel(256,i);");
    let request = h.reach("hit");
    h.engine.cancel(context);
    assert!(request.cancelled());
    request.respond(Ok(Response::HitPlane(krkr_protocol::hit::Plane {
        size: Size {
            width: 512,
            height: 128,
        },
        data: krkr_protocol::hit::Data::Uniform(7),
    })));
    assert_eq!(h.run("art.getMaskPixel(256,90);"), "128");
    assert_eq!(h.hit_reads, 1, "the cancelled completion was not reused");
}

#[test]
fn zero_gamma_builds_a_defined_channel_table_without_replacing_it_with_identity() {
    let mut h = Harness::new(1);
    h.run(SETUP);
    h.run("art.loadImages('plain');");
    let id = h.submit("art.adjustGamma(0,17,230); 'passed';");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut checked = false;
    while Instant::now() < deadline {
        while let Some(request) = h.host.next_request() {
            if let Command::Graphics(Draw::Adjust {
                operation: krkr_protocol::graphics::Adjustment::Gamma { table, additive },
                ..
            }) = &request.command
            {
                assert!(!additive);
                for (index, row) in table.iter().enumerate() {
                    assert_eq!(row[0], if index == 255 { 0 } else { 17 });
                    assert_eq!(row[1], index as u32);
                    assert_eq!(row[2], index as u32);
                }
                checked = true;
                request.respond(Ok(Response::Done));
            } else {
                h.respond(request);
            }
        }
        match h.step() {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(_),
            } if context == id => {
                assert!(checked);
                return;
            }
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            event => panic!("zero gamma failed: {event:?}"),
        }
        h.scenes();
    }
    panic!("zero gamma did not complete");
}

#[test]
fn compressed_tiles_return_character_tags_on_first_load_cache_hit_and_preload() {
    for preload in [false, true] {
        let mut h = Harness::new(1);
        let bytes = krkr_image::compressed::ktx_tiles(
            Size {
                width: 96,
                height: 32,
            },
            Size {
                width: 32,
                height: 16,
            },
            krkr_protocol::texture::Format::Etc1,
            &[0x12, 0x34, 0x56, 0, 0, 0, 0, 0].repeat(192),
            &vec![
                ("groundLevel".into(), "454".into()),
                ("monoList".into(), "a@a2@b".into()),
                ("centerCorrect".into(), "-34".into()),
            ],
        )
        .unwrap();
        std::fs::write(h._directory.path().join("character.ktx"), bytes).unwrap();
        h.run(SETUP);
        h.run("System.graphicCacheLimit=65536;");
        if preload {
            h.run("System.touchImages(['character.ktx']);");
        }
        for _ in 0..2 {
            assert_eq!(h.run("var tags=art.loadImages('character.ktx'); tags.groundLevel==='454' && tags.monoList==='a@a2@b' && tags.centerCorrect==='-34' && art.imageWidth==96 && art.getMainPixel(64,16)==0x133557;"), "1");
        }
        assert_eq!(h.compressed_loads, 1);
        h.run("art.loadImages('character.ktx',0x02ffffff);");
        assert_eq!(
            h.compressed_loads, 1,
            "equivalent no-key values reuse the preload"
        );
    }
}

#[test]
fn repeated_universal_rules_reuse_uploaded_luminance_and_clear_with_graphic_cache() {
    let mut h = Harness::new(10000);
    h.run(SETUP);
    h.run("var other=new Layer(w,root); var color=new Layer(w,root); color.loadImages('plain');");
    let code = "art.beginTransition('universal',false,other,%[time:10000,rule:'plain']);art.stopTransition();";
    h.run(code);
    h.run(code);
    assert_eq!(h.province_uploads, 1);
    h.run("System.clearGraphicCache();");
    h.run(code);
    assert_eq!(h.province_uploads, 2);
}

#[test]
fn image_sidecar_lookup_refreshes_after_a_managed_provider_runs() {
    use krkr_engine::storages::managed::{self, Medium, Operation};
    use tjs_core::{NativeContinuation, NativeCx, NativeResult, NativeStep, Trace, Value};

    struct Provider(std::path::PathBuf);
    impl Trace for Provider {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl Medium for Provider {
        fn resolve(
            &self,
            cx: &mut NativeCx<'_>,
            path: Vec<u16>,
            operation: Operation,
            next: Box<dyn NativeContinuation>,
        ) -> NativeResult<NativeStep> {
            let value = if matches!(operation, Operation::Read)
                && String::from_utf16_lossy(&path).ends_with("/provided.png")
            {
                // The explicit local lookup already missed and snapshotted
                // this directory. A provider can create the next sidecar.
                std::fs::write(
                    self.0.join("provided_m.png"),
                    include_bytes!("fixtures/images/gradient_m.png"),
                )
                .unwrap();
                Value::Octet(
                    cx.heap_mut()
                        .alloc_octet(include_bytes!("fixtures/images/gradient.png").to_vec()),
                )
            } else {
                Value::Void
            };
            next.resume(cx, value)
        }
    }

    let mut h = Harness::new(1);
    let provider = std::rc::Rc::new(Provider(h._directory.path().to_owned()));
    let registration =
        managed::register(&mut h.engine.runtime_mut().heap, "fixture", provider).unwrap();
    h.run(SETUP);
    h.run("Storages.addAutoPath('fixture://./');art.loadImages('art');var expectedMask=art.getMaskPixel(256,10);");
    assert_eq!(
        h.run("art.loadImages('plain');art.getMaskPixel(256,10)!==expectedMask;"),
        "1"
    );
    assert_eq!(
        h.run("art.loadImages('provided.png');art.getMaskPixel(256,10)===expectedMask;"),
        "1"
    );
    assert!(h._directory.path().join("provided_m.png").exists());
    drop(registration);
}

#[test]
fn compressed_preloads_count_native_blocks_and_respect_fallback_storage() {
    for fallback in [false, true] {
        let mut h = Harness::new(1);
        h.compressed_fallback = fallback;
        let bytes = krkr_image::compressed::ktx(
            Size {
                width: 64,
                height: 32,
            },
            &[0x12, 0x34, 0x56, 0, 0, 0, 0, 0].repeat(128),
        )
        .unwrap();
        for name in ["first.ktx", "second.ktx"] {
            std::fs::write(h._directory.path().join(name), &bytes).unwrap();
        }
        h.run(SETUP);
        h.run("System.graphicCacheLimit=4096; System.touchImages(['first.ktx','second.ktx']);");
        // Native blocks fit both images; the fallback fills the batch's byte
        // limit after one upload and must not be retained as a 1 KiB texture.
        assert_eq!(h.compressed_loads, if fallback { 1 } else { 2 });
        assert_eq!(h.run("art.loadImages('first.ktx'); art.imageWidth==64 && art.getMainPixel(0,0)==0x133557;"), "1");
        assert_eq!(h.compressed_loads, 2);
        h.run("art.loadImages('second.ktx');");
        assert_eq!(h.compressed_loads, if fallback { 3 } else { 2 });
    }
}

#[test]
fn compressed_loads_skip_rgba_reservation_keep_cache_and_roll_back_rejected_uploads() {
    let mut h = Harness::new(1);
    let stored = Size {
        width: 64,
        height: 32,
    };
    let logical = Size {
        width: 126,
        height: 70,
    };
    let bytes = krkr_image::compressed::ktx(stored, &[0x12, 0x34, 0x56, 0, 0, 0, 0, 0].repeat(128))
        .unwrap();
    for name in ["packed.png", "other.png"] {
        std::fs::write(h._directory.path().join(name), &bytes).unwrap();
        std::fs::write(
            h._directory.path().join(format!("{name}.krkr-scale")),
            krkr_image::scale::Metadata { logical, stored }
                .encode()
                .unwrap(),
        )
        .unwrap();
    }
    h.run(SETUP);
    // ETC1 fits with metadata; an 8192-byte RGBA charge would reject retention.
    h.run("System.graphicCacheLimit=4096;");
    assert_eq!(h.run("art.loadImages('packed'); art.imageWidth==126 && art.getMainPixel(0,0)==0x133557 && art.getMaskPixel(0,0)==255;"), "1");
    assert_eq!(h.compressed_loads, 1);
    assert_eq!(h.run("var cached=new Layer(w,root);cached.loadImages('packed');cached.getMainPixel(0,0)==art.getMainPixel(0,0);"), "1");
    assert_eq!(
        h.compressed_loads, 1,
        "cache hit reuploaded compressed blocks"
    );
    h.reject = Some("compressed");
    assert_eq!(h.run("try { art.loadImages('other'); } catch(e) {} art.imageWidth==126 && art.getMainPixel(0,0)==0x133557;"), "1");
    assert_eq!(h.compressed_loads, 2);
    assert_eq!(h.engine.pending_operations(), 0);
    let context = h.submit("art.loadImages('other');");
    let request = h.reach("compressed");
    h.engine.cancel(context);
    assert!(request.cancelled());
    request.respond(Ok(Response::Done));
    assert_eq!(
        h.run("art.imageWidth==126 && art.getMainPixel(0,0)==0x133557;"),
        "1"
    );
}

#[test]
fn compact_image_metadata_survives_async_load_cache_and_logical_pixel_queries() {
    let mut h = Harness::new(1);
    h.run(SETUP);
    assert_eq!(h.run("art.loadImages('plain'); art.imageWidth==512;"), "1");
    let metadata = krkr_image::scale::Metadata {
        logical: Size {
            width: 1024,
            height: 576,
        },
        stored: Size {
            width: 512,
            height: 288,
        },
    }
    .encode()
    .unwrap();
    std::fs::write(h._directory.path().join("plain.png.krkr-scale"), metadata).unwrap();
    assert_eq!(h.run("art.loadImages('plain'); art.imageWidth==1024 && art.imageHeight==576 && art.clipWidth==1024 && art.getMainPixel(512,180)==0x005a5a;"), "1");
    assert_eq!(h.run("var cached=new Layer(w,root); cached.loadImages('plain'); cached.imageWidth==1024 && cached.getMainPixel(512,180)==art.getMainPixel(512,180);"), "1");
    assert_eq!(h.run("cached.assignImages(art); cached.imageHeight==576 && cached.getMaskPixel(512,180)==art.getMaskPixel(512,180);"), "1");
    assert_eq!(h.engine.pending_operations(), 0);
}

#[test]
fn hit_planes_are_cached_and_script_vetoes_visibility_and_disabled_layers_keep_original_order() {
    let mut h = Harness::new(1);
    assert_eq!(h.run(r#"
        var w=new Window(),root=new Layer(w,null),veto=false;
        class Hit extends Layer {
            function Hit(w,p){super.Layer(w,p);visible=true;}
            function onHitTest(x,y,hit){System.wait(0);super.onHitTest(x,y,!global.veto);return false;}
        }
        var art=new Hit(w,root);art.loadImages('art');art.imageLeft=-256;
        art.hitThreshold=128;art.opacity=0;art.setClip(0,0,1,1);
        for(var i=0;i<5;i++) if(root.getLayerAt(1,10)!==art) throw 'alpha hit';
        art.hitThreshold=129;if(root.getLayerAt(0,10)!==root) throw 'threshold';
        art.hitThreshold=128;
        if(art.getLayerAt(0,10,true)!==root) throw 'exclude receiver';
        art.enabled=false;
        if(root.getLayerAt(0,10)!==null || root.getLayerAt(0,10,false,true)!==art) throw 'disabled occlusion';
        art.enabled=true;veto=true;
        if(root.getLayerAt(0,10)!==root) throw 'script veto';
        veto=false;art.visible=false;
        if(root.getLayerAt(0,10)!==root) throw 'visibility';
        art.visible=true;art.imageLeft=0;art.hitType=htProvince;
        if(root.getLayerAt(5,3)!==art) throw 'province hit';
        if(root.getLayerAt(0,0)!==art) throw 'province nonzero ignores alpha threshold';
        42;
    "#),"42");
    assert_eq!(
        h.hit_reads, 2,
        "one alpha read and one province read despite many queries"
    );
    h.run("art.loadImages('plain');art.hitType=htMask;art.hitThreshold=16;");
    assert_eq!(
        h.host.staging_budget().used(),
        0,
        "replacement releases old cached planes"
    );
    let context = h.submit("root.getLayerAt(1,1);");
    let request = h.reach("hit");
    h.engine.cancel(context);
    assert!(request.cancelled());
    request.respond(Ok(Response::HitPlane(krkr_protocol::hit::Plane {
        size: Size {
            width: 32,
            height: 32,
        },
        data: krkr_protocol::hit::Data::Uniform(0),
    })));
    assert_eq!(
        h.run("root.getLayerAt(1,1)===art;"),
        "1",
        "cancelled completion must not become a cached miss"
    );
    assert_eq!(h.hit_reads, 3);
    h.engine.reset();
    assert_eq!(h.host.staging_budget().used(), 0);
}

#[test]
fn hit_test_finishes_while_later_frames_keep_changing_the_mask() {
    let mut h = Harness::new(1);
    h.run(SETUP);
    h.run("root.hitThreshold=0;art.loadImages('art');art.imageLeft=-256;art.hitThreshold=128;");
    let context = h.submit("root.getLayerAt(1,1)===art;");
    let first = h.reach("hit");
    let mut pending = Some(first);
    let mut writes = Vec::new();
    let mut reads = 0;
    let mut finished = None;
    for _ in 0..1000 {
        while let Some(request) = pending.take().or_else(|| h.host.next_request()) {
            if let Command::Graphics(Draw::ReadHitPlane { image, .. }) = &request.command {
                reads += 1;
                assert!(
                    reads <= 8,
                    "hit testing kept chasing newer animation frames"
                );
                // A media frame can enter the same queue while an input read
                // is pending. It follows the read, so that snapshot is valid
                // for this query even though it cannot become the current cache.
                writes.push(
                    h.client
                        .request(
                            request.window,
                            Command::Graphics(Draw::Fill {
                                image: image.clone(),
                                fills: vec![krkr_protocol::graphics::Fill {
                                    rectangle: Size {
                                        width: 512,
                                        height: 128,
                                    }
                                    .rect(),
                                    color: 0,
                                    face: krkr_protocol::graphics::DrawFace::Alpha,
                                    hold_alpha: false,
                                }],
                            }),
                        )
                        .unwrap(),
                );
            }
            h.respond(request);
        }
        if let EngineEvent::Completed {
            context: id,
            result: RuntimeExit::Finished(value),
        } = h.step()
            && id == context
        {
            finished = Some(h.engine.runtime().heap.display(value).unwrap());
            h.engine.take_result(context);
            break;
        }
        h.scenes();
    }
    assert_eq!(finished.as_deref(), Some("1"));
    assert_eq!(reads, 1, "one input query uses one ordered snapshot");
    assert!(
        writes
            .iter()
            .all(|ticket| matches!(ticket.take(), Some(Ok(Response::Done))))
    );
    assert_eq!(h.run("root.getLayerAt(1,1)===root;"), "1");
    assert_eq!(h.hit_reads, 2, "a later query must read the changed mask");
    assert_eq!(h.engine.pending_operations(), 0);
}

#[test]
fn changing_drawable_type_restores_an_image_only_after_host_success() {
    let mut h = Harness::new(1);
    h.run(SETUP);
    assert_eq!(h.run("art.hasImage=false;art.type=ltPsSoftLight;art.hasImage && art.face==dfAuto && art.getMainPixel(0,0)==0x808080 && art.getMaskPixel(0,0)==0;"), "1");
    h.run("art.hasImage=false;");
    h.reject = Some("enable");
    assert_eq!(h.run("var caught=false;try{art.type=ltSubtractive;}catch(e){caught=true;}caught && !art.hasImage && art.type==ltPsSoftLight;"), "1");
    // Assigning the same type does not allocate; changing it does.
    assert_eq!(h.run("art.type=ltPsSoftLight;!art.hasImage;"), "1");
    assert_eq!(h.run("art.type=ltSubtractive;art.hasImage && art.getMainPixel(0,0)==0xffffff && art.getMaskPixel(0,0)==0;"), "1");
}

#[test]
fn assignment_empty_layers_and_cancelled_replacements_preserve_state() {
    let mut h = Harness::new(1);
    h.run(SETUP);
    h.run("art.loadImages('art'); var copy=new Layer(w,root); copy.assignImages(art);");
    assert_eq!(h.run("copy.imageWidth==512 && copy.width==32 && copy.getProvincePixel(5,3)==12 && copy.getMaskPixel(256,90)==128;"),"1");
    assert_eq!(
        h.run("copy.setClip(2,3,4,5);copy.assignImages(copy);copy.clipWidth==512;"),
        "1"
    );
    assert_eq!(h.run("copy.hasImage=false;var errors=0;try{copy.imageWidth;}catch(e){errors++;}try{copy.loadImages('art');}catch(e){errors++;}try{copy.setClip(0,0,1,1);}catch(e){errors++;}!copy.hasImage && copy.getProvincePixel(0,0)==0 && errors==3;"),"1");
    assert_eq!(h.run("copy.hasImage=true;copy.imageWidth==32 && copy.clipWidth==32 && copy.getMaskPixel(0,0)==0;"),"1");
    let context = h.submit("copy.assignImages(art);");
    let request = h.reach("assign");
    let Command::Graphics(Draw::Assign { image, .. }) = &request.command else {
        unreachable!()
    };
    let lifetime = Arc::downgrade(&image.lifetime);
    h.engine.cancel(context);
    h.engine.collect([]);
    assert!(request.cancelled());
    request.respond(Ok(Response::Done));
    assert!(lifetime.upgrade().is_none());
    assert_eq!(
        h.run("copy.imageWidth==32 && copy.getMaskPixel(0,0)==0;"),
        "1"
    );
    h.reject = Some("assign");
    assert_eq!(h.run("var caught=false;try{copy.assignImages(art);}catch(e){caught=true;}caught && copy.imageWidth==32;"),"1");
    assert_eq!(h.run("copy.hasImage=false;art.assignImages(copy);!art.hasImage && art.getProvincePixel(5,3)==0;"),"1");
    h.run("System.clearGraphicCache();");
    assert_eq!(h.images.len(), 1);
    assert_eq!(h.engine.pending_operations(), 0);
}

#[test]
fn image_calls_gc_metadata_failures_and_plane_replacement() {
    for slice in [1, 10000] {
        let mut h = Harness::new(slice);
        h.run(SETUP);
        assert_eq!(h.run("var tags=art.loadImages('art'); tags.offs_x==='-2' && tags.offs_y==='3' && art.width==32 && art.imageWidth==512 && art.imageHeight==288 && art.clipWidth==512;"),"1");
        assert_eq!(h.run(UNCHANGED), "1");
        assert_eq!(h.run("art.loadProvinceImage('art_p.png')===void;"), "1");
        assert_eq!(h.run(UNCHANGED), "1");
        for phase in ["prepare", "upload"] {
            h.reject = Some(phase);
            assert_eq!(h.run("var caught=false; try {art.loadImages('plain.png');} catch(e) {caught=true;} caught;"),"1");
            assert_eq!(h.run(UNCHANGED), "1");
            assert_eq!(h.images.len(), 3); // Root, layer and immutable cache alias.
        }
        assert_eq!(h.run("var caught=0; try {art.loadImages('broken.png');} catch(e) {caught++;} try {art.loadImages('absent');} catch(e) {caught++;} caught;"),"2");
        assert_eq!(h.run(UNCHANGED), "1");
        assert_eq!(h.run("art.loadImages('plain.png'); art.getProvincePixel(5,3)==0 && art.getMaskPixel(256,90)==255;"),"1");
        h.run("System.clearGraphicCache();");
        assert_eq!(h.host.staging_budget().used(), 0);
        assert_eq!(h.engine.pending_operations(), 0);
        assert_eq!(h.images.len(), 2);
    }
}

#[test]
fn cancelling_at_reservation_and_upload_drops_staging_and_rejects_late_completion() {
    for phase in ["prepare", "upload", "assign"] {
        let mut h = Harness::new(1);
        h.run(SETUP);
        h.run("art.loadImages('art');");
        let context = h.submit("art.loadImages('plain.png');");
        let request = h.reach(phase);
        let lifetime = match &request.command {
            Command::Graphics(
                Draw::BeginUpload { image, .. }
                | Draw::Upload { image, .. }
                | Draw::Assign { image, .. },
            ) => Arc::downgrade(&image.lifetime),
            _ => unreachable!(),
        };
        h.engine.cancel(context);
        h.engine.collect([]);
        assert!(request.cancelled());
        request.respond(Ok(Response::Done));
        assert!(lifetime.upgrade().is_none());
        assert_eq!(h.run(UNCHANGED), "1");
        assert_eq!(h.host.staging_budget().used(), 0);
        assert_eq!(h.images.len(), 3); // Cancelled loads never add a cache alias.
        assert_eq!(h.engine.pending_operations(), 0);
        assert_eq!(
            h.run("art.loadImages('plain'); art.getMaskPixel(256,90)==255;"),
            "1"
        );
    }
}

#[test]
fn save_waits_for_file_completion_keeps_gc_roots_and_returns_errors_to_the_call_site() {
    for slice in [1, 10000] {
        let mut h = Harness::new(slice);
        h.run(SETUP);
        h.run("art.loadImages('art');art.setClip(1,2,3,4);var copy=new Layer(w,root);");
        assert_eq!(h.run("art.saveLayerImage('saved.png','png')===void && Storages.isExistentStorage('saved.png');"), "1");
        assert_eq!(h.run("copy.loadImages('saved.png');copy.imageWidth==512 && copy.imageHeight==288 && copy.getMainPixel(256,90)==0x005a5a && copy.getMaskPixel(256,90)==128 && copy.getProvincePixel(5,3)==0 && art.clipWidth==3;"), "1");
        assert_eq!(h.run("art.saveLayerImage('saved.tlg','tlg5');var tags=copy.loadImages('saved.tlg');tags.mode==='alpha' && copy.getMaskPixel(256,90)==128;"), "1");
        assert_eq!(h.run("art.saveLayerImage('saved.bmp');copy.loadImages('saved.bmp');copy.getMaskPixel(256,90)==128;"), "1");
        assert_eq!(h.run("var caught=0;try{art.saveLayerImage('bad.png','other');}catch(e){caught++;}try{art.saveLayerImage('missing/file.png','png');}catch(e){caught++;}copy.hasImage=false;try{copy.saveLayerImage('absent.bmp');}catch(e){caught++;}caught;"), "3");
        h.reject = Some("readback");
        assert_eq!(h.run("var caught=false;try{art.saveLayerImage('failed.png','png');}catch(e){caught=true;}caught && !Storages.isExistentStorage('failed.png');"), "1");
        assert_eq!(h.host.staging_budget().used(), 0);
        assert_eq!(h.engine.pending_operations(), 0);
    }
}

#[test]
fn cancelled_save_discards_late_pixels_without_creating_a_file() {
    let mut h = Harness::new(1);
    h.run(SETUP);
    h.run("art.loadImages('art');");
    let id = h.submit("art.saveLayerImage('cancelled.png','png');throw 'resumed cancelled save';");
    let request = h.reach("readback");
    h.engine.cancel(id);
    assert!(request.cancelled());
    let mut main = krkr_protocol::pixels::Bytes::zeroed(4, &h.host.staging_budget()).unwrap();
    main.as_mut_slice().copy_from_slice(&[1, 2, 3, 4]);
    request.respond(Ok(Response::Image(krkr_protocol::pixels::Pixels {
        size: Size {
            width: 1,
            height: 1,
        },
        main: Some(main),
        province: None,
    })));
    assert_eq!(h.run("!Storages.isExistentStorage('cancelled.png');"), "1");
    assert_eq!(h.host.staging_budget().used(), 0);
    assert_eq!(h.engine.pending_operations(), 0);
}
