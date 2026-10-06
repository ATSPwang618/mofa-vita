use tjs_bind as tjs;
use tjs_core::{RunBudget, SourceMap, Value, Vm, VmExit};

#[tjs::class(name = "ResultProbe")]
mod probe {
    use super::*;
    #[derive(Default, tjs::Trace)]
    pub struct State {
        seen: i64,
    }
    impl State {
        #[tjs::constructor]
        fn new(cx: &mut tjs::NativeCx<'_>) -> Self {
            assert!(!cx.result_needed(), "constructor return is ignored");
            Self::default()
        }
        #[tjs::method]
        fn observe(&mut self, cx: &mut tjs::NativeCx<'_>) -> i64 {
            self.seen = self.seen * 10 + if cx.result_needed() { 2 } else { 1 };
            7
        }
        #[tjs::getter]
        fn seen(&self, cx: &mut tjs::NativeCx<'_>) -> i64 {
            assert!(cx.result_needed());
            self.seen
        }
        #[tjs::setter(name = "seen")]
        fn set_seen(&mut self, cx: &mut tjs::NativeCx<'_>, value: i64) {
            assert!(!cx.result_needed());
            self.seen = value;
        }
        #[tjs::method(resumable = true)]
        fn bridge(&self, cx: &mut tjs::NativeCx<'_>, function: Value) -> tjs::NativeStep {
            tjs::NativeStep::Call {
                function,
                arguments: vec![],
                continuation: Box::new(Resume {
                    needed: cx.result_needed(),
                    wait: true,
                }),
            }
        }
    }
    #[derive(tjs::Trace)]
    struct Resume {
        needed: bool,
        wait: bool,
    }
    impl tjs::NativeContinuation for Resume {
        fn resume(
            self: Box<Self>,
            cx: &mut tjs::NativeCx<'_>,
            result: Value,
        ) -> tjs::NativeResult<tjs::NativeStep> {
            assert_eq!(
                cx.result_needed(),
                self.needed,
                "outer demand survives callback/wait"
            );
            Ok(if self.wait {
                assert_eq!(result.as_integer(), Some(7));
                tjs::NativeStep::Wait {
                    request: tjs::WaitRequest {
                        mode: tjs::WaitMode::Internal,
                        token: 41,
                    },
                    continuation: Box::new(Resume {
                        needed: self.needed,
                        wait: false,
                    }),
                }
            } else {
                tjs::NativeStep::Return(result)
            })
        }
    }
}

fn check(script: &str, expected: i64) {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("call-results", script).unwrap();
    let module = tjs_front::compile(&sources, source).unwrap();
    for slice in [1, 4096] {
        let mut heap = tjs::new_heap();
        probe::install(&mut heap).unwrap();
        let baseline = heap.collect([]).after;
        let mut vm = Vm::new(&module);
        loop {
            let exit = vm.run_slice(&mut heap, RunBudget::new(slice).unwrap());
            heap.collect(vm.roots());
            match exit {
                VmExit::Yielded => assert!(vm.work_executed() < 100_000),
                VmExit::Waiting(request) => {
                    assert_eq!(request.token, 41);
                    vm.resume_wait(Ok(Value::Int(9))).unwrap();
                }
                VmExit::Finished(value) => {
                    assert_eq!(value.as_integer(), Some(expected), "{script}");
                    break;
                }
                exit => panic!("{script}: {exit:?}"),
            }
        }
        drop(vm);
        assert_eq!(heap.collect([]).after, baseline);
    }
}

#[test]
fn calls_keep_argument_effects_lookup_and_native_result_demand_separate() {
    for (script, expected) in [
        (
            "var p=new ResultProbe(); p.observe(); var v=p.observe(); p.observe(), p.observe(); p.seen;",
            1211,
        ),
        (
            "var p=new ResultProbe(); p.observe(p.observe()); p.seen;",
            21,
        ),
        (
            "var p=new ResultProbe(); property f {getter{p.observe(); return p.observe;}} f(); p.seen;",
            11,
        ),
        (
            "var p=new ResultProbe(); (1 ? p.observe() : p.observe()); p.observe() if 1; p.seen;",
            11,
        ),
        ("var p=new ResultProbe(); p.seen=4; p.seen+=2; p.seen;", 6),
        (
            "var p=new ResultProbe(); p.bridge(p.observe); var n=p.bridge(p.observe); p.seen*100+n;",
            2209,
        ),
        (
            "var n=0; function arg(){++n;return null;} Math.abs(arg()); Math.max(null); Math.pow(null,null); n;",
            1,
        ),
        (
            "var n=0; try {Math.abs();} catch(e){++n;} try{var x=Math.abs(null);}catch(e){n+=10;} n;",
            11,
        ),
        (
            "var n=0; function wrap(){return Math.abs(null);} try{wrap();}catch(e){n=1;} n;",
            1,
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn primitive_and_container_methods_follow_their_own_discard_rules() {
    for (script, expected) in [
        ("'%bad'.sprintf(null); 'abc'.repeat(null); 7;", 7),
        (
            "var n=0; try{'abc'.charAt(null);}catch(e){n++;} try{'abc'.substring(0,null);}catch(e){n+=10;} ''.charAt(null); n;",
            11,
        ),
        (
            "var n=0; try{'abc'.repeat();}catch(e){n++;} try{'abc'.trim(1);}catch(e){n+=10;} n;",
            11,
        ),
        ("[null].pack('?'); <%01%>.unpack('?'); 7;", 7),
        (
            "var n=0; try{[1].pack(null);}catch(e){n++;} try{<%01%>.unpack(null);}catch(e){n+=10;} try{<%%>.unpack('C');}catch(e){n+=100;} n;",
            110,
        ),
        (
            "var a=[3,2,1]; a.sort(); a.push(4); a.pop(); a.shift(); a.join(',')=='2,3';",
            1,
        ),
        (
            "var a=new Math.RandomGenerator(1), b=new Math.RandomGenerator(1); a.random32(); var skipped=b.random32(); a.random32()===b.random32();",
            1,
        ),
        (
            "var a=new Math.RandomGenerator(1), b=new Math.RandomGenerator(1); a.serialize(); a.random32()===b.random32();",
            1,
        ),
    ] {
        check(script, expected);
    }
}
