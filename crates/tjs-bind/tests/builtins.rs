use std::collections::HashMap;
use tjs_core::{
    NativeError, NativeResult, RunBudget, SourceMap, Trace, Value, Vm, VmExit, storage::Storage,
};

#[derive(Default)]
struct Memory {
    text: HashMap<Vec<u16>, Vec<u16>>,
    binary: HashMap<Vec<u16>, Vec<u8>>,
}
impl Trace for Memory {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl Storage for Memory {
    fn read_text(&mut self, name: &[u16], _: &[u16]) -> NativeResult<Vec<u16>> {
        self.text
            .get(name)
            .cloned()
            .ok_or(NativeError::Message("missing text"))
    }
    fn write_text(&mut self, name: &[u16], _: &[u16], text: &[u16]) -> NativeResult<()> {
        self.text.insert(name.to_vec(), text.to_vec());
        Ok(())
    }
    fn read_binary(&mut self, name: &[u16], _: &[u16]) -> NativeResult<Vec<u8>> {
        self.binary
            .get(name)
            .cloned()
            .ok_or(NativeError::Message("missing binary"))
    }
    fn write_binary(&mut self, name: &[u16], _: &[u16], bytes: &[u8]) -> NativeResult<()> {
        self.binary.insert(name.to_vec(), bytes.to_vec());
        Ok(())
    }
}
fn check(script: &str, expected: &str) {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("standard library", script).unwrap();
    let module = tjs_front::compile(&sources, source).unwrap_or_else(|e| panic!("{script}: {e}"));
    for slice in [1, 10000] {
        let mut heap = tjs_bind::new_heap();
        heap.set_storage(Memory::default());
        let mut vm = Vm::new(&module);
        loop {
            match vm.run_slice(&mut heap, RunBudget::new(slice).unwrap()) {
                VmExit::Finished(result) => {
                    assert_eq!(heap.display(result).unwrap(), expected, "{script}");
                    break;
                }
                VmExit::Yielded => {
                    heap.collect(vm.roots());
                }
                exit => panic!("{script}: {exit:?}"),
            }
            assert!(vm.work_executed() < 1_000_000, "{script}");
        }
    }
}

#[test]
fn loading_lines_keeps_mixed_newlines_nul_and_array_identity() {
    for (text, expected) in [
        ("", "1:0:"),
        ("\r\n", "1:1:"),
        ("a\r\nb\n\rc\0ignored", "1:4:a/b//c"),
        ("甲\r乙\n", "1:2:甲/乙"),
    ] {
        let mut sources = SourceMap::new();
        let source = sources
            .add_utf8(
                "line-load",
                "var a=[9];var same=a.load('lines')===a;same+':'+a.count+':'+a.join('/');",
            )
            .unwrap();
        let module = tjs_front::compile(&sources, source).unwrap();
        let mut heap = tjs_bind::new_heap();
        heap.set_storage(Memory {
            text: HashMap::from([(
                "lines".encode_utf16().collect(),
                text.encode_utf16().collect(),
            )]),
            ..Default::default()
        });
        let mut vm = Vm::new(&module);
        loop {
            match vm.run_slice(&mut heap, RunBudget::new(1).unwrap()) {
                VmExit::Finished(value) => {
                    assert_eq!(heap.display(value).unwrap(), expected);
                    break;
                }
                VmExit::Yielded => {
                    heap.collect(vm.roots());
                }
                other => panic!("{other:?}"),
            }
        }
    }
}

#[test]
fn presence_checks_bypass_getters_and_keep_void_members() {
    for (script, expected) in [
        (
            r#"var d=%[a:1, v:void]; ("a" in d) + 2*("v" in d) + 4*("z" in d);"#,
            "3",
        ),
        (
            r#"var calls=0; class A { property p { getter { ++calls; throw 1; } } } var a=new A(); ("p" in a)+calls;"#,
            "1",
        ),
        (
            r#"class A { function p() {} } class B extends A {} "p" in B;"#,
            "1",
        ),
        (
            r#"var a=[void]; ((-1) in a) + 2*(1 in a) + 4*("push" in a);"#,
            "5",
        ),
        (r#"var d=%[a:1]; delete d.a; "a" in d;"#, "0"),
        (r#"var d=%["1.5"=>2]; 1.5 in d;"#, "1"),
    ] {
        check(script, expected);
    }
}

#[test]
fn string_methods_keep_utf16_counts_coercion_and_tjs_format_rules() {
    for (script, expected) in [
        (
            r#""abc".charAt("1") + "abc".charAt(-1) + "abc".charAt(3);"#,
            "b",
        ),
        (r#"#"😀".charAt(1);"#, "56832"),
        (r#""ababa".indexOf("ba",2);"#, "3"),
        (r#""abc".indexOf("");"#, "-1"),
        (
            r#""Abé中文".toUpperCase() + ":" + "AbÉ".toLowerCase();"#,
            "ABé中文:abÉ",
        ),
        (
            r#""abcdef".substring(2,3) + ":" + "abcdef".substr(2,3);"#,
            "cde:cde",
        ),
        (r#"" abc\t\n".trim().reverse().repeat(2);"#, "cbacba"),
        (r#""%04X %c %5d".sprintf(10,'b',30);"#, "000A b    30"),
        (
            r#""%*.*f/%.0s/%u".sprintf(7,2,3.25,"ab",-1);"#,
            "   3.25/ab/18446744073709551615",
        ),
        (
            r#""%.3d/%d/%#x".sprintf(7,-9223372036854775807-1,-1);"#,
            "007/-9223372036854775808/0xffffffffffffffff",
        ),
        (r#""%s".sprintf($"0xd800").length;"#, "1"),
        (r#"("\x01"+"f\n").escape();"#, "\\x01\\x66\\n"),
        (
            r#"var n=0; try { "abc".charAt(); } catch(e) { n += e instanceof "Exception"; } try { "abc"[9]; } catch(e) { n += 2*(e.message.length>0); } n;"#,
            "3",
        ),
        (
            r#"var e=new Exception(42); e.message += 1; e.message;"#,
            "43",
        ),
        (
            r#"class Replacement { var message, stored; function Replacement(m) { message=m; } property trace { setter(v) { stored=v; } getter { return stored; } } } Exception=Replacement; var n=0; try { function f() { return 1\0; } f(); } catch(e) { n=(e instanceof "Replacement")+(e.message.length>0)+(e.trace.length>0); } n;"#,
            "3",
        ),
        (
            r#"class Replacement { function Replacement(m) { throw 7; } } Exception=Replacement; var n=0; try { try { 1\0; } catch(e) { n=100; } } catch(e) { n=e; } n;"#,
            "7",
        ),
        (
            r#"var p=%[replace:function(a*) {return a.count;}]; "x".replace(p,"y",1,2);"#,
            "2",
        ),
        (
            r#"var old=Array.split; Array.split=function(a*) {this.push(a.count); return 99;}; var a="x".split(",",void,true,77); Array.split=old; a[0];"#,
            "4",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn container_mutation_copy_and_structural_recursion_follow_reference_rules() {
    for (script, expected) in [
        (r#"var a=[1,void,"",2]; a.join(":",void,true);"#, "1::2"),
        (
            r#"var a=[1,2,1,"1"]; a.remove(1,false); a.insert(-1,9); a.erase(-1); a.join(",");"#,
            "2,1,9",
        ),
        (r#"var a=[1,"1",1]; a.find(1,-1)+a.remove(1)*10;"#, "22"),
        (r#"var a=[]; a.count="3"; a.length;"#, "3"),
        (r#"var a=[1,2]; a.assign(a); a.count;"#, "0"),
        (
            r#"var d=%[x:7]; var a=[]; a.assign(d); a.join(":");"#,
            "x:7",
        ),
        (
            r#"var d=%[old:1]; (Dictionary.assign incontextof d)(["x",2,"y",3,"orphan"],false); d.old+d.x+d.y;"#,
            "6",
        ),
        (
            r#"var child=%[v:1]; var a=[child,child]; a.push(a); var b=[]; b.assignStruct(a); b[0].v=7; b[1].v + child.v + 10*(b[2]===null);"#,
            "12",
        ),
        (
            r#"var d=%[child:%[v:3]]; d.self=d; var copy=%[]; (Dictionary.assignStruct incontextof copy)(d); copy.child.v=4; d.child.v+10*(copy.self===null);"#,
            "13",
        ),
        (
            r#"var a=[]; var n=0; try { a.assignStruct(%[]); } catch(e) { n=1; } n;"#,
            "1",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn native_and_script_sorting_survive_suspension_collection_and_exceptions() {
    for (script, expected) in [
        (r#"var a=[3,1,2]; a.sort(); a.join(",");"#, "1,2,3"),
        (r#"var a=["2","10","1"]; a.sort(); a.join(",");"#, "1,10,2"),
        (
            r#"var a=["2","10","1"]; a.sort("0"); a.join(",");"#,
            "1,2,10",
        ),
        (r#"var a=[2,10,1]; a.sort("z"); a.join(",");"#, "2,10,1"),
        (
            r#"var a=[3,2,1]; a.sort(function(a,b){return a>b;}); a.join(",");"#,
            "3,2,1",
        ),
        (
            r#"var a=[%[v:1,n:"a"],%[v:0,n:"b"],%[v:1,n:"c"]]; a.sort(function(a,b){return a.v<b.v;},true); a[0].n+a[1].n+a[2].n;"#,
            "bac",
        ),
        (
            r#"var a=[3,1,2]; var n=0; try { a.sort(function(a,b){return 1\0;}); } catch(e) { n=e instanceof "Exception"; } a.sort(); n+":"+a.join(",");"#,
            "1:1,2,3",
        ),
        (
            r#"var a=[3,1,2]; try { a.sort(function(a,b){throw 7;}); } catch(e) { a.push(e); } a.sort(); a.join(",");"#,
            "1,2,3,7",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn math_dates_and_reproducible_random_generators_are_callable_from_script() {
    for (script, expected) in [
        (
            r#"Math.abs("-3")+Math.pow(2,3)+Math.round(-1.5)+Math.floor(1.9)+Math.ceil(1.1);"#,
            "13",
        ),
        (
            r#"Math.sin(0)+Math.cos(0)+Math.tan(0)+Math.log(1)+Math.exp(0)+Math.sqrt(9);"#,
            "5",
        ),
        (
            r#"Math.atan2(0,1)+Math.atan(0)+Math.asin(0)+Math.acos(1);"#,
            "0",
        ),
        (
            r#"Math.min() === Infinity && Math.max() === -Infinity && (1/Math.min(0,-0.0))===-Infinity && (1/Math.max(-0.0,0))===Infinity;"#,
            "1",
        ),
        (
            r#"var d=new Date("Thu., 1-Jan.-1970 00:00:00 GMT"); d.getTime();"#,
            "0",
        ),
        (
            r#"var d=new Date("1970/1/1 09:30 acst"); d.getTime();"#,
            "0",
        ),
        (
            r#"var d=new Date("Ju. 1 2020 pm 1:02:03 GMT"); var e=new Date("1 June 13:02:03 2020 UTC"); d.getTime()===e.getTime();"#,
            "1",
        ),
        (
            r#"var d=new Date(2024,0,32,0,0,0); d.setMonth(12); d.getYear()+":"+d.getMonth()+":"+d.getDate();"#,
            "2025:0:1",
        ),
        (
            r#"var d=new Date(); d.setTime(-1500); d.getTime();"#,
            "-1000",
        ),
        (
            r#"var a=new Math.RandomGenerator(123456789); var b=new Math.RandomGenerator(123456789); var ok=1; for(var i=0;i<700;i++) if(a.random32()!==b.random32()) ok=0; ok;"#,
            "1",
        ),
        (
            r#"var a=new Math.RandomGenerator(-123); a.random(); var b=new Math.RandomGenerator(a.serialize()); var ok=1; for(var i=0;i<1300;i++) if(a.random64()!==b.random64()) ok=0; ok;"#,
            "1",
        ),
        (
            r#"var saved=new Math.RandomGenerator(8).serialize(); var count=0; class Seed { property state { getter { ++count; return saved.state; } } property left { getter { ++count; return saved.left; } } property next { getter { ++count; return saved.next; } } } var a=new Math.RandomGenerator(new Seed()); var b=new Math.RandomGenerator(saved); a.randomize(new Seed()); (a.random64()===b.random64())+count;"#,
            "7",
        ),
        (
            r#"var a=new Math.RandomGenerator(7); var s=a.serialize(); var b=new Math.RandomGenerator(s); a.randomize(s); (a.random()===b.random()) && (a.random63()>=0);"#,
            "1",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn pack_unpack_and_storage_keep_binary_types_and_container_identity() {
    for (script, expected) in [
        (
            r#"[-1,255,-2,65535,0x12345678].pack("cCsSN").unpack("cCsSN").join(",");"#,
            "-1,255,-2,65535,305419896",
        ),
        (
            r#"["1011","123"].pack("B4H3").unpack("B4H3").join(":");"#,
            "1011:123",
        ),
        (r#"["SGVsbG8="].pack("m").unpack("a*")[0];"#, "Hello"),
        (r#"<%48656c6c6f%>.unpack("m")[0];"#, "SGVsbG8="),
        (
            r#"[1.5,-2.25].pack("fd").unpack("fd").join(",");"#,
            "1.5,-2.25",
        ),
        (r#"<%0102ff%>.unpack("v2").join(",");"#, "513,255"),
        (
            r#"var a=["a","",3,void,null]; var b=[]; a.save("lines"); b.load("lines"); b.join(":");"#,
            "a::3::",
        ),
        (
            r#"var a=[void,null,1,-1,1.5,"😀",<%0001ff%>,%[key:7]]; a.push(a); a.saveStruct("data","b"); var b=[]; var c=b.loadStruct("data"); (c===b)+":"+(b[0]===void)+":"+(b[1]===null)+":"+b[7].key+":"+(b[8]===null)+":"+b[6].unpack("H*")[0];"#,
            "1:1:1:7:1:0001FF",
        ),
        (
            r#"var d=%[v:[7,8]]; (Dictionary.saveStruct incontextof d)("dict","b"); var e=Dictionary.loadStruct("dict"); e.v.join(",");"#,
            "7,8",
        ),
        (
            r#"var a=[%[v:2],[3,4]]; a.saveStruct("text"); var lines=[]; lines.load("text"); lines[0].indexOf("(const)");"#,
            "0",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn pack_preserves_null_results_hex_rules_and_base64_lookup_semantics() {
    for (script, expected) in [
        (
            r#"(typeof [].pack('C')) + ':' + ([1].pack('C0') === null) + ':' + ([1].pack('p') === null) + ':' + ([1].pack('') === null);"#,
            "Object:1:1:1",
        ),
        (r#"['0fAE'].pack('H*').unpack('H*')[0];"#, "0FAE"),
        (
            r#"var n=0; try { var v=['F'].pack('h'); } catch(e) { n++; } try { var v=['F'].pack('H'); } catch(e) { n+=10; } ['F'].pack('H'); n;"#,
            "11",
        ),
        (
            r#"([''].pack('m')===null) + ':' + (['QQ'].pack('m')===null) + ':' + (['日'].pack('m')===null);"#,
            "1:1:1",
        ),
        (r#"['!!!!'].pack('m').unpack('H*')[0];"#, "000000"),
        (r#"['Q! ='].pack('m').unpack('H*')[0];"#, "4000"),
        (r#"['QR=='].pack('m').unpack('H*')[0];"#, "41"),
        (r#"['QUJ='].pack('m').unpack('H*')[0];"#, "4142"),
        (r#"['QQ==QUJD'].pack('m').unpack('H*')[0];"#, "410000414243"),
        (r#"['QQ=日'].pack('m').unpack('H*')[0];"#, "41"),
        (r#"['QUJDREU'].pack('m').unpack('H*')[0];"#, "414243444500"),
        (
            r#"var n=0; try { var v=['日AAA'].pack('m'); } catch(e) { n++; } try { var v=['AAAAA'].pack('m'); } catch(e) { n+=10; } try { var v=['AAAAAA'].pack('m'); } catch(e) { n+=100; } n;"#,
            "111",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn unpack_text_uses_cp932_after_removing_zero_bytes() {
    for (script, expected) in [
        (r#"<%82a093fa967bb6%>.unpack('a*')[0];"#, "あ日本ｶ"),
        (r#"<%8200a041%>.unpack('A3a*').join(':');"#, "あ:A"),
        (r#"<%000041%>.unpack('a2a*').join(':');"#, ":A"),
        (r#"<%fa4081605c7e%>.unpack('a*')[0];"#, "ⅰ～\\~"),
        (
            r#"var n=0; try { var v=<%82%>.unpack('a*'); } catch(e) { n++; } try { var v=<%80%>.unpack('a*'); } catch(e) { n+=10; } try { var v=<%f040%>.unpack('A*'); } catch(e) { n+=100; } try { var v=<%a0%>.unpack('a*'); } catch(e) { n+=1000; } n;"#,
            "1111",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn array_lengths_narrow_to_uint_and_dictionary_noops_still_validate_instances() {
    for (script, expected) in [
        (
            r#"var a=[7,8,9]; a.length=4294967297; a.count + ':' + a[0];"#,
            "1:7",
        ),
        (
            r#"var a=[7]; a.count='-4294967294'; a.length + ':' + (a[1]===void);"#,
            "2:1",
        ),
        (r#"var a=[7]; a.length=-4294967296; a.count;"#, "0"),
        (
            r#"var d=%[x:7], load=Dictionary.load incontextof d, save=Dictionary.save incontextof d; var n=(load()===void)+(save()===void); invalidate d; try { load(); } catch(e) { n+=10; } try { var x=save(); } catch(e) { n+=100; } n;"#,
            "112",
        ),
    ] {
        check(script, expected);
    }
}

#[test]
fn array_length_accessors_preserve_binding_overrides_and_invalidation() {
    check(
        r#"
        var calls=0;
        property custom { getter { return ++calls; } }
        function verify() {
        var a=[1,2], b=[3,4,5];
        var original=&a.count, rebound=original incontextof b;
        var ok=(*original==2 && *rebound==3);
        b.push(6);
        ok=ok && *rebound==4;
        &a.count=&custom;
        ok=ok && a.count==1 && a.count==2 && a.length==2 && *original==2;
        invalidate b;
        var caught=false;
        try { *rebound; } catch(e) { caught=true; }
        return ok && caught;
        }
        verify();
        "#,
        "1",
    );
}
