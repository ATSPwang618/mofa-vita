//! One integrated source-derived clipboard scenario, using only in-memory
//! clipboard storage and the existing ordered graphics protocol.
use krkr_engine::{
    Engine, EngineEvent,
    clipboard::{self, Data, Host, TJS_FORMAT, Watch},
    protocol::{
        self,
        budget::Budget,
        graphics::{Command as Draw, ImageId, ImageLifetime, Size},
        pixels::{Bytes, Pixels},
        window::{Command, Response},
    },
};
use std::{
    cell::RefCell,
    collections::HashMap,
    num::NonZeroUsize,
    rc::Rc,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
use tjs_core::RunBudget;
use tjs_runtime::{ContextId, Runtime, RuntimeExit, clock::MonotonicClock};
#[derive(Default)]
struct Stored {
    text: Option<Vec<u16>>,
    tjs: Option<Vec<u8>>,
    image: Option<(Size, Vec<u8>)>,
    changed: Option<Arc<dyn Fn() + Send + Sync>>,
    starts: usize,
    stops: usize,
    writes: usize,
    failure: Option<String>,
    reject: bool,
}
#[derive(Clone, Default)]
struct Memory(Rc<RefCell<Stored>>);
impl Memory {
    fn notify(&self) {
        if let Some(changed) = self.0.borrow().changed.clone() {
            changed();
        }
    }
}
struct Subscription(Memory);
impl Watch for Subscription {
    fn error(&self) -> Option<String> {
        self.0.0.borrow().failure.clone()
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        let mut state = self.0.0.borrow_mut();
        state.changed = None;
        state.stops += 1;
    }
}
impl Host for Memory {
    fn text(&mut self) -> Result<Option<Vec<u16>>, String> {
        Ok(self.0.borrow().text.clone())
    }
    fn buffer(&mut self, format: &str) -> Result<Option<Vec<u8>>, String> {
        assert_eq!(format, TJS_FORMAT);
        Ok(self.0.borrow().tjs.clone())
    }
    fn image(&mut self, budget: &Budget) -> Result<Option<Pixels>, String> {
        let state = self.0.borrow();
        let Some((size, bytes)) = &state.image else {
            return Ok(None);
        };
        let mut main = Bytes::zeroed(bytes.len(), budget).map_err(|e| e.to_string())?;
        main.as_mut_slice().copy_from_slice(bytes);
        Ok(Some(Pixels {
            size: *size,
            main: Some(main),
            province: None,
        }))
    }
    fn write(&mut self, data: Data) -> Result<(), String> {
        let mut state = self.0.borrow_mut();
        if state.reject {
            state.reject = false;
            return Err("injected clipboard rejection".into());
        }
        state.text = data.text;
        state.tjs = data.tjs;
        state.image = data
            .image
            .map(|p| (p.size, p.main.as_ref().unwrap().as_slice().to_vec()));
        state.writes += 1;
        let changed = state.changed.clone();
        drop(state);
        if let Some(changed) = changed {
            changed();
        }
        Ok(())
    }
    fn has(&mut self, format: i32) -> Result<bool, String> {
        let s = self.0.borrow();
        Ok(match format {
            1 => s.text.is_some(),
            2 => s.image.is_some(),
            3 => s.tjs.is_some(),
            _ => false,
        })
    }
    fn watch(&mut self, changed: Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn Watch>, String> {
        let mut s = self.0.borrow_mut();
        s.changed = Some(changed);
        s.starts += 1;
        Ok(Box::new(Subscription(self.clone())))
    }
}
struct Image {
    size: Size,
    pixels: Vec<u8>,
    lifetime: Weak<ImageLifetime>,
}
struct Harness {
    engine: Engine<MonotonicClock>,
    host: protocol::window::Host,
    memory: Memory,
    images: HashMap<ImageId, Image>,
}
impl Harness {
    fn new() -> Self {
        let mut runtime = Runtime::new();
        krkr_engine::scripts::install(&mut runtime.heap).unwrap();
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
        let memory = Memory::default();
        clipboard::set_host(&mut engine.runtime_mut().heap, memory.clone()).unwrap();
        Self {
            engine,
            host,
            memory,
            images: HashMap::new(),
        }
    }
    fn submit(&mut self, text: &str) -> ContextId {
        let source = self
            .engine
            .runtime_mut()
            .sources
            .add_utf8("clipboard scenario", text)
            .unwrap();
        let module = tjs_front::compile(&self.engine.runtime().sources, source).unwrap();
        self.engine
            .submit(&module)
            .unwrap_or_else(|_| panic!("context capacity"))
    }
    fn step(&mut self) -> EngineEvent {
        let event = self
            .engine
            .poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        self.engine.collect([]);
        event
    }
    fn respond(&mut self, request: protocol::window::Request) {
        if request.cancelled() {
            return;
        }
        let response = match &request.command {
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
                        pixels: [p[1], p[2], p[3], p[0]]
                            .repeat((size.width * size.height) as usize),
                        lifetime: lifetime.clone(),
                    },
                );
                Response::Done
            }
            Command::Graphics(Draw::EnableImage {
                image, size, color, ..
            }) => {
                let p = color.to_be_bytes();
                self.images.insert(
                    image.id,
                    Image {
                        size: *size,
                        pixels: [p[1], p[2], p[3], p[0]]
                            .repeat((size.width * size.height) as usize),
                        lifetime: Arc::downgrade(&image.lifetime),
                    },
                );
                Response::Done
            }
            Command::Graphics(Draw::AssignBitmap { image, pixels, .. }) => {
                self.images.insert(
                    image.id,
                    Image {
                        size: pixels.size,
                        pixels: pixels.main.as_ref().unwrap().as_slice().to_vec(),
                        lifetime: Arc::downgrade(&image.lifetime),
                    },
                );
                Response::Done
            }
            Command::Graphics(Draw::ReadImage { image }) => {
                let image = &self.images[&image.id];
                let mut main =
                    Bytes::zeroed(image.pixels.len(), &self.host.staging_budget()).unwrap();
                main.as_mut_slice().copy_from_slice(&image.pixels);
                Response::Image(Pixels {
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
                assert!(!province);
                let image = &self.images[&image.id];
                let at = (*y as usize * image.size.width as usize + *x as usize) * 4;
                let p = &image.pixels[at..at + 4];
                Response::Pixel(u32::from_be_bytes([p[3], p[0], p[1], p[2]]))
            }
            command => panic!("unexpected clipboard graphics request {command:?}"),
        };
        request.respond(Ok(response));
    }
    fn scenes(&mut self) {
        self.host.take_scenes(u64::MAX);
        self.images
            .retain(|_, image| image.lifetime.upgrade().is_some());
    }
    fn run(&mut self, text: &str) -> String {
        let id = self.submit(text);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(Instant::now() < deadline, "clipboard scenario timed out");
            while let Some(request) = self.host.next_request() {
                self.respond(request);
            }
            let event = self.step();
            self.scenes();
            match event {
                EngineEvent::Completed {
                    context,
                    result: RuntimeExit::Finished(value),
                } if context == id => {
                    let out = self.engine.runtime().heap.display(value).unwrap();
                    self.engine.take_result(id);
                    return out;
                }
                EngineEvent::Window {
                    context,
                    result: RuntimeExit::Finished(_),
                    ..
                } => {
                    self.engine.take_result(context);
                }
                EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
                other => panic!("clipboard scenario failed: {other:?}"),
            }
        }
    }
    fn drain(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline);
            while let Some(request) = self.host.next_request() {
                self.respond(request);
            }
            let event = self.step();
            self.scenes();
            match event {
                EngineEvent::Idle => break,
                EngineEvent::Window {
                    context,
                    result: RuntimeExit::Finished(_),
                    ..
                } => {
                    self.engine.take_result(context);
                }
                EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
                other => panic!("clipboard callback failed: {other:?}"),
            }
        }
    }
}
#[test]
fn clipboard_formats_pixels_notifications_callbacks_unload_and_cancellation() {
    let mut h = Harness::new();
    assert_eq!(h.run(include_str!("fixtures/clipboard.tjs")), "passed");
    assert_eq!(h.memory.0.borrow().starts, 1);
    h.memory.notify();
    h.drain();
    assert_eq!(h.run("notifications == 1 && aliasNotifications == 0;"), "1");
    let late_changed = h.memory.0.borrow().changed.clone().unwrap();
    h.run("w.clipboardWatchEnabled=false; w.clipboardWatchEnabled=true; delete w.onDrawClipboard;");
    late_changed();
    h.drain();
    assert_eq!(h.run("notifications == 1 && aliasNotifications == 0;"), "1");
    h.memory.notify();
    h.drain();
    assert_eq!(h.run("notifications == 1 && aliasNotifications == 1;"), "1");
    h.run("w.clipboardWatchEnabled=false;");
    h.memory.notify();
    h.drain();
    assert_eq!(h.run("aliasNotifications;"), "1");
    h.memory.0.borrow_mut().reject = true;
    assert_eq!(
        h.run("fails(function(){Clipboard.asText='rejected';}) && Clipboard.asText=='text only';"),
        "1"
    );
    h.run("Clipboard.asTJS=expression;");
    let bytes = h.memory.0.borrow().tjs.clone().unwrap();
    let text = String::from_utf16(
        &bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(text.contains("real 0x1.0000000000001p0 /*"));
    assert!(text.contains("real -0.0 /* -0.0 */"));
    // Cancellation while the source Layer readback is pending never writes.
    let writes = h.memory.0.borrow().writes;
    let id = h.submit("Clipboard.setAsBitmap(art); throw 'cancelled call resumed';");
    let request = loop {
        if let Some(request) = h.host.next_request() {
            if matches!(request.command, Command::Graphics(Draw::ReadImage { .. })) {
                break request;
            }
            h.respond(request);
        }
        match h.step() {
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            other => panic!("readback not reached {other:?}"),
        }
    };
    h.engine.cancel(id);
    assert!(request.cancelled());
    request.respond(Err("late reply".into()));
    h.drain();
    assert_eq!(h.memory.0.borrow().writes, writes);
    assert_eq!(h.engine.pending_operations(), 0);
    assert_eq!(h.host.staging_budget().used(), 0);
    // Original callback wins over alias; exceptions use the engine handler.
    h.run("var watchErrors=0;System.exceptionHandler=function(e){global.watchErrors++;return true;};w.onDrawClipboard=function(){throw 'watch callback';};w.clipboardWatchEnabled=true;");
    h.memory.notify();
    h.drain();
    assert_eq!(h.run("watchErrors;"), "1");
    // Watcher backend failure clears the registration and follows same handler.
    h.memory.0.borrow_mut().failure = Some("injected watcher failure".into());
    h.memory.notify();
    h.drain();
    assert_eq!(h.run("!w.clipboardWatchEnabled && watchErrors==2;"), "1");
    h.memory.0.borrow_mut().failure = None;
    h.run("System.exceptionHandler=void;");
    // Retained exports own their capture; unload stops subscriptions, releases
    // class slots, and cannot overwrite script replacements.
    h.run("w.clipboardWatchEnabled=true;var savedMultiple=Clipboard.setMultipleData;Clipboard.getAsBitmap=function(){return 'replacement';};check(Plugins.unlink('clipboardEx.dll'),'loaded clipboard plugin unlinks');");
    assert!(h.memory.0.borrow().changed.is_none());
    assert_eq!(h.run("savedMultiple(%[text:'retained',tjs:expression]); Clipboard.getAsBitmap()=='replacement' && fails(function(){return cbfBitmap;}) && Clipboard.hasFormat(1) && !Clipboard.hasFormat(3);"),"1");
    h.run("Plugins.link('clipboardEx.dll');var unused=new Window;unused.clipboardWatchEnabled=true;unused=null;");
    h.engine.collect([]);
    h.drain();
    // Explicit invalidation is deterministic even while another window lives.
    h.run("w.clipboardWatchEnabled=true;invalidate w;savedMultiple=void;invalidate b;invalidate out;check(Plugins.unlink('clipboardEx.dll'),'reloaded clipboard plugin unlinks');");
    h.drain();
    assert!(h.memory.0.borrow().changed.is_none());
    assert_eq!(h.memory.0.borrow().starts, h.memory.0.borrow().stops);
    assert_eq!(h.host.staging_budget().used(), 0);
}
