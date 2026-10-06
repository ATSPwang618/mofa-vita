use tjs_bind as tjs;
use tjs_core::{Heap, RunBudget, SourceMap, Value, Vm, VmExit};

struct OwnedRoots(Vec<Value>);
impl tjs_core::Trace for OwnedRoots {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for &value in &self.0 {
            visit(value);
        }
    }
}
impl tjs_core::NativeContinuation for OwnedRoots {
    fn resume(
        self: Box<Self>,
        cx: &mut tjs::NativeCx<'_>,
        _: Value,
    ) -> tjs::NativeResult<tjs::NativeStep> {
        for (i, value) in self.0.iter().enumerate() {
            let Value::Str(id) = value else {
                unreachable!()
            };
            assert_eq!(cx.heap().string(*id)?, [65 + i as u16]);
        }
        Ok(tjs::NativeStep::Return(Value::Int(self.0.len() as i64)))
    }
}

#[tjs::class(name = "TextProbe")]
mod probe {
    use super::*;
    #[derive(Default, tjs::Trace)]
    pub struct State;
    impl State {
        #[tjs::method(resumable = true)]
        fn rooted(cx: &mut tjs::NativeCx<'_>, count: i64) -> tjs::NativeResult<tjs::NativeStep> {
            let values = (0..count)
                .map(|i| Value::Str(cx.heap_mut().alloc_string([65 + i as u16])))
                .collect();
            Ok(tjs::NativeStep::Continue(Box::new(OwnedRoots(values))))
        }
        #[tjs::constructor]
        fn new() -> Self {
            Self
        }
        #[tjs::method]
        fn raw(cx: &mut tjs::NativeCx<'_>, input: Value) -> tjs::NativeResult<Value> {
            let mut units = tjs_core::value::to_string_units(cx.heap(), input)?;
            for unit in &mut units {
                if *unit == 94 {
                    *unit = 0;
                }
            }
            Ok(Value::Str(cx.heap_mut().alloc_string(units)))
        }
    }
}
fn compile(script: &str) -> tjs_core::Module {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("string-work", script).unwrap();
    tjs_front::compile(&sources, source).unwrap_or_else(|error| panic!("{script}: {error}"))
}
fn run(vm: &mut Vm, heap: &mut Heap, slice: u32) -> Value {
    loop {
        let exit = vm.run_slice(heap, RunBudget::new(slice).unwrap());
        heap.collect(vm.roots());
        match exit {
            VmExit::Yielded => assert!(vm.work_executed() < 100000),
            VmExit::Finished(value) => return value,
            other => panic!("{other:?}"),
        }
    }
}
fn check(script: &str, expected: &str) {
    let module = compile(script);
    for slice in [1, 4096] {
        let mut heap = tjs::new_heap();
        probe::install(&mut heap).unwrap();
        let baseline = heap.collect([]).after;
        let mut vm = Vm::new(&module);
        let value = run(&mut vm, &mut heap, slice);
        assert_eq!(heap.display(value).unwrap(), expected, "{script}");
        drop(vm);
        assert_eq!(heap.collect([]).after, baseline);
    }
}

#[test]
fn conversions_and_result_demand_follow_each_method() {
    for (script, expected) in [
        (
            "var n=0;try{''.indexOf('a',null);}catch(e){n++;} ''.indexOf('',null); n;",
            "1",
        ),
        ("'a'.indexOf('a',-1)==-1 && 'a'.indexOf('',-1)==-1;", "1"),
        (
            "var n=0;try{'a'.substring(0,null);}catch(e){n++;}'a'.substring(5,null);n;",
            "1",
        ),
        (
            "var n=0;try{''.charAt(null);}catch(e){n=9;}try{'a'.charAt(null);}catch(e){n++;}n;",
            "1",
        ),
        (
            "'a'.repeat(null);'a'.toUpperCase();'a'.reverse();'a'.escape();7;",
            "7",
        ),
        (
            "var n=0;try{'x'.toUpperCase(0);}catch(e){n++;}n+'ab'.repeat(-1).length;",
            "1",
        ),
        (
            "var n=0;try{[<%00%>].join(',');}catch(e){n++;}try{[].join(<%00%>);}catch(e){n++;}n;",
            "2",
        ),
        ("var a=[void,'',1,void,2];a.join(':',void,1);", ":1:2"),
    ] {
        check(script, expected);
    }
}

#[test]
fn native_string_lengths_nul_and_surrogates_remain_distinct() {
    for (script, expected) in [
        (
            "var s=TextProbe.raw('a^bc');s.substr(1,2).length*100+s.substr(0,3).length*10+s.substr(2).length;",
            "32",
        ),
        ("TextProbe.raw('  ^a  ').trim().length;", "0"),
        ("TextProbe.raw(' a^b ').trim().length;", "3"),
        ("TextProbe.raw('a^bc').indexOf('bc',2);", "2"),
        ("'abc'.indexOf(TextProbe.raw('^x'),1);", "1"),
        (
            "var n=0;try{''.indexOf(TextProbe.raw('^x'),null);}catch(e){n=1;}n;",
            "1",
        ),
        ("TextProbe.raw('a^B').toUpperCase();", "A"),
        ("TextProbe.raw('A^b').toLowerCase();", "a"),
        ("TextProbe.raw('a^b').reverse();", "b"),
        ("TextProbe.raw('^abc').reverse().length;", "0"),
        (
            "var s=TextProbe.raw('ab^').reverse();s.length*100+#(s.charAt(1));",
            "298",
        ),
        (
            "var s=TextProbe.raw('ab^').repeat(3);s.length*100+#(s.charAt(7));",
            "998",
        ),
        (
            "var s='😀';var r=s.reverse();r.length*100000+#(r.charAt(0));",
            "256832",
        ),
        (
            "var s=TextProbe.raw('a^b');[s,s].join(TextProbe.raw(':^x'));",
            "a:a",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn mixed_string_addition_uses_the_same_conversion_in_all_update_paths() {
    check(
        "var s=TextProbe.raw('a^b');(s+1)+':'+(1+s)+':'+(s+void);",
        "a1:1a:a",
    );
    check(
        "var s=TextProbe.raw('a^b');function f(v){v+=1;return v;}var a=[s];a[0]+=2;var d=%[v:s];d.v+=3;f(s)+':'+a[0]+':'+d.v+':'+s.length;",
        "a1:a2:a3:3",
    );
    check(
        "var s=TextProbe.raw('a^b'),log='';class C{property p{getter{log+='g';return s;}setter(v){log+='s';s=v;}}}var o=new C();o.p+=4;log+':'+s;",
        "gs:a4",
    );
    check(
        "var s=TextProbe.raw('a^b'),log='';class C{property p{getter{log+='g';return s;}setter(v){log+='s';s=v;}}}var o=new C();try{o.p+=<%01%>;}catch(e){log+='e';}log+':'+s.length;",
        "ge:3",
    );
}

#[test]
fn escape_resets_hex_state_across_plain_characters_and_budget_boundaries() {
    check(r#"('\x01'+'gf').escape();"#, r"\x01gf");
    check(r#"('\x01'+'f').escape();"#, r"\x01\x66");
    check(
        r#"var s='z'.repeat(1023)+'\x01'+'f'+'g'+'f';s.escape().substr(1023);"#,
        r"\x01\x66gf",
    );
    check(r#"TextProbe.raw('a^b').escape();"#, "a");
}

#[test]
fn long_transformations_search_and_join_preserve_results() {
    check(
        "var s=' abC '.repeat(3000);var r=s.toUpperCase().trim().reverse();r.length==14998 && r.substr(0,3)=='CBA' && r.substr(14995)=='CBA';",
        "1",
    );
    check(
        "var s='a'.repeat(20000)+'b', p='a'.repeat(9000)+'b';s.indexOf(p);",
        "11000",
    );
    check(
        "var s='ab'.repeat(11000);s.indexOf('ababababababac');",
        "-1",
    );
    check(
        "var a=[];for(var i=0;i<250;i++)a.push('abc'.repeat(20));var s=a.join('||');s.length==15498 && s.substr(15438)=='abc'.repeat(20);",
        "1",
    );
    check(
        "var s='x'.repeat(20000);s.substring(4095,12000).length;",
        "12000",
    );
}

#[test]
fn search_matches_an_independent_utf16_window_oracle() {
    let module = compile("s.indexOf(p,start);");
    let mut seed = 0x8391_u32;
    let mut next = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        seed >> 16
    };
    for case in 0..192 {
        let length = next() as usize % 64;
        let needle_length = next() as usize % 12;
        let mut source: Vec<u16> = (0..length)
            .map(|_| [97, 98, 0xd800, 0][next() as usize % 4])
            .collect();
        let mut needle: Vec<u16> = (0..needle_length)
            .map(|_| [97, 98, 0xd800, 0][next() as usize % 4])
            .collect();
        // Include successful matches and repeated prefixes as well as random
        // mismatches; the oracle uses windows, never the prefix-table logic.
        if case % 3 == 0 {
            source.fill(97);
            needle.fill(97);
        } else if case % 3 == 1 && length >= needle_length {
            needle.copy_from_slice(&source[length - needle_length..]);
        }
        let start = next() as usize % (length + 2);
        let expected = if needle.is_empty() || start >= source.len() {
            -1
        } else {
            let haystack = tjs_core::string::c_string(&source[start..]);
            let pattern = tjs_core::string::c_string(&needle);
            if pattern.is_empty() {
                start as i64
            } else {
                haystack
                    .windows(pattern.len())
                    .position(|window| window == pattern)
                    .map_or(-1, |offset| (start + offset) as i64)
            }
        };
        let mut heap = tjs::new_heap();
        let global = heap.alloc_global();
        for (name, units) in [("s", source), ("p", needle)] {
            let symbol = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
            let value = Value::Str(heap.alloc_string(units));
            heap.set_member(global, symbol, value).unwrap();
        }
        let symbol = heap.intern(&"start".encode_utf16().collect::<Vec<_>>());
        heap.set_member(global, symbol, Value::Int(start as i64))
            .unwrap();
        let mut vm = Vm::with_global(&module, global);
        let Value::Int(actual) = run(&mut vm, &mut heap, 1) else {
            panic!("non-integer search result in case {case}");
        };
        assert_eq!(actual, expected, "case {case}");
    }
}

#[test]
fn sprintf_grammar_consumption_and_unsigned_fields_follow_reference() {
    for (script, expected) in [
        ("'%q/%*%/%d'.sprintf(17);", "q/%/17"),
        ("'%12*d/%--s'.sprintf();", "*d/-s"),
        ("'%*.*d'.sprintf(-5,2,3);", "03   "),
        (
            "var n=0;try{var s='%*d'.sprintf(-5,3);}catch(e){n++;}try{var s='%.*d'.sprintf(-1,3);}catch(e){n++;}n;",
            "2",
        ),
        (
            "'%4294967297s/%.*c/%*s'.sprintf('x',-1,'ab',-1,'');",
            "x/a/",
        ),
        (
            "var n=0;try{var s='%.q'.sprintf();}catch(e){n++;}try{var s='%0'.sprintf();}catch(e){n++;}'%'.sprintf(null);n;",
            "2",
        ),
        (
            "var a='',b='';try{var s='%901d'.sprintf();}catch(e){a=e.message;}try{var s='%901d'.sprintf(null);}catch(e){b=e.message;}a==b;",
            "1",
        ),
        (
            "var a='',b='';try{var s='%*d'.sprintf(901);}catch(e){a=e.message;}try{var s='%*d'.sprintf(901,null);}catch(e){b=e.message;}a==b;",
            "1",
        ),
        ("('%'+'0'.repeat(1100)+'.1s').sprintf('xyz');", "x"),
        (
            "var n=0;try{var s=('%'+'0'.repeat(1100)+'.s').sprintf('x');}catch(e){n=1;}n;",
            "1",
        ),
        ("('%'+'0'.repeat(64)+'d').sprintf(7);", "7"),
        ("('%'+'0'.repeat(66)+'f').sprintf(1);", "1.000000"),
        (
            "var n=0;try{var s=('%'+'0'.repeat(65)+'d').sprintf(1);}catch(e){n++;}try{var s=('%'+'0'.repeat(67)+'f').sprintf(1);}catch(e){n++;}n;",
            "2",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn sprintf_batches_preserve_padding_nul_and_later_failures() {
    for (script, expected) in [
        (
            "'%5.2s/%-5.2s/%05s'.sprintf('abcd','abcd','ab');",
            "   ab/ab   /   ab",
        ),
        ("'%5s!'.sprintf('');", "!"),
        ("'%s!'.sprintf(TextProbe.raw('^a'));", ""),
        ("'%5s!'.sprintf(TextProbe.raw('a^b'));", "  a"),
        ("'%5.4s!'.sprintf('ab');", " ab"),
        ("TextProbe.raw('%s^%d').sprintf('x',null);", "x"),
        (
            "var s='ab'.repeat(10000);var r='%22000s/%-22000s'.sprintf(s,s);r.length==44001 && r.substr(1998,4)=='  ab' && r.substr(41999)=='ab'+' '.repeat(2000);",
            "1",
        ),
        (
            "var s=TextProbe.raw('a'.repeat(1023)+'^b');('%s'+'z'.repeat(3000)).sprintf(s).length;",
            "1023",
        ),
        (
            "var n=0;try{var s=('%s'+'z'.repeat(3000)+'%d').sprintf(TextProbe.raw('a^b'),null);}catch(e){n=1;}n;",
            "1",
        ),
        (
            "var a=[];for(var i=0;i<1500;i++)a.push(i);var r=('%d,'.repeat(1500)).sprintf(a*);r.substr(r.length-5);",
            "1499,",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn long_string_work_has_budget_boundaries_and_releases_after_reset() {
    for expression in [
        "s.toUpperCase()",
        "s.toLowerCase()",
        "s.substring(0,18000)",
        "padded.trim()",
        "s.reverse()",
        "s.repeat(2)",
        "s.escape()",
        "s.indexOf(p)",
        "s.indexOf('b')",
        "[s,s].join(s)",
        "s.sprintf()",
        "'%s'.sprintf(s)",
        "'%50000s'.sprintf(s)",
        "fmt.sprintf('x')",
    ] {
        let module = compile(&format!("var result={expression};result;"));
        let mut heap = tjs::new_heap();
        let baseline = heap.collect([]).after;
        let global = heap.alloc_global();
        for (name, units) in [
            ("s", vec![97u16; 20000]),
            ("padded", [vec![32u16; 20000], vec![97; 20000]].concat()),
            ("p", [vec![97u16; 9000], vec![98]].concat()),
            ("fmt", [vec![37u16], vec![48; 20000], vec![115]].concat()),
        ] {
            let symbol = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
            let value = Value::Str(heap.alloc_string(units));
            heap.set_member(global, symbol, value).unwrap();
        }
        let mut vm = Vm::with_global(&module, global);
        // The script takes only a few instructions to enter the operation;
        // each selected operation requires many owned native continuations.
        for _ in 0..15 {
            let exit = vm.run_slice(&mut heap, RunBudget::new(1).unwrap());
            assert!(matches!(exit, VmExit::Yielded), "{expression}: {exit:?}");
            heap.collect(vm.roots());
        }
        vm.reset();
        drop(vm);
        assert_eq!(heap.collect([]).after, baseline, "{expression}");
    }
}

#[test]
fn single_unit_search_preserves_nul_start_and_slice_boundaries() {
    for (script, expected) in [
        ("TextProbe.raw('a^b').indexOf('b');", "-1"),
        ("TextProbe.raw('a^b').indexOf('b',2);", "2"),
        ("'abc'.indexOf(TextProbe.raw('b^ignored'));", "1"),
        ("('a'.repeat(1024)+'b').indexOf('b');", "1024"),
        ("('a'.repeat(2047)+'b').indexOf('b',1024);", "2047"),
        ("('a'.repeat(4096)+'\\xD800').indexOf('\\xD800');", "4096"),
        ("('a'.repeat(1023)+TextProbe.raw('^b')).indexOf('b');", "-1"),
    ] {
        check(script, expected);
    }
}

#[test]
fn bulk_repeat_keeps_phase_across_vm_slices() {
    for seed in ["a", "ab", "1234567", "abcdefghijklmnopqrstuvw"] {
        let script = format!(
            "var s='{seed}'.repeat(1401);s.length+':'+s.substr(1017,37)+':'+s.substr(s.length-23);"
        );
        let text = seed.repeat(1401);
        let expected = format!(
            "{}:{}:{}",
            text.len(),
            &text[1017.min(text.len())..1054.min(text.len())],
            &text[text.len().saturating_sub(23)..]
        );
        check(&script, &expected);
    }
    check("TextProbe.raw('a^b').repeat(700).length;", "2100");
}

#[test]
fn integer_join_preserves_boundaries_extremes_and_gc_roots() {
    let numbers = (0..400)
        .map(|i| (i as i64 * -10000000000000).to_string())
        .collect::<Vec<_>>();
    check(
        "var a=[];for(var i=0;i<400;i++)a.push(i*-10000000000000);a.join('::');",
        &numbers.join("::"),
    );
    check(
        "[-9223372036854775807-1,9223372036854775807].join('|');",
        "-9223372036854775808|9223372036854775807",
    );
}

#[test]
fn native_roots_survive_collection_with_inline_and_spilled_storage() {
    for count in [0, 1, 8, 9, 32] {
        check(&format!("TextProbe.rooted({count});"), &count.to_string());
    }
}
