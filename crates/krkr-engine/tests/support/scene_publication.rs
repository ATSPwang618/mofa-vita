use super::*;
use krkr_engine::protocol::graphics::Scene;

fn snapshot(engine: &mut Engine<Manual>, host: &mut Desktop, script: &str, slice: u32) -> Scene {
    host.service();
    let context = submit(engine, script);
    let mut scene = None;
    for _ in 0..10000 {
        host.service();
        let event = engine.poll(
            RunBudget::new(slice).unwrap(),
            NonZeroUsize::new(64).unwrap(),
        );
        for (_, next) in host.host.take_scenes(u64::MAX) {
            scene = Some(next);
        }
        engine.collect([]);
        match event {
            EngineEvent::Completed {
                context: done,
                result: RuntimeExit::Finished(_),
            } if done == context => {
                engine.take_result(done);
                return scene.expect("explicit update must publish");
            }
            EngineEvent::Window {
                context,
                result: RuntimeExit::Finished(_),
                ..
            } => {
                engine.take_result(context);
            }
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => (),
            event => panic!("{event:?}"),
        }
    }
    panic!("scene script did not finish");
}

#[test]
fn presentation_prunes_hidden_trees_but_keeps_paint_order_and_show_hide_changes() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(
            &mut engine,
            &mut host,
            r#"
            var w=new Window(),r=new Layer(w,null),page=new Layer(w,r),child=new Layer(w,page);
            var transparent=new Layer(w,r),nested=new Layer(w,transparent),a=new Layer(w,r);
            var detached=new Layer(w,r),offscreen=new Layer(w,detached),paints='';
            detached.parent=null;
            r.setSize(100,100);page.setSize(40,40);transparent.setSize(40,40);
            page.left=10;child.left=11;transparent.left=20;nested.left=21;a.left=30;
            page.visible=false;child.visible=true;transparent.visible=true;transparent.opacity=0;
            nested.visible=true;a.visible=true;detached.visible=true;offscreen.visible=true;
            page.onPaint=function(){global.paints+='p';child.update();};
            child.onPaint=function(){global.paints+='c';};
            transparent.onPaint=function(){global.paints+='t';};
        "#,
            slice,
        );
        let scene = snapshot(
            &mut engine,
            &mut host,
            "page.update();transparent.update();w.update();if(paints!='pct')throw 'hidden paint lost';",
            slice,
        );
        assert_eq!(scene.nodes.len(), 2);
        assert_eq!(scene.nodes[1].rectangle.left, 30);
        assert_eq!(scene.nodes[1].parent, Some(0));

        let scene = snapshot(
            &mut engine,
            &mut host,
            "page.visible=true;transparent.opacity=128;a.visible=false;w.update();",
            slice,
        );
        assert_eq!(
            scene
                .nodes
                .iter()
                .map(|n| n.rectangle.left)
                .collect::<Vec<_>>(),
            [0, 10, 11, 20, 21]
        );
        assert_eq!(
            scene.nodes.iter().map(|n| n.parent).collect::<Vec<_>>(),
            [None, Some(0), Some(1), Some(0), Some(3)]
        );
        assert_eq!(scene.nodes[3].opacity, 128);
        let scene = snapshot(
            &mut engine,
            &mut host,
            "page.visible=false;transparent.opacity=0;a.visible=true;w.update();",
            slice,
        );
        assert_eq!(scene.nodes.len(), 2);
        assert_eq!(run(&mut engine, &mut host, "paints;", slice), "pct");
        engine.reset();
    }
}

#[test]
fn transition_snapshots_retain_hidden_detached_sources_and_their_children() {
    for slice in [1, 10000] {
        let (mut engine, mut host, _) = setup();
        run(
            &mut engine,
            &mut host,
            r#"
            var w=new Window(),r=new Layer(w,null),a=new Layer(w,r),source=new Layer(w,r);
            source.parent=null;
            var child=new Layer(w,source),paints=0;
            r.setSize(100,100);a.setSize(40,40);source.setSize(40,40);
            a.visible=true;source.visible=false;child.visible=true;child.left=7;
            source.onPaint=function(){global.paints++;child.update();};
            child.onPaint=function(){global.paints++;};
            a.beginTransition('crossfade',true,source,%[time:1000]);
        "#,
            slice,
        );
        let scene = snapshot(
            &mut engine,
            &mut host,
            "source.update();w.update();if(paints!=2)throw 'transition paint lost';",
            slice,
        );
        assert_eq!(scene.transitions.len(), 1);
        let transition = &scene.transitions[0];
        assert!(scene.nodes[transition.destination].visible);
        assert!(!scene.nodes[transition.source].visible);
        assert!(
            scene
                .nodes
                .iter()
                .any(|n| n.parent == Some(transition.source) && n.rectangle.left == 7)
        );
        let scene = snapshot(
            &mut engine,
            &mut host,
            "a.stopTransition();w.update();",
            slice,
        );
        assert!(scene.transitions.is_empty());
        engine.reset();
    }
}
