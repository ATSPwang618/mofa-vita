use tjs_core::{Heap, Value};

#[test]
fn inspection_checks_the_native_type_and_object_lifetime() {
    let mut heap = Heap::new();
    let owner = heap.alloc_object();
    assert!(
        heap.inspect_native_state::<Value, _>(owner, |_| ())
            .is_err()
    );
    heap.initialize_native_state(owner, Value::Int(7)).unwrap();
    assert_eq!(
        heap.inspect_native_state::<Value, _>(owner, |value| value.as_integer())
            .unwrap(),
        Some(7)
    );
    assert!(
        heap.inspect_native_state::<Vec<Value>, _>(owner, |_| ())
            .is_err()
    );
    heap.with_native_state::<Value, _>(owner, |value| *value = Value::Int(9))
        .unwrap();
    assert_eq!(
        heap.inspect_native_state::<Value, _>(owner, |value| value.as_integer())
            .unwrap(),
        Some(9)
    );
    heap.collect([]);
    let replacement = heap.alloc_object();
    heap.initialize_native_state(replacement, Value::Int(11))
        .unwrap();
    assert!(
        heap.inspect_native_state::<Value, _>(owner, |_| ())
            .is_err()
    );
}
