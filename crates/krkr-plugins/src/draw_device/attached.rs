//! Legacy bookkeeping on ordinary Layer objects; the reference compositor
//! stores these independently of D3DLayer's ordering and does not read them.
use krkr_engine::{
    extensions,
    plugins::{Context, Exports},
};
use tjs_core::{NativeCx, NativeProperty, NativeResult, Value};
#[derive(Default, tjs_bind::Trace)]
struct State {
    plane: i32,
    front: i32,
    back: i32,
}
fn state<T>(cx: &mut NativeCx<'_>, f: impl FnOnce(&mut State) -> T) -> NativeResult<T> {
    let owner = cx.this();
    extensions::layer_size(cx, Value::Obj(owner.into()))?;
    cx.heap_mut().initialize_native_default::<State>(owner)?;
    cx.heap_mut().with_native_state::<State, _>(owner, f)
}
macro_rules! property {
    ($get:ident, $set:ident, $field:ident) => {
        #[tjs_bind::function]
        fn $get(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            state(cx, |s| i64::from(s.$field))
        }
        #[tjs_bind::function]
        fn $set(cx: &mut NativeCx<'_>, #[tjs(coerce)] value: i32) -> NativeResult<()> {
            state(cx, |s| s.$field = value)
        }
    };
}
property!(plane, set_plane, plane);
property!(front, set_front, front);
property!(back, set_back, back);
static PROPERTIES: &[NativeProperty] = &[
    NativeProperty {
        name: "drawPlane",
        doc: "D3D layer plane",
        hidden: false,
        class_only: false,
        get: Some(plane::CALL),
        set: Some(set_plane::CALL),
    },
    NativeProperty {
        name: "frontIndex",
        doc: "D3D front ordering",
        hidden: false,
        class_only: false,
        get: Some(front::CALL),
        set: Some(set_front::CALL),
    },
    NativeProperty {
        name: "backIndex",
        doc: "D3D back ordering",
        hidden: false,
        class_only: false,
        get: Some(back::CALL),
        set: Some(set_back::CALL),
    },
];
pub(super) fn install(cx: &mut Context<'_>, exports: &mut Exports) -> NativeResult<()> {
    let layer = crate::exports::class(cx, "Layer")?;
    for property in PROPERTIES {
        exports.property(cx, layer, property)?;
    }
    Ok(())
}
