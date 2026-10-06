use tjs_core::{CollectionPhase, Heap, HeapCounts, Value, value};

fn append(heap: &mut Heap, left: Value, right: Value) -> Value {
    value::add_in(heap, left, right).unwrap()
}
fn units(heap: &Heap, value: Value) -> Vec<u16> {
    let Value::Str(id) = value else {
        panic!("expected string")
    };
    heap.string(id).unwrap().to_vec()
}

#[test]
fn repeated_appends_self_aliases_and_forks_preserve_utf16_snapshots_through_gc() {
    let mut heap = Heap::new();
    let seed = [65, 0, 0xd800, 0xdc00].repeat(80);
    let initial = Value::Str(heap.alloc_string(seed.clone()));
    let suffix = Value::Str(heap.alloc_string(vec![66, 0, 0xdc00]));
    let mut snapshots = vec![(initial, seed.clone())];
    let mut current = initial;
    let mut expected = seed;
    for _ in 0..80 {
        current = append(&mut heap, current, suffix);
        expected.extend_from_slice(&[66, 0, 0xdc00]);
        snapshots.push((current, expected.clone()));
    }
    current = append(&mut heap, current, current);
    expected.extend_from_within(..);
    snapshots.push((current, expected.clone()));
    current = append(&mut heap, current, initial);
    expected.extend_from_slice(&snapshots[0].1);
    snapshots.push((current, expected.clone()));
    // Forking an older prefix must not modify the active append chain.
    let fork = append(&mut heap, snapshots[1].0, suffix);
    snapshots.push((fork, snapshots[2].1.clone()));
    // A prefix owned by another buffer follows the general append path.
    let other = Value::Str(heap.alloc_string(vec![90; 300]));
    let other = append(&mut heap, other, suffix);
    current = append(&mut heap, current, other);
    expected.extend(units(&heap, other));
    snapshots.push((current, expected));
    let roots: Vec<_> = snapshots.iter().map(|(value, _)| *value).collect();
    for _ in 0..10000 {
        if heap.collect_step(roots.iter().copied(), 1).phase == CollectionPhase::Idle {
            break;
        }
    }
    assert!(!heap.is_collecting());
    heap.collect(roots);
    for (snapshot, expected) in snapshots {
        assert_eq!(units(&heap, snapshot), expected);
    }
    assert_eq!(heap.collect([]).after, HeapCounts::default());
}
