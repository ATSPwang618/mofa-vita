use tjs_core::{RunBudget, SourceMap, Vm, VmExit};

fn compile(script: &str) -> tjs_core::Module {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("numbers", script).unwrap();
    tjs_front::compile(&sources, source).unwrap_or_else(|error| panic!("{script}: {error}"))
}

fn check(script: &str, expected: &str) {
    let module = compile(script);
    for slice in [1, 10000] {
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&module);
        loop {
            let exit = vm.run_slice(&mut heap, RunBudget::new(slice).unwrap());
            heap.collect(vm.roots());
            match exit {
                VmExit::Yielded => assert!(vm.work_executed() < 100_000),
                VmExit::Finished(value) => {
                    assert_eq!(heap.display(value).unwrap(), expected, "{script}");
                    break;
                }
                exit => panic!("{script}: {exit:?}"),
            }
        }
    }
}

#[test]
fn numeric_literals_division_and_conversion_follow_tjs_types() {
    for (script, expected) in [
        (".5 + 1.25e1 + 2.;", "15"),
        ("012 + 0xff + 0b11;", "268"),
        ("1e + 2;", "100"),
        ("0xffffffffffffffff;", "-1"),
        ("5 / 2 === 2.5;", "1"),
        ("4 / 2 === 2;", "0"),
        ("5 \\ 2;", "2"),
        ("-5 \\ 2;", "-2"),
        ("-5.9 % 2.8;", "-1"),
        ("2.5 * 4 - 1;", "9"),
        ("var n; +n + (int)3.9 + real 2;", "5"),
        ("(int)-2.9 + int(5.9);", "3"),
        ("real 2 === 2.0;", "1"),
        ("+\"12.5tail\";", "12.5"),
        ("int \"0xff\" + +\"nonnumeric\";", "255"),
        ("+\" 12\";", "0"),
        ("+\"- 12\";", "-12"),
        ("+\"true2\";", "1"),
        ("!\"0.5\";", "1"),
        ("!0.5;", "0"),
        ("\"7\" - \"2\";", "5"),
        ("void * 9;", "0"),
        ("\"9\" / 2;", "4.5"),
        ("var x = \"4\"; x++; x === 5;", "1"),
        ("var x = .5; ++x; x;", "1.5"),
    ] {
        check(script, expected);
    }
}

#[test]
fn bitwise_operators_precedence_and_compound_targets_work_together() {
    for (script, expected) in [
        ("1 | 2 ^ 3 & 1;", "3"),
        ("1 << 2 + 1;", "8"),
        ("8 >> 1 < 5;", "1"),
        ("7 & 3 == 3;", "1"),
        ("~0;", "-1"),
        ("-8 >> 2;", "-2"),
        ("-1 >>> 1;", "9223372036854775807"),
        ("1 << 63;", "-9223372036854775808"),
        ("\"6\" & 3.9;", "2"),
        (
            "var n=23; n %= 7; n <<= 4; n |= 3; n ^= 1; n &= 63; n >>= 1; n >>>= 1; n;",
            "8",
        ),
        ("var a=[25]; a[0] /= 2; a[0] \\= 3; a[0];", "4"),
        (
            "var d=%[n:20], calls=0; function get() { ++calls; return d; } get().n %= 7; calls*100+d.n;",
            "106",
        ),
        (
            "var d=%[n:20], order=0; function get() { order=order*10+2; return d; } function rhs() { order=order*10+1; return 2; } get().n >>= rhs(); order*100+d.n;",
            "1205",
        ),
        (
            "function pair(a,b) { return a*10+b; } function f() { var x=8; return pair(x>>=1,x=2); } f();",
            "22",
        ),
        ("var d=%[x:7]; d.x % 3;", "1"),
    ] {
        check(script, expected);
    }
}

#[test]
fn equality_distinguishes_types_and_bound_contexts() {
    for (script, expected) in [
        ("1 == 1.0;", "1"),
        ("1 === 1.0;", "0"),
        ("\"2\" == 2;", "1"),
        ("\"02\" == 2;", "0"),
        ("\"2\" === 2;", "0"),
        ("\"\" == void;", "1"),
        ("0.0 == void;", "1"),
        ("null == 0;", "0"),
        ("null === null;", "1"),
        (
            "function f() {} var a=%[], b=%[]; (f incontextof a) == (f incontextof b);",
            "1",
        ),
        (
            "function f() {} var a=%[], b=%[]; (f incontextof a) === (f incontextof b);",
            "0",
        ),
        (
            "function f() {} var a=%[]; (f incontextof a) === (f incontextof a);",
            "1",
        ),
        (
            "function f() { var x=1; if (x === (x=2)) return 7; return 8; } f();",
            "7",
        ),
        ("function f() { var x=1; return x === (x=2); } f();", "0"),
        ("\"10\" < \"2\";", "1"),
        ("\"10\" < 2;", "0"),
        ("9007199254740993 > 9007199254740992;", "1"),
    ] {
        check(script, expected);
    }
}

#[test]
fn nonfinite_reals_and_string_formatting_match_reference_paths() {
    for (script, expected) in [
        ("1 / 0 === Infinity;", "1"),
        ("0 / 0 == NaN;", "0"),
        ("NaN !== NaN;", "1"),
        ("NaN < 0;", "0"),
        ("NaN <= 0;", "1"),
        ("NaN >= 0;", "1"),
        ("-0.0 === 0.0;", "1"),
        ("string -0.0;", "-0.0"),
        ("(string)0.0;", "+0.0"),
        ("string (1/0);", "+Infinity"),
        ("string (-1/0);", "-Infinity"),
        ("\"result=\" + NaN;", "result=NaN"),
        ("string 1.2345678901234568;", "1.23456789012346"),
        ("string 1e20;", "1e+20"),
        ("string 1e-5;", "1e-05"),
        ("string 0.0001;", "0.0001"),
        ("\"value=\" + 2.5;", "value=2.5"),
    ] {
        check(script, expected);
    }
}

#[test]
fn numeric_faults_are_explicit_and_do_not_partially_store_compound_results() {
    for script in [
        "1 \\ 0;",
        "1 % 0;",
        "0x8000000000000000 \\ -1;",
        "0x8000000000000000 % -1;",
        "1 << 64;",
        "1 >> -1;",
        "null - 1;",
    ] {
        let module = compile(script);
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&module);
        assert!(
            matches!(
                vm.run_slice(&mut heap, RunBudget::new(10000).unwrap()),
                VmExit::Fault(_)
            ),
            "{script}"
        );
    }
    let module = compile("var x=17; x \\= 0;");
    let mut heap = tjs_bind::new_heap();
    let global = heap.alloc_global();
    let mut vm = Vm::with_global(&module, global);
    assert!(matches!(
        vm.run_slice(&mut heap, RunBudget::new(10000).unwrap()),
        VmExit::Fault(_)
    ));
    // Inspect through the same public script interface after the failed write.
    let read = compile("x;");
    let mut vm = Vm::with_global(&read, global);
    let VmExit::Finished(value) = vm.run_slice(&mut heap, RunBudget::new(10000).unwrap()) else {
        panic!("read after fault");
    };
    assert_eq!(value.as_integer(), Some(17));
}

#[test]
fn integer_words_and_real_conversions_are_shared_across_language_consumers() {
    for (script, expected) in [
        ("9223372036854775807 + 1;", "-9223372036854775808"),
        ("9223372036854775808 - 1;", "9223372036854775807"),
        ("9223372036854775807 * 3;", "9223372036854775805"),
        ("-9223372036854775808;", "-9223372036854775808"),
        ("-9223372036854775809;", "9223372036854775807"),
        ("18446744073709551615 === -1;", "1"),
        ("18446744073709551616 === 0;", "1"),
        ("0x10000000000000001 === 1;", "1"),
        ("0xffffffffffffffffffffffffffffffff === -1;", "1"),
        ("+\"18446744073709551615tail\" === -1;", "1"),
        ("!\"18446744073709551616\";", "1"),
        ("\"9223372036854775808\" - 1;", "9223372036854775807"),
        ("\"9223372036854775807\" * 3;", "9223372036854775805"),
        ("(const)[18446744073709551615][0];", "-1"),
        (
            "function f(){var n=9223372036854775807; n++; return n;} f();",
            "-9223372036854775808",
        ),
        (
            "var a=[9223372036854775808]; --a[0]; a[0];",
            "9223372036854775807",
        ),
        (
            "var n=9223372036854775807, gets=0, sets=0; property p {getter{++gets; return n;} setter(v){++sets; n=v;}} p+=1; n===0x8000000000000000 && gets==1 && sets==1;",
            "1",
        ),
        ("int Infinity;", "-9223372036854775808"),
        ("int -Infinity;", "-9223372036854775808"),
        ("int NaN;", "-9223372036854775808"),
        ("int \"- NaN\";", "-9223372036854775808"),
        ("int 9223372036854775808.0;", "-9223372036854775808"),
        ("int 9223372036854774784.0;", "9223372036854774784"),
        ("int -9223372036854777856.0;", "-9223372036854775808"),
        ("int -0.999;", "0"),
        ("~Infinity;", "9223372036854775807"),
        ("$Infinity === '';", "1"),
        ("!!\"NaN\";", "1"),
        ("Math.abs('18446744073709551615');", "1"),
        ("'%d'.sprintf(Infinity);", "-9223372036854775808"),
        ("@set(N=18446744073709551617) @if(N==1) 7; @endif", "7"),
        ("@set(N=1e999) @if(N==0) 7; @endif", "7"),
    ] {
        check(script, expected);
    }
}

#[test]
fn numeric_prefix_type_exponent_and_decimal_rounding_are_preserved() {
    for (script, expected) in [
        ("+\".\" === 0.0;", "1"),
        ("+\"e+ 2tail\" === 0.0;", "1"),
        ("string +\"- .e- 2\";", "-0.0"),
        ("+\"0p2\" === 0;", "1"),
        ("+\"0P2\" === 0;", "1"),
        ("+\"0x\" === 0;", "1"),
        ("+\"0x.p\" === 0.0;", "1"),
        ("+\"1e+ trailing\" === 1.0;", "1"),
        ("0x1p4294967296 === 1.0;", "1"),
        ("0x1p-4294967295 === 2.0;", "1"),
        ("0x1p2147483648 === 0.0;", "1"),
        ("0x1p-2147483648 === 0.0;", "1"),
        ("string 999999999999999.5;", "1e+15"),
        ("string 99999999999999.95;", "100000000000000"),
        ("string 0.00009999999999999995;", "0.0001"),
        ("string 0.0009999999999999995;", "0.001"),
        ("string 1.7976931348623157e308;", "1.79769313486232e+308"),
        ("string -1.7976931348623157e308;", "-1.79769313486232e+308"),
        ("string 4.9406564584124654e-324;", "4.94065645841247e-324"),
        ("'1e+15' == 999999999999999.5;", "1"),
        ("'n=' + 999999999999999.5;", "n=1e+15"),
        ("var d=%['1e+15'=>7]; d[999999999999999.5];", "7"),
    ] {
        check(script, expected);
    }
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("invalid-number", "0p2;").unwrap();
    assert!(tjs_front::compile(&sources, source).is_err());
}
#[test]
fn nondecimal_reals_share_source_conversion_and_constant_container_semantics() {
    for (script, expected) in [
        ("0x1.8p2;", "6"),
        ("0b1.01p3 + 01.4p2;", "16"),
        ("0x.8p1 + 0b.1p2;", "3"),
        ("0x1.07CC000000000p14;", "16883"),
        ("0x100000000000000000000.p-80;", "1"),
        ("real \"- 0x1.8p+ 2tail\";", "-6"),
        ("(const)[0x1.8p2][0];", "6"),
        ("@set(N=0b1.1p2) @if(N==6) 7; @endif", "7"),
        ("string -0x1p-1023;", "-0.0"),
        ("0x1p1024 === Infinity;", "1"),
        ("0x1p-1022 === 2.2250738585072014e-308;", "1"),
        ("0x1.0000000000000fp0 === 1.0;", "1"),
    ] {
        check(script, expected);
    }
}
