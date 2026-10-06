use super::*;

fn result(heap: &mut Heap, source: StrId, method: Method, args: &[Value]) -> Value {
    let NativeStep::Return(value) = call(heap, source, method, args, true).unwrap() else {
        panic!("short operation unexpectedly yielded");
    };
    value
}

#[test]
fn identity_operations_reuse_strings_without_allocation_debt() {
    let mut heap = Heap::new();
    let source = heap.alloc_string([0x4e2d, 0xd800, 0x6587]);
    for (method, args) in [
        (Method::Upper, vec![]),
        (Method::Lower, vec![]),
        (Method::Trim, vec![]),
        (Method::Escape, vec![]),
        (Method::Repeat, vec![Value::Int(1)]),
        (Method::Substring, vec![Value::Int(0), Value::Int(99)]),
    ] {
        let debt = heap.allocation_debt();
        assert!(matches!(result(&mut heap, source, method, &args), Value::Str(id) if id == source));
        assert_eq!(heap.allocation_debt(), debt);
    }
    let empty = Value::Str(heap.alloc_string([]));
    let debt = heap.allocation_debt();
    for (left, right) in [(empty, Value::Str(source)), (Value::Str(source), empty)] {
        assert!(
            matches!(value::add_in(&mut heap, left, right).unwrap(), Value::Str(id) if id == source)
        );
    }
    assert_eq!(heap.allocation_debt(), debt);
    heap.collect([Value::Str(source)]);
    assert_eq!(heap.string(source).unwrap(), [0x4e2d, 0xd800, 0x6587]);
    assert!(value::add_in(&mut heap, empty, Value::Str(source)).is_err());
}

#[test]
fn reuse_keeps_each_methods_distinct_nul_contract() {
    let mut heap = Heap::new();
    let source = heap.alloc_string([65, 0, 66]);
    for (method, args, expected) in [
        (Method::Upper, vec![], vec![65]),
        (Method::Lower, vec![], vec![97]),
        (Method::Escape, vec![], vec![65]),
        (Method::Trim, vec![], vec![65, 0, 66]),
        (Method::Repeat, vec![Value::Int(1)], vec![65, 0, 66]),
        (Method::Substring, vec![Value::Int(0)], vec![65]),
        (
            Method::Substring,
            vec![Value::Int(0), Value::Int(3)],
            vec![65, 0, 66],
        ),
    ] {
        let Value::Str(id) = result(&mut heap, source, method, &args) else {
            panic!()
        };
        assert_eq!(heap.string(id).unwrap(), expected);
    }
    let nul = heap.alloc_string([0, 65]);
    let Value::Str(id) = result(&mut heap, nul, Method::Trim, &[]) else {
        panic!()
    };
    assert!(heap.string(id).unwrap().is_empty());
    let long = heap.alloc_string(vec![65; 4096]);
    assert!(matches!(
        call(&mut heap, long, Method::Upper, &[], true).unwrap(),
        NativeStep::Continue(_)
    ));
}

#[test]
fn string_dispatch_and_short_or_spilled_search_tables() {
    for (name, method) in [
        ("charAt", Method::CharAt),
        ("indexOf", Method::IndexOf),
        ("toUpperCase", Method::Upper),
        ("toLowerCase", Method::Lower),
        ("substring", Method::Substring),
        ("substr", Method::Substring),
        ("trim", Method::Trim),
        ("reverse", Method::Reverse),
        ("repeat", Method::Repeat),
        ("escape", Method::Escape),
        ("sprintf", Method::Sprintf),
    ] {
        let units: Vec<_> = name.encode_utf16().collect();
        assert_eq!(
            std::mem::discriminant(&Method::from_name(&units).unwrap()),
            std::mem::discriminant(&method)
        );
        assert!(Method::from_name(&[units, vec![0]].concat()).is_none());
    }
    let mut heap = Heap::new();
    for (source, needle, expected) in [
        ("abababac", "ababac", 2),
        ("xxabcdefghijklmnopqrstuv", "abcdefghijklmnopqrstuv", 2),
        ("abababab", "abac", -1),
    ] {
        let source = heap.alloc_string(source.encode_utf16().collect::<Vec<_>>());
        let needle = heap.alloc_string(needle.encode_utf16().collect::<Vec<_>>());
        assert_eq!(
            result(&mut heap, source, Method::IndexOf, &[Value::Str(needle)]).as_integer(),
            Some(expected)
        );
    }
}
