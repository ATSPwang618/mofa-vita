use super::*;
use crate::{NativeCallable, NativeCx, NativeProperty, NativeResult};

#[test]
fn ordinary_and_raw_writes_replace_slot_flags() {
    let mut heap = Heap::new();
    let owner = heap.alloc_object();
    let name = heap.intern_str("slot");
    let key = Value::Str(heap.alloc_string("slot".encode_utf16().collect::<Vec<_>>()));
    let text = Value::Str(heap.alloc_string(vec![65]));
    let object = Value::Obj(heap.alloc_object().into());
    for raw in [false, true] {
        for old in [Value::Void, Value::Int(1), Value::Real(2.), text, object] {
            heap.set_member_flags(owner, name, old, true, true).unwrap();
            set_flags(
                &mut heap,
                Value::Obj(owner.into()),
                key,
                Value::Int(7),
                true,
                raw,
                (false, false),
            )
            .unwrap();
            let (value, hidden, class_only) = heap.member_with_flags(owner, name).unwrap().unwrap();
            assert_eq!(value.as_integer(), Some(7));
            assert!(!hidden && !class_only);
        }
    }
}

fn failing_setter(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    let name = cx
        .heap()
        .find_symbol(&"slot".encode_utf16().collect::<Vec<_>>())
        .unwrap();
    let (_, hidden, class_only) = cx.heap().member_with_flags(cx.this(), name)?.unwrap();
    assert!(hidden && class_only, "flags must precede the setter");
    Err(NativeError::Detail("setter failed".into()))
}

#[test]
fn setter_failure_keeps_updated_flags_and_raw_write_bypasses_setter() {
    static PROPERTY: NativeProperty = NativeProperty {
        hidden: false,
        class_only: false,
        name: "slot",
        doc: "",
        get: None,
        set: Some(NativeCallable::Leaf(failing_setter)),
    };
    let mut heap = Heap::new();
    let owner = heap.alloc_object();
    let name = heap.intern_str("slot");
    let key = Value::Str(heap.alloc_string("slot".encode_utf16().collect::<Vec<_>>()));
    let property = Value::Obj(heap.alloc_native_property(&PROPERTY).into());
    heap.set_member(owner, name, property).unwrap();
    let result = set_flags(
        &mut heap,
        Value::Obj(owner.into()),
        key,
        Value::Int(9),
        true,
        false,
        (true, true),
    );
    assert!(matches!(result, Err(MemberError::Native(_))));
    let (old, hidden, class_only) = heap.member_with_flags(owner, name).unwrap().unwrap();
    assert!(matches!(old, Value::Obj(_)));
    assert!(hidden && class_only);
    set_flags(
        &mut heap,
        Value::Obj(owner.into()),
        key,
        Value::Int(9),
        true,
        true,
        (false, false),
    )
    .unwrap();
    let (value, hidden, class_only) = heap.member_with_flags(owner, name).unwrap().unwrap();
    assert_eq!(value.as_integer(), Some(9));
    assert!(!hidden && !class_only);
}
