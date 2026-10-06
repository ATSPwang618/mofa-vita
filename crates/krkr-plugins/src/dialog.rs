//! SDL's WIN32Dialog is a script-facing adapter to System.inform.
use tjs_bind::{RestArgs, flow};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Value};
krkr_engine::native_plugin! {
    pub(crate) Dialog {
        names: ["win32dialog.dll", "win32dialog.tpm"],
        link(cx, exports) {
            let class = bindings::install_with_state(cx.heap, bindings::State { global:Some(cx.global) })?;
            exports.value(cx,cx.global,"WIN32Dialog",Value::Obj(class.into()))
        }
    }
}
#[tjs_bind::class(name = "WIN32Dialog")]
mod bindings {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        pub(super) global: Option<ObjId>,
    }
    impl State {
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>) -> NativeResult<Self> {
            let class = cx
                .heap()
                .registered_class("WIN32Dialog")
                .ok_or(NativeError::This)?;
            Ok(Self {
                global: with_state(cx, class, |s| s.global)?,
            })
        }
        #[tjs::method(name = "messageBox", resumable = true)]
        fn message(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let global = cx.with_state::<State, _>(|s, _| s.global.ok_or(NativeError::This))?;
            let arguments = vec![
                args.first().copied().unwrap_or(Value::Void),
                args.get(1).copied().unwrap_or(Value::Void),
                Value::Int(2),
            ];
            Ok(NativeStep::Get {
                object: Value::Obj(global.into()),
                key: key(cx, "System"),
                continuation: flow::callback(arguments, |arguments, cx, system| {
                    Ok(NativeStep::CallMember {
                        object: system,
                        key: key(cx, "inform"),
                        arguments,
                        continuation: flow::callback((), |_, cx, result| {
                            Ok(NativeStep::Return(Value::Int(i64::from(
                                !result.truthy(cx.heap())?,
                            ))))
                        }),
                    })
                }),
            })
        }
    }
}
fn key(cx: &mut NativeCx<'_>, text: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(text.encode_utf16().collect::<Vec<_>>()),
    )
}
