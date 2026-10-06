use super::*;

#[test]
fn deleted_and_cleared_class_members_do_not_reuse_old_snapshots() {
    let mut heap = Heap::new();
    let class = heap.alloc_object();
    let first = heap.intern_str("first");
    let second = heap.intern_str("second");
    heap.set_member(class, first, Value::Int(1)).unwrap();
    heap.set_member(class, second, Value::Int(2)).unwrap();
    let original = heap.alloc_object();
    heap.copy_native_members(class, original).unwrap();
    heap.remove_member(class, first).unwrap();
    let after_delete = heap.alloc_object();
    heap.copy_native_members(class, after_delete).unwrap();
    assert!(heap.member(after_delete, first).unwrap().is_none());
    assert_eq!(
        heap.member(after_delete, second)
            .unwrap()
            .unwrap()
            .as_integer(),
        Some(2)
    );
    heap.clear_members(class).unwrap();
    let after_clear = heap.alloc_object();
    heap.copy_native_members(class, after_clear).unwrap();
    assert!(heap.member(after_clear, second).unwrap().is_none());
    heap.set_member(class, first, Value::Int(3)).unwrap();
    let after_write = heap.alloc_object();
    heap.copy_native_members(class, after_write).unwrap();
    assert_eq!(
        heap.member(after_write, first)
            .unwrap()
            .unwrap()
            .as_integer(),
        Some(3)
    );
    assert_eq!(
        heap.member(original, first).unwrap().unwrap().as_integer(),
        Some(1)
    );
}

#[test]
fn overwritten_shared_members_return_bound_values_and_tombstones_stay_absent() {
    let mut heap = Heap::new();
    let class = heap.alloc_object();
    let callable = heap.alloc_object();
    let name = heap.intern(&[65]);
    heap.set_member(class, name, Value::Obj(callable.into()))
        .unwrap();
    let instance = heap.alloc_object();
    heap.copy_native_members(class, instance).unwrap();
    let old = heap
        .set_member_flags(instance, name, Value::Int(7), true, false)
        .unwrap();
    let Some(Value::Obj(reference)) = old else {
        panic!("bound closure")
    };
    assert_eq!(reference.object, Some(callable));
    assert_eq!(reference.this, Some(instance));
    assert!(heap.member_with_flags(instance, name).unwrap().unwrap().1);
    let debt = heap.allocation_debt();
    assert_eq!(
        heap.set_member(instance, name, Value::Int(8))
            .unwrap()
            .unwrap()
            .as_integer(),
        Some(7)
    );
    assert_eq!(heap.allocation_debt(), debt);
    let deleted = heap.alloc_object();
    heap.copy_native_members(class, deleted).unwrap();
    assert!(heap.remove_member(deleted, name).unwrap().is_some());
    assert!(
        heap.set_member(deleted, name, Value::Int(9))
            .unwrap()
            .is_none()
    );
    heap.set_member(class, name, Value::Int(11)).unwrap();
    let next = heap.alloc_object();
    heap.copy_native_members(class, next).unwrap();
    assert_eq!(
        heap.member(next, name).unwrap().unwrap().as_integer(),
        Some(11)
    );
    heap.collect([
        Value::Obj(instance.into()),
        Value::Obj(deleted.into()),
        Value::Obj(next.into()),
    ]);
    assert_eq!(
        heap.member(instance, name).unwrap().unwrap().as_integer(),
        Some(8)
    );
}
