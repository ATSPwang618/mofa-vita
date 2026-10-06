use std::{cell::RefCell, rc::Rc};
use tjs_core::{CollectionPhase, CollectionStats, Heap, HeapCounts, ObjId, ObjRef, Trace, Value};

fn object(id: ObjId) -> Value {
    Value::Obj(id.into())
}

fn finish(heap: &mut Heap, roots: &[Value], budget: usize) -> CollectionStats {
    for _ in 0..100_000 {
        let step = heap.collect_step(roots.iter().copied(), budget);
        assert!(step.work <= budget);
        if let Some(stats) = step.completed {
            assert_eq!(step.phase, CollectionPhase::Idle);
            return stats;
        }
    }
    panic!("collector did not complete");
}

#[test]
fn tiny_slices_match_graph_reachability_and_reuse_sparse_arenas() {
    for budget in [1, 3, 17] {
        for seed in 0..16 {
            let mut heap = Heap::new();
            let ids: Vec<_> = (0..96).map(|_| heap.alloc_array()).collect();
            let graph: Vec<Vec<usize>> = (0..96)
                .map(|i| {
                    (0..3)
                        .filter(|j| (i + j + seed) % 4 != 0)
                        .map(|j| (i * 7 + j * 11 + seed) % 96)
                        .collect()
                })
                .collect();
            for (i, edges) in graph.iter().enumerate() {
                for &edge in edges {
                    heap.array_push(ids[i], object(ids[edge])).unwrap();
                }
            }
            let mut live = [false; 96];
            let mut todo = vec![seed];
            while let Some(i) = todo.pop() {
                if !std::mem::replace(&mut live[i], true) {
                    todo.extend(&graph[i]);
                }
            }
            finish(&mut heap, &[object(ids[seed])], budget);
            for (i, id) in ids.iter().enumerate() {
                assert_eq!(id.downgrade().upgrade(&heap).is_some(), live[i]);
            }
            assert_eq!(finish(&mut heap, &[], budget).after, HeapCounts::default());
            // Exercise reused SlotMap slots and the dense sweep key list.
            for _ in 0..100 {
                heap.alloc_object();
            }
            assert_eq!(finish(&mut heap, &[], budget).after, HeapCounts::default());
        }
    }
}

#[test]
fn black_container_accepts_an_untraced_edge_removed_from_a_gray_container() {
    for members in [false, true] {
        let mut heap = Heap::new();
        let target = heap.alloc_array();
        let source = heap.alloc_array();
        let child = heap.alloc_array();
        let context = heap.alloc_object();
        let text = heap.alloc_string(vec![0xd800, 0, 97]);
        let bytes = heap.alloc_octet(vec![0, 255]);
        heap.array_push(child, Value::Str(text)).unwrap();
        heap.array_push(child, Value::Octet(bytes)).unwrap();
        let edge = Value::Obj(ObjRef {
            object: Some(child),
            this: Some(context),
        });
        heap.array_push(source, edge).unwrap();
        let step = heap.collect_step([object(source), object(target)], 1);
        assert_eq!(step.phase, CollectionPhase::Mark);
        // target is black, source is gray, and its descendants are white.
        heap.array_resize(source, 0).unwrap();
        if members {
            let key = heap.intern(&[88]);
            heap.set_member(target, key, edge).unwrap();
        } else {
            heap.array_replace(target, vec![edge]).unwrap();
        }
        finish(&mut heap, &[object(target)], 1);
        assert!(heap.object(child).is_ok());
        assert!(heap.object(context).is_ok());
        assert_eq!(heap.string(text).unwrap(), &[0xd800, 0, 97]);
        assert_eq!(heap.octet(bytes).unwrap(), &[0, 255]);
        heap.collect([]);
        assert_eq!(heap.counts(), HeapCounts::default());
    }
}

struct Shared(Rc<RefCell<Value>>);
impl Trace for Shared {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(*self.0.borrow());
    }
}

#[test]
fn externally_mutated_native_edges_are_revisited_without_a_heap_write() {
    let mut heap = Heap::new();
    let owner = heap.alloc_object();
    let source = heap.alloc_array();
    let child = heap.alloc_array();
    let text = heap.alloc_string(vec![42]);
    heap.array_push(child, Value::Str(text)).unwrap();
    heap.array_push(source, object(child)).unwrap();
    let shared = Rc::new(RefCell::new(Value::Void));
    heap.initialize_native_state(owner, Shared(shared.clone()))
        .unwrap();
    heap.collect_step([object(source), object(owner)], 1);
    heap.array_resize(source, 0).unwrap();
    *shared.borrow_mut() = object(child);
    finish(&mut heap, &[object(owner)], 1);
    assert_eq!(heap.string(text).unwrap(), &[42]);
    *shared.borrow_mut() = Value::Void;
    finish(&mut heap, &[object(owner)], 1);
    assert!(heap.object(child).is_err());
    assert!(heap.string(text).is_err());
}

#[test]
fn roots_allocations_and_native_updates_during_sweep_survive_and_keep_debt() {
    let mut heap = Heap::new();
    let owner = heap.alloc_object();
    let shared = Rc::new(RefCell::new(Value::Void));
    heap.initialize_native_state(owner, Shared(shared.clone()))
        .unwrap();
    for _ in 0..40 {
        heap.alloc_object();
        heap.alloc_string(vec![1]);
    }
    let root = heap.root(object(owner));
    while heap.collect_step([], 1).phase != CollectionPhase::Sweep {}
    let child = heap.alloc_array();
    let text = heap.alloc_string(vec![11, 12]);
    let octet = heap.alloc_octet(vec![13]);
    heap.array_push(child, Value::Str(text)).unwrap();
    heap.array_push(child, Value::Octet(octet)).unwrap();
    let name = heap.intern(&[14]);
    heap.set_member(child, name, object(child)).unwrap();
    *shared.borrow_mut() = object(child);
    let debt = heap.allocation_debt();
    finish(&mut heap, &[], 1);
    assert_eq!(heap.allocation_debt(), debt);
    assert!(debt > 0);
    assert_eq!(heap.string(text).unwrap(), &[11, 12]);
    assert_eq!(heap.octet(octet).unwrap(), &[13]);
    assert!(heap.symbol(name).is_ok());
    heap.set_root(root, object(child)).unwrap();
    finish(&mut heap, &[], 1);
    assert!(heap.object(owner).is_err());
    heap.release_root(root);
    assert_eq!(finish(&mut heap, &[], 1).after, HeapCounts::default());
}

#[test]
fn full_collection_restarts_each_partial_phase_and_reclaims_floating_garbage() {
    for phase in [
        CollectionPhase::Mark,
        CollectionPhase::Finalizers,
        CollectionPhase::Sweep,
    ] {
        let mut heap = Heap::new();
        let held = heap.alloc_array();
        for _ in 0..20 {
            let child = heap.alloc_object();
            heap.array_push(held, object(child)).unwrap();
        }
        loop {
            let step = heap.collect_step([object(held)], 1);
            assert!(step.completed.is_none());
            if step.phase == phase {
                break;
            }
        }
        assert_eq!(heap.collect([]).after, HeapCounts::default());
        assert!(!heap.is_collecting());
    }
}

#[test]
fn weak_upgrade_cannot_recover_a_partially_swept_graph_and_zero_budget_is_a_noop() {
    let mut heap = Heap::new();
    let parent = heap.alloc_array();
    let child = heap.alloc_object();
    heap.array_push(parent, object(child)).unwrap();
    let debt = heap.allocation_debt();
    assert_eq!(heap.collect_step([], 0).phase, CollectionPhase::Idle);
    assert_eq!(heap.allocation_debt(), debt);
    while heap.collect_step([], 1).phase != CollectionPhase::Sweep {}
    assert!(heap.object(child).is_err());
    assert!(heap.object(parent).is_ok());
    assert!(parent.downgrade().upgrade(&heap).is_none());
    finish(&mut heap, &[], 1);
}

#[test]
fn updates_to_a_large_array_do_not_retrace_the_whole_container() {
    let mut heap = Heap::new();
    let array = heap
        .alloc_array_from(&vec![Value::Int(0); 100_000])
        .unwrap();
    heap.collect_step([object(array)], 1);
    for i in 0..1000 {
        heap.array_set(array, i, Value::Int(i as i64)).unwrap();
    }
    let stats = finish(&mut heap, &[object(array)], 1);
    assert_eq!(stats.traced_objects, 1);
    assert_eq!(heap.array(array).unwrap()[999].as_integer(), Some(999));
}

#[test]
fn native_remark_does_not_scale_with_the_number_of_sweep_slices() {
    use std::cell::Cell;
    struct Count(Rc<Cell<usize>>);
    impl Trace for Count {
        fn trace(&self, _: &mut dyn FnMut(Value)) {
            self.0.set(self.0.get() + 1);
        }
    }
    let mut heap = Heap::new();
    let owner = heap.alloc_object();
    let calls = Rc::new(Cell::new(0));
    heap.initialize_native_state(owner, Count(calls.clone()))
        .unwrap();
    for _ in 0..1000 {
        heap.alloc_object();
    }
    finish(&mut heap, &[object(owner)], 1);
    assert!(
        calls.get() <= 3,
        "native state was scanned {} times",
        calls.get()
    );
    calls.set(0);
    heap.collect([object(owner)]);
    assert_eq!(calls.get(), 1, "full collection needs no native remark");
}

#[test]
fn weak_upgrade_during_marking_retains_the_graph_even_if_the_temporary_is_dropped() {
    let mut heap = Heap::new();
    let held = heap.alloc_object();
    let parent = heap.alloc_array();
    let child = heap.alloc_object();
    heap.array_push(parent, object(child)).unwrap();
    heap.collect_step([object(held)], 1);
    assert_eq!(parent.downgrade().upgrade(&heap), Some(parent));
    finish(&mut heap, &[object(held)], 1);
    assert!(heap.object(parent).is_ok());
    assert!(heap.object(child).is_ok());
    finish(&mut heap, &[object(held)], 1);
    assert!(heap.object(parent).is_err());
    assert!(heap.object(child).is_err());
}
