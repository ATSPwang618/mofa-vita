use tjs_bind as tjs;
use tjs_core::{RunBudget, SourceMap, Vm, VmExit};

#[tjs::class(name = "Counter")]
/// 一个同时保存整数和脚本对象的 Rust 类。
mod counter {
    use super::tjs;

    #[derive(Default, tjs::Trace)]
    pub struct State {
        value: i64,
        saved: tjs::Value,
    }
    impl State {
        #[tjs::constant(name = "defaultAmount")]
        const DEFAULT_AMOUNT: i64 = 7;
        #[tjs::method]
        fn converted(&self, #[tjs(coerce, default = State::DEFAULT_AMOUNT)] amount: i64) -> i64 {
            amount
        }
        #[tjs::method]
        fn adapter_names(
            &self,
            state: i64,
            args: i64,
            value: i64,
            cx: i64,
            __tjs_argument_0: i64,
        ) -> i64 {
            state + args + value + cx + __tjs_argument_0
        }
        #[tjs::constructor]
        fn new(initial: i64) -> Self {
            Self {
                value: initial,
                saved: tjs::Value::Void,
            }
        }
        #[tjs::getter(name = "value")]
        fn value(&self) -> i64 {
            self.value
        }
        #[tjs::setter(name = "value")]
        fn set_value(&mut self, value: i64) {
            self.value = value;
        }
        /// 累加并返回当前值。
        #[tjs::method]
        fn add(&mut self, amount: i64) -> i64 {
            self.value += amount;
            self.value
        }
        #[tjs::method(hidden = true)]
        fn save(&mut self, value: tjs::Value) {
            self.saved = value;
        }
        #[tjs::getter(name = "saved")]
        fn saved(&self) -> tjs::Value {
            self.saved
        }
        #[tjs::method]
        fn fail(&mut self) -> tjs::NativeResult<()> {
            self.value += 1;
            Err(tjs::NativeError::Message("intentional native failure"))
        }
    }
}

fn compile(script: &str) -> tjs_core::Module {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("native classes", script).unwrap();
    tjs_front::compile(&sources, source).unwrap()
}

#[test]
fn adapter_parameters_cannot_capture_generated_locals() {
    check("(new Counter(0)).adapter_names(1,2,3,4,5);", "15");
}

#[test]
fn explicit_defaults_coercion_and_constants_preserve_strict_arguments() {
    check(
        r#"
        var c=new Counter(0), strict=false, missing=false;
        try { c.add('2'); } catch(e) { strict=true; }
        try { c.add(); } catch(e) { missing=true; }
        strict && missing && Counter.defaultAmount==7 && c.converted()==7
            && c.converted(void)==0 && c.converted('12')==12 && c.converted(3.9)==3;
    "#,
        "1",
    );
}
fn run(vm: &mut Vm, heap: &mut tjs::Heap, slice: u32) -> VmExit {
    loop {
        let exit = vm.run_slice(heap, RunBudget::new(slice).unwrap());
        heap.collect(vm.roots());
        if !matches!(exit, VmExit::Yielded) {
            return exit;
        }
    }
}
fn check(script: &str, expected: &str) {
    check_result(script, expected, true);
}

fn check_result(script: &str, expected: &str, reclaim: bool) {
    let module = compile(script);
    for slice in [1, 10000] {
        let mut heap = tjs::new_heap();
        counter::install(&mut heap).unwrap();
        let baseline = heap.collect([]).after;
        let mut vm = Vm::new(&module);
        let exit = run(&mut vm, &mut heap, slice);
        let VmExit::Finished(value) = exit else {
            panic!("{script}: {exit:?}")
        };
        assert_eq!(heap.display(value).unwrap(), expected, "{script}");
        drop(vm);
        if reclaim {
            assert_eq!(heap.collect([]).after, baseline, "{script}");
        }
    }
}

#[test]
fn rust_classes_construct_bind_properties_and_trace_owned_script_values() {
    check(
        "var c = new Counter(2); c.value = 4; var add = c.add; add(3); c.value;",
        "7",
    );
    check(
        "var c = new Counter(2); c.save(%[owner: c, text: \"kept\"]); c.saved.text;",
        "kept",
    );
    check(
        "var a = new Counter(2); var b = new Counter(8); (a.add incontextof b)(3); a.value * 100 + b.value;",
        "211",
    );
    check(
        "function make(args*) { return new Counter(args*); } make(9).value;",
        "9",
    );
    check("var C = Counter; new C(4).add(3);", "7");
    assert!(counter::CLASS.doc.contains("Rust 类"));
    assert!(
        counter::CLASS
            .methods
            .iter()
            .any(|m| m.name == "add" && m.doc.contains("累加"))
    );
}

#[test]
fn builtin_arrays_and_dictionaries_use_generated_class_members() {
    for (script, expected) in [
        (
            "var a = new Array; a.push(1,2,3); a.pop() * 100 + a.shift() * 10 + a.count;",
            "311",
        ),
        (
            "var a = []; a.unshift(1,2); a.push([3,4]*); a.reverse(); a[0] * 10 + a[-1];",
            "41",
        ),
        ("var a = new Array(99); a.add(7) * 10 + a.length;", "1"),
        ("var a = [1]; var push = a.push; push(3); a[-1];", "3"),
        ("var a = []; a.append = Array.push; a.append(7); a[0];", "7"),
        (
            "var a = [1]; a.note = 7; a.clear(); a.note * 10 + a.count;",
            "70",
        ),
        (
            "var a = [1]; var b = [2]; delete a.count; a.count = 7; a.count * 100 + b.count * 10 + a.length;",
            "711",
        ),
        ("var a = []; delete a.push; a.push = 9; a.push;", "9"),
        (
            "var d = new Dictionary; d.x = 1; (Dictionary.clear incontextof d)(); d.x;",
            "void",
        ),
        ("var d = new Dictionary; d.clear;", "void"),
        (
            "var d = %[erase: Dictionary.clear, x: 1]; d.erase(); d.x;",
            "void",
        ),
        (
            "var d = %[clear: 7]; (Dictionary.clear incontextof d)(); d.clear;",
            "void",
        ),
        ("new Array().pop();", "void"),
    ] {
        check(script, expected);
    }
}

#[test]
fn raw_native_properties_keep_context_and_report_their_runtime_types() {
    for (script, expected) in [
        (
            "function f() { var a = new Counter(2); var b = new Counter(8); var p = &a.value; *p += 3; var q = p incontextof b; *q = 11; return a.value * 100 + b.value; } f();",
            "511",
        ),
        (
            "var c = new Counter(2); var a = [&c.value]; a[0] += 4; c.value * 10 + a[0];",
            "66",
        ),
        (
            "var c = new Counter(2); var d = %[]; &d.p = &c.value; d.p = 9; c.value;",
            "9",
        ),
        (
            "function f() { var a = [1,2]; var p = &a.count; *p = 4; &a.count = 7; return a.count * 100 + *p * 10 + a.length; } f();",
            "744",
        ),
        (
            "(Counter instanceof 'Counter') + (Counter instanceof 'Class') + (new Counter(1) instanceof 'Counter') + (Array instanceof 'Array') + (new Array() instanceof 'Array') + (Dictionary instanceof 'Dictionary');",
            "6",
        ),
        (
            "var c = new Counter(3); (typeof c.value) + ',' + (typeof &c.value) + ',' + ((&c.value) instanceof 'Property') + ',' + (c.add instanceof 'Function');",
            "Integer,Object,1,1",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn native_errors_restore_state_and_leave_the_vm_reusable() {
    let mut heap = tjs::new_heap();
    counter::install(&mut heap).unwrap();
    let global = heap.alloc_global();
    let root = heap.root(tjs::Value::Obj(global.into()));
    let module = compile("var c = new Counter(3); c.fail();");
    let mut vm = Vm::with_global(&module, global);
    let VmExit::Fault(error) = run(&mut vm, &mut heap, 1) else {
        panic!()
    };
    assert!(error.message.contains("intentional native failure"));
    drop(vm);
    let module = compile("c.add(2);");
    let mut vm = Vm::with_global(&module, global);
    let VmExit::Finished(value) = run(&mut vm, &mut heap, 1) else {
        panic!()
    };
    assert_eq!(value.as_integer(), Some(6));
    heap.release_root(root).unwrap();
    for script in [
        "new Counter();",
        "new Counter(\"x\");",
        "var a = []; (a.push incontextof %[x:1])(2);",
    ] {
        let mut vm = Vm::new(&compile(script));
        assert!(matches!(run(&mut vm, &mut heap, 1), VmExit::Fault(_)));
    }
}

#[tjs::class(name = "Label")]
mod label {
    use super::tjs;
    #[derive(Default, tjs::Trace)]
    pub struct State {
        text: tjs::Value,
    }
    impl State {
        #[tjs::constructor]
        fn new(text: tjs::Value) -> Self {
            Self { text }
        }
        #[tjs::getter]
        fn text(&self) -> tjs::Value {
            self.text
        }
    }
}

#[test]
fn script_subclasses_initialize_native_bases_without_implicit_constructor_calls() {
    for (script, expected) in [
        (
            "class C extends Counter { function C(n) { super.Counter(n); } function twice(n) { return super.add(n * 2); } } var c = new C(7); c.twice(3); c.value + ',' + (c instanceof 'C') + ',' + (c instanceof 'Counter');",
            "13,1,1",
        ),
        (
            "class C extends Counter {} var c = new C; c.add(4); c.value;",
            "4",
        ),
        (
            "class C extends Counter { var x = value + 2; function C(n) { global.Counter.Counter(n); } } var c = new C(9); c.x * 100 + c.value;",
            "209",
        ),
        (
            "var c = new Counter(3); var result = c.Counter(8); (typeof result) + ',' + c.value;",
            "void,8",
        ),
        (
            "class A extends global.Counter {} class B extends A {} class C extends A, B {} var c = new C; c.Counter(2); c.add(5); c.value;",
            "7",
        ),
        (
            "var c = new Counter(1); c.save(%[owner:c]); class C extends Counter { function C() { throw 7; } } try { new C; } catch(e) { c.add(e); } c.value;",
            "8",
        ),
        (
            "class A extends Array { function A() { super.Array(); push(7,9); } } var a = new A; a.count * 100 + a.pop();",
            "209",
        ),
        (
            "class D extends Dictionary { function D() { super.Dictionary(); } } var d = new D; d.x = 5; (Dictionary.clear incontextof d)(); typeof d.x;",
            "undefined",
        ),
        (
            "var d = %[]; (Array incontextof d)(); (Array.push incontextof d)(7); (Array.pop incontextof d)();",
            "7",
        ),
        (
            "class A extends Array {} var a = new A; a.push(1,2); var b = []; b.assign(a); b.join(',');",
            "1,2",
        ),
        (
            "class A extends Array {} var a = new A; a.push(1,2); typeof a[0];",
            "undefined",
        ),
        (
            "class R extends RegExp { function R() { super.RegExp('a+'); } } var r = new R; r.test('aaa'); RegExp.last === r;",
            "1",
        ),
        (
            "class R extends Math.RandomGenerator { function R() { super.RandomGenerator(4); } } (new R).random32() === (new Math.RandomGenerator(4)).random32();",
            "1",
        ),
        (
            "class D extends Date { function D() { super.Date(2000, 0, 2); } } (new D).getYear();",
            "2000",
        ),
        (
            "class E extends Exception { function E() { super.Exception('custom'); } } var e = new E; e.message;",
            "custom",
        ),
    ] {
        check_result(script, expected, !script.contains("RegExp.last"));
    }
}

#[test]
fn native_member_snapshots_preserve_flags_and_context_after_class_mutation() {
    for (script, expected) in [
        ("delete Counter.Counter; (new Counter(8)).value;", "0"),
        (
            "Counter.Counter = 1; var failed = 0; try { new Counter(2); } catch(e) { failed = 1; } failed;",
            "1",
        ),
        (
            "var initialize = Counter.Counter; Counter.Counter = function(n) { (initialize incontextof this)(n + 2); return 9; }; (new Counter(3)).value;",
            "5",
        ),
        (
            "var a = new Counter(2); Counter.add = function(n) { return value * n; }; var b = new Counter(3); a.add(4) * 100 + b.add(5);",
            "615",
        ),
        (
            "var c = new Counter(6); delete Counter.value; c.value * 10 + (typeof (new Counter(2)).value == 'undefined');",
            "61",
        ),
        (
            "var c = new Counter(2); &Counter.saved = 7; var d = new Counter(3); c.saved === void && d.saved === 7;",
            "1",
        ),
        (
            "var a = new Counter(2); Counter.alias = a.add; var b = new Counter(3); b.alias(4); a.value * 10 + b.value;",
            "63",
        ),
        (
            "Counter.f = function() { return value; }; var c = new Counter(7); var f = c.f; f();",
            "7",
        ),
        (
            "var a = new Counter(2); var d = %[]; (Dictionary.assign incontextof d)(a); d.value * 10 + !('save' in d);",
            "21",
        ),
        (
            "var a = []; var b = []; delete a.push; Array.push = 8; a.count * 100 + (typeof a.push == 'undefined') * 10 + (b.push instanceof 'Function');",
            "11",
        ),
        ("var d = new Dictionary; typeof d.Dictionary;", "undefined"),
        ("var m = new Math; typeof m.PI;", "undefined"),
    ] {
        check_result(script, expected, false);
    }
}

#[test]
fn independent_native_states_and_resumable_base_constructors_survive_collection() {
    let script = "
        class Pair extends Counter, Label {
            function Pair(n, text) { global.Counter.Counter(n); global.Label.Label(text); }
        }
        var p = new Pair(3, %[message:'kept']);
        p.save(%[pair:p]); p.add(5);
        var seed = new Math.RandomGenerator(11).serialize();
        class Seed {
            property state { getter() { p.add(1); return seed.state; } }
            property left { getter() { return seed.left; } }
            property next { getter() { return seed.next; } }
        }
        class R extends Math.RandomGenerator, Label {
            function R(s) { global.Math.RandomGenerator.RandomGenerator(s); global.Label.Label(p); }
        }
        var r = new R(new Seed);
        (r.random32() === (new Math.RandomGenerator(11)).random32()) + ',' + r.text.value + ',' + p.text.message;
    ";
    for slice in [1, 10000] {
        let mut heap = tjs::new_heap();
        counter::install(&mut heap).unwrap();
        label::install(&mut heap).unwrap();
        let baseline = heap.collect([]).after;
        let mut vm = Vm::new(&compile(script));
        let exit = run(&mut vm, &mut heap, slice);
        let VmExit::Finished(value) = exit else {
            panic!("{exit:?}");
        };
        assert_eq!(heap.display(value).unwrap(), "1,9,kept");
        drop(vm);
        assert_eq!(heap.collect([]).after, baseline);
    }
}

#[test]
fn old_native_member_snapshots_keep_values_alive_until_the_last_instance_dies() {
    let mut heap = tjs::new_heap();
    let class = counter::install(&mut heap).unwrap();
    let baseline = heap.collect([]).after;
    let name = heap.intern(&"payload".encode_utf16().collect::<Vec<_>>());
    let old = heap.alloc_string("old".encode_utf16().collect::<Vec<_>>());
    heap.set_member(class, name, tjs::Value::Str(old)).unwrap();
    let a = heap.alloc_native(class, counter::State::default()).unwrap();
    let root = heap.root(tjs::Value::Obj(a.into()));
    let new = heap.alloc_string("new".encode_utf16().collect::<Vec<_>>());
    heap.set_member(class, name, tjs::Value::Str(new)).unwrap();
    let b = heap.alloc_native(class, counter::State::default()).unwrap();
    heap.collect([tjs::Value::Obj(b.into())]);
    assert_eq!(
        heap.display(heap.member(a, name).unwrap().unwrap())
            .unwrap(),
        "old"
    );
    assert_eq!(
        heap.display(heap.member(b, name).unwrap().unwrap())
            .unwrap(),
        "new"
    );
    heap.release_root(root).unwrap();
    heap.collect([]);
    assert!(heap.string(old).is_err());
    heap.remove_member(class, name).unwrap();
    assert_eq!(heap.collect([]).after, baseline);
}
