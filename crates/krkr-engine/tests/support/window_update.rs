use super::*;

#[test]
fn window_update_submits_the_scene_before_the_next_host_operation() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(
            &mut engine,
            &mut host,
            "var w=new Window(),r=new Layer(w,null),a=new Layer(w,r);a.visible=true;",
            slice,
        );
        host.service();
        let context = submit(
            &mut engine,
            "a.left=23;w.update();w.caption='after update';a.left=99;",
        );
        let mut observed = false;
        for _ in 0..10000 {
            let event = engine.poll(
                RunBudget::new(slice).unwrap(),
                NonZeroUsize::new(64).unwrap(),
            );
            engine.collect([]);
            if let Some(request) = host.host.next_request() {
                assert!(matches!(&request.command, Command::Caption(_)));
                let scenes = host.host.take_scenes(u64::MAX);
                assert_eq!(
                    scenes.len(),
                    1,
                    "update must submit before returning to script"
                );
                assert_eq!(scenes[0].1.nodes[1].rectangle.left, 23);
                request.complete(Ok(host.windows[&scenes[0].0].0));
                observed = true;
            }
            match event {
                EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
                EngineEvent::Completed {
                    context: done,
                    result: RuntimeExit::Finished(_),
                } => {
                    assert_eq!(done, context);
                    engine.take_result(done);
                    break;
                }
                event => panic!("{event:?}"),
            }
        }
        assert!(observed);
        engine.reset();
    }
}

#[test]
fn explicit_window_update_finishes_paint_in_order_before_returning() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            class KAGWindow extends Window { function KAGWindow(){super.Window();} }
            var w=new KAGWindow(),r=new Layer(w,null),a=new Layer(w,r),b=new Layer(w,a);
            var other=new Window(),otherRoot=new Layer(other,null),order='';
            a.onPaint=function(){
                global.order+='a';
                b.update();
                w.update(); // no recursive paint delivery
                System.wait(0);
                a.setSize(7,9); // native IO can suspend the same paint pass
            };
            b.onPaint=function(){global.order+='b';};
            otherRoot.onPaint=function(){throw 'another window was painted';};
            otherRoot.update();
            a.update();
            w.update();
            if(order!='ab' || a.callOnPaint || b.callOnPaint) throw 'incomplete paint';
            if(!otherRoot.callOnPaint) throw 'other window consumed';
            otherRoot.callOnPaint=false;
            w.update(utNormal);w.update(utEntire);w.update(void);
            if(order!='ab') throw 'exposure must not request fresh onPaint';
            'ok';
        "#,
                slice
            ),
            "ok"
        );
        host.service();
        assert!(!host.snapshots.is_empty());
        engine.reset();
    }
}

#[test]
fn window_update_recovers_after_paint_errors_and_skips_invalidated_children() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        assert_eq!(
            run(
                &mut engine,
                &mut host,
                r#"
            var w=new Window(),r=new Layer(w,null),a=new Layer(w,r),b=new Layer(w,r),order='';
            a.onPaint=function(){global.order+='a';throw 'paint failed';};
            b.onPaint=function(){global.order+='b';};
            a.update();b.update();
            var caught=false;
            try{w.update();}catch(e){caught=true;}
            if(!caught || order!='a') throw 'paint exception swallowed';
            w.update();
            if(order!='ab') throw 'paint lock survived exception';
            a.onPaint=function(){invalidate b;w.update();global.order+='c';};
            a.update();b.update();w.update();
            if(order!='abc') throw 'invalidated child was painted';
            invalidate w;
            'ok';
        "#,
                slice
            ),
            "ok"
        );
        engine.reset();
    }
}

#[test]
fn ordinary_paint_callback_can_request_window_update_without_recursion() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(
            &mut engine,
            &mut host,
            r#"
            var w=new Window(),r=new Layer(w,null),count=0;
            r.onPaint=function(){global.count++;w.update();};
            r.update();
        "#,
            slice,
        );
        for _ in 0..10000 {
            match step(&mut engine, &mut host, slice) {
                EngineEvent::Window {
                    context,
                    result: RuntimeExit::Finished(_),
                    ..
                } => {
                    engine.take_result(context);
                }
                EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
                EngineEvent::Idle => break,
                event => panic!("{event:?}"),
            }
        }
        assert_eq!(run(&mut engine, &mut host, "count;", slice), "1");
        engine.reset();
    }
}
