use krkr_engine::{
    Engine, EngineEvent,
    protocol::window::{self, Command, Geometry, Rectangle, Response, Snapshot},
};
use std::{collections::HashMap, num::NonZeroUsize, sync::Arc, time::Duration};
use tjs_core::{NativeCallable, NativeCx, NativeError, NativeResult, RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit};

struct Clock;
impl tjs_runtime::clock::Clock for Clock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}
fn arm_missing(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let Value::Obj(reference) = args[0] else {
        return Err(NativeError::Type("object"));
    };
    cx.heap_mut().set_call_missing(reference.object.unwrap())?;
    Ok(Value::Void)
}

#[test]
fn screen_getters_and_host_coordinates_share_one_resumable_path() {
    let mut runtime = Runtime::new();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(runtime, Clock, Default::default(), Default::default()).unwrap();
    let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
    engine.attach_windows(client).unwrap();
    let global = engine.global();
    let heap = &mut engine.runtime_mut().heap;
    let arm = heap.intern(&"armMissing".encode_utf16().collect::<Vec<_>>());
    let function = heap.alloc_native_function(NativeCallable::Leaf(arm_missing));
    heap.set_member(global, arm, Value::Obj(function.into()))
        .unwrap();
    let mode = heap.intern(&"screenHostMode".encode_utf16().collect::<Vec<_>>());
    heap.set_member(global, mode, Value::Int(0)).unwrap();
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("LayerExScreen group", include_str!("fixtures/screen.tjs"))
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    let context = engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    let mut windows = HashMap::new();
    let mut queries = 0;
    for _ in 0..100_000 {
        while let Some(request) = host.next_request() {
            if request.cancelled() {
                continue;
            }
            match &request.command {
                Command::Create { .. } => {
                    let x = 100 + windows.len() as i32 * 200;
                    let geometry = Geometry {
                        left: x,
                        top: -300,
                        client_left: x + 13,
                        client_top: -263,
                        width: 666,
                        height: 517,
                        inner_width: 640,
                        inner_height: 480,
                    };
                    windows.insert(request.window, geometry);
                    // Constructor state is deliberately stale. A Screen query
                    // must obtain the snapshot client's actual global origin.
                    request.complete(Ok(Geometry {
                        client_left: 0,
                        client_top: 0,
                        ..geometry
                    }));
                }
                Command::Graphics(_) => request.respond(Ok(Response::Done)),
                Command::QueryState => {
                    queries += 1;
                    let geometry = windows[&request.window];
                    let mode = engine.runtime().heap.member(global, mode).unwrap().unwrap();
                    if matches!(mode, Value::Int(2)) {
                        request.respond(Err("screen host query failed".into()));
                        continue;
                    }
                    let client = (!matches!(mode, Value::Int(1))).then_some(Rectangle {
                        x: geometry.client_left,
                        y: geometry.client_top,
                        width: geometry.inner_width,
                        height: geometry.inner_height,
                    });
                    request.respond(Ok(Response::Snapshot(Snapshot {
                        // Also prove the helper chooses the capability-bearing
                        // client rectangle over the generic geometry fallback.
                        geometry: Geometry {
                            client_left: 0,
                            client_top: 0,
                            ..geometry
                        },
                        visible: false,
                        outer: None,
                        client,
                        normal: None,
                        normal_workspace: None,
                        maximized: false,
                        minimized: Some(false),
                        maximize_box: None,
                        minimize_box: None,
                    })));
                }
                Command::Position(x, y) => {
                    let geometry = windows.get_mut(&request.window).unwrap();
                    geometry.left = *x;
                    geometry.top = *y;
                    geometry.client_left = x + 13;
                    geometry.client_top = y + 37;
                    request.complete(Ok(*geometry));
                }
                _ => {
                    let geometry = windows[&request.window];
                    request.complete(Ok(geometry));
                }
            }
        }
        host.take_scenes(u64::MAX);
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            EngineEvent::Completed {
                context: done,
                result: RuntimeExit::Finished(value),
            } if done == context => {
                assert_eq!(engine.runtime().heap.display(value).unwrap(), "passed");
                assert!(
                    queries >= 20,
                    "both entry points must query fresh host state"
                );
                engine.take_result(context);
                return;
            }
            other => panic!("LayerExScreen fixture failed: {other:?}"),
        }
    }
    panic!("LayerExScreen fixture did not complete within its instruction budget");
}
