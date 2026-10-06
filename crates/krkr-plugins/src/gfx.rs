//! Both SDL gfxEffect.cpp and gfxFire.cpp declare gfxEffect.dll and the same
//! diagnostic-only gfxFire class. There is no particle renderer in that source.
use tjs_bind::{RestArgs, flow};
use tjs_core::{NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Value};
krkr_engine::native_plugin! {
    pub(crate) Gfx {
        names: ["gfxEffect.dll", "gfxEffect.tpm"],
        link(cx, exports) {
            let class = fire::install_with_state(cx.heap, fire::State { global: Some(cx.global) })?;
            exports.value(cx, cx.global, "gfxFire", Value::Obj(class.into()))
        }
    }
}
#[tjs_bind::class(name = "gfxFire")]
mod fire {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        pub(super) global: Option<ObjId>,
    }
    impl State {
        #[tjs::constructor(resumable = true)]
        fn create(cx: &mut NativeCx<'_>, _args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let class = cx
                .heap()
                .registered_class("gfxFire")
                .ok_or(NativeError::This)?;
            let global = with_state(cx, class, |s| s.global)?.ok_or(NativeError::This)?;
            log(cx, global, true)
        }
        #[tjs::method(resumable = true)]
        fn finalize(cx: &mut NativeCx<'_>, _args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let global = with_state(cx, cx.this(), |s| s.global)?.ok_or(NativeError::This)?;
            log(cx, global, false)
        }
    }
}
#[derive(tjs_bind::Trace)]
struct Log {
    global: ObjId,
    construct: bool,
}
fn key(cx: &mut NativeCx<'_>, text: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(text.encode_utf16().collect::<Vec<_>>()),
    )
}
fn log(cx: &mut NativeCx<'_>, global: ObjId, construct: bool) -> NativeResult<NativeStep> {
    Ok(NativeStep::Get {
        object: Value::Obj(global.into()),
        key: key(cx, "Debug"),
        continuation: Box::new(Log { global, construct }),
    })
}
impl NativeContinuation for Log {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, debug: Value) -> NativeResult<NativeStep> {
        let message = key(
            cx,
            if self.construct {
                "gfxFire construct"
            } else {
                "gfxFire finalize"
            },
        );
        Ok(NativeStep::CallMember {
            object: debug,
            key: key(cx, "message"),
            arguments: vec![message],
            continuation: flow::callback(*self, |s, cx, _| {
                if s.construct {
                    cx.construct(fire::State {
                        global: Some(s.global),
                    })
                    .map(NativeStep::Return)
                } else {
                    Ok(NativeStep::Return(Value::Void))
                }
            }),
        })
    }
}
