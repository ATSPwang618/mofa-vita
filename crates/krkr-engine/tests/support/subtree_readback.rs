use super::*;
use tjs_core::{NativeCx, NativeResult, NativeStep, Value};

#[tjs_bind::class(name = "SubtreeReader")]
mod reader {
    use super::*;

    #[derive(Default, tjs_bind::Trace)]
    pub struct State;

    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self
        }

        #[tjs::method(resumable = true)]
        fn read(cx: &mut NativeCx<'_>, source: Value) -> NativeResult<NativeStep> {
            krkr_engine::extensions::layer_read_subtree(cx, source, Box::new(Pixels))
        }
    }

    #[derive(tjs_bind::Trace)]
    struct Pixels;

    impl krkr_engine::extensions::PixelContinuation for Pixels {
        fn pixels(
            self: Box<Self>,
            _: &mut NativeCx<'_>,
            pixels: Arc<krkr_protocol::pixels::Pixels>,
        ) -> NativeResult<NativeStep> {
            assert_eq!(
                pixels.size,
                Size {
                    width: 8,
                    height: 4
                }
            );
            Ok(NativeStep::Return(Value::Int(i64::from(
                pixels.main.as_ref().unwrap().as_slice()[0],
            ))))
        }
    }
}

#[test]
fn subtree_readback_paints_then_composes_once_and_releases_failed_captures() {
    for slice in [1, 10000] {
        for reject in [false, true] {
            let mut h = Harness::new(slice);
            let global = h.engine.global();
            let heap = &mut h.engine.runtime_mut().heap;
            let class = reader::install(heap).unwrap();
            let name = heap.intern(&"SubtreeReader".encode_utf16().collect::<Vec<_>>());
            heap.set_member(global, name, Value::Obj(class.into()))
                .unwrap();
            h.run(SETUP);
            h.run("art.setSize(8,4); var child=new Layer(w,art); child.visible=true;");
            let id = h.submit(
                "var reader=new SubtreeReader(); art.onPaint=function(){child.left=3;};
                 art.update(); var result=0;
                 try { result=reader.read(art); } catch(e) { result=1; } result;",
            );
            let mut composed = None;
            let mut reads = 0;
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                assert!(Instant::now() < deadline, "subtree readback did not finish");
                while let Some(request) = h.host.next_request() {
                    if let Command::Graphics(Draw::ComposeScene { image, size, scene }) =
                        &request.command
                    {
                        assert!(composed.is_none(), "capture was recomposited");
                        assert_eq!(
                            *size,
                            Size {
                                width: 8,
                                height: 4
                            }
                        );
                        assert_eq!(scene.nodes.len(), 2);
                        assert_eq!(scene.nodes[1].rectangle.left, 3, "onPaint was skipped");
                        composed = Some((image.id, Arc::downgrade(&image.lifetime)));
                        request.respond(if reject {
                            Err("composition rejected".into())
                        } else {
                            Ok(Response::Done)
                        });
                        continue;
                    }
                    if let Command::Graphics(Draw::ReadImage { image }) = &request.command {
                        assert!(!reject, "failed composition must not be read back");
                        assert_eq!(image.id, composed.as_ref().unwrap().0);
                        reads += 1;
                        let size = Size {
                            width: 8,
                            height: 4,
                        };
                        let mut main = krkr_protocol::pixels::Bytes::zeroed(
                            size.rgba_bytes().unwrap(),
                            &h.host.staging_budget(),
                        )
                        .unwrap();
                        main.as_mut_slice()[0] = 42;
                        request.respond(Ok(Response::Image(krkr_protocol::pixels::Pixels {
                            size,
                            main: Some(main),
                            province: None,
                        })));
                        continue;
                    }
                    if matches!(
                        &request.command,
                        Command::Graphics(Draw::Create { .. } | Draw::PiledCopy { .. })
                    ) {
                        panic!("capture allocated or copied an extra image");
                    }
                    h.respond(request);
                }
                match h.step() {
                    EngineEvent::Completed {
                        context,
                        result: RuntimeExit::Finished(value),
                    } => {
                        assert_eq!(context, id);
                        assert_eq!(value.as_integer(), Some(if reject { 1 } else { 42 }));
                        h.engine.take_result(id);
                        break;
                    }
                    EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
                    event => panic!("{event:?}"),
                }
            }
            assert_eq!(reads, usize::from(!reject));
            assert_eq!(composed.unwrap().1.strong_count(), 0);
            assert_eq!(h.engine.pending_operations(), 0);
        }
    }
}
