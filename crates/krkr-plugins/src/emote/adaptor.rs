//! SeparateLayerAdaptor owns a real child Layer. All VM properties and
//! constructors execute through resumable dispatch on the original VM.
use krkr_engine::extensions;
use tjs_bind::flow;
use tjs_core::{NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Value, value};

fn key(cx: &mut NativeCx<'_>, name: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    )
}
#[tjs_bind::class(name = "Motion.SeparateLayerAdaptor")]
pub(super) mod bindings {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        pub layer: Value,
        #[trace(skip = "Managed GPU image lease")]
        pub target: Option<krkr_engine::extensions::GpuImage>,
        pub generation: u64,
    }
    impl State {
        #[tjs::constructor(resumable = true)]
        fn construct(cx: &mut NativeCx<'_>, parent: Value) -> NativeResult<NativeStep> {
            extensions::layer_size(cx, parent)?;
            Box::new(Create {
                parent,
                layer: Value::Void,
                width: 0,
                height: 0,
                phase: 0,
            })
            .resume(cx, Value::Void)
        }
        #[tjs::method]
        fn assign(&self, _other: Value) {} // Explicitly empty in the reference.
        #[tjs::invalidate(resumable = true)]
        fn invalidate(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            Self::clear(cx)
        }
        #[tjs::method(resumable = true)]
        fn clear(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            let layer = cx.with_state::<Self, _>(|s, _| {
                s.target = None;
                s.generation = s.generation.wrapping_add(1);
                Ok(std::mem::replace(&mut s.layer, Value::Void))
            })?;
            if matches!(layer, Value::Void) {
                return Ok(NativeStep::Return(Value::Void));
            }
            Ok(NativeStep::Invalidate {
                object: layer,
                continuation: flow::callback((), |_, _, _| Ok(NativeStep::Return(Value::Void))),
            })
        }
        #[tjs::getter(name = "absolute", resumable = true)]
        fn absolute(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            read(cx, "absolute", Value::Int(0))
        }
        #[tjs::setter(name = "absolute", resumable = true)]
        fn set_absolute(
            cx: &mut NativeCx<'_>,
            #[tjs(coerce)] input: i64,
        ) -> NativeResult<NativeStep> {
            let layer = cx.with_state::<Self, _>(|s, _| Ok(s.layer))?;
            if matches!(layer, Value::Void) {
                return Ok(NativeStep::Return(Value::Void));
            }
            Ok(NativeStep::Set {
                object: layer,
                key: key(cx, "absolute"),
                value: Value::Int(input),
                continuation: flow::callback((), |_, _, _| Ok(NativeStep::Return(Value::Void))),
            })
        }
        #[tjs::getter(name = "isPrimary", resumable = true)]
        fn primary(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            read(cx, "isPrimary", Value::Int(0))
        }
        #[tjs::setter(name = "isPrimary")]
        fn set_primary(&self, _input: Value) {} // Reference setter intentionally ignores assignments.
        #[tjs::getter(name = "parent", resumable = true)]
        fn parent(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            read(cx, "parent", Value::Void)
        }
        #[tjs::setter(name = "parent")]
        fn set_parent(&self, _input: Value) {}
    }
    fn read(cx: &mut NativeCx<'_>, name: &str, fallback: Value) -> NativeResult<NativeStep> {
        let layer = cx.with_state::<State, _>(|s, _| Ok(s.layer))?;
        if matches!(layer, Value::Void) {
            return Ok(NativeStep::Return(fallback));
        }
        Ok(NativeStep::Get {
            object: layer,
            key: key(cx, name),
            continuation: flow::callback((), |_, _, v| Ok(NativeStep::Return(v))),
        })
    }
}
#[derive(tjs_bind::Trace)]
struct Create {
    parent: Value,
    layer: Value,
    width: i64,
    height: i64,
    phase: u8,
}
impl NativeContinuation for Create {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        let phase = self.phase;
        self.phase += 1;
        match phase {
            0 => Ok(NativeStep::Get {
                object: self.parent,
                key: key(cx, "width"),
                continuation: self,
            }),
            1 => {
                self.width = value::to_integer(cx.heap(), value)?;
                Ok(NativeStep::Get {
                    object: self.parent,
                    key: key(cx, "height"),
                    continuation: self,
                })
            }
            2 => {
                self.height = value::to_integer(cx.heap(), value)?;
                Ok(NativeStep::Get {
                    object: self.parent,
                    key: key(cx, "window"),
                    continuation: self,
                })
            }
            3 => {
                let class = cx
                    .heap()
                    .registered_class("Layer")
                    .ok_or(NativeError::This)?;
                Ok(NativeStep::Construct {
                    class: Value::Obj(class.into()),
                    arguments: vec![value, self.parent],
                    continuation: self,
                })
            }
            4 | 5 => {
                if phase == 4 {
                    self.layer = value;
                }
                Ok(NativeStep::CallMember {
                    object: self.layer,
                    key: key(
                        cx,
                        if phase == 4 {
                            "setSize"
                        } else {
                            "setImageSize"
                        },
                    ),
                    arguments: vec![Value::Int(self.width), Value::Int(self.height)],
                    continuation: self,
                })
            }
            6..=8 => {
                let (name, value) = match phase {
                    6 => ("visible", 1),
                    7 => ("type", 2),
                    _ => ("hitType", 1),
                };
                Ok(NativeStep::Set {
                    object: self.layer,
                    key: key(cx, name),
                    value: Value::Int(value),
                    continuation: self,
                })
            }
            _ => cx
                .construct(bindings::State {
                    layer: self.layer,
                    ..Default::default()
                })
                .map(NativeStep::Return),
        }
    }
}
