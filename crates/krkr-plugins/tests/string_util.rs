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
fn keyframes_preserve_literal_closing_parentheses_and_nested_fields_across_yields() {
    let mut runtime = Runtime::new();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(runtime, Clock, Default::default(), Default::default()).unwrap();
    let source = engine.runtime_mut().sources.add_utf8("keyframes", r#"
Plugins.link("stringUtil.dll");
Plugins.link("equations.dll");
function check(ok) { if (!ok) throw "keyframe mismatch"; }
var text = "(0,0,n,ev05b05(青子影s),745,124,4200,0,16,248,552,14,1))(1000,,l,,,,,,,,,,)(2000,,,,,,,255,,,,~,)(3000,,,,,,,0,,,,-11,)";
var keys = parseKeyFrame(text);
check(keys.count == 4 && keys[0][0] === 0 && keys[0][3] === "ev05b05(青子影s)");
check(keys[0][12] === "1)" && keys[1][0] === 1000 && keys[3][12] === "");
var equations = new Equations();
var eased = parseKeyFrame("(0,3,l)(12000,0,l)");
check(equations.calc(eased[0][1], 16, 0, 868, 12000) == equations.easeOutQuad(16, 0, 868, 12000));
check(equations.calc(3.9, 16, 0, 868, 12000) == equations.easeOutQuad(16, 0, 868, 12000));
check(equations.calc("", 16, 0, 868, 12000) == equations.easeNone(16, 0, 868, 12000));
check(parseKeyFrame(keys) === keys);
var long = "";
for (var i=0; i<100; i++) long += "(12,func(a,(b,c)),tail))";
var parsed = parseKeyFrame(long);
check(parsed.count == 100 && parsed[99][0] === 12);
check(parsed[99][1] === "func(a,(b,c))" && parsed[99][2] === "tail)");
"passed";
"#).unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    loop {
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Yielded => {}
            EngineEvent::Completed {
                result: RuntimeExit::Finished(value),
                ..
            } => {
                assert_eq!(engine.runtime().heap.display(value).unwrap(), "passed");
                break;
            }
            other => panic!("keyframe fixture failed: {other:?}"),
        }
    }
}
