use tjs_core::{RunBudget, SourceMap, Vm, VmExit};

fn compile(script: &str) -> Result<tjs_core::Module, tjs_core::Diagnostic> {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("delete names", script).unwrap();
    tjs_front::compile(&sources, source)
}

#[test]
fn delete_removes_local_names_at_compile_time_and_other_names_at_runtime() {
    for (script, expected) in [
        ("var x=1; (delete x)*10 + (delete x);", 10),
        ("function f() { var x=1; return delete x; } f();", 1),
        (
            "var x=8; function f() { var x=1; if(0) delete x; return x; } f();",
            8,
        ),
        (
            "function f() { var x=3; { var x=1; delete x; return x; } } f();",
            3,
        ),
        ("function f(x) { delete x; var x=9; return x; } f(2);", 9),
        (
            "function f() { var x=1; delete x; var x=7; return x; } f();",
            7,
        ),
        (
            "var x=5; class C { var x=8; function drop() { return delete x; } } var c=new C; c.drop()*10+x;",
            15,
        ),
        (
            "var x=5; class C { function drop() { return delete x; } } var c=new C; c.drop()*10+(delete x);",
            10,
        ),
        ("var a=1,b=2; delete (1 ? a : b); b*10+(delete a);", 20),
        (
            "var d=%[x:1], n=0; delete (++n,d.x); n*10+(delete d.x);",
            10,
        ),
    ] {
        let module = compile(script).unwrap();
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&module);
        loop {
            let exit = vm.run_slice(&mut heap, RunBudget::new(1).unwrap());
            heap.collect(vm.roots());
            match exit {
                VmExit::Yielded => {}
                VmExit::Finished(value) => {
                    assert_eq!(value.as_integer(), Some(expected), "{script}");
                    break;
                }
                exit => panic!("{script}: {exit:?}"),
            }
        }
    }
    let error = compile("delete 1;").unwrap_err();
    assert_eq!(error.phase, tjs_core::Phase::Compile);
    assert!(error.span.is_some());
}
