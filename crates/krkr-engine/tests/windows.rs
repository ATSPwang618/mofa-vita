#[path = "support/application.rs"]
mod application;
#[path = "support/focus.rs"]
mod focus;
#[path = "support/menus.rs"]
mod menus;
#[path = "support/presentation.rs"]
mod presentation;
#[path = "support/scene_publication.rs"]
mod scene_publication;
#[path = "support/events.rs"]
#[allow(dead_code)]
mod support;
#[path = "support/transitions.rs"]
mod transitions;
#[path = "support/trees.rs"]
mod trees;
#[path = "support/viewport.rs"]
mod viewport;
#[path = "support/window_state.rs"]
mod window_state;
#[path = "support/window_update.rs"]
mod window_update;
use krkr_engine::{
    Engine, EngineEvent,
    protocol::window::{self, Client, Command, Event, Geometry, Host, Input, WindowId},
};
use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use support::Manual;
use tjs_core::RunBudget;
use tjs_runtime::{ContextId, Runtime, RuntimeExit, SchedulerLimits};

struct Desktop {
    host: Host,
    windows: HashMap<WindowId, (Geometry, Weak<AtomicBool>)>,
    styles: Vec<krkr_engine::protocol::input_style::Style>,
    cursor_moves: Vec<(i32, i32)>,
    borders: Vec<window::BorderStyle>,
    moves: usize,
    transforms: Vec<(
        krkr_protocol::transform::ImageOperation,
        krkr_protocol::transform::Sampling,
    )>,
    operations: Vec<krkr_protocol::graphics::BlendOptions>,
    cursors: HashMap<i32, Weak<krkr_engine::protocol::input_style::CursorImage>>,
    snapshots: Vec<(
        WindowId,
        krkr_engine::protocol::viewport::Viewport,
        usize,
        usize,
    )>,
}
impl Desktop {
    fn new() -> (Client, Self) {
        Self::with_limits(Default::default())
    }
    fn with_limits(limits: window::Limits) -> (Client, Self) {
        let (client, host) = window::channel(limits, Arc::new(|| {}));
        (
            client,
            Self {
                host,
                windows: HashMap::new(),
                styles: Vec::new(),
                cursor_moves: Vec::new(),
                borders: Vec::new(),
                moves: 0,
                transforms: Vec::new(),
                operations: Vec::new(),
                cursors: HashMap::new(),
                snapshots: Vec::new(),
            },
        )
    }
    fn service(&mut self) {
        self.windows
            .retain(|_, (_, alive)| alive.upgrade().is_some_and(|a| a.load(Ordering::Acquire)));
        while let Some(request) = self.host.next_request() {
            if request.cancelled() {
                continue;
            }
            match &request.command {
                Command::Graphics(command) => {
                    if let krkr_engine::protocol::graphics::Command::Operate { options, .. } =
                        command
                    {
                        self.operations.push(*options);
                        request.respond(Ok(window::Response::Done));
                        continue;
                    }
                    if let krkr_engine::protocol::graphics::Command::Transform {
                        operation,
                        sampling,
                        ..
                    } = command
                    {
                        self.transforms.push((*operation, *sampling));
                        request.respond(Ok(window::Response::Done));
                        continue;
                    }
                    assert!(matches!(
                        command,
                        krkr_engine::protocol::graphics::Command::Create { .. }
                            | krkr_engine::protocol::graphics::Command::Resize { .. }
                    ));
                    // This host acknowledges metadata allocation only. Pixel
                    // behavior is exercised against the real GPU backend.
                    request.respond(Ok(window::Response::Done));
                    continue;
                }
                Command::Create { alive, .. } => {
                    self.windows.insert(
                        request.window,
                        (
                            Geometry {
                                width: 656,
                                height: 519,
                                inner_width: 640,
                                inner_height: 480,
                                ..Default::default()
                            },
                            alive.clone(),
                        ),
                    );
                }
                Command::Size {
                    width,
                    height,
                    inner,
                } => {
                    let geometry = &mut self.windows.get_mut(&request.window).unwrap().0;
                    if *inner {
                        geometry.inner_width = *width;
                        geometry.inner_height = *height;
                        geometry.width = width + 16;
                        geometry.height = height + 39;
                    } else {
                        geometry.width = *width;
                        geometry.height = *height;
                        geometry.inner_width = width - 16;
                        geometry.inner_height = height - 39;
                    }
                }
                Command::InputStyle(style) => self.styles.push(*style),
                Command::BorderStyle(style) => self.borders.push(*style),
                Command::BeginMove => self.moves += 1,
                Command::CursorPosition(x, y) => self.cursor_moves.push((*x, *y)),
                Command::RegisterCursor { id, image } => {
                    self.cursors.insert(*id, Arc::downgrade(image));
                }
                Command::Position(x, y) => {
                    let geometry = &mut self.windows.get_mut(&request.window).unwrap().0;
                    geometry.left = *x;
                    geometry.top = *y;
                }
                _ => {}
            }
            let geometry = self.windows[&request.window].0;
            request.complete(Ok(geometry));
        }
        for (window, scene) in self.host.take_scenes(u64::MAX) {
            self.snapshots.push((
                window,
                scene.viewport,
                scene.nodes.capacity(),
                scene.transitions.capacity(),
            ));
        }
    }
}
fn setup() -> (Engine<Manual>, Desktop, Manual) {
    setup_with_exit_policy(false)
}
fn setup_with_exit_policy(exit_on_close: bool) -> (Engine<Manual>, Desktop, Manual) {
    let clock = Manual::default();
    let mut engine = Engine::new(
        Runtime::new(),
        clock.clone(),
        SchedulerLimits {
            max_contexts: 1,
            max_event_depth: NonZeroUsize::new(1).unwrap(),
        },
        Default::default(),
    )
    .unwrap();
    let (client, mut host) = Desktop::new();
    engine.attach_windows(client).unwrap();
    if !exit_on_close {
        // These component tests inspect state and retry after closing windows.
        run(
            &mut engine,
            &mut host,
            "System.exitOnWindowClose=false;",
            10000,
        );
    }
    (engine, host, clock)
}
fn submit(engine: &mut Engine<Manual>, script: &str) -> ContextId {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("window test", script)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"))
}
fn step(engine: &mut Engine<Manual>, host: &mut Desktop, slice: u32) -> EngineEvent {
    host.service();
    let event = engine.poll(
        RunBudget::new(slice).unwrap(),
        NonZeroUsize::new(64).unwrap(),
    );
    engine.collect([]);
    event
}
fn run(engine: &mut Engine<Manual>, host: &mut Desktop, script: &str, slice: u32) -> String {
    let id = submit(engine, script);
    for _ in 0..10000 {
        match step(engine, host, slice) {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } if context == id => {
                let result = engine.runtime().heap.display(value).unwrap();
                engine.take_result(context);
                return result;
            }
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            event => panic!("{event:?}"),
        }
    }
    panic!("script did not finish");
}

#[test]
fn constructor_properties_close_and_finalizers_suspend_in_the_original_context() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            var finalized=0;
            class W extends Window {
                var refuse=true;
                function W(){super.Window();caption='hello';setInnerSize(320,200);visible=true;}
                function onCloseQuery(b){super.onCloseQuery(!refuse);}
                function finalize(){System.wait(0);global.finalized++;}
            }
            var w=new W();
            if(w.caption!='hello' || w.innerWidth!=320 || w.innerHeight!=200 || w.width!=336 || !w.visible) throw 'state';
            w.setSize('416',339.9);w.left='-50';w.top=75;
            if(w.innerWidth!=400 || w.innerHeight!=300 || w.left!=-50 || w.top!=75 || Window.mainWindow!==w) throw 'geometry';
            w.close();if(!isvalid w) throw 'veto';
            w.refuse=false;w.close();
            if(isvalid w || finalized!=1 || Window.mainWindow!==null) throw 'close';
            42;
        "#,
                slice
            ),
            "42"
        );
        host.service();
        assert!(host.windows.is_empty());
        assert_eq!(engine.pending_operations(), 0);
    }
}

#[test]
fn associations_exist_before_the_native_window_constructor() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
                var order="";
                class Item {
                    var label;
                    function Item(value){label=value;}
                    function finalize(){global.order+=label;System.wait(0);}
                }
                var removed=new Item("x");
                class Base extends Window {function Base(){super.Window();}}
                class W extends Base {
                    function W(){
                        var item=new Item("a");
                        add(item);add(item);add(removed);remove(removed);remove(removed);
                        add(new Item("b"));System.wait(0);
                        super.Base();add(new Item("c"));
                    }
                }
                var w=new W();invalidate w;
                if(order!="abc" || !isvalid removed) throw "constructor lost associations";
                class Early extends Window {
                    function Early(){add(new Item("d"));System.wait(0);}
                }
                var early=new Early();invalidate early;
                if(order!="abcd") throw "unconstructed window skipped cleanup";
                var failed=void;
                class Failed extends Window {
                    function Failed(){global.failed=this;add(new Item("e"));throw "constructor";}
                }
                try{new Failed();}catch(e){}
                invalidate failed;
                if(order!="abcde") throw "failed constructor lost associations";
                var errors=0, other=new Early();
                try{other.add(1);}catch(e){errors++;}
                try{Window.add(removed);}catch(e){errors++;}
                try{(Window.add incontextof %[])(removed);}catch(e){errors++;}
                if(errors!=3) throw "invalid association receiver or argument accepted";
                invalidate other;
                42;
                "#,
                slice,
            ),
            "42"
        );
        host.service();
        assert!(host.windows.is_empty());
        assert_eq!(engine.pending_operations(), 0);
    }
}

#[test]
fn associated_cleanup_is_intrinsic_ordered_and_catches_only_child_failures() {
    struct Log;
    impl krkr_engine::debug::LogOutput for Log {
        fn timestamp(&mut self) -> String {
            "12:34:56".into()
        }
        fn console(&mut self, _: &[u16]) {}
    }
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        let global = engine.global();
        let heap = &mut engine.runtime_mut().heap;
        let debug = krkr_engine::debug::install(heap, Log).unwrap();
        let name = heap.intern(&"Debug".encode_utf16().collect::<Vec<_>>());
        heap.set_member(global, name, tjs_core::Value::Obj(debug.into()))
            .unwrap();
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            var order="",outer=false;
            var logged="",logCount=0;
            Debug.addLoggingHandler(function(line){global.logged+=line;global.logCount++;System.wait(0);});
            class W extends Window {
                function W(){super.Window();}
                function finalize(){global.order+="w";System.wait(0);}
            }
            class Item {
                var label;
                function Item(s){label=s;}
                function finalize(){
                    global.order+=label;
                    global.w.remove(global.c);global.w.add(global.late);
                    if(Window.mainWindow!==null) throw "window still registered";
                    System.wait(0);
                    if(label=="a") try{throw "inner";}catch(e){global.order+="!";}
                    if(label=="b") throw "child failure";
                    if(label=="d") missing.member();
                }
            }
            var w=new W(),a=new Item("a"),b=new Item("b"),c=new Item("c"),d=new Item("d"),e=new Item("e");
            var removed=new Item("removed"),late=new Item("late");
            w.add(a);w.add(a);w.add(b);w.add(c);w.add(d);w.add(e);
            w.add(removed);w.remove(removed);w.remove(removed);w.add(w);w.add(null);
            try{w.close();}catch(e){outer=true;}
            if(outer || order!="wa!bcde") throw "cleanup order or catch boundary";
            if(isvalid w || isvalid a || isvalid c || isvalid e) throw "surviving association";
            if(!isvalid b || !isvalid d || !isvalid removed || !isvalid late) throw "failed or removed association";
            if(logCount!=3 || logged.indexOf("child failure")<0) throw "cleanup logging";
            class Plain {}
            var z=new Window(),bad=new Plain(),good=new Plain();
            bad.finalize=function(){throw "bad child";};
            z.add(bad);z.add(good);
            Debug.addLoggingHandler(function(line){System.wait(0);throw 99;});
            var logError=0;
            try{z.close();}catch(e){logError=e;}
            if(logError!=99 || !isvalid z || !isvalid bad || !isvalid good) throw "logging exception boundary";
            bad.finalize=function(){};z.close();
            if(isvalid z || isvalid bad || isvalid good) throw "retry after logging error";
            42;
        "#,
                slice
            ),
            "42"
        );
        host.service();
        assert!(host.windows.is_empty());
        assert_eq!(engine.pending_operations(), 0);
    }
}

#[test]
fn cancelling_associated_cleanup_unlocks_the_window_and_preserves_remaining_roots() {
    let (mut engine, mut host, _) = setup();
    run(
        &mut engine,
        &mut host,
        r#"
        var w=new Window(),order="";
        class Item {
            var label;
            function Item(s){label=s;}
            function finalize(){
                global.order+=label;
                if(label=="b") System.wait(1000);
            }
        }
        var first=new Item("a"),waiting=new Item("b");
        w.add(first);w.add(waiting);w.add(new Item("c"));
        w.finalize=function(){throw "before native cleanup";};
        try{w.close();}catch(e){}
        if(order!="" || !isvalid first || Window.mainWindow!==w) throw "premature cleanup";
        delete w.finalize;
    "#,
        1,
    );
    let context = submit(&mut engine, "w.close();");
    loop {
        match step(&mut engine, &mut host, 1) {
            EngineEvent::Yielded => {}
            EngineEvent::Waiting { .. } => break,
            event => panic!("{event:?}"),
        }
    }
    engine.cancel(context);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        if(order!="ab" || isvalid first || !isvalid waiting || !isvalid w || Window.mainWindow!==null)
            throw "cancel state";
        w.remove(waiting);w.add(new Item("d"));w.close();
        if(order!="abcd" || isvalid w || !isvalid waiting) throw "retry or association roots";
        42;
    "#,
            1
        ),
        "42"
    );
    host.service();
    assert!(host.windows.is_empty());
    assert_eq!(engine.pending_operations(), 0);
}

#[test]
fn input_obeys_event_gate_and_user_close_hides_secondary_but_invalidates_main() {
    let (mut engine, mut host, _) = setup();
    run(
        &mut engine,
        &mut host,
        r#"
        var seen='', main=new Window(), secondary=new Window();
        secondary.visible=true;
        secondary.action=function(e){global.seen+=e.type+':'+e.key+';';};
        System.eventDisabled=true;
    "#,
        10000,
    );
    let mut ids: Vec<_> = host.windows.keys().copied().collect();
    ids.sort();
    host.host
        .post(Event {
            window: ids[1],
            input: Input::KeyPress(0x65e5),
        })
        .unwrap();
    host.host
        .post(Event {
            window: ids[1],
            input: Input::Close,
        })
        .unwrap();
    assert!(matches!(
        step(&mut engine, &mut host, 10000),
        EngineEvent::Idle
    ));
    assert_eq!(run(&mut engine, &mut host, "seen;", 10000), "");
    run(&mut engine, &mut host, "System.eventDisabled=false;", 10000);
    for _ in 0..10000 {
        match step(&mut engine, &mut host, 1) {
            EngineEvent::Window {
                context,
                result: RuntimeExit::Finished(_),
                ..
            } => {
                engine.take_result(context);
            }
            EngineEvent::Idle => break,
            EngineEvent::Waiting { .. } | EngineEvent::Yielded => {}
            event => panic!("{event:?}"),
        }
    }
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "seen=='onKeyPress:日;' && isvalid secondary && !secondary.visible;",
            10000
        ),
        "1"
    );
    host.host
        .post(Event {
            window: ids[0],
            input: Input::Close,
        })
        .unwrap();
    for _ in 0..10000 {
        match step(&mut engine, &mut host, 1) {
            EngineEvent::Window {
                context,
                result: RuntimeExit::Finished(_),
                ..
            } => {
                engine.take_result(context);
                break;
            }
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            event => panic!("{event:?}"),
        }
    }
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "!isvalid main && Window.mainWindow===null && isvalid secondary;",
            10000
        ),
        "1"
    );
    run(&mut engine, &mut host, "secondary.close();", 10000);
    host.service();
    assert!(host.windows.is_empty());
}

#[test]
fn decorative_video_layer_does_not_intercept_primary_layer_clicks() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(
            &mut engine,
            &mut host,
            r#"
            var w=new Window(),root=new Layer(w,null),clicks=0;
            root.setSize(100,100);root.hitThreshold=0;
            root.onClick=function(x,y){global.clicks++;};
            var movie=new Layer(w,root);
            movie.setSize(100,100);movie.type=ltOpaque;movie.visible=true;
            movie.enabled=false;movie.hitThreshold=256;
        "#,
            slice,
        );
        let window = *host.windows.keys().next().unwrap();
        for input in [
            Input::MouseDown {
                x: 30,
                y: 30,
                button: 0,
                shift: 0,
            },
            Input::Click { x: 30, y: 30 },
            Input::MouseUp {
                x: 30,
                y: 30,
                button: 0,
                shift: 0,
            },
        ] {
            host.host.post(Event { window, input }).unwrap();
            let mut completed = false;
            for _ in 0..10000 {
                match step(&mut engine, &mut host, slice) {
                    EngineEvent::Window {
                        context,
                        result: RuntimeExit::Finished(_),
                        ..
                    } => {
                        engine.take_result(context);
                        completed = true;
                    }
                    EngineEvent::Idle if completed => break,
                    EngineEvent::Waiting { .. } | EngineEvent::Yielded => {}
                    event => panic!("{event:?}"),
                }
            }
            assert!(completed);
        }
        assert_eq!(run(&mut engine, &mut host, "clicks;", slice), "1");
        assert_eq!(engine.pending_operations(), 0);
    }
}

#[test]
fn mouse_dispatch_rechecks_the_tree_and_preserves_capture_coordinates_and_window_results() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(
            &mut engine,
            &mut host,
            r#"
            var log="",release=false;
            var w=new Window(),root=new Layer(w,null);root.setSize(100,100);
            w.onMouseMove=function(x,y,s){global.log+="Wmove;";return 77;};
            w.onMouseDown=function(x,y,b,s){global.log+="Wdown;";};
            w.onMouseUp=function(x,y,b,s){global.log+="Wup;";};
            w.onClick=function(x,y){global.log+="Wclick;";};
            w.onDoubleClick=function(x,y){global.log+="Wdouble;";};
            class Button extends Layer {
                function Button(n,x){super.Layer(global.w,global.root);name=n;setSize(20,20);left=x;hasImage=false;hitThreshold=0;visible=true;}
                function emit(s){global.log+=name+s+";";System.wait(0);}
                function onMouseEnter(){emit("enter");}
                function onMouseLeave(){emit("leave");}
                function onMouseMove(x,y,s){emit("move:"+x+","+y);}
                function onMouseDown(x,y,b,s){emit("down:"+x+","+y);if(global.release)releaseCapture();}
                function onMouseUp(x,y,b,s){emit("up:"+x+","+y);}
                function onClick(x,y){emit("click");}
                function onDoubleClick(x,y){emit("double");}
            }
            var back=new Button("B",0),front=new Button("F",10);
        "#,
            slice,
        );
        let window = *host.windows.keys().next().unwrap();
        for input in [
            Input::MouseMove {
                x: 12,
                y: 3,
                shift: 0,
            },
            Input::MouseDown {
                x: 12,
                y: 3,
                button: 0,
                shift: 8,
            },
            Input::MouseMove {
                x: 2,
                y: 3,
                shift: 8,
            },
            Input::Click { x: 2, y: 3 },
            Input::MouseUp {
                x: 2,
                y: 3,
                button: 0,
                shift: 0,
            },
            Input::DoubleClick { x: 2, y: 3 },
        ] {
            host.host.post(Event { window, input }).unwrap();
            let mut first = true;
            loop {
                match step(&mut engine, &mut host, slice) {
                    EngineEvent::Window {
                        context,
                        result: RuntimeExit::Finished(value),
                        ..
                    } => {
                        if first && matches!(input, Input::MouseMove { .. }) {
                            assert_eq!(engine.runtime().heap.display(value).unwrap(), "77");
                        }
                        engine.take_result(context);
                        first = false;
                    }
                    EngineEvent::Idle if !first => break,
                    EngineEvent::Waiting { .. } | EngineEvent::Yielded => {}
                    event => panic!("{event:?}"),
                }
            }
        }
        assert_eq!(
            run(&mut engine, &mut host, "log;", slice),
            "Wmove;Fenter;Fmove:2,3;Wdown;Fdown:2,3;Wmove;Fmove:-8,3;Wclick;Wup;Fup:-8,3;Fleave;Benter;Wdouble;Bdouble;"
        );
        run(
            &mut engine,
            &mut host,
            r#"
            log="";release=true;
            back.onMouseLeave=function(){invalidate global.front;};
        "#,
            slice,
        );
        for input in [
            Input::MouseDown {
                x: 2,
                y: 3,
                button: 0,
                shift: 8,
            },
            Input::Click { x: 2, y: 3 },
            Input::MouseMove {
                x: 25,
                y: 3,
                shift: 0,
            },
        ] {
            host.host.post(Event { window, input }).unwrap();
            let mut first = true;
            loop {
                match step(&mut engine, &mut host, slice) {
                    EngineEvent::Window {
                        context,
                        result: RuntimeExit::Finished(_),
                        ..
                    } => {
                        engine.take_result(context);
                        first = false;
                    }
                    EngineEvent::Idle if !first => break,
                    EngineEvent::Waiting { .. } | EngineEvent::Yielded => {}
                    event => panic!("{event:?}"),
                }
            }
        }
        assert_eq!(
            run(&mut engine, &mut host, "log;", slice),
            "Wdown;Bdown:2,3;Wclick;Wmove;"
        );
        assert_eq!(run(&mut engine, &mut host, "!isvalid front;", slice), "1");
        engine.reset();
    }
}

#[test]
fn cancelled_creation_and_late_completion_cannot_keep_or_replace_a_window() {
    let (mut engine, mut host, _) = setup();
    let id = submit(&mut engine, "var abandoned=new Window();");
    let event = engine.poll(
        RunBudget::new(10000).unwrap(),
        NonZeroUsize::new(64).unwrap(),
    );
    assert!(matches!(event, EngineEvent::Waiting { .. }));
    let request = host.host.next_request().unwrap();
    engine.cancel(id);
    engine.collect([]);
    assert!(request.cancelled());
    if let Command::Create { alive, .. } = &request.command {
        assert!(alive.upgrade().is_none());
    }
    request.complete(Ok(Geometry::default()));
    assert_eq!(engine.window_count(), 0);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "var kept=new Window();kept.caption='kept';kept.caption;",
            10000
        ),
        "kept"
    );
    engine.reset();
    host.service();
    assert!(host.windows.is_empty());
    assert_eq!(engine.pending_operations(), 0);
}

#[test]
fn close_veto_guard_is_released_on_exception_and_cancellation() {
    let (mut engine, mut host, clock) = setup();
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        var w=new Window();w.onCloseQuery=function(b){throw 'veto error';};
        try{w.close();}catch(e){}
        w.onCloseQuery=function(b){System.wait(10);};
        isvalid w;
    "#,
            10000
        ),
        "1"
    );
    let id = submit(&mut engine, "w.close();");
    assert!(matches!(
        step(&mut engine, &mut host, 10000),
        EngineEvent::Waiting { .. }
    ));
    engine.cancel(id);
    clock.0.set(Duration::from_millis(10));
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "w.onCloseQuery=function(b){};w.close();!isvalid w;",
            10000
        ),
        "1"
    );
}

#[test]
fn desktop_focus_and_window_errors_use_system_handlers_with_one_context_slot() {
    let (mut engine, mut host, _) = setup();
    run(
        &mut engine,
        &mut host,
        r#"
        var transitions='', errors=0, w=new Window();
        System.onDeactivate=function(){global.transitions+='D';};
        System.onActivate=function(){global.transitions+='A';};
        System.exceptionHandler=function(e){System.wait(0);global.errors++;return true;};
        w.onKeyDown=function(key,shift){throw 'key error';};
    "#,
        10000,
    );
    let id = *host.windows.keys().next().unwrap();
    for input in [
        Input::Focus(false),
        Input::Focus(true),
        Input::KeyDown { key: 65, shift: 0 },
    ] {
        host.host.post(Event { window: id, input }).unwrap();
    }
    for _ in 0..10000 {
        let event = step(&mut engine, &mut host, 1);
        match event {
            EngineEvent::Window {
                context,
                result: RuntimeExit::Finished(_),
                ..
            }
            | EngineEvent::System {
                context,
                result: RuntimeExit::Finished(_),
                ..
            } => {
                engine.take_result(context);
            }
            EngineEvent::Idle if engine.sleep_duration().is_none() => break,
            EngineEvent::Idle | EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            event => panic!("{event:?}"),
        }
    }
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "transitions=='DA' && errors==1;",
            10000
        ),
        "1"
    );
}

#[test]
fn layer_cached_is_an_inherited_per_instance_property() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            var w=new Window(), root=new Layer(w,null);
            class Animated extends Layer {
                function Animated(w,p){
                    super.Layer(w,p);
                    if(cached!==false) throw "initial cache state";
                    cached=true;
                }
            }
            var a=new Animated(w,root), b=new Layer(w,root);
            if(!a.cached || b.cached || root.cached) throw "cache state leaked";
            a.cached=true;a.cached=0;
            if(a.cached) throw "cache disable";
            a.cached="1";a.cached=0.5;
            if(!a.cached) throw "boolean conversion";
            a.imageModified=false;a.cached=false;a.cached=true;
            if(a.imageModified) throw "cache changed image content";
            invalidate w;
            42;
        "#,
                slice
            ),
            "42"
        );
    }
}

#[test]
fn layer_geometry_tree_and_cached_children_survive_gc_at_every_slice() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            var w=new Window(), root=new Layer(w,null), a=new Layer(w,root), b=new Layer(w,root);
            if (w.primaryLayer !== root || !root.visible || !root.isPrimary || a.visible) throw 'defaults';
            var cache=root.children;
            if (cache !== root.children || cache.count != 2) throw 'children cache';
            b.parent=a;
            if (root.children !== cache || cache.count != 1 || a.children[0] !== b) throw 'reparent';
            a.setImageSize(80,40); a.setSize(60,30); a.setImagePos(-20,-10);
            if (a.width != 60 || a.imageWidth != 80 || a.imageLeft != -20) throw 'geometry';
            a.width=75;
            if (a.imageLeft != -5) throw 'image coverage';
            a.imageWidth=50;
            if (a.width != 50 || a.imageLeft != 0 || a.clipWidth != 50) throw 'shrink';
            a.setClip(100,2,10,20);
            if (a.clipLeft != 100 || a.clipWidth != 0) throw 'empty clip origin';
            var caught=0;
            try { root.visible=false; } catch(e) { caught++; }
            try { root.opacity=100; } catch(e) { caught++; }
            try { a.parent=b; } catch(e) { caught++; }
            if (caught != 3) throw 'tree validation';
            invalidate a;
            if (!isvalid b || b.parent !== null) throw 'children must detach on invalidation';
            invalidate b; invalidate root;
            var released=false;
            try { w.primaryLayer; } catch(e) { released=true; }
            if (!released) throw 'primary release';
            w.close();
            caught;
        "#,
                slice
            ),
            "3"
        );
        assert!(engine.window_host_error().is_none());
        assert_eq!(engine.pending_operations(), 0);
    }
}

#[test]
fn subclass_can_name_a_layer_before_the_native_constructor_waits() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            class Button extends Layer {
                function Button(w, p, text) {
                    if(name!='') throw 'initial name';
                    name=text;
                    if(name!=text) throw 'pending name';
                    super.Layer(w,p);
                    if(name!=text) throw 'lost name';
                }
            }
            var w=new Window(), root=new Layer(w,null), a=new Button(w,root,'选择');
            a.name='新的名称';
            var result=a.name;
            invalidate w; result;
        "#,
                slice
            ),
            "新的名称"
        );
    }
}

#[test]
fn legacy_blends_keep_source_alpha_encoding_opacity_and_sampling() {
    use krkr_protocol::{
        graphics::Blend,
        transform::{Filter, ImageOperation},
    };
    let (mut engine, mut host, _) = setup();
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        var w=new Window(), root=new Layer(w,null), a=new Layer(w,root), b=new Layer(w,root);
        b.type=ltAdditive;
        (Layer.pileRect incontextof a)(0,0,b,0,0,8,8,73);
        a.blendRect(0,0,b,0,0,8,8);
        a.stretchBlend(0,0,8,8,b,0,0,8,8,83,stFastLinear);
        a.affineBlend(b,0,0,8,8,false,0,0,8,0,0,8,,stFastLinear);
        b.type=ltAddAlpha;
        a.face=dfOpaque;
        a.pileRect(0,0,b,0,0,8,8,113);
        a.stretchPile(0,0,8,8,b,0,0,8,8,93);
        a.affinePile(b,0,0,8,8,false,0,0,8,0,0,8,103);
        invalidate w; 42;
    "#,
            1
        ),
        "42"
    );
    assert_eq!(
        host.operations
            .iter()
            .map(|v| (v.mode, v.opacity))
            .collect::<Vec<_>>(),
        [
            (Blend::Alpha, 73),
            (Blend::Opaque, 255),
            (Blend::AddAlpha, 113)
        ]
    );
    assert_eq!(host.transforms.len(), 4);
    for ((operation, sampling), (mode, opacity, filter)) in host.transforms.iter().zip([
        (Blend::Opaque, 83, Filter::FastLinear),
        (Blend::Opaque, 255, Filter::FastLinear),
        (Blend::AddAlpha, 93, Filter::Nearest),
        (Blend::AddAlpha, 103, Filter::Nearest),
    ]) {
        let ImageOperation::Blend(options) = operation else {
            panic!("blend expected")
        };
        assert_eq!(
            (options.mode, options.opacity, sampling.filter),
            (mode, opacity, filter)
        );
    }
}

#[test]
fn affine_pile_uses_alpha_mode_and_legacy_optional_arguments() {
    use krkr_protocol::{
        graphics::Blend,
        transform::{Filter, ImageOperation},
    };
    let (mut engine, mut host, _) = setup();
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        var w=new Window(), root=new Layer(w,null), a=new Layer(w,root), b=new Layer(w,root);
        b.type=ltAdditive;
        a.affinePile(b,0,0,8,8,false,0,0,8,0,0,8);
        a.holdAlpha=true;
        a.affinePile(b,0,0,8,8,true,1,0,0,1,0,0,128,stFastLinear);
        a.affinePile(b,0,0,8,8,false,0,0,8,0,0,8,,stFastLinear);
        a.face=dfMask;
        var caught=false;
        try { a.affinePile(b,0,0,8,8,false,0,0,8,0,0,8); } catch(e) { caught=true; }
        if(!caught) throw 'unsupported draw face';
        invalidate w; 42;
    "#,
            1
        ),
        "42"
    );
    assert_eq!(host.transforms.len(), 3);
    for ((operation, sampling), (opacity, hold, filter)) in host.transforms.iter().zip([
        (255, false, Filter::Nearest),
        (128, true, Filter::FastLinear),
        (255, true, Filter::FastLinear),
    ]) {
        let ImageOperation::Blend(options) = operation else {
            panic!("expected blend")
        };
        assert_eq!(options.mode, Blend::Alpha);
        assert_eq!(options.opacity, opacity);
        assert_eq!(options.hold_alpha, hold);
        assert_eq!(sampling.filter, filter);
    }
}

#[test]
fn signed_layer_extents_survive_animation_and_image_resize() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            var w=new Window(), root=new Layer(w,null), a=new Layer(w,root);
            a.setImageSize(80,40); a.setSize(60,30); a.setImagePos(-20,-10);
            a.width=-8; a.height=-16;
            if(a.width!=-8 || a.height!=-16) throw 'signed getters';
            if(a.imageWidth!=80 || a.imageHeight!=40) throw 'negative extent resized image';
            a.setImagePos(-88,-56);
            a.imageWidth=50;
            if(a.width!=-8 || a.height!=-16 || a.imageLeft!=-58) throw 'signed image coverage';
            a.width=20;
            if(a.height!=-16 || a.imageLeft!=-30) throw 'other axis lost';
            a.setSize(-2147483648,-1);
            if(a.width!=-2147483648 || a.imageWidth!=50) throw 'minimum extent';
            var caught=0;
            try { a.setPos(11,22,-3,4); } catch(e) { caught++; }
            try { a.setImageSize(-1,4); } catch(e) { caught++; }
            if(caught!=2 || a.left!=0 || a.top!=0) throw 'bounds validation';
            a.setSize(60,30);
            if(a.width!=60 || a.height!=30 || a.imageWidth!=60 || a.imageLeft!=0) throw 'restore';
            invalidate w;
            42;
        "#,
                slice
            ),
            "42"
        );
        assert!(engine.window_host_error().is_none());
    }
}

#[test]
fn cancelling_layer_creation_releases_the_image_lease_and_primary_slot() {
    let (mut engine, mut host, _) = setup();
    run(&mut engine, &mut host, "var w=new Window();", 10000);
    let context = submit(&mut engine, "var aborted=new Layer(w,null);");
    assert!(matches!(
        step(&mut engine, &mut host, 10000),
        EngineEvent::Waiting { .. }
    ));
    let request = host.host.next_request().unwrap();
    engine.cancel(context);
    engine.collect([]);
    assert!(request.cancelled());
    if let Command::Graphics(krkr_engine::protocol::graphics::Command::Create {
        lifetime, ..
    }) = &request.command
    {
        assert!(lifetime.upgrade().is_none());
    } else {
        panic!("expected image creation");
    }
    request.respond(Ok(window::Response::Done));
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "var root=new Layer(w,null);w.primaryLayer===root;",
            10000
        ),
        "1"
    );
    engine.reset();
    host.service();
    assert!(host.windows.is_empty());
    assert_eq!(engine.pending_operations(), 0);
}
