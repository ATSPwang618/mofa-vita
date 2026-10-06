use krkr_engine::{
    Engine, EngineEvent,
    assets::{ReadPlan, Vfs},
    protocol::{
        self,
        budget::Budget,
        graphics,
        pixels::{Bytes, Pixels},
        window::{Command, Geometry, Response},
    },
};
use krkr_video::{Backend, Decoded, Decoder, Frame, Info};
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    },
    time::{Duration, Instant},
};
use tjs_core::RunBudget;
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};
mod support;

#[derive(Default)]
struct Probe {
    block: AtomicBool,
    entered: AtomicBool,
    interrupted: AtomicBool,
    alive: AtomicUsize,
}
struct Source(Arc<Probe>);
impl Backend for Source {
    fn open(
        &self,
        _: ReadPlan,
        _: Budget,
        _: Arc<AtomicBool>,
    ) -> krkr_video::Result<Box<dyn Decoder>> {
        Err("effect movie used audible open".into())
    }
    fn open_silent(
        &self,
        _: ReadPlan,
        budget: Budget,
        cancel: Arc<AtomicBool>,
    ) -> krkr_video::Result<Box<dyn Decoder>> {
        if self.0.block.load(SeqCst) {
            self.0.entered.store(true, SeqCst);
            while !cancel.load(SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.0.interrupted.store(true, SeqCst);
            return Err("cancelled".into());
        }
        self.0.alive.fetch_add(1, SeqCst);
        Ok(Box::new(Movie {
            probe: self.0.clone(),
            budget,
            frame: 0,
            info: Info {
                size: graphics::Size {
                    width: 8,
                    height: 4,
                },
                fps: 100.0,
                frames: 3,
                duration: 0.03,
                audio_streams: 1,
                video_streams: 1,
                video_stream: 0,
                audio_rate: 48000,
            },
        }))
    }
}
struct Movie {
    probe: Arc<Probe>,
    budget: Budget,
    frame: u32,
    info: Info,
}
impl Drop for Movie {
    fn drop(&mut self) {
        self.probe.alive.fetch_sub(1, SeqCst);
    }
}
impl Decoder for Movie {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next(&mut self, video: bool, audio: bool) -> krkr_video::Result<Option<Decoded>> {
        assert!(!audio, "silent movie requested audio decoding");
        if self.frame == 3 {
            return Ok(None);
        }
        if !video {
            return Ok(Some(Decoded::Pending));
        }
        let pixels = Pixels {
            size: self.info.size,
            main: Some(Bytes::zeroed(8 * 4 * 4, &self.budget).map_err(|e| e.to_string())?),
            province: None,
        };
        let frame = Frame {
            time: f64::from(self.frame) / 100.0,
            pixels: Arc::new(pixels).into(),
        };
        self.frame += 1;
        Ok(Some(Decoded::Video(frame)))
    }
    fn seek(&mut self, _: f64) -> krkr_video::Result<()> {
        self.frame = 0;
        Ok(())
    }
    fn audio_stream(&mut self, _: Option<usize>) -> krkr_video::Result<()> {
        Ok(())
    }
    fn video_stream(&mut self, _: usize) -> krkr_video::Result<()> {
        Ok(())
    }
}
fn submit(engine: &mut Engine<MonotonicClock>, text: &str) -> tjs_runtime::ContextId {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("movie lifetime", text)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine.submit(&module).unwrap_or_else(|_| panic!("submit"))
}

#[test]
fn movie_callbacks_survive_gc_and_cancelled_open_releases_worker() {
    movie_lifetime(false);
}
#[test]
fn scaled_effect_movie_preserves_alpha_layout_and_logical_frame_size() {
    movie_lifetime(true);
}
fn movie_lifetime(scaled: bool) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("movie"), []).unwrap();
    if scaled {
        let metadata = krkr_image::scale::Metadata {
            stored: graphics::Size {
                width: 8,
                height: 4,
            },
            logical: graphics::Size {
                width: 16,
                height: 8,
            },
        }
        .encode()
        .unwrap();
        std::fs::write(directory.path().join("movie.krkr-scale"), metadata).unwrap();
    }
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, support::Log).unwrap();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(directory.path(), Default::default()).unwrap(),
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
    engine.attach_windows(client).unwrap();
    let probe = Arc::new(Probe::default());
    engine.set_video_backend(Source(probe.clone()));
    let mut id = submit(
        &mut engine,
        r#"
        Plugins.link('layerExMovie.dll'); System.exitOnWindowClose=false;
        var w=new Window(), root=new Layer(w,null), a=new Layer(w,root);
        var frames=0, starts=0, stops=0;
        a.onStartMovie=function(){ global.starts++; };
        a.onStopMovie=function(){ global.stops++; };
        a.onUpdateMovie=function(){ global.frames++; System.wait(1); if(frames==2) stopMovie(); };
        a.openMovie('movie',true); a.startMovie(false);
        while(!stops) System.wait(50);
        if(frames!=2 || starts!=1 || stops!=1 || a.isPlayingMovie()) throw 'movie lifetime';
        'passed';
    "#,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut phase = 0;
    let mut writes = 0;
    loop {
        assert!(
            Instant::now() < deadline,
            "movie lifetime timeout, phase {phase}, writes {writes}"
        );
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            if let Command::Graphics(graphics::Command::CopyPixels {
                split_alpha,
                size,
                pixels,
                ..
            }) = &request.command
            {
                assert!(*split_alpha);
                assert_eq!(
                    (size.width, size.height),
                    if scaled { (8, 8) } else { (4, 4) }
                );
                assert_eq!(
                    (pixels.size.width, pixels.size.height),
                    if scaled { (16, 8) } else { (8, 4) }
                );
                writes += 1;
            }
            if matches!(request.command, Command::Graphics(_)) {
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
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } if context == id => {
                assert_eq!(engine.runtime().heap.display(value).unwrap(), "passed");
                engine.take_result(id);
                if phase == 3 {
                    break;
                }
                assert_eq!(phase, 0);
                assert_eq!(writes, 2);
                probe.block.store(true, SeqCst);
                id = submit(
                    &mut engine,
                    "a.openMovie('movie',true); throw 'cancel resumed';",
                );
                phase = 1;
            }
            EngineEvent::System {
                result: RuntimeExit::Finished(_),
                ..
            }
            | EngineEvent::Yielded
            | EngineEvent::Waiting { .. }
            | EngineEvent::Idle => {}
            other => panic!("{other:?}"),
        }
        if phase == 1 && probe.entered.load(SeqCst) {
            engine.cancel(id);
            engine.collect([]);
            assert_eq!(engine.pending_operations(), 0);
            phase = 2;
        }
        if phase == 2 && probe.interrupted.load(SeqCst) && probe.alive.load(SeqCst) == 0 {
            id = submit(
                &mut engine,
                "if(a.isPlayingMovie() || !Plugins.unlink('layerExMovie.dll')) throw 'cancel leak'; invalidate w; 'passed';",
            );
            phase = 3;
        }
        if phase == 2 || matches!(event, EngineEvent::Waiting { .. } | EngineEvent::Idle) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(probe.alive.load(SeqCst), 0);
    assert_eq!(host.staging_budget().used(), 0);
}
