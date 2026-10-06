use krkr_engine::{Engine, EngineEvent};
use std::{num::NonZeroUsize, time::Duration};
use tjs_core::RunBudget;
use tjs_runtime::{Runtime, RuntimeExit};
struct Clock;
impl tjs_runtime::clock::Clock for Clock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

#[test]
fn menu_focus_keeps_the_script_entry_without_menu_work() {
    let mut runtime = Runtime::new();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(runtime, Clock, Default::default(), Default::default()).unwrap();
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8(
            "MenuFocus contract",
            r#"
        function check(v) { if(!v) throw 'MenuFocus contract'; }
        check(typeof System.menuFocus == "void" || typeof System.menuFocus == "undefined");
        Plugins.link('MenuFocus.dll');
        check(System.menuFocus() === 0);
        var retained = System.menuFocus;
        check(Plugins.unlink('MenuFocus.dll'));
        check(typeof System.menuFocus == "void" || typeof System.menuFocus == "undefined");
        check(retained() === 0);
        Plugins.link('MenuFocus.tpm');
        check(System.menuFocus() === 0);
        'passed';
    "#,
        )
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    for _ in 0..1000 {
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Completed {
                result: RuntimeExit::Finished(value),
                ..
            } => {
                assert_eq!(engine.runtime().heap.display(value).unwrap(), "passed");
                return;
            }
            EngineEvent::Completed {
                result: RuntimeExit::Thrown(exception),
                ..
            } => panic!("MenuFocus threw: {:?}", exception.diagnostic),
            EngineEvent::Completed { result, .. } => panic!("MenuFocus failed: {result:?}"),
            _ => {}
        }
    }
    panic!("MenuFocus timeout");
}
