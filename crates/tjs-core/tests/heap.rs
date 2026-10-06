use tjs_core::{Heap, HeapCounts, ObjRef, ObjectKind, Value, value};

fn object(id: tjs_core::ObjId) -> Value {
    Value::Obj(id.into())
}

#[test]
fn cycles_bound_contexts_containers_and_symbols_are_traced_then_reclaimed() {
    let mut heap = Heap::new();
    let owner = heap.alloc_object();
    let context = heap.alloc_dictionary();
    let array = heap.alloc_array();
    let text = heap.alloc_string(vec![0xd800, 0, 0xdc00]);
    let octet = heap.alloc_octet(vec![0, 255, 7]);
    let name = heap.intern(&[65]);
    assert_eq!(heap.intern(&[65]), name);
    heap.set_member(owner, name, object(array)).unwrap();
    heap.set_member(context, name, object(owner)).unwrap();
    heap.array_push(array, Value::Str(text)).unwrap();
    heap.array_push(array, Value::Octet(octet)).unwrap();
    heap.array_push(array, object(owner)).unwrap();
    let root = heap.root(Value::Obj(ObjRef {
        object: Some(owner),
        this: Some(context),
    }));
    let counts = heap.counts();
    assert_eq!(heap.collect([]).after, counts);
    assert_eq!(heap.string(text).unwrap(), &[0xd800, 0, 0xdc00]);
    assert_eq!(heap.octet(octet).unwrap(), &[0, 255, 7]);
    assert_eq!(heap.symbol(name).unwrap(), &[65]);
    assert_eq!(heap.object(context).unwrap().kind(), ObjectKind::Dictionary);
    assert_eq!(heap.object(owner).unwrap().members().count(), 1);
    assert_eq!(heap.array(array).unwrap().len(), 3);
    let weak = context.downgrade();
    assert_eq!(weak.upgrade(&heap), Some(context));

    // Drop just the context edge; the remaining cycle and its payload stay live.
    heap.set_root(root, object(owner)).unwrap();
    assert_eq!(heap.collect([]).after.objects, 2);
    assert!(weak.upgrade(&heap).is_none());
    heap.release_root(root).unwrap();
    assert!(heap.rooted(root).is_none());
    assert!(heap.set_root(root, Value::Void).is_err());
    assert_eq!(heap.collect([]).after, HeapCounts::default());
    assert!(heap.object(owner).is_err());
    assert!(heap.string(text).is_err());
    assert!(heap.octet(octet).is_err());
    assert!(heap.symbol(name).is_err());
    assert_ne!(heap.intern(&[65]), name); // Weak reverse lookup cannot return a dead ID.
    assert_ne!(heap.alloc_object(), owner);
    assert_ne!(heap.alloc_string(vec![65]), text);
    assert_ne!(heap.alloc_octet(vec![65]), octet);
    assert!(weak.upgrade(&heap).is_none());
}

#[test]
fn deleting_and_overwriting_edges_releases_their_payloads() {
    let mut heap = Heap::new();
    let object_id = heap.alloc_array();
    let name = heap.intern(&[65]);
    let text = heap.alloc_string(vec![97]);
    heap.set_member(object_id, name, Value::Str(text)).unwrap();
    heap.array_push(object_id, Value::Str(text)).unwrap();
    assert!(heap.remove_member(object_id, name).unwrap().is_some());
    heap.collect([object(object_id)]);
    assert!(heap.symbol(name).is_err());
    assert_eq!(heap.string(text).unwrap(), &[97]);
    heap.array_set(object_id, 0, Value::Void).unwrap();
    heap.collect([object(object_id)]);
    assert!(heap.string(text).is_err());
    assert!(heap.array_set(object_id, 1, Value::Void).is_err());
}

#[test]
fn deep_cycles_use_an_iterative_worklist_and_retain_capacity_for_reuse() {
    let mut heap = Heap::new();
    let edge = heap.intern(&[110]);
    let objects: Vec<_> = (0..20_000).map(|_| heap.alloc_object()).collect();
    for (index, &id) in objects.iter().enumerate() {
        heap.set_member(id, edge, object(objects[(index + 1) % objects.len()]))
            .unwrap();
    }
    let capacity = heap.capacities();
    assert_eq!(
        heap.collect([object(objects[0])]).traced_objects,
        objects.len()
    );
    assert_eq!(heap.collect([]).after, HeapCounts::default());
    assert_eq!(heap.capacities(), capacity);
    assert_eq!(heap.allocation_debt(), 0);
}

#[test]
fn collector_matches_an_independent_reachability_model() {
    // A deterministic changing graph includes self edges, cycles and dead components.
    for seed in 0..32 {
        let mut heap = Heap::new();
        let ids: Vec<_> = (0..24).map(|_| heap.alloc_array()).collect();
        let graph: Vec<Vec<usize>> = (0..24)
            .map(|i| {
                (0..3)
                    .filter(|j| (i + j + seed) % 4 != 0)
                    .map(|j| (i * 7 + j * 11 + seed) % 24)
                    .collect()
            })
            .collect();
        for (i, edges) in graph.iter().enumerate() {
            for &edge in edges {
                heap.array_push(ids[i], object(ids[edge])).unwrap();
            }
        }
        let mut reachable = [false; 24];
        reachable[seed % 24] = true;
        loop {
            let before = reachable;
            for (i, edges) in graph.iter().enumerate() {
                if before[i] {
                    for &edge in edges {
                        reachable[edge] = true;
                    }
                }
            }
            if reachable == before {
                break;
            }
        }
        heap.collect([object(ids[seed % 24])]);
        for (i, &id) in ids.iter().enumerate() {
            assert_eq!(
                heap.object(id).is_ok(),
                reachable[i],
                "seed={seed}, node={i}"
            );
        }
    }
}

#[test]
fn value_identity_keeps_context_tracing_separate_from_equality() {
    let mut heap = Heap::new();
    let function = heap.alloc_object();
    let context = heap.alloc_object();
    assert!(
        value::equal(
            &heap,
            object(function),
            Value::Obj(ObjRef {
                object: Some(function),
                this: Some(context),
            })
        )
        .unwrap()
    );
    assert!(!Value::Obj(ObjRef::default()).truthy(&heap).unwrap());
    let empty = heap.alloc_octet(vec![]);
    assert!(!Value::Octet(empty).truthy(&heap).unwrap());
    let full = heap.alloc_octet(vec![1]);
    assert!(Value::Octet(full).truthy(&heap).unwrap());
    let other = heap.alloc_octet(vec![1]);
    assert!(value::equal(&heap, Value::Octet(full), Value::Octet(other)).unwrap());
}
