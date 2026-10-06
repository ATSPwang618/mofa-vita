use super::*;

#[test]
fn unchanged_layer_properties_do_not_publish_but_explicit_paint_still_runs() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(
            &mut engine,
            &mut host,
            r#"
            var w=new Window(),r=new Layer(w,null),a=new Layer(w,r);
            a.visible=true;a.setSize(16,16);a.setPos(3,4);a.opacity=137;
            var paints=0;a.onPaint=function(){global.paints++;};
        "#,
            slice,
        );
        drain(&mut engine, &mut host, slice);
        host.service();
        let count = host.snapshots.len();
        run(
            &mut engine,
            &mut host,
            r#"
            for(var i=0;i<8;i++) {
                a.left=3;a.top=4;a.setPos(3,4);a.setSize(16,16);
                a.opacity=137;a.setImagePos(a.imageLeft,a.imageTop);
                a.neutralColor=a.neutralColor;
            }
        "#,
            slice,
        );
        drain(&mut engine, &mut host, slice);
        host.service();
        assert_eq!(host.snapshots.len(), count);
        run(&mut engine, &mut host, "a.left=5;", slice);
        drain(&mut engine, &mut host, slice);
        host.service();
        assert!(host.snapshots.len() > count);
        run(&mut engine, &mut host, "a.update();", slice);
        drain(&mut engine, &mut host, slice);
        assert_eq!(run(&mut engine, &mut host, "paints;", slice), "1");
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            var errors=0;
            try {r.left=1;} catch(e) {errors++;}
            try {r.opacity=137;} catch(e) {errors++;}
            try {a.setImagePos(1,0);} catch(e) {errors++;}
            errors;
        "#,
                slice
            ),
            "3"
        );
        engine.reset();
    }
}

fn drain(engine: &mut Engine<Manual>, host: &mut Desktop, slice: u32) {
    for _ in 0..10000 {
        match step(engine, host, slice) {
            EngineEvent::Window {
                context,
                result: RuntimeExit::Finished(_),
                ..
            } => {
                engine.take_result(context);
            }
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            EngineEvent::Idle => return,
            event => panic!("{event:?}"),
        }
    }
    panic!("input callbacks did not finish");
}
fn post(engine: &mut Engine<Manual>, host: &mut Desktop, input: Input, slice: u32) {
    let window = *host.windows.keys().next().unwrap();
    host.host.post(Event { window, input }).unwrap();
    drain(engine, host, slice);
}
const BASE: &str = r#"
var w=new Window(), root=new Layer(w,null), a=new Layer(w,root), b=new Layer(w,root);
root.setSize(100,100);a.setSize(20,20);b.setSize(20,20);
root.hitThreshold=a.hitThreshold=b.hitThreshold=0;
a.setPos(10,20);b.setPos(40,20);a.visible=b.visible=true;a.focusable=b.focusable=true;
var log='', moves=0, enters=0;
w.onHintChanged=function(text,x,y,show){System.wait(0);global.log+=(show?text:'hide')+':'+x+','+y+';';};
w.onMouseMove=function(x,y,s){global.moves++;};
a.onMouseEnter=function(){global.enters++;};
root.hint='parent';root.cursor=-21;
"#;

#[test]
fn hints_inherit_delay_cancel_and_recheck_stationary_pointer_without_window_motion() {
    for slice in [1, 10000] {
        let (mut engine, mut host, clock) = setup();
        run(&mut engine, &mut host, BASE, slice);
        post(
            &mut engine,
            &mut host,
            Input::MouseMove {
                x: 12,
                y: 23,
                shift: 0,
            },
            slice,
        );
        assert_eq!(run(&mut engine, &mut host, "log;", slice), "hide:12,23;");
        assert_eq!(host.styles.last().unwrap().cursor, -21);
        assert_eq!(engine.sleep_duration(), Some(Duration::from_millis(500)));
        clock.0.set(Duration::from_millis(300));
        post(
            &mut engine,
            &mut host,
            Input::MouseMove {
                x: 13,
                y: 23,
                shift: 0,
            },
            slice,
        );
        assert_eq!(engine.sleep_duration(), Some(Duration::from_millis(200)));
        clock.0.set(Duration::from_millis(500));
        drain(&mut engine, &mut host, slice);
        assert_eq!(
            run(&mut engine, &mut host, "log;", slice),
            "hide:12,23;parent:13,23;"
        );
        run(&mut engine, &mut host, "a.hint='own';", slice);
        drain(&mut engine, &mut host, slice);
        post(
            &mut engine,
            &mut host,
            Input::MouseDown {
                x: 13,
                y: 23,
                button: 0,
                shift: 8,
            },
            slice,
        );
        clock.0.set(Duration::from_millis(1000));
        drain(&mut engine, &mut host, slice);
        assert_eq!(
            run(&mut engine, &mut host, "log;", slice),
            "hide:12,23;parent:13,23;hide:13,23;hide:13,23;"
        );
        post(
            &mut engine,
            &mut host,
            Input::MouseUp {
                x: 13,
                y: 23,
                button: 0,
                shift: 0,
            },
            slice,
        );
        run(
            &mut engine,
            &mut host,
            "w.hintDelay=0;a.left=60;log='';",
            slice,
        );
        clock.0.set(Duration::from_millis(2000));
        drain(&mut engine, &mut host, slice);
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "log+'moves='+moves+';enters='+enters;",
                slice
            ),
            "hide:13,23;parent:13,23;moves=2;enters=1"
        );
        run(
            &mut engine,
            &mut host,
            "b.ignoreHintSensing=true;log='';",
            slice,
        );
        post(
            &mut engine,
            &mut host,
            Input::MouseMove {
                x: 43,
                y: 23,
                shift: 0,
            },
            slice,
        );
        assert_eq!(run(&mut engine, &mut host, "log;", slice), "");
        run(&mut engine, &mut host, "b.hint='new';", slice);
        drain(&mut engine, &mut host, slice);
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "!b.ignoreHintSensing && !b.showParentHint && log=='hide:43,23;new:43,23;';",
                slice
            ),
            "1"
        );
        run(
            &mut engine,
            &mut host,
            "w.hintDelay=-1;b.hint='disabled';",
            slice,
        );
        drain(&mut engine, &mut host, slice);
        clock.0.set(Duration::from_secs(5));
        drain(&mut engine, &mut host, slice);
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "log.indexOf('disabled')==-1;",
                slice
            ),
            "1"
        );
        run(
            &mut engine,
            &mut host,
            "delete w.onHintChanged;w.close();",
            slice,
        );
        drain(&mut engine, &mut host, slice);
        assert_eq!(engine.window_count(), 0);
        assert!(engine.sleep_duration().is_none());
    }
}

#[test]
fn cursor_coordinates_ime_focus_and_keyboard_posting_use_owned_host_operations() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(&mut engine, &mut host, BASE, slice);
        post(
            &mut engine,
            &mut host,
            Input::MouseMove {
                x: 12,
                y: 23,
                shift: 0,
            },
            slice,
        );
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "a.cursorX==2 && a.cursorY==3;",
                slice
            ),
            "1"
        );
        run(&mut engine, &mut host, "a.cursorX=7;", slice);
        assert!(host.cursor_moves.is_empty());
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "w.hideMouseCursor();a.cursorY=9;a.cursorX==7 && a.cursorY==9 && w.mouseCursorState==0;",
                slice
            ),
            "1"
        );
        assert_eq!(host.cursor_moves, [(17, 29)]);
        run(
            &mut engine,
            &mut host,
            "a.setCursorPos(-1,5);root.setAttentionPos(4,6);root.useAttention=true;a.imeMode=2;a.focus();",
            slice,
        );
        let style = *host.styles.last().unwrap();
        assert_eq!(style.ime, 2);
        let area = style.attention.unwrap();
        assert_eq!((area.left, area.top), (4, 6));
        run(
            &mut engine,
            &mut host,
            "a.setAttentionPos(3,4);a.useAttention=true;",
            slice,
        );
        let area = host.styles.last().unwrap().attention.unwrap();
        assert_eq!((area.left, area.top), (13, 24));
        let before = host.styles.len();
        run(&mut engine, &mut host, "a.attentionLeft=3;", slice);
        assert_eq!(host.styles.len(), before);
        run(&mut engine, &mut host, "b.focus();", slice);
        assert_eq!(host.styles.last().unwrap().ime, 0);
        run(
            &mut engine,
            &mut host,
            r#"
            var order='', keys='';
            class Params {
                property key { getter(){global.order+='k';System.wait(0);return 0x10041;} }
                property shift { getter(){global.order+='s';System.wait(0);return 4;} }
            }
            w.onKeyDown=function(k,s){global.keys+='d'+k+','+s+';';};
            w.onKeyUp=function(k,s){global.keys+='u'+k+','+s+';';};
            w.onKeyPress=function(k){global.keys+='p'+#k+';';};
            w.postInputEvent('onKeyDown',new Params());
            w.postInputEvent('onKeyUp',%[key:65,shift:0]);
            w.postInputEvent('onKeyPress',%[key:0xe9]);
            var caught=0;
            try{w.postInputEvent('onMouseMove',%[]);}catch(e){caught++;}
            w.postInputEvent('onKeyDown',%[key:1]);
            class MissingShift {var key=1;}
            try{w.postInputEvent('onKeyDown',new MissingShift());}catch(e){caught++;}
            try{w.postInputEvent('onKeyPress',null);}catch(e){caught++;}
        "#,
            slice,
        );
        drain(&mut engine, &mut host, slice);
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "order+'|'+caught+'|'+keys+'|'+System.getKeyState(65);",
                slice
            ),
            "ks|3|d65,4;u65,0;p65513;d1,0;|0"
        );
        post(
            &mut engine,
            &mut host,
            Input::KeyDown { key: 65, shift: 0 },
            slice,
        );
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "System.getKeyState(65,false) && !System.getKeyState(65,false) && System.getKeyState(65);",
                slice
            ),
            "1"
        );
        post(
            &mut engine,
            &mut host,
            Input::KeyDown {
                key: 65,
                shift: 128,
            },
            slice,
        );
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "!System.getKeyState(65,false);",
                slice
            ),
            "1"
        );
        post(&mut engine, &mut host, Input::Focus(false), slice);
        assert_eq!(
            run(&mut engine, &mut host, "!System.getKeyState(65);", slice),
            "1"
        );
        engine.reset();
    }
}

#[test]
fn custom_cursor_loads_through_vfs_caches_handles_and_releases_its_budget_on_reset() {
    let (mut engine, mut host, _) = setup();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("pointer.cur"),
        include_bytes!("../fixtures/cursors/pointer.cur"),
    )
    .unwrap();
    krkr_engine::storages::install(
        &mut engine.runtime_mut().heap,
        krkr_engine::assets::Vfs::new(directory.path(), Default::default()).unwrap(),
    )
    .unwrap();
    run(&mut engine, &mut host, BASE, 1);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "a.cursor='pointer.cur';var handle=a.cursor;b.cursor='POINTER.CUR';handle>=2 && b.cursor===handle;",
            1
        ),
        "1"
    );
    assert_eq!(host.cursors.len(), 1);
    {
        let image = host.cursors.values().next().unwrap().upgrade().unwrap();
        assert_eq!((image.width, image.height, image.hotspot), (32, 32, (2, 2)));
    }
    post(
        &mut engine,
        &mut host,
        Input::MouseMove {
            x: 12,
            y: 23,
            shift: 0,
        },
        1,
    );
    assert_eq!(host.styles.last().unwrap().cursor, 2);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "var caught=0;try{a.cursor='absent.cur';}catch(e){caught++;}try{a.cursor=999;}catch(e){caught++;}caught==2 && a.cursor==handle;",
            1
        ),
        "1"
    );
    engine.reset();
    host.service();
    assert!(host.cursors.values().all(|image| image.upgrade().is_none()));
}

#[test]
fn cancelling_an_applied_style_does_not_make_the_next_update_trust_stale_state() {
    let (mut engine, mut host, _) = setup();
    run(&mut engine, &mut host, "var w=new Window();", 1);
    let id = submit(&mut engine, "w.imeMode=imOpen;");
    let event = engine.poll(
        RunBudget::new(10000).unwrap(),
        NonZeroUsize::new(64).unwrap(),
    );
    assert!(matches!(event, EngineEvent::Waiting { .. }));
    let request = host.host.next_request().unwrap();
    let Command::InputStyle(style) = request.command else {
        panic!("expected IME update");
    };
    assert_eq!(style.ime, 2);
    let geometry = host.windows[&request.window].0;
    // The host applied the command before the script cancellation arrived.
    request.complete(Ok(geometry));
    engine.cancel(id);
    engine.collect([]);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "w.imeMode=imDisable;VK_LSHIFT==160 && VK_PAD1==448 && crHBeam==1 && mcsHidden==2;",
            1
        ),
        "1"
    );
    assert_eq!(host.styles.last().unwrap().ime, 0);
    run(
        &mut engine,
        &mut host,
        "var root=new Layer(w,null);root.hitThreshold=0;root.cursor=crHandPoint;",
        1,
    );
    let window = *host.windows.keys().next().unwrap();
    host.host
        .post(Event {
            window,
            input: Input::MouseMove {
                x: 1,
                y: 1,
                shift: 0,
            },
        })
        .unwrap();
    let context = loop {
        match step(&mut engine, &mut host, 10000) {
            EngineEvent::Waiting { context, .. } => break context,
            EngineEvent::Yielded => {}
            event => panic!("{event:?}"),
        }
    };
    let request = host.host.next_request().unwrap();
    assert!(matches!(request.command, Command::InputStyle(_)));
    engine.cancel(context);
    drop(request);
    // The hover identity is already committed, but its first host command was
    // cancelled. Even motion within that same layer must retry the style.
    post(
        &mut engine,
        &mut host,
        Input::MouseMove {
            x: 1,
            y: 1,
            shift: 0,
        },
        1,
    );
    assert_eq!(host.styles.last().unwrap().cursor, -21);
    engine.reset();
}
