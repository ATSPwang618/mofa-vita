use std::{cell::RefCell, collections::HashMap, rc::Rc};
use tjs_core::{NativeError, NativeResult, RunBudget, Value, Vm, storage::Storage};
use tjs_runtime::{Runtime, RuntimeExit};

#[derive(Default)]
struct Output {
    muted: bool,
    timestamps: usize,
    console: Vec<String>,
    files: Vec<(String, String, bool)>,
}
struct Log(Rc<RefCell<Output>>);
impl krkr_engine::debug::LogOutput for Log {
    fn enabled(&self) -> bool {
        !self.0.borrow().muted
    }
    fn timestamp(&mut self) -> String {
        self.0.borrow_mut().timestamps += 1;
        "12:34:56".into()
    }
    fn console(&mut self, line: &[u16]) {
        self.0
            .borrow_mut()
            .console
            .push(String::from_utf16_lossy(line));
    }
    fn file_output(&mut self) -> Option<&mut dyn krkr_engine::debug::FileOutput> {
        Some(self)
    }
}
impl krkr_engine::debug::FileOutput for Log {
    fn normalize_directory(&mut self, path: &[u16]) -> NativeResult<Vec<u16>> {
        Ok(path.to_vec())
    }
    fn write_file(&mut self, directory: &[u16], text: &[u16], clear: bool) -> NativeResult<()> {
        self.0.borrow_mut().files.push((
            String::from_utf16_lossy(directory),
            String::from_utf16_lossy(text),
            clear,
        ));
        Ok(())
    }
}
struct Files(HashMap<String, Vec<u8>>);

#[test]
fn console_only_host_ignores_file_controls_without_duplicating_output() {
    struct Console(Rc<RefCell<Vec<String>>>);
    impl krkr_engine::debug::LogOutput for Console {
        fn timestamp(&mut self) -> String {
            "console".into()
        }
        fn console(&mut self, line: &[u16]) {
            self.0.borrow_mut().push(String::from_utf16_lossy(line));
        }
    }
    let lines = Rc::new(RefCell::new(Vec::new()));
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, Console(lines.clone())).unwrap();
    let (_, exit) = run(
        &mut runtime,
        r#"
        Debug.logLocation = "missing/console-only-directory";
        Debug.message("before");
        Debug.startLogToFile(true);
        Debug.logAsError();
        Debug.message("after");
        if (Debug.logLocation != "") throw "console host retained a file path";
        if (Debug.getLastLog().indexOf("after") < 0) throw "lost log history";
    "#,
    );
    assert!(matches!(exit, RuntimeExit::Finished(_)), "{exit:?}");
    assert_eq!(&*lines.borrow(), &["console before", "console after"]);
}

impl tjs_core::Trace for Files {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl Storage for Files {
    fn read_binary(&mut self, name: &[u16], _: &[u16]) -> NativeResult<Vec<u8>> {
        self.0
            .get(&String::from_utf16_lossy(name))
            .cloned()
            .ok_or(NativeError::Message("missing file"))
    }
    fn write_binary(&mut self, name: &[u16], _: &[u16], bytes: &[u8]) -> NativeResult<()> {
        self.0
            .insert(String::from_utf16_lossy(name), bytes.to_vec());
        Ok(())
    }
    fn read_text(&mut self, name: &[u16], mode: &[u16]) -> NativeResult<Vec<u16>> {
        Ok(String::from_utf8(self.read_binary(name, mode)?)
            .unwrap()
            .encode_utf16()
            .collect())
    }
    fn write_text(&mut self, name: &[u16], mode: &[u16], text: &[u16]) -> NativeResult<()> {
        self.write_binary(name, mode, String::from_utf16_lossy(text).as_bytes())
    }
}
fn setup() -> (Runtime, Rc<RefCell<Output>>) {
    let mut runtime = Runtime::new();
    let output = Rc::new(RefCell::new(Output::default()));
    krkr_engine::install(&mut runtime, Log(output.clone())).unwrap();
    (runtime, output)
}
fn run(runtime: &mut Runtime, script: &str) -> (Vm, RuntimeExit) {
    let source = runtime.sources.add_utf8("host classes", script).unwrap();
    let module = tjs_front::compile(&runtime.sources, source).unwrap();
    let mut vm = Vm::new(&module);
    for _ in 0..30000 {
        let exit = runtime.run_slice(&mut vm, RunBudget::new(1).unwrap());
        runtime.collect(vm.roots());
        if !matches!(exit, RuntimeExit::Yielded) {
            return (vm, exit);
        }
    }
    panic!("script did not finish");
}
fn check(runtime: &mut Runtime, script: &str, expected: &str) {
    let (_vm, exit) = run(runtime, script);
    let RuntimeExit::Finished(value) = exit else {
        panic!("{script}: {exit:?}");
    };
    assert_eq!(runtime.heap.display(value).unwrap(), expected, "{script}");
}

#[test]
fn scripts_share_stack_globals_context_and_catch_compile_and_execution_errors() {
    let (mut runtime, _) = setup();
    for (script, expected) in [
        (
            "class M { var seen=''; function missing(set,name,value) { seen+=name+','; if(!set) *value=7; return true; } } var m=new M; Scripts.setCallMissing(m); m[1.5]=3; m[null]; m.seen;",
            "1.5,(object 0x0000000000000000:0x0000000000000000),",
        ),
        (
            "class M { function missing(set,name,value) { if(name=='1.5') { *value=function(){return 9;}; return true; } return false; } } var m=new M; Scripts.setCallMissing(m); m[1.5]();",
            "9",
        ),
        (
            "class M { property p { getter(){return 42;} } function missing(set,name,value){ *value=&this.p; return true; } } var m=new M; Scripts.setCallMissing(m); m.answer;",
            "42",
        ),
        (
            "class M { var calls=0; function missing(set,name,value){ calls++; try {this.recursive;} catch(e) {} *value=42; return true; } } var m=new M; Scripts.setCallMissing(m); m.answer; m.calls;",
            "1",
        ),
        (
            "class M { function missing(set,name,value) { if(set) { this.last=*value; return true; } *value=42; return true; } } var m=new M; Scripts.setCallMissing(m); m.answer;",
            "42",
        ),
        (
            "class M { var last; function missing(set,name,value) { if(set) { last=*value; return true; } return false; } } var m=new M; Scripts.setCallMissing(m); m.answer=42; m.last;",
            "42",
        ),
        (
            "var d=%[]; var saved; d.missing=function(set,name,value){ global.saved=value; *value=42; return true; }; Scripts.setCallMissing(d); d.answer; *&saved;",
            "42",
        ),
        (
            "var d=%[]; d.missing=function(set,name,value){return false;}; Scripts.setCallMissing(d); d.answer=42; d.answer;",
            "42",
        ),
        (
            "class M { function missing(set,name,value){ *value=function(){return 42;}; return true; } } var m=new M; Scripts.setCallMissing(m); m.answer();",
            "42",
        ),
        (
            "class M { var n=0; function missing(set,name,value){ n++; throw 7; } } var m=new M; Scripts.setCallMissing(m); try {m.a;} catch(e) {} try {m.b;} catch(e) {} m.n;",
            "2",
        ),
        (
            "Scripts.textEncoding='UTF-8'; (Scripts.Scripts incontextof Scripts)(); (Debug.Debug incontextof Debug)(); Debug.message('still installed'); Scripts.textEncoding;",
            "UTF-8",
        ),
        ("Scripts.exec('var n=40;'); Scripts.eval('n+2');", "42"),
        (
            "class C { var n=3; function f() { var local=9; return Scripts.eval('n',void,void,this); } } (new C).f();",
            "3",
        ),
        (
            "var n=1; class C { var n=7; function f() { return Scripts.eval('n'); } } (new C).f();",
            "1",
        ),
        (
            "var d=%[]; Scripts.exec('var n=4; function f(){return n;}',void,void,d); d.f();",
            "4",
        ),
        (
            "var count=0; try { Scripts.exec('var =;', 'broken.tjs', 30); } catch(e) { count++; } try { Scripts.exec('throw 7;'); } catch(e) { count+=e; } count;",
            "8",
        ),
        (
            "Scripts.eval('function(){ return Scripts.eval(\"6*7\"); }()');",
            "42",
        ),
        (
            "Scripts.exec('var x=9;'); Scripts.exec('x+1;') === void;",
            "1",
        ),
        (
            "function inner(){ return Scripts.getTraceString(1); } inner().indexOf('[inner]') >= 0;",
            "1",
        ),
        ("Scripts.dump(); void;", "void"),
        (
            "class C extends Array {} Scripts.getClassNames(new C).join(',');",
            "C,Array",
        ),
        (
            "var n=0; try { new Scripts; } catch(e) { n++; } try { new Debug; } catch(e) { n++; } n;",
            "2",
        ),
    ] {
        check(&mut runtime, script, expected);
    }
    let (_vm, exit) = run(&mut runtime, "Scripts.exec('\nnull.x;', 'nested.tjs', 40);");
    let RuntimeExit::Fault(error) = exit else {
        panic!("{exit:?}")
    };
    let span = error.span.unwrap();
    let file = runtime.sources.get(span.source()).unwrap();
    assert_eq!(file.name(), "nested.tjs");
    assert_eq!(file.line_column(span.start()).unwrap().0, 42);
}

#[test]
fn storage_loading_preserves_script_dependencies_and_legacy_encoding() {
    let (mut runtime, _) = setup();
    let (jp, _, _) = encoding_rs::SHIFT_JIS.encode("var name='日本語';");
    runtime.heap.set_storage(Files(HashMap::from([
        (
            "main.tjs".into(),
            b"Scripts.execStorage('base.tjs'); var result=answer();".to_vec(),
        ),
        ("base.tjs".into(), b"function answer(){return 42;}".to_vec()),
        ("expression.tjs".into(), b"result+1".to_vec()),
        ("jp.tjs".into(), jp.into_owned()),
    ])));
    check(
        &mut runtime,
        "Scripts.execStorage('main.tjs'); Scripts.evalStorage('expression.tjs');",
        "43",
    );
    check(
        &mut runtime,
        "Scripts.textEncoding='shift_jis'; Scripts.execStorage('jp.tjs'); name;",
        "日本語",
    );
}

#[test]
fn compile_storage_emits_executable_code_without_running_or_reparsing_source() {
    let (mut runtime, _) = setup();
    runtime.heap.set_storage(Files(HashMap::from([
        ("unit.tjs".into(), b"global.ran++; class C { function f(){try {throw 42;} catch(e) {return e;}} } var result=(new C).f();".to_vec()),
        ("expr.tjs".into(), b"6*7".to_vec()),
        ("bad.tjs".into(), b"var =;".to_vec()),
    ])));
    check(
        &mut runtime,
        "var ran=0; Scripts.compileStorage('unit.tjs','unit.bin',false,true); ran;",
        "0",
    );
    let bytes = runtime
        .heap
        .storage()
        .unwrap()
        .read_binary(&"unit.bin".encode_utf16().collect::<Vec<_>>(), &[])
        .unwrap();
    assert!(bytes.starts_with(b"KRRSBC"));
    runtime
        .heap
        .storage()
        .unwrap()
        .write_binary(
            &"unit.tjs".encode_utf16().collect::<Vec<_>>(),
            &[],
            b"invalid source",
        )
        .unwrap();
    check(
        &mut runtime,
        "var ran=0; Scripts.execStorage('unit.bin'); ran+result;",
        "43",
    );
    check(
        &mut runtime,
        "Scripts.compileStorage('expr.tjs','expr.bin',true,false,true); Scripts.evalStorage('expr.bin');",
        "42",
    );
    check(
        &mut runtime,
        "var caught=0; try {Scripts.compileStorage('bad.tjs','bad.bin');} catch(e) {caught=1;} caught;",
        "1",
    );
    assert!(
        runtime
            .heap
            .storage()
            .unwrap()
            .read_binary(&"bad.bin".encode_utf16().collect::<Vec<_>>(), &[])
            .is_err()
    );
}

#[test]
fn compile_storage_preserves_preprocessor_guards_across_cached_and_fallback_loads() {
    let (mut runtime, _) = setup();
    runtime.heap.set_storage(Files(HashMap::from([(
        "guard.tjs".into(),
        b"@if(__unit_loaded==0)\n@set(__unit_loaded=1)\nglobal.ran++;\n@endif".to_vec(),
    )])));
    check(
        &mut runtime,
        "var ran=0; Scripts.compileStorage('guard.tjs','guard.bc'); ran;",
        "0",
    );
    assert_eq!(runtime.preprocessor.get("__unit_loaded"), 1);
    runtime.preprocessor.set("__unit_loaded", 0);
    check(
        &mut runtime,
        "var ran=0; Scripts.execStorage('guard.bc'); Scripts.execStorage('guard.bc'); Scripts.compileStorage('guard.bc','guard-copy.bc'); ran;",
        "1",
    );
    assert_eq!(runtime.preprocessor.get("__unit_loaded"), 1);
    runtime.preprocessor.set("__unit_loaded", 0);
    check(
        &mut runtime,
        "var ran=0; Scripts.execStorage('guard-copy.bc'); ran;",
        "1",
    );
    assert_eq!(runtime.preprocessor.get("__unit_loaded"), 1);
}

#[test]
fn log_handlers_resume_suppress_recursion_and_recover_after_exceptions() {
    let (mut runtime, output) = setup();
    check(
        &mut runtime,
        "
        var log='';
        function a(line) { log+='a'; Debug.message('nested'); Debug.removeLoggingHandler(b); }
        function b(line) { log+='b'; }
        Debug.addLoggingHandler(a); Debug.addLoggingHandler(a); Debug.addLoggingHandler(b);
        Debug.message('one',2);
        Debug.removeLoggingHandler(a);
        function fail(line) { throw 9; }
        Debug.addLoggingHandler(fail);
        try { Debug.notice('bad'); } catch(e) { log+=e; }
        Debug.message('after');
        log;",
        "a9",
    );
    assert_eq!(
        output.borrow().console,
        [
            "12:34:56 one, 2",
            "12:34:56 nested",
            "12:34:56 bad",
            "12:34:56 after"
        ]
    );
    check(&mut runtime, "Debug.getLastLog(1);", "12:34:56 after\r\n");
}

#[test]
fn muted_debug_skips_host_work_but_keeps_registered_script_handlers() {
    let (mut runtime, output) = setup();
    output.borrow_mut().muted = true;
    check(
        &mut runtime,
        "Debug.message('hidden'); Debug.notice('hidden'); Debug.startLogToFile(true); Debug.logAsError(); Debug.getLastLog()=='';",
        "1",
    );
    assert_eq!(output.borrow().timestamps, 0);
    assert!(output.borrow().console.is_empty());
    assert!(output.borrow().files.is_empty());
    check(
        &mut runtime,
        "var seen=''; Debug.addLoggingHandler(function(line){seen=line;}); Debug.message('observed'); seen;",
        "12:34:56 observed",
    );
    assert_eq!(output.borrow().timestamps, 1);
    assert!(output.borrow().console.is_empty());
    assert!(output.borrow().files.is_empty());
}

#[test]
fn file_logging_applies_location_clear_and_error_policy() {
    let (mut runtime, output) = setup();
    check(
        &mut runtime,
        "Debug.logLocation='logs'; Debug.message('before'); Debug.logToFileOnError=0; Debug.logAsError(); Debug.clearLogFileOnError=1; Debug.logToFileOnError=1; Debug.logAsError(); Debug.message('after'); Debug.startLogToFile(true); Debug.logLocation;",
        "logs",
    );
    let output = output.borrow();
    assert_eq!(output.files.len(), 2);
    assert_eq!(
        output.files[0],
        ("logs".into(), "12:34:56 before\r\n".into(), true)
    );
    assert_eq!(
        output.files[1],
        ("logs".into(), "12:34:56 after\r\n".into(), false)
    );
}
