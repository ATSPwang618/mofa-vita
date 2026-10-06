use super::*;
use krkr_engine::protocol::graphics::Size;

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
    panic!("viewport callbacks did not finish");
}
fn post(engine: &mut Engine<Manual>, host: &mut Desktop, input: Input, slice: u32) {
    host.host
        .post(Event {
            window: *host.windows.keys().next().unwrap(),
            input,
        })
        .unwrap();
    drain(engine, host, slice);
}
const BASE: &str = r#"
var w=new Window(), root=new Layer(w,null), a=new Layer(w,root);
root.setSize(100,100);a.setSize(20,20);a.setPos(10,20);a.visible=true;a.focusable=true;
root.hitThreshold=a.hitThreshold=0;
var log='';
w.onMouseMove=function(x,y,s){global.log+='w'+x+','+y+';';};
a.onMouseMove=function(x,y,s){global.log+='a'+x+','+y+';';};
a.onMouseLeave=function(){global.log+='leave;';};
a.onMouseEnter=function(){global.log+='enter;';};
w.setZoom('6',3);w.setLayerPos(7,-9);
"#;

#[test]
fn fullscreen_fits_resized_client_and_restores_script_viewport() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(&mut engine, &mut host, BASE, slice);
        run(&mut engine, &mut host, "w.setInnerSize(100,100);", slice);
        let id = *host.windows.keys().next().unwrap();
        let windowed_geometry = host.windows[&id].0;
        let canvas = Size {
            width: 100,
            height: 100,
        };
        run(
            &mut engine,
            &mut host,
            "w.fullScreen=true;w.setZoom(4,1);",
            slice,
        );
        for (width, height, left, top) in [(600, 300, 150, 0), (300, 600, 0, 150)] {
            let geometry = Geometry {
                width,
                height,
                inner_width: width,
                inner_height: height,
                ..Default::default()
            };
            host.windows.get_mut(&id).unwrap().0 = geometry;
            post(&mut engine, &mut host, Input::Resize(geometry), slice);
            // Repeating the setter must keep the original client extent.
            run(&mut engine, &mut host, "w.fullScreen=true;log='';", slice);
            host.service();
            let viewport = host.snapshots.last().unwrap().1;
            assert_eq!((viewport.left, viewport.top), (left, top));
            assert_eq!((viewport.numer(), viewport.denom()), (3, 1));
            post(
                &mut engine,
                &mut host,
                Input::MouseMove {
                    x: left + 36,
                    y: top + 69,
                    shift: 0,
                },
                slice,
            );
            assert_eq!(
                run(
                    &mut engine,
                    &mut host,
                    "a.cursorX==2 && a.cursorY==3 && log.indexOf('a2,3;')>=0;",
                    slice
                ),
                "1"
            );
            run(
                &mut engine,
                &mut host,
                "a.setCursorPos(4,5);a.setAttentionPos(3,4);a.useAttention=true;a.focus();",
                slice,
            );
            assert_eq!(host.cursor_moves.last(), Some(&(left + 42, top + 75)));
            let attention = host.styles.last().unwrap().attention.unwrap();
            assert_eq!((attention.left, attention.top), (left + 39, top + 72));
        }
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "w.fullScreen && w.zoomNumer==4 && w.zoomDenom==1 && w.layerLeft==7 && w.layerTop==-9;",
                slice
            ),
            "1"
        );
        run(
            &mut engine,
            &mut host,
            "w.fullScreen=false;a.setCursorPos(4,5);",
            slice,
        );
        drain(&mut engine, &mut host, slice);
        host.service();
        let viewport = host.snapshots.last().unwrap().1;
        // Fullscreen requests and native resize events can complete separately.
        // This host still reports a 300x600 client: fit the 100x100 canvas at 3x,
        // including its script offset and 4x zoom, until restoration is confirmed.
        assert_eq!((viewport.left, viewport.top), (21, 123));
        assert_eq!((viewport.numer(), viewport.denom()), (4, 1));
        let destination = viewport.destination(canvas);
        assert_eq!((destination.width, destination.height), (1200, 1200));
        assert_eq!(host.cursor_moves.last(), Some(&(189, 423)));
        let attention = host.styles.last().unwrap().attention.unwrap();
        assert_eq!((attention.left, attention.top), (177, 411));

        // The OS restores the windowed extent with a later resize notification.
        // Do not reset the script viewport to make these assertions pass.
        host.windows.get_mut(&id).unwrap().0 = windowed_geometry;
        post(
            &mut engine,
            &mut host,
            Input::Resize(windowed_geometry),
            slice,
        );
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "a.setCursorPos(4,5);!w.fullScreen && w.innerWidth==100 && w.innerHeight==100 && w.zoomNumer==4 && w.zoomDenom==1 && w.layerLeft==7 && w.layerTop==-9;",
                slice,
            ),
            "1"
        );
        host.service();
        let viewport = host.snapshots.last().unwrap().1;
        assert_eq!((viewport.left, viewport.top), (7, -9));
        assert_eq!((viewport.numer(), viewport.denom()), (4, 1));
        let destination = viewport.destination(canvas);
        assert_eq!((destination.width, destination.height), (400, 400));
        assert_eq!(host.cursor_moves.last(), Some(&(63, 91)));
        let attention = host.styles.last().unwrap().attention.unwrap();
        assert_eq!((attention.left, attention.top), (59, 87));
        engine.reset();
    }
}

#[test]
fn window_viewport_maps_pixels_input_cursor_and_ime_in_both_directions() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(&mut engine, &mut host, BASE, slice);
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "w.zoomNumer==2 && w.zoomDenom==1 && w.layerLeft==7 && w.layerTop==-9;",
                slice
            ),
            "1"
        );
        post(
            &mut engine,
            &mut host,
            Input::MouseMove {
                x: 31,
                y: 37,
                shift: 0,
            },
            slice,
        );
        assert_eq!(
            run(&mut engine, &mut host, "log;", slice),
            "w31,37;enter;a2,3;"
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
        run(
            &mut engine,
            &mut host,
            "a.setCursorPos(4,5);a.setAttentionPos(3,4);a.useAttention=true;a.imeMode=2;a.focus();",
            slice,
        );
        assert_eq!(host.cursor_moves.last(), Some(&(35, 41)));
        let area = host.styles.last().unwrap().attention.unwrap();
        assert_eq!((area.left, area.top), (33, 39));
        run(
            &mut engine,
            &mut host,
            "log='';a.onMouseWheel=function(s,d,x,y){global.log+=x+','+y;};",
            slice,
        );
        post(
            &mut engine,
            &mut host,
            Input::Wheel {
                shift: 0,
                delta: 120,
                x: 31,
                y: 37,
            },
            slice,
        );
        assert_eq!(run(&mut engine, &mut host, "log;", slice), "12,23");
        run(&mut engine, &mut host, "log='';w.layerLeft=80;", slice);
        drain(&mut engine, &mut host, slice);
        assert_eq!(run(&mut engine, &mut host, "log;", slice), "leave;");
        let area = host.styles.last().unwrap().attention.unwrap();
        assert_eq!((area.left, area.top), (106, 39));
        post(&mut engine, &mut host, Input::MouseLeave, slice);
        run(
            &mut engine,
            &mut host,
            "log='';w.layerLeft=7;w.zoomDenom=2;w.zoomNumer=3;w.layerTop=5;",
            slice,
        );
        drain(&mut engine, &mut host, slice);
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "log=='' && w.zoomNumer==3 && w.zoomDenom==1;",
                slice
            ),
            "1"
        );
        host.service();
        let (_, view, _, _) = host.snapshots.last().unwrap();
        assert_eq!(
            (view.left, view.top, view.numer(), view.denom()),
            (7, 5, 3, 1)
        );
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                "var caught=0;try{w.setZoom(0,0);}catch(e){caught++;}caught==1 && w.zoomNumer==3;",
                slice
            ),
            "1"
        );
        run(&mut engine, &mut host, "w.close();", slice);
        drain(&mut engine, &mut host, slice);
        assert_eq!(engine.window_count(), 0);
    }
}

#[test]
fn scene_limits_accept_small_transitions_and_bound_each_snapshot_allocation() {
    for windows in [1, 2] {
        let clock = Manual::default();
        let mut engine = Engine::new(
            Runtime::new(),
            clock,
            SchedulerLimits::default(),
            Default::default(),
        )
        .unwrap();
        let (client, mut host) = Desktop::with_limits(window::Limits {
            scene_nodes: 3 * windows,
            ..Default::default()
        });
        engine.attach_windows(client).unwrap();
        run(
            &mut engine,
            &mut host,
            r#"
            var w=new Window(),r=new Layer(w,null),a=new Layer(w,r),b=new Layer(w,r);
            a.visible=true;b.visible=false;
            a.beginTransition('crossfade',true,b,%[time:1000]);w.setZoom(2,1);
        "#,
            1,
        );
        if windows == 2 {
            run(
                &mut engine,
                &mut host,
                r#"
                var w2=new Window(),r2=new Layer(w2,null),a2=new Layer(w2,r2),b2=new Layer(w2,r2);
                a2.visible=true;b2.visible=false;
                a2.beginTransition('crossfade',true,b2,%[time:1000]);w2.setLayerPos(-2,4);
                w.layerLeft=2;
            "#,
                1,
            );
        }
        host.service();
        assert_eq!(
            host.snapshots
                .iter()
                .filter(|(_, _, nodes, trans)| *nodes == 3 && *trans == 1)
                .map(|(id, _, _, _)| *id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            windows
        );
        assert!(
            host.snapshots
                .iter()
                .all(|(_, _, nodes, trans)| *nodes <= 3 && *trans <= 1)
        );
        engine.reset();
    }
}
