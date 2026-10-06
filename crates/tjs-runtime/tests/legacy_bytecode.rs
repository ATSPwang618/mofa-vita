//! Independently encoded fixtures following krkrz's ExportByteCode layout.
//! These are format/semantic regressions, not executions of the C++ reference.
use tjs_core::{Module, RunBudget, Value, Vm};
use tjs_runtime::{Runtime, RuntimeExit, bytecode};

struct Object {
    parent: i32,
    name: i32,
    kind: i32,
    variables: i32,
    frames: i32,
    arguments: i32,
    unnamed: i32,
    collapse: i32,
    setter: i32,
    getter: i32,
    super_getter: i32,
    code: Vec<i16>,
    data: Vec<(i16, i16)>,
    entries: Vec<i32>,
    members: Vec<(i32, i32)>,
    positions: Vec<(i32, i32)>,
}
impl Object {
    fn new(kind: i32, code: &[i16], data: &[(i16, i16)]) -> Self {
        Self {
            parent: -1,
            name: -1,
            kind,
            variables: 0,
            frames: if kind == 3 { 0 } else { 8 },
            arguments: 0,
            unnamed: 0,
            collapse: -1,
            setter: -1,
            getter: -1,
            super_getter: -1,
            code: code.into(),
            data: data.into(),
            entries: vec![],
            members: vec![],
            positions: vec![],
        }
    }
}
#[derive(Default)]
struct File {
    strings: Vec<Vec<u16>>,
    integers: Vec<i64>,
    reals: Vec<f64>,
    octets: Vec<Vec<u8>>,
    objects: Vec<Object>,
}
fn i32s(out: &mut Vec<u8>, values: impl IntoIterator<Item = i32>) {
    for value in values {
        out.extend(value.to_le_bytes());
    }
}
fn chunk(tag: &[u8; 4], payload: Vec<u8>, header: bool) -> Vec<u8> {
    let mut out = tag.to_vec();
    i32s(
        &mut out,
        [(payload.len() + if header { 8 } else { 0 }) as i32],
    );
    out.extend(payload);
    out
}
fn table(out: &mut Vec<u8>, count: usize, bytes: impl IntoIterator<Item = u8>) {
    i32s(out, [count as i32]);
    out.extend(bytes);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}
impl File {
    fn encode(&self) -> Vec<u8> {
        let mut data = Vec::new();
        // Deliberately distinct scalar pools verify signed byte/short/int decoding.
        table(&mut data, 1, [0xfe]);
        table(&mut data, 1, (-300_i16).to_le_bytes());
        table(&mut data, 1, (-70_000_i32).to_le_bytes());
        table(
            &mut data,
            self.integers.len(),
            self.integers.iter().flat_map(|n| n.to_le_bytes()),
        );
        table(
            &mut data,
            self.reals.len(),
            self.reals.iter().flat_map(|n| n.to_bits().to_le_bytes()),
        );
        i32s(&mut data, [self.strings.len() as i32]);
        for text in &self.strings {
            table(
                &mut data,
                text.len(),
                text.iter().flat_map(|u| u.to_le_bytes()),
            );
        }
        i32s(&mut data, [self.octets.len() as i32]);
        for bytes in &self.octets {
            table(&mut data, bytes.len(), bytes.iter().copied());
        }
        let mut objects = Vec::new();
        i32s(
            &mut objects,
            [
                if self.objects.is_empty() { -1 } else { 0 },
                self.objects.len() as i32,
            ],
        );
        for object in &self.objects {
            let mut record = Vec::new();
            i32s(
                &mut record,
                [
                    object.parent,
                    object.name,
                    object.kind,
                    object.variables,
                    if object.kind == 3 { 0 } else { 2 },
                    object.frames,
                    object.arguments,
                    object.unnamed,
                    object.collapse,
                    object.setter,
                    object.getter,
                    object.super_getter,
                ],
            );
            i32s(&mut record, [object.positions.len() as i32]);
            i32s(&mut record, object.positions.iter().map(|p| p.0));
            i32s(&mut record, object.positions.iter().map(|p| p.1));
            table(
                &mut record,
                object.code.len(),
                object.code.iter().flat_map(|w| w.to_le_bytes()),
            );
            i32s(&mut record, [object.data.len() as i32]);
            for &(kind, index) in &object.data {
                record.extend(kind.to_le_bytes());
                record.extend(index.to_le_bytes());
            }
            i32s(&mut record, [object.entries.len() as i32]);
            i32s(&mut record, object.entries.iter().copied());
            i32s(&mut record, [object.members.len() as i32]);
            for &(name, target) in &object.members {
                i32s(&mut record, [name, target]);
            }
            objects.extend(chunk(b"TJS2", record, false));
        }
        let mut result = b"TJS2100\0".to_vec();
        i32s(&mut result, [0]);
        result.extend(chunk(b"DATA", data, true));
        result.extend(chunk(b"OBJS", objects, true));
        let length = result.len() as i32;
        result[8..12].copy_from_slice(&length.to_le_bytes());
        result
    }
    fn strings(mut self, strings: &[&str]) -> Self {
        self.strings = strings.iter().map(|s| s.encode_utf16().collect()).collect();
        self
    }
}
fn execute(runtime: &mut Runtime, vm: &mut Vm, slice: u32) -> Value {
    for _ in 0..100_000 {
        let exit = runtime.run_slice(vm, RunBudget::new(slice).unwrap());
        runtime.collect(vm.roots());
        match exit {
            RuntimeExit::Finished(value) => return value,
            RuntimeExit::Yielded => {}
            exit => panic!("{exit:?}"),
        }
    }
    panic!("execution did not terminate")
}
fn source(runtime: &mut Runtime, script: &str) -> Module {
    let id = runtime.sources.add_utf8("fixture.tjs", script).unwrap();
    tjs_front::compile(&runtime.sources, id).unwrap()
}
fn check(file: &File, setup: &str, expected: &str) {
    for slice in [1, 4096] {
        for cached in [false, true] {
            let mut runtime = Runtime::new();
            let setup = source(&mut runtime, setup);
            let mut boot = Vm::new(&setup);
            execute(&mut runtime, &mut boot, slice);
            let global = boot.global().unwrap();
            let (mut module, debug) =
                bytecode::decode(&file.encode(), "fixture.tjb", &mut runtime.sources).unwrap();
            assert!(debug.is_none());
            if cached {
                let bytes = bytecode::encode(&module, None).unwrap();
                module = bytecode::decode(&bytes, "cached", &mut runtime.sources)
                    .unwrap()
                    .0;
                assert_eq!(
                    module.functions()[1].origin().unwrap().storage,
                    "fixture.tjb"
                );
            }
            let mut vm = Vm::with_global(&module, global);
            drop(boot);
            let value = execute(&mut runtime, &mut vm, slice);
            assert_eq!(
                runtime.heap.display(value).unwrap(),
                expected,
                "slice={slice} cached={cached}"
            );
        }
    }
}

#[test]
fn scalar_pools_utf16_octets_and_independent_result_slot() {
    for (data, expected) in [
        ((6, 0), "-2"),
        ((7, 0), "-300"),
        ((8, 0), "-70000"),
        ((9, 0), "9223372036854775807"),
        ((5, 0), "1.5"),
        ((0, 0), "void"),
        ((3, 0), "A"),
    ] {
        let file = File {
            strings: vec![vec![65, 0, 66]],
            integers: vec![i64::MAX],
            reals: vec![1.5],
            objects: vec![Object::new(0, &[1, 1, 0, 118, 1, 3, 0, 119], &[data])],
            ..Default::default()
        };
        check(&file, "var keep=0;", expected);
    }
    let file = File {
        octets: vec![vec![1, 2, 3]],
        objects: vec![Object::new(
            0,
            &[1, 1, 0, 103, 2, 1, 1, 118, 2, 119],
            &[(4, 0), (3, 0)],
        )],
        ..Default::default()
    }
    .strings(&["length"]);
    check(&file, "var keep=0;", "3");
    let file = File {
        objects: vec![Object::new(
            0,
            &[1, 1, 0, 3, 2, 7, 1, 2, 11, 3, 118, 3, 119],
            &[(1, 0)],
        )],
        ..Default::default()
    };
    check(&file, "var keep=0;", "0"); // NormalCompare: null differs from void.
}

#[test]
fn arithmetic_families_share_register_named_computed_and_property_semantics() {
    for (opcode, expected) in [
        (26, "1"),
        (30, "1"),
        (34, "7"),
        (38, "5"),
        (42, "2"),
        (46, "0"),
        (50, "48"),
        (54, "0"),
        (58, "9"),
        (62, "3"),
        (66, "0"),
        (70, "2"),
        (74, "2"),
        (78, "18"),
    ] {
        for form in 0..4 {
            let mut code = vec![103, 1, -2, 0, 1, 2, 1, 1, 3, 2];
            match form {
                0 => {
                    code.extend([103, 4, 1, 1]);
                    code.extend([opcode, 4, 3]);
                }
                1 => code.extend([opcode + 1, 4, 1, 1, 3]),
                2 => code.extend([opcode + 2, 4, 1, 2, 3]),
                3 => {
                    code.extend([110, 5, 1, 1]);
                    code.extend([opcode + 3, 4, 5, 3]);
                }
                _ => unreachable!(),
            }
            code.extend([118, 4, 119]);
            let file = File {
                integers: vec![3],
                objects: vec![Object::new(0, &code, &[(3, 0), (3, 1), (9, 0)])],
                ..Default::default()
            }
            .strings(&["d", "x"]);
            check(
                &file,
                "var n=6; property p {getter{return n;} setter(v){n=v;}} var d=%[x:&p];",
                expected,
            );
        }
    }
    for (opcode, expected) in [(18, "7"), (22, "5")] {
        for form in 0..4 {
            let mut code = vec![103, 1, -2, 0, 1, 2, 1];
            match form {
                0 => code.extend([103, 4, 1, 1, opcode, 4]),
                1 => code.extend([opcode + 1, 4, 1, 1]),
                2 => code.extend([opcode + 2, 4, 1, 2]),
                3 => code.extend([110, 5, 1, 1, opcode + 3, 4, 5]),
                _ => unreachable!(),
            }
            code.extend([118, 4, 119]);
            let file = File {
                objects: vec![Object::new(0, &code, &[(3, 0), (3, 1)])],
                ..Default::default()
            }
            .strings(&["d", "x"]);
            check(
                &file,
                "var n='6'; property p {getter{return n;} setter(v){n=v;}} var d=%[x:&p];",
                expected,
            );
        }
    }
}

#[test]
fn original_arguments_rest_array_forwarding_and_internal_context_types() {
    // Top calls f(4,5); f changes its first parameter, but forwards original args.
    let top = Object::new(
        0,
        &[1, 1, 0, 1, 2, 1, 1, 3, 2, 99, 4, 1, 2, 2, 3, 118, 4, 119],
        &[(2, 1), (9, 0), (9, 1)],
    );
    let mut function = Object::new(
        1,
        &[1, -3, 0, 103, 1, -2, 1, 99, 2, 1, -1, 118, 2, 119],
        &[(9, 2), (3, 0)],
    );
    function.variables = 2;
    function.arguments = 2;
    let file = File {
        integers: vec![4, 5, 99],
        objects: vec![top, function],
        ..Default::default()
    }
    .strings(&["sum"]);
    check(&file, "function sum(a,b){return a*10+b;}", "45");
    let mut file = file;
    file.objects[1].collapse = 0;
    file.objects[1].code = vec![103, 1, -2, 1, 99, 2, 1, -2, 1, 1, -3, 118, 2, 119];
    check(&file, "function sum(a,b){return a*10+b;}", "45");
    file.objects[1].unnamed = 1;
    file.objects[1].code = vec![103, 1, -2, 1, 99, 2, 1, -2, 2, 0, -3, 2, 0, 118, 2, 119];
    file.objects[1].collapse = -1;
    check(&file, "function sum(a,b){return a*10+b;}", "45");
    file.objects[0].code = vec![1, 1, 0, 1, 2, 3, 88, 1, 2, 118, 1, 119];
    file.objects[0].data.push((3, 1));
    file.strings.push("Function".encode_utf16().collect());
    check(&file, "var keep=0;", "1");
    file.objects[1].kind = 5;
    check(&file, "var keep=0;", "0");
}

#[test]
fn numeric_boundaries_share_imported_operation_forms_and_cache() {
    for (opcode, left, right, expected) in [
        (58, "9223372036854775807", 1, "-9223372036854775808"),
        (62, "9223372036854775808", 1, "9223372036854775807"),
        (78, "'9223372036854775807'", 3, "9223372036854775805"),
        (34, "Infinity", 1, "-9223372036854775807"),
        (54, "NaN", 63, "1"),
    ] {
        for form in 0..4 {
            let mut code = vec![103, 1, -2, 0, 1, 2, 1, 1, 3, 2];
            match form {
                0 => code.extend([103, 4, 1, 1, opcode, 4, 3]),
                1 => code.extend([opcode + 1, 4, 1, 1, 3]),
                2 => code.extend([opcode + 2, 4, 1, 2, 3]),
                3 => code.extend([110, 5, 1, 1, opcode + 3, 4, 5, 3]),
                _ => unreachable!(),
            }
            code.extend([118, 4, 119]);
            let file = File {
                integers: vec![right],
                objects: vec![Object::new(0, &code, &[(3, 0), (3, 1), (9, 0)])],
                ..Default::default()
            }
            .strings(&["d", "x"]);
            check(
                &file,
                &format!(
                    "var n={left}; property p {{getter{{return n;}} setter(v){{n=v;}}}} var d=%[x:&p];"
                ),
                expected,
            );
        }
    }
}

#[test]
fn original_zero_result_calls_reach_native_methods_without_a_scratch_result() {
    for opcode in [99, 100, 101] {
        // Math.abs(null), with CALL/CALLD/CALLI result operand zero.
        let mut code = vec![103, 1, -2, 0, 1, 3, 2];
        match opcode {
            99 => code.extend([103, 2, 1, 1, 99, 0, 2, 1, 3]),
            100 => code.extend([100, 0, 1, 1, 1, 3]),
            101 => code.extend([1, 2, 1, 101, 0, 1, 2, 1, 3]),
            _ => unreachable!(),
        }
        code.extend([1, 4, 3, 118, 4, 119]);
        let file = File {
            integers: vec![7],
            objects: vec![Object::new(0, &code, &[(3, 0), (3, 1), (1, 0), (9, 0)])],
            ..Default::default()
        }
        .strings(&["Math", "abs"]);
        check(&file, "var keep=0;", "7");
    }
}

#[test]
fn nested_try_restores_outer_flags_for_normal_return_and_throw() {
    // Outer flag true; ENTRY starts false; an inner RET is EXTRY, not function return.
    let mut object = Object::new(
        0,
        &[
            1, 1, 0, 5, 1, 120, 9, 2, 12, 3, 119, 11, 4, 119, 11, 4, 118, 4, 119,
        ],
        &[(9, 0)],
    );
    // The protected body's RET resumes at SETF, then root RET returns SRV below.
    object.code = vec![
        1, 1, 0, 5, 1, 120, 10, 2, 12, 3, 119, 11, 4, 118, 4, 119, 11, 4, 118, 4, 119,
    ];
    let mut file = File {
        integers: vec![1],
        objects: vec![object],
        ..Default::default()
    };
    check(&file, "var keep=0;", "1");
    file.objects[0].code = vec![
        1, 1, 0, 5, 1, 120, 26, 2, 6, 1, 120, 10, 2, 5, 1, 122, 1, 121, 17, 5, 11, 2, 121, 11, 3,
        58, 3, 2, 118, 3, 119, 118, 2, 119,
    ];
    check(&file, "var keep=0;", "1");
    file.objects[0].code = vec![
        1, 1, 0, 5, 1, 120, 8, 2, 6, 1, 122, 1, 119, 11, 4, 118, 4, 119,
    ];
    check(&file, "var keep=0;", "1");
    // Arithmetic faults use the same catch routing, not only explicit THROW.
    file.objects[0].code = vec![
        1, 1, 0, 5, 1, 120, 10, 2, 3, 3, 74, 1, 3, 121, 119, 11, 4, 118, 4, 119,
    ];
    check(&file, "var keep=0;", "1");
}

#[test]
fn class_members_accessors_and_original_pc_survive_cache_and_gc() {
    let top = Object::new(
        0,
        &[1, 1, 0, 102, 2, 1, 0, 103, 3, 2, 1, 118, 3, 119],
        &[(2, 1), (3, 1)],
    );
    let mut class = Object::new(6, &[1, 1, 0, 125, -1, 1, 126, 119], &[(3, 0)]);
    class.name = 0;
    let mut property = Object::new(3, &[], &[]);
    property.parent = 1;
    property.getter = 3;
    property.members.push((1, 2));
    let mut getter = Object::new(5, &[1, 1, 0, 118, 1, 119], &[(9, 0)]);
    getter.parent = 2;
    getter.positions = vec![(0, 123), (3, 127)];
    let file = File {
        integers: vec![42],
        objects: vec![top, class, property, getter],
        ..Default::default()
    }
    .strings(&["C", "x"]);
    check(&file, "var keep=0;", "42");
    let module = tjs_front::bytecode::import(&file.encode(), "origin.tjb").unwrap();
    let origin = module.functions()[4].origin().unwrap();
    assert_eq!(origin.entries[0].source_offset, Some(123));
    assert_eq!(origin.entries[1].legacy_pc, 3);
    assert!(module.disassemble().contains("TJS2100 pc=3"));
}

#[test]
fn malformed_sections_operands_targets_contexts_and_limits_are_rejected() {
    let file = File {
        integers: vec![42],
        objects: vec![Object::new(0, &[1, 1, 0, 118, 1, 119], &[(9, 0)])],
        ..Default::default()
    };
    let bytes = file.encode();
    for length in 0..bytes.len() {
        assert!(tjs_front::bytecode::import(&bytes[..length], "truncated").is_err());
    }
    let mut file = file;
    for code in [
        vec![128, 119],
        vec![1, 9, 0, 119],
        vec![1, 1, 1, 119],
        vec![17, 1, 119],
        vec![99, 1, 2, -3, 119],
        vec![120, 4, 1, 17, -3, 119],
    ] {
        file.objects[0].code = code;
        assert!(tjs_front::bytecode::import(&file.encode(), "invalid").is_err());
    }
    file.objects[0].code = vec![119];
    file.objects[0].parent = 0;
    assert!(tjs_front::bytecode::import(&file.encode(), "cycle").is_err());
    file.objects[0].parent = -1;
    let limits = tjs_front::bytecode::Limits {
        max_output_instructions: 2,
        ..Default::default()
    };
    assert!(tjs_front::bytecode::import_with_limits(&file.encode(), "limit", &limits).is_err());
    let mut bytes = file.encode();
    bytes[16..20].copy_from_slice(&8_i32.to_le_bytes());
    assert!(tjs_front::bytecode::import(&bytes, "chunk").is_err());
}

#[test]
fn superclass_entry_points_search_in_reverse_order_and_bind_instance_scope() {
    let top = Object::new(0, &[1, 1, 0, 103, 2, 1, 1, 118, 2, 119], &[(2, 1), (3, 3)]);
    let mut class = Object::new(6, &[119], &[]);
    class.name = 0;
    class.super_getter = 2;
    let mut resolver = Object::new(
        7,
        &[103, 1, -2, 0, 118, 1, 119, 103, 1, -2, 1, 118, 1, 119],
        &[(3, 1), (3, 2)],
    );
    resolver.entries = vec![0, 7];
    resolver.parent = 1;
    let file = File {
        objects: vec![top, class, resolver],
        ..Default::default()
    }
    .strings(&["C", "A", "B", "answer"]);
    check(
        &file,
        "class A {} class B {} A.answer=41; B.answer=42;",
        "42",
    );
    // Imported function binds d as this, obtains a global method via its scope
    // proxy, and retains that this through CALLI and suspended member lookup.
    let top = Object::new(
        0,
        &[1, 1, 0, 103, 2, -2, 1, 123, 1, 2, 99, 3, 1, 0, 118, 3, 119],
        &[(2, 1), (3, 0)],
    );
    let function = Object::new(1, &[1, 1, 0, 101, 2, -2, 1, 0, 118, 2, 119], &[(3, 1)]);
    let file = File {
        objects: vec![top, function],
        ..Default::default()
    }
    .strings(&["d", "read"]);
    check(
        &file,
        "var d=%[answer:42]; var read=function(){return this.answer;};",
        "42",
    );
}

#[test]
fn direct_indirect_stores_default_access_and_typeof_do_not_skip_execution() {
    // A discarded delete must leave old ra[0] untouched.
    let file = File {
        integers: vec![42],
        objects: vec![Object::new(
            0,
            &[1, 0, 0, 103, 1, -2, 1, 116, 0, 1, 2, 118, 0, 119],
            &[(9, 0), (3, 0), (3, 1)],
        )],
        ..Default::default()
    }
    .strings(&["d", "x"]);
    check(&file, "var d=%[x:7];", "42");
    let top = Object::new(
        0,
        &[1, 1, 0, 1, 2, 1, 99, 2, 1, 0, 118, 2, 119],
        &[(2, 1), (9, 1)],
    );
    let resolver = Object::new(7, &[1, 1, 0, 118, 1, 119], &[(9, 0)]);
    let file = File {
        integers: vec![42, 99],
        objects: vec![top, resolver],
        ..Default::default()
    };
    check(&file, "var keep=0;", "99");
    for (store, direct) in [
        (104, true),
        (105, true),
        (106, true),
        (107 + 1, false),
        (109, false),
        (111, true),
        (113, false),
    ] {
        let mut code = vec![103, 1, -2, 0, 1, 2, 1, 1, 3, 2];
        code.extend([store, 1, if direct { 1 } else { 2 }, 3]);
        code.extend([107, 4, 1, 2, 118, 4, 119]);
        let file = File {
            integers: vec![42],
            objects: vec![Object::new(0, &code, &[(3, 0), (3, 1), (9, 0)])],
            ..Default::default()
        }
        .strings(&["d", "x"]);
        check(&file, "var d=%[x:0];", "42");
    }
    let file = File {
        objects: vec![Object::new(0, &[84, 1, -2, 0, 118, 1, 119], &[(3, 0)])],
        ..Default::default()
    }
    .strings(&["absent"]);
    check(&file, "var d=0;", "undefined");
    let file = File {
        integers: vec![42],
        objects: vec![Object::new(
            0,
            &[110, 1, -2, 0, 1, 2, 1, 114, 1, 2, 115, 3, 1, 118, 3, 119],
            &[(3, 0), (9, 0)],
        )],
        ..Default::default()
    }
    .strings(&["p"]);
    check(
        &file,
        "var n=0; property p {getter{return n;} setter(v){n=v;}}",
        "42",
    );
}

#[test]
fn unary_comparisons_branches_eval_and_legacy_error_locations() {
    for (opcode, input, expected) in [
        (14, 1, "1"),
        (13, 1, "0"),
        (82, 1, "-2"),
        (83, 1, "Integer"),
        (91, 1, "1"),
        (92, 1, "-1"),
        (93, 1, "0"),
        (94, 1, "1"),
        (95, 1, "1"),
        (96, 1, "1"),
        (97, 1, "1"),
    ] {
        let mut code = vec![1, 1, 0];
        if opcode == 14 {
            code.extend([14, 11, 1]);
        } else {
            code.extend([opcode, 1]);
        }
        code.extend([118, 1, 119]);
        let file = File {
            integers: vec![input],
            objects: vec![Object::new(0, &code, &[(9, 0)])],
            ..Default::default()
        };
        check(&file, "var keep=0;", expected);
    }
    for (opcode, expected) in [(7, "0"), (8, "0"), (9, "1"), (10, "0")] {
        let file = File {
            integers: vec![2, 3],
            objects: vec![Object::new(
                0,
                &[1, 1, 0, 1, 2, 1, opcode, 1, 2, 11, 3, 118, 3, 119],
                &[(9, 0), (9, 1)],
            )],
            ..Default::default()
        };
        check(&file, "var keep=0;", expected);
    }
    for opcode in [15, 16] {
        let file = File {
            integers: vec![1, 42],
            objects: vec![Object::new(
                0,
                &[1, 1, 0, 5, 1, opcode, 8, 1, 2, 1, 118, 2, 119, 118, 1, 119],
                &[(9, 0), (9, 1)],
            )],
            ..Default::default()
        };
        check(&file, "var keep=0;", if opcode == 15 { "1" } else { "42" });
    }
    let file = File {
        objects: vec![Object::new(0, &[1, 1, 0, 86, 1, 118, 1, 119], &[(3, 0)])],
        ..Default::default()
    }
    .strings(&["6*7"]);
    check(&file, "var keep=0;", "42");
    let mut runtime = Runtime::new();
    let file = File {
        objects: vec![Object::new(0, &[103, 1, -2, 0, 119], &[(3, 0)])],
        ..Default::default()
    }
    .strings(&["absent"]);
    let (module, _) = bytecode::decode(&file.encode(), "broken.tjb", &mut runtime.sources).unwrap();
    let mut vm = Vm::new(&module);
    loop {
        match runtime.run_slice(&mut vm, RunBudget::new(4096).unwrap()) {
            RuntimeExit::Yielded => {}
            RuntimeExit::Fault(error) => {
                assert!(
                    error
                        .trace
                        .iter()
                        .any(|frame| frame.function.contains("broken.tjb#0") && frame.pc == 0)
                );
                break;
            }
            RuntimeExit::Thrown(exception) => {
                assert!(
                    exception
                        .diagnostic
                        .trace
                        .iter()
                        .any(|frame| frame.function.contains("broken.tjb#0") && frame.pc == 0)
                );
                break;
            }
            exit => panic!("unexpected {exit:?}"),
        }
    }
}

#[test]
fn source_property_operations_retain_setter_and_failure_side_effects() {
    for (script, expected) in [
        (
            "var n=0; var d=%[]; property p { getter { &d.x=100; return 6; } setter(v){n=v;} } &d.x=&p; d.x+=3; n*1000+d.x;",
            "9100",
        ),
        (
            "var n=0; var d=%[]; property p { getter { &d.x=100; return 6; } setter(v){n=v;} } &d.x=&p; ++d.x; n*1000+d.x;",
            "7100",
        ),
        (
            "var n=0; var d=%[]; property p { getter { &d.x=100; return 6; } setter(v){n=v;} } &d.x=&p; var old=d.x++; old*1000+d.x+n;",
            "6101",
        ),
        ("var a=[]; try {a[3]\\=0;} catch(e) {} a.length;", "4"),
        (
            "var d=%[]; try {d.x\\=0;} catch(e) {} (typeof d.x);",
            "void",
        ),
    ] {
        for slice in [1, 4096] {
            let mut runtime = Runtime::new();
            let module = source(&mut runtime, script);
            let mut vm = Vm::new(&module);
            let value = execute(&mut runtime, &mut vm, slice);
            assert_eq!(runtime.heap.display(value).unwrap(), expected, "{script}");
        }
    }
}
