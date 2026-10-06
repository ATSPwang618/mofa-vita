use krkr_engine::{
    Engine, EngineEvent,
    protocol::window::{self, Command, Response},
};
use std::{num::NonZeroUsize, sync::Arc, time::Duration};
use tjs_core::{RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit};
struct Clock;
impl tjs_runtime::clock::Clock for Clock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

#[test]
fn dialog_uses_resumable_host_and_reference_button_results() {
    let mut runtime = Runtime::new();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(runtime, Clock, Default::default(), Default::default()).unwrap();
    let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
    engine.attach_windows(client).unwrap();
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8(
            "dialog roundtrip",
            r#"
        function check(v) { if(!v) throw 'dialog contract'; }
        Plugins.link('win32dialog.dll');
        var d = new WIN32Dialog();
        check(d.messageBox('ok', 'caption', 999) === 1);
        check(d.messageBox('cancel') === 0);
        check(System.inform('notice') === void);
        check(System.inform('labels', 'custom', ['one', 'two', 'three']) === 2);
        Plugins.unlink('win32dialog.dll');
        42;
    "#,
        )
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    let mut calls = 0;
    for _ in 0..10_000 {
        let result = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            let Command::Inform {
                text,
                caption,
                buttons,
            } = &request.command
            else {
                panic!("unexpected dialog request")
            };
            let index = match calls {
                0 => {
                    assert_eq!(text, "ok");
                    assert_eq!(caption, "caption");
                    assert_eq!(buttons, &["OK", "Cancel"]);
                    0
                }
                1 => {
                    assert_eq!(text, "cancel");
                    assert_eq!(caption, "Information");
                    assert_eq!(buttons, &["OK", "Cancel"]);
                    1
                }
                2 => {
                    assert_eq!(text, "notice");
                    assert_eq!(buttons, &["OK"]);
                    0
                }
                3 => {
                    assert_eq!(caption, "custom");
                    assert_eq!(buttons, &["one", "two", "three"]);
                    2
                }
                _ => panic!("duplicate dialog"),
            };
            calls += 1;
            request.respond(Ok(Response::Informed(index)));
        }
        match result {
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            EngineEvent::Completed {
                result: RuntimeExit::Finished(Value::Int(42)),
                ..
            } => {
                assert_eq!(calls, 4);
                return;
            }
            other => panic!("dialog fixture failed: {other:?}"),
        }
    }
    panic!("dialog did not complete");
}
