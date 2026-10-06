//! KAGParserEx's independent native class and ordered parameter-macro state.
use super::*;
use std::sync::LazyLock;
use tjs_core::{NativeCallable, NativeClass, NativeProperty};

static EX_CLASS: LazyLock<NativeClass> = LazyLock::new(|| {
    let base = bindings::implementation::CLASS;
    let mut properties = base.properties.to_vec();
    properties.extend([
        NativeProperty {
            hidden: false,
            class_only: false,
            name: "paramMacros",
            doc: "",
            get: Some(NativeCallable::Leaf(param_macros)),
            set: None,
        },
        NativeProperty {
            hidden: false,
            class_only: false,
            name: "multiLineTagEnabled",
            doc: "",
            get: Some(NativeCallable::Leaf(multiline)),
            set: Some(NativeCallable::Leaf(set_multiline)),
        },
    ]);
    NativeClass {
        initialize,
        properties: Box::leak(properties.into_boxed_slice()),
        ..base
    }
});
fn initialize(heap: &mut Heap, object: ObjId) -> NativeResult<()> {
    heap.initialize_native_default::<State>(object)?;
    heap.with_native_state::<State, _>(object, |s| s.extended = true)
}
pub(super) fn install(heap: &mut Heap) -> NativeResult<ObjId> {
    heap.register_class_variant("krkr.plugins.KAGParserEx", &EX_CLASS)
}
fn param_macros(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    cx.with_state::<State, _>(|s, _| {
        Ok(Value::Obj(ObjRef::bound(
            s.param_macros.ok_or(NativeError::This)?,
        )))
    })
}
fn multiline(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    cx.with_state::<State, _>(|s, _| Ok(Value::Int(i64::from(s.parser.multiline_tags))))
}
fn set_multiline(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let value = args
        .first()
        .copied()
        .ok_or(NativeError::Missing(0))?
        .truthy(cx.heap())?;
    cx.with_state::<State, _>(|s, _| {
        s.parser.multiline_tags = value;
        Ok(Value::Void)
    })
}
