use tjs_bind as tjs;
use tjs_core::{RunBudget, SourceMap, Value, Vm, VmExit};

#[tjs::class(name = "StateGate")]
mod gate {
    use super::*;
    #[derive(Default, tjs::Trace)]
    pub struct State;
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self
        }
        #[tjs::method(resumable = true)]
        fn wait() -> tjs::NativeStep {
            tjs::NativeStep::Wait {
                request: tjs::WaitRequest {
                    mode: tjs::WaitMode::Internal,
                    token: 64,
                },
                continuation: Box::new(Resume),
            }
        }
    }
    #[derive(tjs::Trace)]
    struct Resume;
    impl tjs::NativeContinuation for Resume {
        fn resume(
            self: Box<Self>,
            _: &mut tjs::NativeCx<'_>,
            value: Value,
        ) -> tjs::NativeResult<tjs::NativeStep> {
            Ok(tjs::NativeStep::Return(value))
        }
    }
}

fn run(heap: &mut tjs::Heap, global: tjs::ObjId, script: &str, slice: u32) -> i64 {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("stateful-stdlib", script).unwrap();
    let module = tjs_front::compile(&sources, source).unwrap_or_else(|e| panic!("{script}: {e}"));
    let mut vm = Vm::with_global(&module, global);
    loop {
        let exit = vm.run_slice(heap, RunBudget::new(slice).unwrap());
        heap.collect(vm.roots());
        match exit {
            VmExit::Yielded => assert!(vm.work_executed() < 200000),
            VmExit::Waiting(request) => {
                assert_eq!(request.token, 64);
                vm.resume_wait(Ok(Value::Void)).unwrap();
            }
            VmExit::Finished(value) => return value.as_integer().unwrap(),
            other => panic!("{script}: {other:?}"),
        }
    }
}
fn check(script: &str, expected: i64) {
    for slice in [1, 4096] {
        let mut heap = tjs::new_heap();
        gate::install(&mut heap).unwrap();
        let baseline = heap.collect([]).after;
        let global = heap.alloc_global();
        assert_eq!(run(&mut heap, global, script, slice), expected, "{script}");
        assert_eq!(heap.collect([]).after, baseline);
    }
}

#[test]
fn random_serialization_preserves_exact_state_and_position() {
    check(
        r#"
        var r=new Math.RandomGenerator(123), s=r.serialize();
        var ok=(s.left===1 && s.next===0 && s.state.substr(0,8)=='80000000');
        var clone=new Math.RandomGenerator(s), before=clone.serialize();
        ok=ok && before.state==s.state && before.left==s.left && before.next==s.next;
        for(var i=0;i<1400;i++) {
            if(r.random32()!==clone.random32()) ok=0;
            if(i==0 || i==622 || i==623 || i==624 || i==1247) {
                var a=r.serialize(), b=clone.serialize();
                if(a.state!=b.state || a.left!=b.left || a.next!=b.next) ok=0;
                if(a.left!==624-(i%624) || a.next!==(i%624)+1) ok=0;
                clone=new Math.RandomGenerator(a);
            }
        }
        ok;
    "#,
        1,
    );
    check(
        r#"
        var data=%[state:'00000001'+'00000000'.repeat(623),left:2,next:0];
        var r=new Math.RandomGenerator(data), s=r.serialize();
        var n=r.random32(), after=r.serialize();
        (s.state==data.state && s.left==2 && s.next==0 && after.left==1 && after.next==1)*10000000+n;
    "#,
        14194449,
    );
}

#[test]
fn random_restore_requires_fields_catches_getter_errors_and_keeps_effects() {
    check(
        r#"
        var r=new Math.RandomGenerator(12), b=new Math.RandomGenerator(12), s=r.serialize(), log='';
        class Seed {
            property state {getter {log+='s';StateGate.wait();return s.state;}}
            property left {getter {log+='l';StateGate.wait();return s.left;}}
            property next {getter {log+='n';StateGate.wait();return s.next;}}
        }
        r.randomize(new Seed());
        (log=='sln')*10+(r.random64()===b.random64());
    "#,
        11,
    );
    check(
        r#"
        var r=new Math.RandomGenerator(12), b=new Math.RandomGenerator(12), s=r.serialize(), log='';
        class Seed {
            property state {getter {log+='s';return s.state;}}
            property left {getter {log+='l';r.random32();throw 77;}}
            property next {getter {log+='n';return s.next;}}
        }
        var caught=0;try{r.randomize(new Seed());}catch(e){caught=(typeof e=='Object' && e.message=='invalid RandomGenerator state');}
        b.random32(); caught*100+(log=='sl')*10+(r.random32()===b.random32());
    "#,
        111,
    );
    check(
        r#"
        var r=new Math.RandomGenerator(5), b=new Math.RandomGenerator(5), s=r.serialize(), n=0;
        var missing=%[state:s.state,left:s.left];
        try{r.randomize(missing);}catch(e){n++;}
        try{r.randomize(null);}catch(e){n++;}
        try{r.randomize(%[state:s.state,left:0,next:0]);}catch(e){n++;}
        try{r.randomize(%[state:s.state,left:625,next:1]);}catch(e){n++;}
        n*10+(r.random32()===b.random32());
    "#,
        41,
    );
}

#[test]
fn date_normalization_failure_commit_and_parse_preservation() {
    check(
        r#"
        var d=new Date();d.setTime(0x7fffffffffffffff);
        d.getYear();d.getMonth();d.getDate();d.getDay();d.getHours();d.getMinutes();d.getSeconds();
        var caught=0;try{var year=d.getYear();}catch(e){caught=1;}
        caught*10+(d.getTime()===9223372036854775000);
    "#,
        11,
    );
    check(
        r#"
        var d=new Date(2024,0,1000001,-24000000,0,0);
        (d.getYear()==2024 && d.getMonth()==0 && d.getDate()==1);
    "#,
        1,
    );
    check(
        r#"
        var d=new Date(2024,0,1), old=d.getTime(), n=0;
        try{d.setMonth(null);}catch(e){n+=(d.getTime()==old);}
        try{d.parse('bad');}catch(e){n+=10*(d.getTime()==old);}
        try{d.setYear(2147483647);}catch(e){n+=100*(d.getTime()==-1000);}
        d.setTime(-1999);n+(d.getTime()==-1000)*1000;
    "#,
        1111,
    );
    check(
        r#"
        var d=new Date(), n=0;
        try{d.parse('2024/2147483648/1 0:0 GMT');}catch(e){n++;}
        try{d.parse(null);}catch(e){n+=10;}
        d.parse('1 Jan 1970 0:0 GMT (comment)');
        n+(d.getTime()==0)*100;
    "#,
        111,
    );
}

struct SeedBytes {
    held: Value,
    fills: std::rc::Rc<std::cell::Cell<u8>>,
}
impl tjs::Trace for SeedBytes {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(self.held);
    }
}
impl tjs::random::SeedSource for SeedBytes {
    fn fill_128(&mut self, bytes: &mut [u8; 16]) {
        let start = self.fills.get() * 16;
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = start + i as u8;
        }
        self.fills.set(self.fills.get() + 1);
    }
}
#[test]
fn host_seed_source_is_per_heap_traced_and_survives_reinstallation() {
    let mut heap = tjs::new_heap();
    let fills = std::rc::Rc::new(std::cell::Cell::new(0));
    let held = heap.alloc_string(vec![0xd800]);
    tjs::random::set_seed_source(
        &mut heap,
        SeedBytes {
            held: Value::Str(held),
            fills: fills.clone(),
        },
    )
    .unwrap();
    tjs::install_builtins(&mut heap).unwrap();
    heap.collect([]);
    assert_eq!(heap.string(held).unwrap(), &[0xd800]);
    let global = heap.alloc_global();
    let actual = run(
        &mut heap,
        global,
        "new Math.RandomGenerator().random32();",
        1,
    );
    assert_eq!(fills.get(), 2);
    let keys = (0u8..32).map(|byte| u32::from_le_bytes([byte, byte, 1, byte]));
    assert_eq!(
        actual,
        i64::from(rand_mt::Mt::new_with_key(keys).next_u32())
    );
    tjs::random::clear_seed_source(&mut heap).unwrap();
    heap.collect([]);
    assert!(heap.string(held).is_err());
}

#[test]
fn cancelling_a_restoration_does_not_commit_partial_state() {
    let mut heap = tjs::new_heap();
    gate::install(&mut heap).unwrap();
    let baseline = heap.collect([]).after;
    let global = heap.alloc_global();
    let root = heap.root(Value::Obj(global.into()));
    run(
        &mut heap,
        global,
        r#"
        var r=new Math.RandomGenerator(9), b=new Math.RandomGenerator(9),s=r.serialize();
        class Seed {property state {getter{StateGate.wait();return s.state;}}}
        1;
    "#,
        1,
    );
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8("cancel-random", "r.randomize(new Seed());")
        .unwrap();
    let module = tjs_front::compile(&sources, source).unwrap();
    let mut vm = Vm::with_global(&module, global);
    loop {
        let exit = vm.run_slice(&mut heap, RunBudget::new(1).unwrap());
        heap.collect(vm.roots());
        match exit {
            VmExit::Yielded => assert!(vm.work_executed() < 10000),
            VmExit::Waiting(_) => break,
            other => panic!("{other:?}"),
        }
    }
    drop(vm);
    heap.collect([]);
    assert_eq!(run(&mut heap, global, "r.random64()===b.random64();", 1), 1);
    heap.release_root(root).unwrap();
    assert_eq!(heap.collect([]).after, baseline);
}
