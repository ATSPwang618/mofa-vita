use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use tjs_bind as tjs;
use tjs_core::{RunBudget, SourceMap, Value, Vm, VmExit};

#[tjs::class(name = "Resource")]
mod resource {
    use super::*;

    #[derive(Default)]
    pub struct State {
        pub drops: Rc<Cell<u32>>,
        pub saved: Value,
    }
    impl tjs::Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            visit(self.saved);
        }
    }
    impl Drop for State {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self {
                drops: Rc::new(Cell::new(0)),
                saved: Value::Void,
            }
        }
        #[tjs::getter(name = "payload")]
        fn payload(&self) -> Value {
            self.saved
        }
    }
}

fn vm(script: &str, global: tjs::ObjId) -> Vm {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("native lifetime", script).unwrap();
    Vm::with_global(&tjs_front::compile(&sources, source).unwrap(), global)
}

fn run(vm: &mut Vm, heap: &mut tjs::Heap) -> VmExit {
    loop {
        let exit = vm.run_slice(heap, RunBudget::new(1).unwrap());
        heap.collect(vm.roots());
        if !matches!(exit, VmExit::Yielded) {
            return exit;
        }
    }
}

#[test]
fn native_state_is_traced_until_finalize_succeeds_then_dropped_exactly_once() {
    for automatic in [false, true] {
        let mut heap = tjs::new_heap();
        let class = resource::install(&mut heap).unwrap();
        let baseline = heap.collect([]).after;
        let global = heap.alloc_global();
        let root = heap.root(Value::Obj(global.into()));
        let drops = Rc::new(Cell::new(0));
        let saved = Value::Str(heap.alloc_string("retained".encode_utf16().collect::<Vec<_>>()));
        let object = heap
            .alloc_native(
                class,
                resource::State {
                    drops: drops.clone(),
                    saved,
                },
            )
            .unwrap();
        let c = heap.intern(&['c' as u16]);
        heap.set_member(global, c, Value::Obj(object.into()))
            .unwrap();
        let mut setup = vm(
            "var log = ''; c.finalize = function() { global.log += this.payload; throw 7; };",
            global,
        );
        assert!(matches!(run(&mut setup, &mut heap), VmExit::Finished(_)));
        drop(setup);
        let mut first = vm("invalidate c;", global);
        assert!(matches!(run(&mut first, &mut heap), VmExit::Thrown(_)));
        drop(first);
        assert_eq!(drops.get(), 0);
        assert!(heap.is_valid(object).unwrap());
        let mut retry = vm(
            if automatic {
                "c.finalize = function() { global.log += this.payload; }; c = void;"
            } else {
                "c.finalize = function() { global.log += this.payload; }; invalidate c;"
            },
            global,
        );
        assert!(matches!(run(&mut retry, &mut heap), VmExit::Finished(_)));
        drop(retry);
        if automatic {
            heap.collect([]);
            assert_eq!(drops.get(), 0);
            assert_eq!(heap.pending_finalizers(), 1);
            let mut finalizer = Vm::take_finalizer(&mut heap).unwrap();
            assert!(matches!(
                run(&mut finalizer, &mut heap),
                VmExit::Finished(_)
            ));
        }
        assert_eq!(drops.get(), 1);
        let mut result = vm("log;", global);
        let VmExit::Finished(value) = run(&mut result, &mut heap) else {
            panic!("log")
        };
        assert_eq!(heap.display(value).unwrap(), "retainedretained");
        drop(result);
        heap.release_root(root);
        assert_eq!(heap.collect([]).after, baseline);
        assert_eq!(drops.get(), 1);
    }
}

#[tjs::class(name = "OtherResource")]
mod other_resource {
    use super::*;
    #[derive(Default, tjs::Trace)]
    pub struct State {
        pub resource: resource::State,
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::getter(name = "otherPayload")]
        fn payload(&self) -> Value {
            self.resource.saved
        }
    }
}

#[test]
fn every_native_base_remains_live_through_script_finalize_then_drops_once() {
    for automatic in [false, true] {
        let mut heap = tjs::new_heap();
        resource::install(&mut heap).unwrap();
        other_resource::install(&mut heap).unwrap();
        let baseline = heap.collect([]).after;
        let global = heap.alloc_global();
        let root = heap.root(Value::Obj(global.into()));
        let mut setup = vm(
            "class C extends Resource, OtherResource {
            function finalize() { global.log = payload + otherPayload; }
        } var c = new C; var log = ''; c;",
            global,
        );
        let VmExit::Finished(Value::Obj(reference)) = run(&mut setup, &mut heap) else {
            panic!("setup");
        };
        let object = reference.object.unwrap();
        drop(setup);
        let a = Rc::new(Cell::new(0));
        let b = Rc::new(Cell::new(0));
        let left = Value::Str(heap.alloc_string("left".encode_utf16().collect::<Vec<_>>()));
        let right = Value::Str(heap.alloc_string("right".encode_utf16().collect::<Vec<_>>()));
        heap.with_native_state::<resource::State, _>(object, |state| {
            state.drops = a.clone();
            state.saved = left;
        })
        .unwrap();
        heap.with_native_state::<other_resource::State, _>(object, |state| {
            state.resource.drops = b.clone();
            state.resource.saved = right;
        })
        .unwrap();
        let mut finish = vm(
            if automatic {
                "c = void;"
            } else {
                "invalidate c;"
            },
            global,
        );
        assert!(matches!(run(&mut finish, &mut heap), VmExit::Finished(_)));
        drop(finish);
        if automatic {
            heap.collect([]);
            assert_eq!((a.get(), b.get()), (0, 0));
            let mut finalizer = Vm::take_finalizer(&mut heap).unwrap();
            assert!(matches!(
                run(&mut finalizer, &mut heap),
                VmExit::Finished(_)
            ));
        }
        assert_eq!((a.get(), b.get()), (1, 1));
        let mut result = vm("log;", global);
        let VmExit::Finished(value) = run(&mut result, &mut heap) else {
            panic!("finalizer");
        };
        assert_eq!(heap.display(value).unwrap(), "leftright");
        drop(result);
        heap.release_root(root).unwrap();
        assert_eq!(heap.collect([]).after, baseline);
        assert_eq!((a.get(), b.get()), (1, 1));
    }
}

#[derive(Clone, Default)]
struct CleanupLog(Rc<RefCell<Vec<&'static str>>>);
impl tjs::Trace for CleanupLog {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl CleanupLog {
    fn push(&self, text: &'static str) {
        self.0.borrow_mut().push(text);
    }
}

#[tjs::class(name = "CleanupBase")]
mod cleanup_base {
    use super::*;
    #[derive(Default, tjs::Trace)]
    pub struct State {
        pub log: CleanupLog,
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method]
        fn record(&self) {
            self.log.push("script");
        }
        #[tjs::invalidate]
        fn cleanup(&self) {
            self.log.push("base");
        }
    }
}

#[tjs::class(name = "CleanupOther")]
mod cleanup_other {
    use super::*;
    #[derive(Default, tjs::Trace)]
    pub struct State {
        pub log: CleanupLog,
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::invalidate(resumable = true)]
        fn cleanup(&self) -> tjs::NativeStep {
            self.log.push("other start");
            tjs::NativeStep::Continue(Box::new(self.log.clone()))
        }
    }
}
impl tjs::NativeContinuation for CleanupLog {
    fn resume(
        self: Box<Self>,
        _: &mut tjs::NativeCx<'_>,
        _: Value,
    ) -> tjs::NativeResult<tjs::NativeStep> {
        self.push("other end");
        Ok(tjs::NativeStep::Return(Value::Void))
    }
}

#[test]
fn intrinsic_hooks_run_in_reverse_facet_order_and_gc_keeps_them_without_script_finalize() {
    for automatic in [false, true] {
        let mut heap = tjs::new_heap();
        cleanup_base::install(&mut heap).unwrap();
        cleanup_other::install(&mut heap).unwrap();
        let global = heap.alloc_global();
        let root = heap.root(Value::Obj(global.into()));
        let mut setup = vm(
            r#"
            class C extends CleanupBase,CleanupOther {
                function finalize(){record();}
            }
            var c=new C();
            if(typeof c.cleanup!="undefined") throw "hook exposed as script member";
            c.cleanup=function(){throw "must not replace native hook";};
            c;
        "#,
            global,
        );
        let VmExit::Finished(Value::Obj(reference)) = run(&mut setup, &mut heap) else {
            panic!("setup");
        };
        drop(setup);
        let object = reference.object.unwrap();
        let log = CleanupLog::default();
        heap.with_native_state::<cleanup_base::State, _>(object, |s| s.log = log.clone())
            .unwrap();
        heap.with_native_state::<cleanup_other::State, _>(object, |s| s.log = log.clone())
            .unwrap();
        let mut finish = vm(
            if automatic {
                "delete c.finalize; c=void;"
            } else {
                "invalidate c;"
            },
            global,
        );
        assert!(matches!(run(&mut finish, &mut heap), VmExit::Finished(_)));
        drop(finish);
        if automatic {
            heap.collect([]);
            assert!(log.0.borrow().is_empty());
            let mut finalizer = Vm::take_finalizer(&mut heap).expect("native-only cleanup queued");
            assert!(matches!(
                run(&mut finalizer, &mut heap),
                VmExit::Finished(_)
            ));
        }
        assert_eq!(
            *log.0.borrow(),
            if automatic {
                vec!["other start", "other end", "base"]
            } else {
                vec!["script", "other start", "other end", "base"]
            }
        );
        heap.release_root(root).unwrap();
    }
}
