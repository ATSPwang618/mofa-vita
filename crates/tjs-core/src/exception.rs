//! Exception has ordinary mutable message/trace members, not property wrappers.
use crate::{Heap, NativeClass, NativeCx, NativeResult, NativeStorage, ObjId, ObjRef, Value};

pub static CLASS: NativeClass = NativeClass {
    name: "Exception",
    doc: "TJS script exception",
    storage: NativeStorage::Object,
    constructor: crate::NativeCallable::Leaf(constructor),
    constructor_class_only: false,
    initialize: |heap, object| heap.initialize_native_state(object, ()),
    invalidate: None,
    literal: None,
    methods: &[crate::NativeMethod {
        name: "finalize",
        hidden: false,
        doc: "",
        class_only: false,
        call: crate::NativeCallable::EmptyFinalizer(|_, _| Ok(Value::Void)),
    }],
    properties: &[],
};

pub fn install(heap: &mut Heap) -> NativeResult<ObjId> {
    heap.register_class(&CLASS)
}

/// Report stored exception text without running getters, missing hooks or script
/// conversions while unwinding. The thrown value itself is left untouched.
pub(crate) fn describe(heap: &mut Heap, value: Value) -> String {
    let mut text = heap.display(value).expect("pending exception is rooted");
    if let Value::Obj(ObjRef {
        object: Some(object),
        ..
    }) = value
    {
        for name in ["message", "trace"] {
            let key = heap.intern_str(name);
            if let Ok(Some(Value::Str(id))) = heap.member(object, key) {
                if let Ok(field) = heap.display(Value::Str(id)) {
                    if name == "message" {
                        text = field;
                    } else if !field.is_empty() {
                        text.push_str("\nException.trace: ");
                        text.push_str(&field);
                    }
                }
            }
        }
    }
    text
}

fn constructor(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let object = cx.this();
    let empty = Value::Str(cx.heap_mut().alloc_string(Vec::new()));
    let argument = |index| {
        args.get(index)
            .copied()
            .filter(|v| !matches!(v, Value::Void))
            .unwrap_or(empty)
    };
    set_fields(cx.heap_mut(), object, argument(0), argument(1))
}

fn create(heap: &mut Heap, class: ObjId, message: Value, trace: Value) -> NativeResult<Value> {
    let object = heap.alloc_native(class, ())?;
    set_fields(heap, object, message, trace)
}

fn set_fields(heap: &mut Heap, object: ObjId, message: Value, trace: Value) -> NativeResult<Value> {
    for (name, value) in [("message", message), ("trace", trace)] {
        let key = heap.intern_str(name);
        heap.set_member(object, key, value)?;
    }
    Ok(Value::Obj(ObjRef::bound(object)))
}

pub(crate) fn runtime(heap: &mut Heap, diagnostic: &crate::Diagnostic) -> NativeResult<Value> {
    let class = install(heap)?;
    let message =
        Value::Str(heap.alloc_string(diagnostic.message.encode_utf16().collect::<Vec<_>>()));
    let trace = diagnostic
        .trace
        .iter()
        .map(|frame| format!("{}:{}", frame.function, frame.pc))
        .collect::<Vec<_>>()
        .join("\n");
    let trace = Value::Str(heap.alloc_string(trace.encode_utf16().collect::<Vec<_>>()));
    create(heap, class, message, trace)
}
