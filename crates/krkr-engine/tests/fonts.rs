use krkr_engine::{
    Engine, EngineEvent,
    assets::Vfs,
    protocol::{
        graphics::Command as Draw,
        window::{self, Command, Response},
    },
};
use std::{
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};
use tjs_core::{RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};

#[test]
fn repeated_font_mapping_preserves_glyphs_but_reload_and_unmap_refresh_them() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("mapped.tft");
    let original = include_bytes!("../../krkr-render/tests/fixtures/fonts/fixture-v0.tft");
    std::fs::write(&path, original).unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(root.path(), Default::default()).unwrap(),
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
    engine.attach_windows(client).unwrap();
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8(
            "repeated-font",
            r#"
        var w=new Window(),layer=new Layer(w,null),font=layer.font;
        font.height=40;
        for(var i=0;i<8;i++) {
            font.mapPrerenderedFont('mapped.tft');
            layer.drawText(0,0,'A',0xffffff);
        }
        font.mapPrerenderedFont('mapped.tft');
        layer.drawText(0,0,'A',0xffffff);
        font.unmapPrerenderedFont();
        font.mapPrerenderedFont('mapped.tft');
        layer.drawText(0,0,'A',0xffffff);
        var caught=false;
        try {font.mapPrerenderedFont('mapped.tft');}catch(e){caught=true;}
        caught && font.getTextWidth('AA')==14;
    "#,
        )
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    let id = engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("capacity"));
    let started = Instant::now();
    let mut glyphs = Vec::new();
    loop {
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            let response = match &request.command {
                Command::Create { .. } => Response::Geometry(Default::default()),
                Command::Graphics(Draw::Text { run, .. }) => {
                    assert_eq!(run.glyphs.len(), 1);
                    glyphs.push(run.glyphs[0].glyph.id);
                    if glyphs.len() == 8 {
                        let mut replacement = original.to_vec();
                        replacement.push(0);
                        std::fs::write(&path, replacement).unwrap();
                    } else if glyphs.len() == 10 {
                        std::fs::write(&path, b"broken font").unwrap();
                    }
                    Response::Done
                }
                Command::Graphics(_) => Response::Done,
                _ => panic!("unexpected font command"),
            };
            request.respond(Ok(response));
        }
        if matches!(event, EngineEvent::Completed {context,..} if context==id) {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(15));
        if matches!(event, EngineEvent::Idle | EngineEvent::Waiting { .. }) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(matches!(
        engine.take_result(id),
        Some(RuntimeExit::Finished(Value::Int(1)))
    ));
    assert_eq!(glyphs.len(), 10);
    assert!(
        glyphs[..8].iter().all(|&id| id == glyphs[0]),
        "repeated mapping discarded glyphs: {glyphs:?}"
    );
    assert_ne!(glyphs[7], glyphs[8], "changed file must reload");
    assert_ne!(glyphs[8], glyphs[9], "unmapping must release the mapping");
}

#[test]
fn font_and_layer_share_settings_and_survive_io_gc_errors_and_invalidation() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../krkr-render/tests/fixtures");
    let mut runtime = Runtime::new();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(&root, Default::default()).unwrap(),
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
    engine.attach_windows(client).unwrap();
    let script = r#"
        System.exitOnWindowClose=false;
        var metrics=new Font();metrics.height=24;metrics.angle=900;
        if(metrics.getTextHeight("")!=24 || metrics.getEscHeightX("unused")!=24) throw "standalone height";
        class CustomLayer extends Layer {
            function CustomLayer(w,p){super.Layer(w,p);}
            function finalize(){System.wait(0);}
        }
        var w=new Window(),layer=new CustomLayer(w,null),font=layer.font;
        if(font.face!="QiushuiShotai" || metrics.getList(0).find("QiushuiShotai")<0) throw "bundled default";
        font.height=24;var defaultWidth=font.getTextWidth("秋水かな");
        font.face="missing-krkr-family";
        if(defaultWidth<=0 || font.getTextWidth("秋水かな")!=defaultWidth) throw "family fallback";
        font.face="";layer.drawText(0,0,"秋水",0xffffff);
        var missingFile=new Font();missingFile.face="missing-font.ttf";missingFile.faceIsFileName=true;
        var fileError=false;try{missingFile.getTextWidth("A");}catch(e){fileError=true;}
        if(!fileError) throw "explicit file failure hidden";
        font.face="fonts/fixture.ttf";font.faceIsFileName=true;font.height=-40;
        var other=new Font(layer);other.height=50;
        if(font.height!=50 || font.getTextWidth("AB")!=50) throw "linked font state";
        font.height=40;var r=font.getGlyphDrawRect("AB");
        if(!(r instanceof "Rect") || r.width!=36 || !r.equal(new Rect(0,4,36,32))) throw "glyph bounds";
        font.mapPrerenderedFont("fonts/fixture-v0.tft");
        if(font.getTextWidth("AA")!=14) throw "mapped metrics";
        var caught=0;try{font.mapPrerenderedFont("fonts/fixture.ttf");}catch(e){caught++;}
        if(font.getTextWidth("AA")!=14) throw "failed map lost previous font";
        layer.drawText(1,2,"AA",0xffffff);font.unmapPrerenderedFont();
        if(font.getTextWidth("AA")!=48) throw "unmap";
        layer.face=dfAlpha;layer.drawText(1,2,"AB",0xff0000,128,true,192,0,1,1,2);
        try{layer.drawText(1,2,"A",0x00ff00);}catch(e){caught++;}
        layer.hasImage=false;font.height=20;
        try{font.getTextWidth("A");}catch(e){caught++;}
        if(caught!=3) throw "error recovery";
        layer.hasImage=true;
        invalidate layer;if(isvalid font) throw "font lifetime";
        try{other.height;}catch(e){caught++;}
        w.close();caught;
    "#;
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("fonts", script)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    let id = engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("capacity"));
    let start = Instant::now();
    let mut texts = 0;
    loop {
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            if request.cancelled() {
                continue;
            }
            let response = match &request.command {
                Command::Create { .. } => Response::Geometry(Default::default()),
                Command::Graphics(Draw::Text { run, style, .. }) => {
                    texts += 1;
                    if texts == 4 {
                        request.respond(Err("test text submission failure".into()));
                        continue;
                    }
                    assert_eq!(run.glyphs.len(), if texts <= 2 { 2 } else { 4 });
                    if texts == 1 {
                        assert!(
                            run.glyphs.iter().all(|g| g
                                .glyph
                                .mask
                                .as_slice()
                                .iter()
                                .any(|&p| p != 0))
                        );
                    } else if texts == 2 {
                        assert_eq!(run.glyphs[0].glyph.levels, 65);
                    } else {
                        assert_eq!(style.opacity, 128);
                    }
                    Response::Done
                }
                Command::Graphics(_) => Response::Done,
                _ => panic!("unexpected font harness command"),
            };
            request.respond(Ok(response));
        }
        if matches!(event,EngineEvent::Completed {context,..} if context==id) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(15));
        if matches!(event, EngineEvent::Idle | EngineEvent::Waiting { .. }) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(matches!(
        engine.take_result(id),
        Some(RuntimeExit::Finished(Value::Int(4)))
    ));
    assert_eq!(texts, 4);
    assert_eq!(engine.pending_operations(), 0);

    // The explicit file face is already loaded. A short metrics query must
    // complete without creating another VFS/IO continuation, just like families.
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8(
            "cached-file-metrics",
            r#"
        var cachedFile = new Font();
        cachedFile.face = "fonts/fixture.ttf";
        cachedFile.faceIsFileName = true;
        cachedFile.height = 40;
        cachedFile.getTextWidth("AB");
    "#,
        )
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    let id = engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("capacity"));
    let event = engine.poll(
        RunBudget::new(10_000).unwrap(),
        NonZeroUsize::new(64).unwrap(),
    );
    assert!(
        matches!(event, EngineEvent::Completed { context, .. } if context == id),
        "{event:?}"
    );
    assert!(matches!(
        engine.take_result(id),
        Some(RuntimeExit::Finished(Value::Int(40)))
    ));
    assert_eq!(engine.pending_operations(), 0);
}
