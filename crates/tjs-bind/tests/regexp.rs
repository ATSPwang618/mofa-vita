use tjs_core::{Heap, RunBudget, SourceMap, Value, Vm, VmExit};

#[test]
fn inline_and_spilled_captures_keep_unmatched_groups_and_utf16_offsets() {
    check(
        "var r=new RegExp('(a)(b)?(c)'); r.match('ac').join(':');",
        "ac:a::c",
    );
    check(
        "var r=new RegExp('(a)(b)?(c)(d)(e)(f)(g)'); r.match('acdefg').join(':');",
        "acdefg:a::c:d:e:f:g",
    );
    check(
        "var r=new RegExp('(日)(本)?(語)(a)(b)'); r.test('😀日語ab'); r.index+':'+r.lastIndex+':'+r.matches.join(':');",
        "4:6:日語ab:日::語:a:b",
    );
}

fn compile(script: &str) -> tjs_core::Module {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("regexp", script).unwrap();
    tjs_front::compile(&sources, source).unwrap_or_else(|error| panic!("{script}: {error}"))
}

fn run(vm: &mut Vm, heap: &mut Heap, slice: u32) -> VmExit {
    loop {
        let exit = vm.run_slice(heap, RunBudget::new(slice).unwrap());
        heap.collect(vm.roots());
        if !matches!(exit, VmExit::Yielded) {
            return exit;
        }
        assert!(vm.work_executed() < 100_000);
    }
}

fn check(script: &str, expected: &str) {
    let module = compile(script);
    for slice in [1, 10000] {
        let mut heap = tjs_bind::new_heap();
        gate::install(&mut heap).unwrap();
        let mut vm = Vm::new(&module);
        let exit = run(&mut vm, &mut heap, slice);
        let VmExit::Finished(value) = exit else {
            panic!("{script}: {exit:?}");
        };
        assert_eq!(heap.display(value).unwrap(), expected, "{script}");
    }
}

#[tjs_bind::class(name = "RegExpGate")]
mod gate {
    #[derive(Default, tjs_bind::Trace)]
    pub struct State;
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self
        }
        #[tjs::method]
        fn unit(cx: &mut tjs_bind::NativeCx<'_>, unit: i64) -> tjs_bind::Value {
            tjs_bind::Value::Str(cx.heap_mut().alloc_string(vec![unit as u16]))
        }
        /// Supply raw host data without relying on script concatenation.
        #[tjs::method]
        fn text(
            cx: &mut tjs_bind::NativeCx<'_>,
            value: tjs_bind::Value,
        ) -> tjs_bind::NativeResult<tjs_bind::Value> {
            let mut units = tjs_core::value::to_string_units(cx.heap(), value)?;
            for unit in &mut units {
                if *unit == 94 {
                    *unit = 0;
                }
            }
            Ok(tjs_bind::Value::Str(cx.heap_mut().alloc_string(units)))
        }
        #[tjs::method(resumable = true)]
        fn wait() -> tjs_bind::NativeStep {
            tjs_bind::NativeStep::Wait {
                request: tjs_bind::WaitRequest {
                    mode: tjs_bind::WaitMode::Internal,
                    token: 65,
                },
                continuation: Box::new(Resume),
            }
        }
    }
    #[derive(tjs_bind::Trace)]
    struct Resume;
    impl tjs_bind::NativeContinuation for Resume {
        fn resume(
            self: Box<Self>,
            _: &mut tjs_bind::NativeCx<'_>,
            value: tjs_bind::Value,
        ) -> tjs_bind::NativeResult<tjs_bind::NativeStep> {
            Ok(tjs_bind::NativeStep::Return(value))
        }
    }
}

#[test]
fn compile_failure_discard_and_literal_protocol_preserve_the_reference_order() {
    for (script, expected) in [
        (
            r#"var r=new RegExp(), n=0; r.match(<%00%>); try{var a=r.match(<%00%>);}catch(e){n++;} try{r.match();}catch(e){n++;} n;"#,
            "2",
        ),
        (
            r#"var r=/a/, n=0; try{r._compile('///b',1);}catch(e){n++;} n*10+r.test('a');"#,
            "11",
        ),
        (
            r#"var r=/a/, n=0; try{r.compile('b',<%00%>);}catch(e){n++;} n*10+r.test('a');"#,
            "11",
        ),
        (
            r#"var r=/a/, n=0; try{r.compile('(');}catch(e){n++;} try{r.test('a');}catch(e){n++;} n;"#,
            "2",
        ),
        (
            r#"var r=/a/, n=0; try{r._compile(RegExpGate.text('//g^/b'));}catch(e){n++;} n*10+r.test('a');"#,
            "11",
        ),
        (
            r#"var r=new RegExp('a',RegExpGate.text('i^g')); r.test('A'); r.start;"#,
            "0",
        ),
        (
            r#"new RegExp('a',RegExpGate.unit(55296)+'i').test('A');"#,
            "1",
        ),
        (
            r#"var r=new RegExp(); r.test('')*100+r.exec('').count*10+r.match('').count;"#,
            "0",
        ),
        (
            r#"var r=/a/; r.exec(RegExpGate.text('a^b')); r.input.length*10+r.rightContext.length;"#,
            "30",
        ),
        (
            r#"var r=/a(?=(bc))/g; r.exec('xabcZ'); r.index*1000+r.lastIndex*100+r.start*10+(r.lastParen=='bc');"#,
            "2441",
        ),
        (r#"var r=/^a/; r.start=1; r.test('xa');"#, "1"),
        (r#"var r=/(?<=x)a/; r.start=1; r.test('xa');"#, "0"),
    ] {
        check(script, expected);
    }
}

#[test]
fn replacement_observes_live_compilation_start_and_raw_utf16_results() {
    for (script, expected) in [
        (
            r#"var r=/a/g,n=0; var s=r.replace('abc',function(a){n++;r.compile('b');return 'X';});s+n;"#,
            "XXc2",
        ),
        (
            r#"var r=/a/,n=0; var s=r.replace('aaa',function(a){n++;r.compile('a','g');return 'X';});s+n;"#,
            "Xaa1",
        ),
        (
            r#"var r=/a|b/g,n=0; r.replace('abcd',function(a){n++;r.start=1;if(n==2)r.compile('z');return a[0];});"#,
            "accd",
        ),
        (
            r#"var r=/x/;r.start=1;r.replace('😀xy',function(a){return a[0];});"#,
            "😀yy",
        ),
        (
            r#"var r=/a/;r.start=1;#r.replace('a😀',function(a){return a[0];});"#,
            "55357",
        ),
        (r#"#(/a/.replace('a',RegExpGate.unit(55296)));"#, "55296"),
        (
            r#"#(/a/.replace('a',function(a){return RegExpGate.unit(56320);}));"#,
            "56320",
        ),
        (
            r#"var r=/a/g,n=0;try{r.replace('aa',function(a){n++;try{r.compile('(');}catch(e){}return 'x';});}catch(e){n+=10;}n;"#,
            "11",
        ),
        (
            r#"var r=/a/g,n=0;r.replace('aa',function(a){n++;return 'x';});n*10+(r.matches===void);"#,
            "21",
        ),
        (
            r#"var r=/a/; r.exec('a'); var old=r.matches; r.replace('a','b');r.split('a'); r.matches===old;"#,
            "1",
        ),
        (r#"/a*/g.replace('baa','X');"#, "bX"),
        (r#"/a*?/g.replace('aaa','X');"#, "XXX"),
    ] {
        check(script, expected);
    }
}

#[test]
fn split_covers_character_sets_nul_fallback_and_group_zero_only() {
    for (script, expected) in [
        (r#"var a=[9];a.split(null,'甲乙');a.join('|');"#, "甲乙"),
        (r#"var p=%[],a=[];a.split(p,'甲乙');a.join('|');"#, "甲乙"),
        (
            r#"var r=/,/, other=%[]; var a=[];a.split(r incontextof other,'a,b');a.join('|');"#,
            "a|b",
        ),
        (
            r#"var nul=RegExpGate.unit(0),a=[];a.split(',', RegExpGate.text('a,b^,c'));a.join('|');"#,
            "a|b",
        ),
        (
            r#"var nul=RegExpGate.unit(0),a=[];a.split(RegExpGate.text(',^;'), 'a;b,c');a.join('|');"#,
            "a;b|c",
        ),
        (
            r#"var nul=RegExpGate.unit(0),a=/,/.split(RegExpGate.text('a^,b'));a.count*100+a[0].length*10+a[1].length;"#,
            "221",
        ),
        (
            r#"var r=/,/,nul=RegExpGate.unit(0);r.compile(nul);r.split(RegExpGate.text('a^b')).join('|');"#,
            "a|b",
        ),
        (
            r#"/()/.split('',void,true).count*10+''.split('',void,true).count;"#,
            "10",
        ),
        (r#"/(,)(?=(b))/.split('a,bc').join('|');"#, "a|bc"),
        (
            r#"var r=/^a/g;r.start=99;var a=r.split('aaa');a.count*100+r.start;"#,
            "499",
        ),
        (
            r#"var a=[1,2];try{a.split(',',<%00%>);}catch(e){}a.count;"#,
            "0",
        ),
        (
            r#"var a=[];a.split(RegExpGate.unit(55296),'x'+RegExpGate.unit(55296)+'y');a.join('|');"#,
            "x|y",
        ),
        (r#"'x9yAy'.split('0123456789ABCDEF').join('|');"#, "x|y|y"),
    ] {
        check(script, expected);
    }
}

#[test]
fn long_replace_and_split_resume_without_changing_results() {
    check(
        r#"var input='a,'.repeat(1500);var a=input.split(/,/),b=input.split(','); var c=/a/g.replace(input,'XY'); a.count==1501 && b.count==a.count && a[1499]=='a' && a[1500]=='' && c.length==4500 && c.substr(4497)=='XY,';"#,
        "1",
    );
    check(
        r#"var r=/a/g,n=0;var s=r.replace('a'.repeat(130),function(a){n++;if(n==64)r.compile('b');return 'X';});n==64 && s.substr(0,64)=='X'.repeat(64) && s.substr(64)=='a'.repeat(66);"#,
        "1",
    );
}

#[test]
fn owned_string_conversion_preserves_nul_while_pointer_apis_stop_there() {
    check(
        r#"var r=/./;r.exec(RegExpGate.text('^b'));r.input.length*100+r.lastIndex*10+r.matches[0].length;"#,
        "210",
    );
    check(
        r#"var a=/,/.split(RegExpGate.text('^a,b'));a.count*100+a[0].length*10+a[1].length;"#,
        "201",
    );
    check(r#"/b/.replace(RegExpGate.text('a^bx'),'Y');"#, "aYx");
    check(r#"/z/.replace(RegExpGate.text('a^bx'),'Y');"#, "a");
    check(
        r#"var nul=RegExpGate.unit(0),s=RegExpGate.text('a^b'); var a=[];a.split(/,/,RegExpGate.text('a^b,c'));a.count*100+a[0].length*10+a[1].length;"#,
        "231",
    );
    check(
        r#"var nul=RegExpGate.unit(0),s=RegExpGate.text('a^b'); var a=s.split(/,/);a.count*10+a[0].length;"#,
        "13",
    );
    check(
        r#"var nul=RegExpGate.unit(0),s=RegExpGate.text('x^y'); var literal=/a/.replace('a',s),callback=/a/.replace('a',function(a){return s;});literal.length*10+callback.length;"#,
        "11",
    );
    check(
        r#"var nul=RegExpGate.unit(0),s=RegExpGate.text('a^b');[s,s].join(nul).length;"#,
        "2",
    );
    check(
        r#"var nul=RegExpGate.unit(0),s=RegExpGate.text('a^b');s.indexOf('b')*100+s.indexOf('b',2)*10+('%s'.sprintf(s)).length;"#,
        "-79",
    );
    check(
        r#"var nul=RegExpGate.unit(0),s=RegExpGate.text('a^b');[s].pack('a*').unpack('H*')[0];"#,
        "610000",
    );
    check(
        r#"var nul=RegExpGate.unit(0),s=RegExpGate.text('a^b');'%5sZ'.sprintf(s);"#,
        "  a",
    );
    check(
        r#"var n=0;try{var s='%s%d'.sprintf(RegExpGate.text('a^b'),null);}catch(e){n=1;}n;"#,
        "1",
    );
}

#[test]
fn waiting_callbacks_keep_owner_and_captures_alive_and_cancel_cleanly() {
    let module = compile(
        r#"var r=/a/g; var s=r.replace('abc',function(a){RegExpGate.wait();r.compile('b');return a[0]+a[0];});s;"#,
    );
    for cancel in [false, true] {
        let mut heap = tjs_bind::new_heap();
        gate::install(&mut heap).unwrap();
        let baseline = heap.collect([]).after;
        let mut vm = Vm::new(&module);
        let mut waits = 0;
        loop {
            let exit = vm.run_slice(&mut heap, RunBudget::new(1).unwrap());
            heap.collect(vm.roots());
            match exit {
                VmExit::Yielded => assert!(vm.work_executed() < 100000),
                VmExit::Waiting(request) => {
                    assert_eq!(request.token, 65);
                    waits += 1;
                    if cancel {
                        break;
                    }
                    vm.resume_wait(Ok(Value::Void)).unwrap();
                }
                VmExit::Finished(value) => {
                    assert_eq!(heap.display(value).unwrap(), "aabbc");
                    assert_eq!(waits, 2);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        drop(vm);
        assert_eq!(heap.collect([]).after, baseline);
    }
}

#[test]
fn split_publishes_bounded_batches_and_cancellation_releases_buffers() {
    for pattern in ["/,/", "','"] {
        let module = compile(&format!(
            "var a=[];a.split({pattern},'x,'.repeat(2000));a.count;"
        ));
        let mut heap = tjs_bind::new_heap();
        let baseline = heap.collect([]).after;
        let mut vm = Vm::new(&module);
        loop {
            let exit = vm.run_slice(&mut heap, RunBudget::new(1).unwrap());
            heap.collect(vm.roots());
            let name = heap.intern(&[97]);
            if let Some(global) = vm.global()
                && let Some(Value::Obj(array)) = heap.member(global, name).unwrap()
            {
                let count = heap.array(array.object.unwrap()).unwrap().len();
                if count > 0 {
                    assert!(count < 2001, "split completed without a budget boundary");
                    assert!(matches!(exit, VmExit::Yielded));
                    break;
                }
            }
            assert!(matches!(exit, VmExit::Yielded));
            assert!(vm.work_executed() < 100000);
        }
        vm.reset();
        drop(vm);
        assert_eq!(heap.collect([]).after, baseline);
    }
}

#[test]
fn literals_follow_grammar_and_use_the_global_constructor_protocol() {
    for (script, expected) in [
        (r#"/=/.test("=");"#, "1"),
        (r#"var n=8; n/=2; n / /x/.match("x").count;"#, "4"),
        (r#"if(1) /[a"'}]/.test("a"); RegExp.last.lastMatch;"#, "a"),
        (r#"function f() {} /a/.test("a");"#, "1"),
        (r#"var f=function() { return 6; }; f()/2;"#, "3"),
        (r#"/\/\*/.test("/*");"#, "1"),
        (r#"/\x{3042}/.test("あ");"#, "1"),
        (r#"/\//.test("/");"#, "1"),
        (r#"/a/iglz.test("A");"#, "1"),
        (r#"@"match=${/["}]/.test('}')}";"#, "match=1"),
        (
            r#"function f() { var RegExp=0; return /a/.test("a"); } f();"#,
            "1",
        ),
        (r#"function f() { return /x/; } f() === f();"#, "0"),
        (
            r#"class RegExp { var encoded; function _compile(p) { encoded=p; } } /a+/gi.encoded;"#,
            "//gi/a+",
        ),
        (
            r#"var n=0; class RegExp { function RegExp() { ++n; } } function f() { /x/; } f(); n;"#,
            "0",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn captures_flags_search_state_and_utf16_offsets_follow_tjs() {
    for (script, expected) in [
        (r#"/(\w+)-\1/.match("ok-ok")[1];"#, "ok"),
        (r#"/(?<=x)a(?=b)/.match("xab")[0];"#, "a"),
        (r#"/(a)?(b)/.match("b")[1] === "";"#, "1"),
        (r#"/a*/.match("baa")[0];"#, "aa"),
        (r#"new RegExp("").test("abc");"#, "0"),
        (
            r#"var r=/a/g; var a=r.match("aba"); r.start*10+(r.matches===void);"#,
            "1",
        ),
        (
            r#"var r=/a/g; r.exec("aba"); r.exec("aba"); r.index*100+r.lastIndex*10+r.start;"#,
            "333",
        ),
        (
            r#"var r=/a/g; r.exec("aba"); r.exec("aba"); r.test("aba"); r.start*100+r.index*10+r.matches.count;"#,
            "330",
        ),
        (
            r#"var r=/(a)(b)/; r.exec("😀xab!"); r.index*100+r.lastIndex*10+(r.lastParen=="b");"#,
            "651",
        ),
        (
            r#"var r=/(a)/; r.start=3; r.exec("😀xa!"); r.lastMatch+r.lastParen+r.leftContext+r.rightContext;"#,
            "aa😀x!",
        ),
        (
            r#"var r=/a/; r.exec("ab"); r.test("x"); r.rightContext;"#,
            "b",
        ),
        (r#"var r=/a/; r.test("a"); RegExp.last === r;"#, "1"),
        (
            r#"var r=/a/g; r.start=1; r.compile("b","i"); r.exec("xB")[0];"#,
            "B",
        ),
        (
            r#"var r=/a/; r.start=-1; r.test("a")*10+r.index;"#,
            "4294967295",
        ),
        (r#"var r=/a/; r.input === "" && r.lastMatch === "";"#, "1"),
    ] {
        check(script, expected);
    }
}

#[test]
fn replacement_and_split_support_strings_regex_and_callback_suspension() {
    for (script, expected) in [
        (r#"/a/.replace("aba","x");"#, "xba"),
        (r#"/a/g.replace("aba","$1");"#, "$1b$1"),
        (r#""aba".replace(/a/g,"x");"#, "xbx"),
        (
            r#"var n=0; try { "aba".replace("a","x"); } catch(e) {n=1;} n;"#,
            "1",
        ),
        (
            r#"var n=0; try { "abc".replace("","x"); } catch(e) {n=1;} n;"#,
            "1",
        ),
        (r#"var a="a,b;c".split(",;"); a[0]+a[1]+a[2];"#, "abc"),
        (
            r#"var a="a\r\nb\n".split(/\r\n|\r|\n/); a.count*10+a[2].length;"#,
            "30",
        ),
        (r#"var a=/([,:])/.split(",a::b,"); a.count;"#, "5"),
        (
            r#"var a=/[,]/.split(",a,,",void,true); a.count*10+a[0].length;"#,
            "11",
        ),
        (
            r#"var a=[1,2,3]; a.split(/,/,",a,",void,true); a.count*10+a[0].length;"#,
            "11",
        ),
        (
            r#"var r=/^a/g; r.start=9; r.replace("aaa","b")+r.start;"#,
            "bbb9",
        ),
        (
            r#"var r=/(\w)/g; r.replace("ab",function(a) { return "["+a[1]+"]"; });"#,
            "[a][b]",
        ),
        (
            r#"var r=/a/g; r.note="!"; r.replace("aba",function(a) { return this.note; });"#,
            "!b!",
        ),
        (
            r#"var r=/a/g; var d=%[note:"?"]; r.replace("aba",function(a) { return this.note; } incontextof d);"#,
            "?b?",
        ),
        (
            r#""ab".replace(/./g,function(a) { return /./.replace(a[0],function(b) { return b[0]+b[0]; }); });"#,
            "aabb",
        ),
        (
            r#"var n=0; try { /./g.replace("ab",function(a) { ++n; throw 7; }); } catch(e) { n+=e; } n;"#,
            "8",
        ),
        (
            r#"var r=/a/; r.replace("a",function(a) { invalidate this; return "x"; });"#,
            "x",
        ),
        (r#"/./g.replace("ab", [2,3].pop);"#, "32"),
        (
            r#"var r=/a/g; r.replace("aa",function(a) { r.compile("b"); return "x"; });"#,
            "xa",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn last_search_and_suspended_replacements_are_gc_roots_and_callbacks_unwind_cleanly() {
    let mut heap = tjs_bind::new_heap();
    let baseline = heap.collect([]).after;
    let module = compile(r#"try { /./g.replace("ab",function(a) { throw a; }); } catch(e) {}"#);
    let mut vm = Vm::new(&module);
    assert!(matches!(run(&mut vm, &mut heap, 1), VmExit::Finished(_)));
    drop(vm);
    assert_eq!(heap.collect([]).after, baseline);

    let module = compile(r#"var r=/a/; r.exec("a"); delete r; RegExp.last.matches[0];"#);
    let mut vm = Vm::new(&module);
    assert!(matches!(
        run(&mut vm, &mut heap, 1),
        VmExit::Finished(Value::Str(_))
    ));
    drop(vm);
    heap.collect([]);
    // The class registry deliberately retains the last tested expression.
    tjs_bind::install_builtins(&mut heap).unwrap();
    let module = compile("RegExp.last.matches[0];");
    let mut vm = Vm::new(&module);
    let VmExit::Finished(value) = run(&mut vm, &mut heap, 1) else {
        panic!("last lost");
    };
    assert_eq!(heap.display(value).unwrap(), "a");
}

#[test]
fn invalid_patterns_and_text_fail_with_source_diagnostics() {
    for (script, message) in [
        (r#"/(/;"#, "RegExp"),
        (r#"new RegExp().test("a");"#, "not been compiled"),
        (r#"/./.test('\xD800');"#, "well-formed UTF-16"),
        (r#"var r=/./; r.start=1; r.test("😀");"#, "surrogate pair"),
        (r#"RegExp.last = 1;"#, "read-only"),
    ] {
        let module = compile(script);
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&module);
        let VmExit::Fault(error) = run(&mut vm, &mut heap, 1) else {
            panic!("{script}");
        };
        assert!(error.message.contains(message), "{script}: {error}");
        assert!(error.span.is_some());
    }
}
