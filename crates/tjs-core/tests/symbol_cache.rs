use tjs_core::{Heap, HeapCounts, HeapError, Value, member, value};

fn text(heap: &mut Heap, name: &str) -> Value {
    Value::Str(heap.alloc_string(name.encode_utf16().collect::<Vec<_>>()))
}

fn collect(heap: &mut Heap, roots: &[Value], incremental: bool) {
    if incremental {
        for _ in 0..100_000 {
            if heap
                .collect_step(roots.iter().copied(), 1)
                .completed
                .is_some()
            {
                return;
            }
        }
        panic!("collection did not finish");
    }
    heap.collect(roots.iter().copied());
}

#[test]
fn cached_names_do_not_keep_symbols_alive_or_return_reused_symbols() {
    for incremental in [false, true] {
        let mut heap = Heap::new();
        let object = Value::Obj(heap.alloc_dictionary().into());
        let key = text(&mut heap, "position");
        member::set(&mut heap, object, key, Value::Int(17)).unwrap();
        assert_eq!(
            member::get(&mut heap, object, key).unwrap().as_integer(),
            Some(17)
        );
        member::delete(&mut heap, object, key).unwrap();
        collect(&mut heap, &[object, key], incremental);
        assert_eq!(heap.counts().symbols, 0);
        let other = text(&mut heap, "unrelated");
        member::set(&mut heap, object, other, Value::Int(91)).unwrap();
        assert!(matches!(
            member::get(&mut heap, object, key).unwrap(),
            Value::Void
        ));
        member::set(&mut heap, object, key, Value::Int(23)).unwrap();
        assert_eq!(
            member::get(&mut heap, object, key).unwrap().as_integer(),
            Some(23)
        );
        assert_eq!(
            member::get(&mut heap, object, other).unwrap().as_integer(),
            Some(91)
        );
        collect(&mut heap, &[], incremental);
        assert_eq!(heap.counts(), HeapCounts::default());
    }
}

#[test]
fn stale_strings_remain_invalid_while_their_cached_symbol_survives() {
    for incremental in [false, true] {
        let mut heap = Heap::new();
        let object = heap.alloc_dictionary();
        let id = heap.alloc_string(vec![65]);
        let symbol = heap.intern_string(id).unwrap();
        heap.set_member(object, symbol, Value::Int(37)).unwrap();
        collect(&mut heap, &[Value::Obj(object.into())], incremental);
        assert_eq!(heap.symbol(symbol).unwrap(), &[65]);
        assert_eq!(heap.intern_string(id), Err(HeapError::StaleString));
        let replacement = heap.alloc_string(vec![66]);
        let replacement_symbol = heap.intern_string(replacement).unwrap();
        assert_ne!(replacement_symbol, symbol);
        assert_eq!(heap.symbol(replacement_symbol).unwrap(), &[66]);
        assert_eq!(heap.intern_string(id), Err(HeapError::StaleString));
    }
}

#[test]
fn cached_prefixes_nul_names_and_collisions_preserve_member_semantics() {
    let mut heap = Heap::new();
    let object = Value::Obj(heap.alloc_dictionary().into());
    let original = text(&mut heap, &"a".repeat(300));
    let suffix = text(&mut heap, "b");
    member::set(&mut heap, object, original, Value::Int(11)).unwrap();
    let extended = value::add_in(&mut heap, original, suffix).unwrap();
    let fork = value::add_in(&mut heap, original, original).unwrap();
    member::set(&mut heap, object, extended, Value::Int(12)).unwrap();
    member::set(&mut heap, object, fork, Value::Int(13)).unwrap();
    let nul_name = text(&mut heap, "x\0ignored");
    let short_name = text(&mut heap, "x");
    member::set(&mut heap, object, nul_name, Value::Int(21)).unwrap();
    assert_eq!(
        member::get(&mut heap, object, short_name)
            .unwrap()
            .as_integer(),
        Some(21)
    );
    member::set(&mut heap, object, short_name, Value::Int(22)).unwrap();
    // More simultaneously live keys than the bounded cache can contain.
    let keys: Vec<_> = (0..4096)
        .map(|i| text(&mut heap, &format!("field{i}")))
        .collect();
    for (i, &key) in keys.iter().enumerate() {
        member::set(&mut heap, object, key, Value::Int(i as i64)).unwrap();
    }
    for _ in 0..2 {
        for (i, &key) in keys.iter().enumerate() {
            assert_eq!(
                member::get(&mut heap, object, key).unwrap().as_integer(),
                Some(i as i64)
            );
        }
        for (key, expected) in [(original, 11), (extended, 12), (fork, 13), (nul_name, 22)] {
            assert_eq!(
                member::get(&mut heap, object, key).unwrap().as_integer(),
                Some(expected)
            );
        }
    }
}
