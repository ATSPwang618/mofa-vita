use krkr_engine::{Engine, EngineEvent};
use std::{ffi::OsStr, num::NonZeroUsize, process::Command, time::Duration};
use tjs_core::{RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit};
mod support;

const CHILD: &str = "KRKR_WINDOWEX_ENV_FIXTURE_CHILD";
const BASE: &str = "KRKR_WINDOWEX_ENV_BASE";
struct Clock;
impl tjs_runtime::clock::Clock for Clock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

// A single integration scenario, isolated in a test subprocess. Command::env
// sets only dedicated fixture names in the child and never mutates this process.
#[test]
fn windowex_environment_group() {
    if std::env::var_os(CHILD).as_deref() != Some(OsStr::new("1")) {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "windowex_environment_group", "--nocapture"])
            .env(CHILD, "1")
            .env(BASE, "fixture")
            .env("KRKR_WINDOWEX_ENV_EMPTY", "")
            .env("KRKR_WINDOWEX_ENV_UNICODE", "海🎨")
            .env("KRKR_WINDOWEX_ENV_Ü", "unicode-name")
            .env("KRKR_WINDOWEX_ENV_NESTED", format!("%{BASE}%"))
            .env("KRKR_WINDOWEX_ENV_LARGE", "x".repeat(32760))
            .env_remove("KRKR_WINDOWEX_ENV_MISSING")
            .env_remove("krkr_windowex_env_base");
        // On Windows, removing a differently-cased name also removes BASE.
        command.env(BASE, "fixture");
        #[cfg(windows)]
        {
            use std::{ffi::OsString, os::windows::ffi::OsStringExt};
            let name = "KRKR_WINDOWEX_ENV_"
                .encode_utf16()
                .chain([0xd800])
                .collect::<Vec<_>>();
            command.env(
                OsString::from_wide(&name),
                OsString::from_wide(&[65, 0xdfff, 90]),
            );
        }
        #[cfg(unix)]
        {
            use std::{ffi::OsString, os::unix::ffi::OsStringExt};
            command.env(
                "KRKR_WINDOWEX_ENV_INVALID_UTF8",
                OsString::from_vec(vec![65, 255]),
            );
        }
        let output = command
            .output()
            .expect("spawn isolated environment fixture");
        assert!(
            output.status.success(),
            "environment fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, support::Log).unwrap();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(runtime, Clock, Default::default(), Default::default()).unwrap();
    let global = engine.global();
    let mut nul_name = BASE.encode_utf16().collect::<Vec<_>>();
    nul_name.extend([0, 88]);
    let nul_source = format!("a%{BASE}%")
        .encode_utf16()
        .chain([0, 37, 88, 37])
        .collect::<Vec<_>>();
    let surrogate_name = "KRKR_WINDOWEX_ENV_"
        .encode_utf16()
        .chain([0xd800])
        .collect::<Vec<_>>();
    let sliced_source = std::iter::repeat_n(65, 4095)
        .chain(format!("%{BASE}%").encode_utf16())
        .chain([0xd800])
        .collect::<Vec<_>>();
    for (name, units) in [
        ("nulName", nul_name),
        ("nulSource", nul_source),
        ("leadingNul", vec![0, 88]),
        ("surrogateName", surrogate_name),
        ("surrogateValue", vec![65, 0xdfff, 90]),
        ("literalSurrogate", vec![0xd800]),
        ("slicedSource", sliced_source),
    ] {
        let heap = &mut engine.runtime_mut().heap;
        let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
        let string = heap.alloc_string(units);
        heap.set_member(global, key, Value::Str(string)).unwrap();
    }
    for (name, flag) in [("nativeWindows", cfg!(windows)), ("nativeUnix", cfg!(unix))] {
        let heap = &mut engine.runtime_mut().heap;
        let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
        heap.set_member(global, key, Value::Int(i64::from(flag)))
            .unwrap();
    }
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8(
            "windowEx environment group",
            include_str!("fixtures/environment.tjs"),
        )
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    let result = loop {
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Yielded => {}
            EngineEvent::Completed {
                result: RuntimeExit::Finished(value),
                ..
            } => break value,
            other => panic!("environment fixture failed: {other:?}"),
        }
    };
    assert_eq!(engine.runtime().heap.display(result).unwrap(), "passed");
}
