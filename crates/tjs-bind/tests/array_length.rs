use tjs_core::{Value, member};

#[test]
fn reading_array_length_between_gc_slices_does_not_retrace_the_array() {
    let mut heap = tjs_bind::new_heap();
    let array = heap.alloc_array();
    for _ in 0..200 {
        let child = heap.alloc_object();
        heap.array_push(array, Value::Obj(child.into())).unwrap();
    }
    let key = Value::Str(heap.alloc_string("count".encode_utf16().collect::<Vec<_>>()));
    let owner = Value::Obj(array.into());
    let mut completed = None;
    for _ in 0..10_000 {
        assert_eq!(
            member::get(&mut heap, owner, key).unwrap().as_integer(),
            Some(200)
        );
        completed = heap.collect_step([owner, key], 1).completed;
        if completed.is_some() {
            break;
        }
    }
    assert!(completed.is_some(), "length reads prevented GC progress");
    for child in heap.array(array).unwrap() {
        let Value::Obj(child) = child else {
            panic!("array element")
        };
        heap.object(child.object.unwrap()).unwrap();
    }
}
