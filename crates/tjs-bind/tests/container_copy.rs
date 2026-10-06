use tjs_bind as tjs;
use tjs_core::{RunBudget, SourceMap, Value, Vm, VmExit};

#[tjs::class(name = "CopyProbe")]
mod probe {
    use super::*;
    #[derive(Default, tjs::Trace)]
    pub struct State;
    fn object(value: Value) -> tjs::ObjId {
        let Value::Obj(reference) = value else {
            panic!("object")
        };
        reference.object.unwrap()
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self
        }
        #[tjs::method]
        fn missing(cx: &mut tjs::NativeCx<'_>, target: Value) -> tjs::NativeResult<()> {
            Ok(cx.heap_mut().set_call_missing(object(target))?)
        }
        #[tjs::method]
        fn define(
            cx: &mut tjs::NativeCx<'_>,
            target: Value,
            name: Value,
            value: Value,
            hidden: bool,
            static_slot: bool,
        ) -> tjs::NativeResult<()> {
            let Value::Str(name) = name else {
                panic!("name")
            };
            let name = cx.heap_mut().intern_string(name)?;
            cx.heap_mut()
                .set_member_flags(object(target), name, value, hidden, static_slot)?;
            Ok(())
        }
        #[tjs::method]
        fn static_slot(
            cx: &mut tjs::NativeCx<'_>,
            target: Value,
            name: Value,
        ) -> tjs::NativeResult<bool> {
            let Value::Str(name) = name else {
                panic!("name")
            };
            let name = cx.heap_mut().intern_string(name)?;
            Ok(cx
                .heap()
                .members_with_flags(object(target))?
                .any(|(key, _, flag)| key == name && flag))
        }
        #[tjs::method(resumable = true)]
        fn pause() -> tjs::NativeStep {
            tjs::NativeStep::Wait {
                request: tjs::WaitRequest {
                    mode: tjs::WaitMode::Internal,
                    token: 63,
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
            _: Value,
        ) -> tjs::NativeResult<tjs::NativeStep> {
            Ok(tjs::NativeStep::Return(Value::Void))
        }
    }
}

fn check(script: &str, expected: i64, expected_waits: usize) {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("container-copy", script).unwrap();
    let module = tjs_front::compile(&sources, source).unwrap_or_else(|e| panic!("{script}: {e}"));
    for slice in [1, 4096] {
        let mut heap = tjs::new_heap();
        probe::install(&mut heap).unwrap();
        let baseline = heap.collect([]).after;
        let mut vm = Vm::new(&module);
        let mut waits = 0;
        loop {
            let exit = vm.run_slice(&mut heap, RunBudget::new(slice).unwrap());
            heap.collect(vm.roots());
            match exit {
                VmExit::Yielded => assert!(vm.work_executed() < 200_000),
                VmExit::Waiting(request) => {
                    assert_eq!(request.token, 63);
                    waits += 1;
                    vm.resume_wait(Ok(Value::Void)).unwrap();
                }
                VmExit::Finished(value) => {
                    assert_eq!(value.as_integer(), Some(expected), "{script}");
                    break;
                }
                other => panic!("{script}: {other:?}"),
            }
        }
        assert_eq!(waits, expected_waits, "{script}");
        drop(vm);
        assert_eq!(heap.collect([]).after, baseline);
    }
}

#[test]
fn conversion_failure_clear_order_and_partial_writes() {
    for (script, expected) in [
        (
            "var a=[1]; try{a.assign(null);}catch(e){} var n=a.count; a.push(2); try{a.assign(7);}catch(e){} n*10+a.count;",
            0,
        ),
        (
            "var a=[1]; try{a.assignStruct(%[]);}catch(e){} var d=%[x:7]; try{(Dictionary.assignStruct incontextof d)([]);}catch(e){} a.count+d.x;",
            7,
        ),
        (
            "var d=%[x:7]; try{(Dictionary.assign incontextof d)(null);}catch(e){} try{(Dictionary.assign incontextof d)(['y',9],null);}catch(e){} d.x;",
            7,
        ),
        (
            "var d=%[old:8], n=0; try{(Dictionary.assign incontextof d)(['x',2,3,4]);}catch(e){n++;} n*100+d.x*10+(typeof d.old=='undefined');",
            121,
        ),
        (
            "var d=%[], n=0; try{(Dictionary.assign incontextof d)(['x',2,3]);}catch(e){n++;} n*10+d.x;",
            12,
        ),
        (
            "var d=%[]; (Dictionary.assign incontextof d)([void,1,'',2,'x',7,'orphan']); d.x+(typeof d['void']=='undefined');",
            8,
        ),
        (
            "var a=[1,2],d=%[x:1]; a.assign(a); (Dictionary.assign incontextof d)(d); a.count+(typeof d.x=='undefined');",
            1,
        ),
        (
            "var n=0; [1].find(1,null); try{var k=[1].find(1,null);}catch(e){n++;} [1].pack(null); try{var b=[1].pack(null);}catch(e){n+=10;} n;",
            11,
        ),
    ] {
        check(script, expected, 0);
    }
}

#[test]
fn raw_properties_hidden_members_and_static_flags() {
    check(
        r#"
        var gets=0, sets=0;
        property p { getter(){gets++; return 7;} setter(v){sets++;} }
        var s=%[], d=%[];
        CopyProbe.define(s,'p',&p,false,true);
        CopyProbe.define(s,'secret',9,true,false);
        CopyProbe.define(d,'p',&p,false,false);
        (Dictionary.assign incontextof d)(s,0);
        var flags=CopyProbe.static_slot(d,'p');
        var deep=%[]; (Dictionary.assignStruct incontextof deep)(s);
        var a=[]; a.assign(s);
        flags*100 + CopyProbe.static_slot(deep,'p')*10 +
            (a.count==2 && a[0]=='p') + gets*1000 + sets*10000 + (typeof d.secret=='undefined');
    "#,
        102,
        0,
    );
}

#[test]
fn dictionary_missing_uses_live_array_values_and_survives_waits() {
    check(
        r#"
        var d=%[], source=['a',1,'b',2], seen='';
        var assign=(Dictionary.assign incontextof d);
        d.missing=function(set,name,value) {
            global.CopyProbe.pause(); global.seen+=name;
            if(name=='a') global.source[3]=9;
            return false;
        };
        CopyProbe.missing(d);
        assign(source,0);
        (seen=='ab')*100+d.a*10+d.b;
    "#,
        119,
        2,
    );
    check(
        r#"
        var d=%[], assign=(Dictionary.assign incontextof d), seen='';
        d.missing=function(set,name,value) {
            global.seen+=name;
            if(name=='b') throw 77;
            return false;
        };
        CopyProbe.missing(d);
        var caught=0;
        try{assign(['a',1,'b',2,'c',3],0);}catch(e){caught=e;}
        caught+(seen=='ab')*100+d.a;
    "#,
        178,
        0,
    );
}

#[test]
fn deep_copy_cycles_aliases_and_dictionary_publication() {
    check(
        r#"
        var leaf=[7], s=[leaf,leaf]; s.push(s);
        var d=[]; d.assignStruct(s);
        d[0][0]=9;
        (d[0]!==d[1])*1000+d[1][0]*100+(d[2]===null)*10+leaf[0];
    "#,
        1717,
        0,
    );
    check(
        r#"
        var leaf=[]; leaf.push(leaf);
        var s=%[child:leaf], d=%[], seen=0, assign=(Dictionary.assignStruct incontextof d);
        assign(s);
        (d.child[0]===null)*10+(d.child!==leaf);
    "#,
        11,
        0,
    );
    check(
        r#"
        var root=[], cursor=root;
        for(var i=0;i<600;i++){var next=[];cursor.push(next);cursor=next;}
        cursor.push(root);
        var copy=[];copy.assignStruct(root);cursor=copy;
        var n=0;while(cursor!==null){n++;cursor=cursor[0];} n;
    "#,
        601,
        0,
    );
}

#[test]
fn large_search_and_stable_removal() {
    check(
        r#"
        var a=[]; for(var i=0;i<1000;i++) a.push(i%3);
        var at=a.find(2,500), removed=a.remove(1), first=a.remove(2,0);
        at+removed+first+a.count+(a[0]===0 && a[1]===0)*10000;
    "#,
        11500,
        0,
    );
}

#[test]
fn failed_enumeration_status_and_invalidated_destination_are_not_exceptions() {
    check(
        r#"
        class Plain {} var source=new Plain(); invalidate source;
        var a=[1], d=%[old:3];
        a.assign(source); (Dictionary.assign incontextof d)(source);
        var child=[source], copy=[]; copy.assignStruct(child);
        a.count+(typeof d.old=='undefined')*10+(copy[0]===source);
    "#,
        11,
        0,
    );
    check(
        r#"
        var d=%[], assign=(Dictionary.assign incontextof d), caught=0;
        d.missing=function(set,name,value){ invalidate this; return false; };
        CopyProbe.missing(d);
        try{assign(['a',1,'b',2],0);}catch(e){caught=1;}
        caught*10+(isvalid d);
    "#,
        0,
        0,
    );
}

#[test]
fn array_publishes_before_descent_dictionary_after_descent() {
    for dictionary in [false, true] {
        let script = if dictionary {
            "var leaf=[];for(var i=0;i<300;i++)leaf.push(i);var s=%[child:leaf],d=%[];(Dictionary.assignStruct incontextof d)(s);1;"
        } else {
            "var leaf=[];for(var i=0;i<300;i++)leaf.push(i);var s=[leaf,leaf],d=[];d.assignStruct(s);1;"
        };
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("publication", script).unwrap();
        let module = tjs_front::compile(&sources, source).unwrap();
        let mut heap = tjs::new_heap();
        let mut vm = Vm::new(&module);
        let mut saw_partial = false;
        let mut saw_child = false;
        loop {
            let exit = vm.run_slice(&mut heap, RunBudget::new(1).unwrap());
            heap.collect(vm.roots());
            if let Some(global) = vm.global() {
                let name = heap.intern(&[100]);
                if let Some(Value::Obj(d)) = heap.member(global, name).unwrap() {
                    let d = d.object.unwrap();
                    let child = if dictionary {
                        let key = heap.intern(&"child".encode_utf16().collect::<Vec<_>>());
                        heap.member(d, key).unwrap()
                    } else {
                        heap.array(d).unwrap().first().copied()
                    };
                    if let Some(Value::Obj(child)) = child {
                        saw_child = true;
                        let length = heap.array(child.object.unwrap()).unwrap().len();
                        if dictionary {
                            assert_eq!(length, 300);
                        } else if length < 300 {
                            assert_eq!(
                                heap.array(d).unwrap().len(),
                                1,
                                "later sibling published too early"
                            );
                            saw_partial = true;
                        }
                    }
                }
            }
            match exit {
                VmExit::Yielded => assert!(vm.work_executed() < 100000),
                VmExit::Finished(_) => break,
                other => panic!("{other:?}"),
            }
        }
        assert!(saw_child);
        assert_eq!(saw_partial, !dictionary);
    }
}

#[test]
fn cancellation_during_missing_releases_copy_roots() {
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8(
            "cancel-copy",
            r#"
        var s=%[a:[1],b:[2]], d=%[], assign=(Dictionary.assign incontextof d);
        d.missing=function(set,name,value){global.CopyProbe.pause();return false;};
        CopyProbe.missing(d); assign(s,0);
    "#,
        )
        .unwrap();
    let module = tjs_front::compile(&sources, source).unwrap();
    let mut heap = tjs::new_heap();
    probe::install(&mut heap).unwrap();
    let baseline = heap.collect([]).after;
    let mut vm = Vm::new(&module);
    loop {
        let exit = vm.run_slice(&mut heap, RunBudget::new(1).unwrap());
        heap.collect(vm.roots());
        match exit {
            VmExit::Yielded => assert!(vm.work_executed() < 10000),
            VmExit::Waiting(request) => {
                assert_eq!(request.token, 63);
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    drop(vm);
    assert_eq!(heap.collect([]).after, baseline);
}
