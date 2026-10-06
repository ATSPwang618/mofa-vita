use tjs_bind as tjs;
use tjs_core::{
    NativeContinuation, NativeCx, NativeResult, NativeStep, RunBudget, Trace, Value, Vm, VmExit,
    WaitMode, WaitRequest,
};

#[tjs::class(name = "AsyncCell")]
mod cell {
    use super::*;
    #[derive(Default, tjs::Trace)]
    pub struct State {
        pub value: Value,
    }
    impl State {
        #[tjs::constructor]
        fn new(value: Value) -> Self {
            Self { value }
        }
        #[tjs::getter(name = "value", resumable = true)]
        fn get(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            let value = cx.with_state::<Self, _>(|s, _| Ok(s.value))?;
            Ok(Pending::wait(value, false))
        }
        #[tjs::setter(name = "value", resumable = true)]
        fn set(_cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            Ok(Pending::wait(value, true))
        }
    }
}
struct Pending {
    value: Value,
    write: bool,
}
impl Pending {
    fn wait(value: Value, write: bool) -> NativeStep {
        NativeStep::Wait {
            request: WaitRequest {
                mode: WaitMode::Internal,
                token: u64::from(write),
            },
            continuation: Box::new(Self { value, write }),
        }
    }
}
impl Trace for Pending {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(self.value);
    }
}
impl NativeContinuation for Pending {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if self.write {
            cx.with_state::<cell::State, _>(|s, _| {
                s.value = self.value;
                Ok(())
            })?;
            Ok(NativeStep::Return(Value::Void))
        } else {
            Ok(NativeStep::Return(self.value))
        }
    }
}

#[test]
fn native_accessors_suspend_keep_context_and_gc_edges_and_propagate_wait_errors() {
    let mut sources = tjs_core::SourceMap::new();
    let source = sources
        .add_utf8(
            "resumable properties",
            r#"
        function check() {
            var a=new AsyncCell(%[text:'kept']), b=new AsyncCell(5);
            var p=&a.value, q=p incontextof b;
            // Instances retain their property/accessor snapshots after class edits.
            delete AsyncCell.value;
            var text=(*p).text;
            *q += 2;
            *p = %[text:text+'!'];
            var ok=a.value.text=='kept!' && b.value==7 && typeof a.value=='Object';
            try { a.value=99; } catch(e) { ok=ok && e.message=='IO failure'; }
            return ok && a.value.text=='kept!';
        }
        check();
    "#,
        )
        .unwrap();
    let module = tjs_front::compile(&sources, source).unwrap();
    let mut heap = tjs::new_heap();
    cell::install(&mut heap).unwrap();
    let mut vm = Vm::new(&module);
    let mut writes = 0;
    let result = loop {
        let exit = vm.run_slice(&mut heap, RunBudget::new(1).unwrap());
        heap.collect(vm.roots());
        match exit {
            VmExit::Yielded => {}
            VmExit::Waiting(request) => {
                writes += usize::from(request.token == 1);
                let result = if request.token == 1 && writes == 3 {
                    Err(tjs::NativeError::Message("IO failure"))
                } else {
                    Ok(Value::Void)
                };
                vm.resume_wait(result).unwrap();
            }
            VmExit::Finished(value) => break value,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(writes, 3);
    assert_eq!(heap.display(result).unwrap(), "1");
}
