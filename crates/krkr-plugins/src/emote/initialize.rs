//! Reference process globals are scoped to one VM heap, including the rooted
//! work-layer adaptor. Script accessors retain ordinary continuation semantics.
use super::{adaptor, manager};
use krkr_engine::plugins;
use tjs_core::{NativeContinuation, NativeCx, NativeResult, NativeStep, ObjId, Value, value};

#[derive(Default, tjs_bind::Trace)]
pub(super) struct Runtime {
    pub window: Value,
    pub work_layer: Value,
    pub decrypt_seed: i32,
    pub decrypt_callback: Value,
}
pub(super) fn runtime<T>(
    cx: &mut NativeCx<'_>,
    f: impl FnOnce(&mut Runtime) -> T,
) -> NativeResult<T> {
    let global = plugins::global(cx)?;
    cx.heap_mut().initialize_native_default::<Runtime>(global)?;
    cx.heap_mut().with_native_state::<Runtime, _>(global, f)
}
pub(super) fn start(cx: &mut NativeCx<'_>, window: Value) -> NativeResult<NativeStep> {
    let global = plugins::global(cx)?;
    cx.heap_mut().initialize_native_default::<Runtime>(global)?;
    Box::new(Init {
        window,
        global,
        phase: 0,
    })
    .resume(cx, Value::Void)
}
#[derive(tjs_bind::Trace)]
struct Init {
    window: Value,
    global: ObjId,
    phase: u8,
}
impl Init {
    fn finish(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        cx.construct(manager::bindings::State {
            window: self.window,
            ..Default::default()
        })
        .map(NativeStep::Return)
    }
    fn get(self: Box<Self>, cx: &mut NativeCx<'_>, name: &str) -> NativeResult<NativeStep> {
        let key = Value::Str(
            cx.heap_mut()
                .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
        );
        Ok(NativeStep::GetOr {
            object: self.window,
            key,
            raw: false,
            fallback: Value::Void,
            continuation: self,
        })
    }
}
impl NativeContinuation for Init {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<NativeStep> {
        let phase = self.phase;
        self.phase += 1;
        match phase {
            0 => {
                if !matches!(self.window, Value::Obj(r) if r.object.is_some()) {
                    return self.finish(cx);
                }
                self.get(cx, "width")
            }
            1 => {
                value::to_integer(cx.heap(), v)?;
                self.get(cx, "height")
            }
            2 => {
                value::to_integer(cx.heap(), v)?;
                let initialized =
                    cx.heap_mut()
                        .with_native_state::<Runtime, _>(self.global, |s| {
                            s.window = self.window;
                            !matches!(s.work_layer, Value::Void)
                        })?;
                if initialized {
                    return self.finish(cx);
                }
                self.get(cx, "poolLayer")
            }
            3 => {
                if !matches!(v, Value::Obj(r) if r.object.is_some()) {
                    return self.finish(cx);
                }
                let class = adaptor::bindings::install(cx.heap_mut())?;
                Ok(NativeStep::Construct {
                    class: Value::Obj(class.into()),
                    arguments: vec![v],
                    continuation: self,
                })
            }
            4 => {
                cx.heap_mut()
                    .with_native_state::<Runtime, _>(self.global, |s| s.work_layer = v)?;
                let key = Value::Str(
                    cx.heap_mut()
                        .alloc_string("motionWorkLayer".encode_utf16().collect::<Vec<_>>()),
                );
                Ok(NativeStep::Set {
                    object: Value::Obj(self.global.into()),
                    key,
                    value: v,
                    continuation: self,
                })
            }
            _ => self.finish(cx),
        }
    }
}
