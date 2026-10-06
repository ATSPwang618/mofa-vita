use super::*;

const BASE: &str = r#"
var w=new Window(), root=new Layer(w,null);
var a=new Layer(w,root), b=new Layer(w,root), c=new Layer(w,root);
a.name='a';b.name='b';c.name='c';root.name='r';
a.visible=b.visible=c.visible=true;
a.focusable=b.focusable=c.focusable=true;
var log='', disabled=0;
w.action=function(e){
    if(e.type=='onNodeDisabled') global.disabled++;
    if(e.type=='onBeforeFocus') {System.wait(0);e.layer=null;}
    if(e.type=='onBlur'||e.type=='onFocus') global.log+=e.target.name+e.type+';';
    return 71;
};
"#;

#[test]
fn enabled_notifications_skip_unchanged_siblings_and_visit_callback_added_children() {
    let (mut engine, mut host, _) = setup();
    run(
        &mut engine,
        &mut host,
        r#"
        var w=new Window(), root=new Layer(w,null), siblings=[];
        for(var i=0;i<512;i++) siblings.add(new Layer(w,root));
        var child=new Layer(w,root), pane=new Layer(w,root), log='';
        pane.onNodeDisabled=function(){
            global.log+='pane;';
            System.wait(0);
            global.child.parent=this;
        };
        child.onNodeDisabled=function(){global.log+='child;';};
        "#,
        10000,
    );
    let id = submit(&mut engine, "pane.enabled=false;log;");
    for polls in 1..200 {
        match step(&mut engine, &mut host, 1) {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } if context == id => {
                assert_eq!(engine.runtime().heap.display(value).unwrap(), "pane;child;");
                engine.take_result(context);
                eprintln!("enabled notification over 512 unchanged siblings: {polls} polls");
                return;
            }
            EngineEvent::Waiting { .. } | EngineEvent::Yielded => {}
            event => panic!("{event:?}"),
        }
    }
    panic!("unchanged siblings exhausted the script work slices");
}

#[test]
fn temporary_focus_targets_survive_gc_and_closed_windows_cannot_regain_focus() {
    let (mut engine, mut host, _) = setup();
    run(&mut engine, &mut host, BASE, 1);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        var created=0;
        c.onBeforeFocus=function(layer,blurred,direction){
            var temporary=new global.Layer(global.w,global.root);
            temporary.visible=true;temporary.focusable=true;temporary.name='temporary';
            (global.Layer.onBeforeFocus incontextof this)(temporary,blurred,direction);
            global.created++;
        };
        c.focus();System.wait(0);
        if(created!=1 || c.focused) throw 'temporary redirect';
        // The candidate and subsequent focus are the only strong references.
        c.onBeforeFocus=Layer.onBeforeFocus incontextof c;
        root.focusPrev();if(!c.focused) throw 'focused target collected';
        a.onBeforeFocus=function(layer,blurred,direction){global.w.close();};
        if(a.focus()) throw 'closed window accepted focus';
        'ok';
    "#,
            1
        ),
        "ok"
    );
    assert_eq!(engine.window_count(), 0);
    engine.reset();
}

#[test]
fn focus_selection_state_changes_and_modal_notifications_share_script_semantics() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(&mut engine, &mut host, BASE, slice);
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            if(root.focusable || !root.joinFocusChain || a.focused || !a.nodeFocusable) throw 'defaults';
            if(!a.focus() || !a.focused || a.focus()) throw 'focus result';
            b.joinFocusChain=false;
            if(a.nextFocusable!==c || a.prevFocusable!==c || !a.focused) throw 'focus getters';
            if(root.focusNext()!==c || !c.focused) throw 'skip non-chain';
            if(root.focusNext()!==a || root.focusPrev()!==c) throw 'wrap';
            if(!b.focus() || !b.focused) throw 'explicit non-chain focus';
            c.onBeforeFocus=function(layer,blurred,direction){
                System.wait(0);(global.Layer.onBeforeFocus incontextof this)(a,blurred,direction);
            };
            if(!c.focus(false) || !a.focused || c.focused) throw 'redirect';
            a.onSearchNextFocusable=function(layer){(global.Layer.onSearchNextFocusable incontextof this)(b);};
            if(a.focusNext()!==b || !b.focused) throw 'search override';
            a.onSearchNextFocusable=Layer.onSearchNextFocusable incontextof a;c.onBeforeFocus=Layer.onBeforeFocus incontextof c;
            b.joinFocusChain=true;
            a.focus();a.visible=false;
            if(a.focused || !b.focused || !a.nodeEnabled || a.nodeFocusable) throw 'hide focus';
            a.visible=true;b.focusable=false;
            if(!c.focused) throw 'lost focusability';
            b.focusable=true;
            a.focus();log='';disabled=0;
            var token=%[message:'original'];
            a.onBlur=function(next){global.c.enabled=false;System.wait(0);throw global.token;};
            var caught=false;
            try{a.enabled=false;}catch(e){caught=e===token;}
            if(!caught || a.enabled || !b.focused || disabled!=2) throw 'finally and identity';
            c.enabled=true;
            a.onBlur=Layer.onBlur incontextof a;a.enabled=true;
            if(!a.focus()) throw 'lock released on error';
            var nested=false;
            b.onFocus=function(previous,direction){try{a.focus();}catch(e){global.nested=true;}};
            b.focus();if(!nested || !b.focused) throw 'focus lock';
            b.onFocus=Layer.onFocus incontextof b;
            var pane=new Layer(w,root), child=new Layer(w,pane);
            pane.name='p';child.name='q';pane.visible=child.visible=true;child.focusable=true;
            pane.setMode();
            if(!child.focused || a.nodeEnabled || !child.nodeEnabled || root.nodeEnabled) throw 'modal scope';
            if(a.focus()) throw 'modal focus escape';
            var rejected=false;try{child.setMode();}catch(e){rejected=true;}
            if(!rejected) throw 'nested modal';
            pane.visible=false;
            if(!a.nodeEnabled || child.focused || a.focused) throw 'hide modal subtree';
            pane.visible=true;child.focus();child.parent=root;
            if(child.focused || !a.focused || child.parent!==root) throw 'part focus';
            a.focus();invalidate a;
            if(isvalid a || !b.focused) throw 'invalidate focus';
            'ok';
        "#,
                slice
            ),
            "ok"
        );
    }
}

#[test]
fn window_keys_navigate_bubble_and_deliver_wheel_to_focus() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(&mut engine, &mut host, BASE, slice);
        run(
            &mut engine,
            &mut host,
            r#"
            var keys='', stop=false;
            w.onKeyDown=function(key,shift){return 77;};
            w.action=function(e){
                if(e.target!==global.w && (e.type=='onKeyDown' || e.type=='onKeyUp' || e.type=='onKeyPress')) {
                    global.keys+=e.target.name+':'+e.type+':'+e.key+';';System.wait(0);
                    e.process=false;
                }
                if(e.target!==global.w && e.type=='onMouseWheel') global.keys+=e.target.name+':wheel:'+e.shift+','+e.delta+','+e.x+','+e.y+';';
            };
            b.onKeyDown=function(key,shift,process){(global.Layer.onKeyDown incontextof this)(key,shift,!global.stop);};
            b.left=10;b.top=20;
        "#,
            slice,
        );
        let window = *host.windows.keys().next().unwrap();
        let post = |engine: &mut Engine<Manual>, host: &mut Desktop, input| {
            host.host.post(Event { window, input }).unwrap();
            for _ in 0..10000 {
                match step(engine, host, slice) {
                    EngineEvent::Window {
                        context,
                        result: RuntimeExit::Finished(value),
                        ..
                    } => {
                        if matches!(input, Input::KeyDown { .. }) {
                            assert_eq!(engine.runtime().heap.display(value).unwrap(), "77");
                        }
                        engine.take_result(context);
                        return;
                    }
                    EngineEvent::Waiting { .. } | EngineEvent::Yielded => {}
                    event => panic!("{event:?}"),
                }
            }
            panic!("input did not finish");
        };
        post(&mut engine, &mut host, Input::KeyDown { key: 9, shift: 0 });
        assert_eq!(
            run(&mut engine, &mut host, "a.focused && keys=='';", slice),
            "1"
        );
        post(&mut engine, &mut host, Input::KeyDown { key: 9, shift: 0 });
        assert_eq!(run(&mut engine, &mut host, "b.focused;", slice), "1");
        run(&mut engine, &mut host, "keys='';stop=true;", slice);
        for input in [
            Input::KeyDown { key: 9, shift: 0 },
            Input::KeyDown { key: 13, shift: 0 },
            Input::KeyUp { key: 13, shift: 0 },
            Input::KeyPress(27),
            Input::Wheel {
                shift: 1,
                delta: -120,
                x: 3,
                y: 4,
            },
        ] {
            post(&mut engine, &mut host, input);
        }
        assert_eq!(
            run(&mut engine, &mut host, "keys;", slice),
            "b:onKeyDown:9;b:onKeyDown:13;b:onKeyUp:13;r:onKeyUp:13;b:onKeyPress:\u{1b};r:onKeyPress:\u{1b};b:wheel:1,-120,3,4;"
        );
        run(&mut engine, &mut host, "stop=false;", slice);
        post(&mut engine, &mut host, Input::KeyDown { key: 9, shift: 1 });
        assert_eq!(run(&mut engine, &mut host, "a.focused;", slice), "1");
    }
}

#[test]
fn cancelled_focus_and_invalidation_release_owned_locks_and_shutdown_state() {
    let (mut engine, mut host, _) = setup();
    run(&mut engine, &mut host, BASE, 10000);
    run(
        &mut engine,
        &mut host,
        "a.focus();a.onBlur=function(next){System.wait(1000);};",
        10000,
    );
    let context = submit(&mut engine, "b.focus();");
    for _ in 0..10000 {
        step(&mut engine, &mut host, 1);
        if engine
            .sleep_duration()
            .is_some_and(|time| time >= Duration::from_millis(1000))
        {
            break;
        }
    }
    assert!(
        engine
            .sleep_duration()
            .is_some_and(|time| time >= Duration::from_millis(1000))
    );
    engine.cancel(context);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "a.onBlur=Layer.onBlur incontextof a;b.focused && a.focus();",
            1
        ),
        "1"
    );
    run(
        &mut engine,
        &mut host,
        "a.onSearchNextFocusable=function(layer){System.wait(1000);};",
        10000,
    );
    let context = submit(&mut engine, "a.enabled=false;");
    for _ in 0..10000 {
        step(&mut engine, &mut host, 1);
        if engine
            .sleep_duration()
            .is_some_and(|time| time >= Duration::from_millis(1000))
        {
            break;
        }
    }
    assert!(
        engine
            .sleep_duration()
            .is_some_and(|time| time >= Duration::from_millis(1000))
    );
    engine.cancel(context);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "a.onSearchNextFocusable=Layer.onSearchNextFocusable incontextof a;a.enabled=true;b.focus();a.focus();a.focused;",
            1
        ),
        "1"
    );
    run(
        &mut engine,
        &mut host,
        "b.onFocus=function(previous,direction){System.wait(1000);};",
        10000,
    );
    let context = submit(&mut engine, "invalidate a;");
    for _ in 0..10000 {
        step(&mut engine, &mut host, 1);
        if engine
            .sleep_duration()
            .is_some_and(|time| time >= Duration::from_millis(1000))
        {
            break;
        }
    }
    assert!(
        engine
            .sleep_duration()
            .is_some_and(|time| time >= Duration::from_millis(1000))
    );
    engine.cancel(context);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "b.onFocus=Layer.onFocus incontextof b;isvalid a && a.focus();",
            1
        ),
        "1"
    );
}
