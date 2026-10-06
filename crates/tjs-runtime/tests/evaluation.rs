use tjs_core::{Module, RunBudget, Value, Vm, VmExit, VmLimits};
use tjs_runtime::{Runtime, RuntimeExit};

fn compile(runtime: &mut Runtime, text: &str) -> Module {
    let source = runtime.sources.add_utf8("test.tjs", text).unwrap();
    tjs_front::compile_with_preprocessor(&runtime.sources, source, &mut runtime.preprocessor)
        .unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn run(runtime: &mut Runtime, vm: &mut Vm, slice: u32) -> Value {
    for _ in 0..100_000 {
        match runtime.run_slice(vm, RunBudget::new(slice).unwrap()) {
            RuntimeExit::Finished(value) => return value,
            RuntimeExit::Yielded => {
                runtime.collect(vm.roots());
            }
            exit => panic!("unexpected exit: {exit:?}"),
        }
    }
    panic!("execution did not finish");
}

#[test]
fn original_value_and_discard_modes_context_and_callbacks() {
    for (text, expected) in [
        (r#"var r = "2+3"! * 2; r;"#, 10),
        (r#"var r = "'7'"!!; r;"#, 7),
        (r#"var r = (40+2)!; r;"#, 42),
        (
            r#"'class A { var x=1; function A(n) {x=n;} } class B extends A { function B(n) {super.A(n+1);} }'!; (new B(41)).x;"#,
            42,
        ),
        (
            r#"var a = void!, b = ""!; (a === void) && (b === void);"#,
            1,
        ),
        (r#"var r = '3\0+99'!; r;"#, 3),
        (r#"var x=0; 'var y=6; x=y; x+=2'!; x;"#, 8),
        (r#"var x=0; var r='x=4; x=99'!; r*10+x;"#, 44),
        (r#"var r='f(); function f(){return 4;}'!; r;"#, 4),
        (r#"var a=2,b=5; 'a <-> b'!; a*10+b;"#, 52),
        (r#"var x=7; function f(){var x=100; return 'x'!;} f();"#, 7),
        (
            r#"var fallback=9;
            class C {
                var x=3;
                function read(){var x=99; return 'x+fallback'!;}
                function install(){
                    'var y=2; function f(){return x+y;}'!;
                    return this.f;
                }
            }
            var c=new C(), f=c.install(); c.read()*10+f();"#,
            125,
        ),
        (r#"var r="this"!; r === global;"#, 1),
        (
            r#"var reads=0;
            property expression { getter {reads++; return '3+4';} }
            var r=expression!; reads*10+r;"#,
            17,
        ),
        (
            r#""abc".replace(/b/, function(m){return "'X'"!;}) == "aXc";"#,
            1,
        ),
        (
            r#"var n=0; var f='function(){return "\'n+=1\'"!!;}'!; f(); n;"#,
            1,
        ),
        (
            r#"'@set(eval_flag=7) var pp=1;'!;
            '@if(eval_flag==7) pp+=4; @endif'!; pp;"#,
            5,
        ),
    ] {
        for slice in [1, 10_000] {
            let mut runtime = Runtime::new();
            let module = compile(&mut runtime, text);
            let mut vm = Vm::new(&module);
            let value = run(&mut runtime, &mut vm, slice);
            assert_eq!(value.as_integer(), Some(expected), "{text}; slice={slice}");
        }
    }
}

#[test]
fn compile_and_runtime_exceptions_share_the_caller_stack() {
    for text in [
        r#"var caught=0; try {var r='1+'!;} catch(e) {
            caught=e.message.indexOf('expected')>=0 && e.message.indexOf('<eval')>=0;
        } caught;"#,
        r#"var caught=0; try {var r='function(){throw 41;}()'!;} catch(e) {
            caught=(e==41);
        } caught;"#,
        r#"var caught=0; try {'throw 41'!;} catch(e) {caught=(e==41);} caught;"#,
        r#"var caught=0; try {var r='null.x'!;} catch(e) {caught=1;} caught;"#,
        r#"var caught=0; try {var r='a <-> b'!;} catch(e) {caught=1;} caught;"#,
        r#"var caught=0; try {var r='1// no terminator'!;} catch(e) {caught=1;} caught;"#,
    ] {
        for slice in [1, 10_000] {
            let mut runtime = Runtime::new();
            let module = compile(&mut runtime, text);
            let mut vm = Vm::new(&module);
            assert_eq!(
                run(&mut runtime, &mut vm, slice).as_integer(),
                Some(1),
                "{text}"
            );
        }
    }

    let mut runtime = Runtime::new();
    let module = compile(
        &mut runtime,
        "function f(){return 'f()'!;} try{f();}catch(e){1;}",
    );
    let mut vm = Vm::with_limits(
        &module,
        VmLimits {
            max_call_depth: 8,
            max_stack_values: 1024,
        },
    )
    .unwrap();
    assert_eq!(run(&mut runtime, &mut vm, 1).as_integer(), Some(1));
}

#[test]
fn requests_suspend_once_and_reset_cancels_pending_compilation() {
    let mut runtime = Runtime::new();
    let module = compile(&mut runtime, "var n=0; var r=(n+=1,'2+3')!; n*10+r;");
    let mut vm = Vm::new(&module);
    let request = loop {
        match vm.run_slice(&mut runtime.heap, RunBudget::new(1).unwrap()) {
            VmExit::Yielded => {}
            VmExit::CompileRequest(request) => break request,
            exit => panic!("{exit:?}"),
        }
        runtime.collect(vm.roots());
    };
    let work = vm.work_executed();
    assert!(request.result_needed);
    assert!(matches!(
        vm.run_slice(&mut runtime.heap, RunBudget::new(100).unwrap()),
        VmExit::CompileRequest(_)
    ));
    assert_eq!(vm.work_executed(), work);
    runtime.collect(vm.roots());
    assert_eq!(run(&mut runtime, &mut vm, 1).as_integer(), Some(15));
    assert!(
        vm.resume_compile(&mut runtime.heap, Ok(module.clone()))
            .is_err()
    );

    vm.reset();
    loop {
        if matches!(
            vm.run_slice(&mut runtime.heap, RunBudget::new(1).unwrap()),
            VmExit::CompileRequest(_)
        ) {
            break;
        }
    }
    vm.reset();
    assert!(vm.resume_compile(&mut runtime.heap, Ok(module)).is_err());
    assert_eq!(run(&mut runtime, &mut vm, 1).as_integer(), Some(15));
}

#[test]
fn escaped_code_and_sources_survive_the_originating_vm_then_are_reclaimed() {
    let mut runtime = Runtime::new();
    let module = compile(
        &mut runtime,
        r#"
        var f='function(x){return x+3;}'!, g='function(x){return x*2;}'!;
        f(g(4));
    "#,
    );
    let mut vm = Vm::new(&module);
    assert_eq!(run(&mut runtime, &mut vm, 1).as_integer(), Some(11));
    let global = vm.global().unwrap();
    let root = runtime.heap.root(Value::Obj(global.into()));
    drop(vm);
    runtime.collect([]);
    let module = compile(&mut runtime, "f(g(5));");
    let mut vm = Vm::with_global(&module, global);
    assert_eq!(run(&mut runtime, &mut vm, 1).as_integer(), Some(13));
    runtime.heap.release_root(root);
    drop(vm);
    runtime.collect([]);
}
