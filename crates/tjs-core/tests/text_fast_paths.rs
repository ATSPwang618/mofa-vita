use tjs_core::{Heap, Value, value};

#[test]
fn bulk_array_removal_preserves_every_surviving_value() {
    let mut heap = Heap::new();
    let id = heap.alloc_array();
    for mask in 0..256 {
        let original: Vec<_> = (0..8).map(Value::Int).collect();
        heap.array_replace(id, original).unwrap();
        let removed: Vec<_> = (0..8).filter(|&i| mask & (1 << i) != 0).collect();
        heap.array_remove_indices(id, &removed).unwrap();
        let expected: Vec<_> = (0..8).filter(|&i| mask & (1 << i) == 0).collect();
        assert_eq!(
            heap.array(id)
                .unwrap()
                .iter()
                .map(|v| v.as_integer().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
    }
    heap.array_replace(id, vec![Value::Int(7); 4]).unwrap();
    for invalid in [&[1, 1][..], &[2, 1], &[4]] {
        assert!(heap.array_remove_indices(id, invalid).is_err());
        assert_eq!(heap.array(id).unwrap().len(), 4);
    }
}

#[test]
fn utf8_interning_handles_unicode_long_names_and_collected_symbols() {
    let mut heap = Heap::new();
    for name in ["onClick".to_string(), "中文😀".into(), "x".repeat(200)] {
        let units: Vec<_> = name.encode_utf16().collect();
        let id = heap.intern_str(&name);
        assert_eq!(heap.intern(&units), id);
        heap.collect([]);
        let new = heap.intern_str(&name);
        assert_eq!(heap.symbol(new).unwrap(), units);
    }
}

#[test]
fn string_equality_keeps_embedded_nul_length_and_stale_id_checks() {
    let mut heap = Heap::new();
    for (a, b, expected) in [
        (&[65, 0, 66][..], &[65, 0, 67][..], true),
        (&[65, 0, 66], &[65, 0], false),
        (&[65, 66], &[65, 67], false),
        (&[0xd800], &[0xd800], true),
    ] {
        let a = Value::Str(heap.alloc_string(a));
        let b = Value::Str(heap.alloc_string(b));
        assert_eq!(value::equal(&heap, a, b).unwrap(), expected);
        assert!(value::equal(&heap, a, a).unwrap());
    }
    let stale = Value::Str(heap.alloc_string([65, 66, 67]));
    heap.collect([]);
    assert!(value::equal(&heap, stale, stale).is_err());
}

#[test]
fn direct_append_preserves_full_string_and_numeric_representation() {
    let mut heap = Heap::new();
    let text = Value::Str(heap.alloc_string([65, 0, 0xd800, 66]));
    for value in [
        text,
        Value::Int(i64::MIN),
        Value::Int(i64::MAX),
        Value::Void,
        Value::Real(-0.),
        Value::Real(f64::NAN),
        Value::Real(f64::INFINITY),
        Value::Real(1.23456789),
    ] {
        let mut output = vec![17];
        value::append_string_units(&heap, value, &mut output).unwrap();
        assert_eq!(output[0], 17);
        assert_eq!(&output[1..], value::to_string_units(&heap, value).unwrap());
    }
}
