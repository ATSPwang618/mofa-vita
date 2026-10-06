//! Key events go to the focused layer. Default navigation and Enter/Escape
//! propagation happen only when the native handler's process argument permits.
use super::*;
use tjs_core::value;

#[derive(Clone, Copy)]
pub(in crate::layer) enum Kind {
    Down,
    Up,
    Press,
}
impl Kind {
    pub(in crate::layer) fn name(self) -> &'static str {
        match self {
            Self::Down => "onKeyDown",
            Self::Up => "onKeyUp",
            Self::Press => "onKeyPress",
        }
    }
    pub(in crate::layer) fn fields(self) -> &'static [usize] {
        match self {
            Self::Down | Self::Up => &[6, 5, 12],
            Self::Press => &[6, 12],
        }
    }
}
pub(in crate::layer) fn start(
    shared: Shared,
    window: WindowId,
    input: Input,
    completion: Box<dyn NativeContinuation>,
) -> NativeStep {
    NativeStep::Continue(Box::new(Delivery {
        shared,
        window,
        input,
        completion,
    }))
}
struct Delivery {
    shared: Shared,
    window: WindowId,
    input: Input,
    completion: Box<dyn NativeContinuation>,
}
impl Trace for Delivery {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.completion.trace(visit);
        let world = self.shared.borrow();
        trace_layer(
            &world,
            world
                .focused(self.window)
                .or_else(|| world.input_primary(self.window)),
            visit,
        );
    }
}
impl NativeContinuation for Delivery {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let focused = self.shared.borrow().focused(self.window);
        let (kind, args) = match self.input {
            Input::KeyDown { key, shift } => (
                Kind::Down,
                vec![
                    Value::Int(key.into()),
                    Value::Int(shift.into()),
                    Value::Int(1),
                ],
            ),
            Input::KeyUp { key, shift } => (
                Kind::Up,
                vec![
                    Value::Int(key.into()),
                    Value::Int(shift.into()),
                    Value::Int(1),
                ],
            ),
            Input::KeyPress(key) => (
                Kind::Press,
                vec![
                    Value::Str(cx.heap_mut().alloc_string(if key == 0 {
                        vec![]
                    } else {
                        vec![key]
                    })),
                    Value::Int(1),
                ],
            ),
            Input::Wheel { shift, delta, x, y } => {
                return Ok(send(
                    &self.shared,
                    focused,
                    "onMouseWheel",
                    vec![
                        Value::Int(shift.into()),
                        Value::Int(delta.into()),
                        Value::Int(x.into()),
                        Value::Int(y.into()),
                    ],
                    self.completion,
                ));
            }
            _ => unreachable!(),
        };
        if focused.is_some() {
            Ok(send(
                &self.shared,
                focused,
                kind.name(),
                args,
                self.completion,
            ))
        } else if let Some(id) = self.shared.borrow().input_primary(self.window) {
            Ok(NativeStep::Continue(Box::new(DefaultKey {
                shared: self.shared.clone(),
                id,
                kind,
                args,
                completion: self.completion,
            })))
        } else {
            self.completion.resume(cx, Value::Void)
        }
    }
}
pub(in crate::layer) struct DefaultKey {
    pub shared: Shared,
    pub id: LayerId,
    pub kind: Kind,
    pub args: Vec<Value>,
    pub completion: Box<dyn NativeContinuation>,
}
impl Trace for DefaultKey {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.args.trace(visit);
        self.completion.trace(visit);
        trace_layer(&self.shared.borrow(), Some(self.id), visit);
    }
}
impl NativeContinuation for DefaultKey {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, result: Value) -> NativeResult<NativeStep> {
        let process = *self.args.last().ok_or(NativeError::Missing(1))?;
        if !process.truthy(cx.heap())? {
            return self.completion.resume(cx, result);
        }
        let (key, shift) = match self.kind {
            Kind::Press => {
                let Value::Str(text) = value::to_string(cx.heap_mut(), self.args[0])? else {
                    unreachable!()
                };
                (
                    u32::from(cx.heap().string(text)?.first().copied().unwrap_or(0)),
                    0,
                )
            }
            _ => (
                bindings::integer(cx, self.args[0])? as u32,
                bindings::integer(cx, self.args[1])? as u32,
            ),
        };
        let world = self.shared.borrow();
        let Some(r) = world.records.get(self.id).filter(|r| !r.shutdown) else {
            drop(world);
            return self.completion.resume(cx, result);
        };
        let window = r.window;
        let parent = r.parent.filter(|&id| world.node_enabled(id));
        drop(world);
        let completion: Box<dyn NativeContinuation> = Box::new(AfterDefault {
            result,
            completion: self.completion,
        });
        if matches!(self.kind, Kind::Down) {
            let forward = if matches!(key, 9 | 39 | 40) && shift & 7 == 0 {
                Some(true)
            } else if (key == 9 && shift & 7 == 1) || matches!(key, 37 | 38) {
                Some(false)
            } else {
                None
            };
            if let Some(forward) = forward {
                return Ok(focus::navigate(self.shared, window, forward, completion));
            }
        }
        if matches!(key, 13 | 27) && shift & 7 == 0 {
            let args = match self.kind {
                Kind::Press => vec![
                    Value::Str(cx.heap_mut().alloc_string(vec![key as u16])),
                    Value::Int(1),
                ],
                _ => vec![
                    Value::Int(key.into()),
                    Value::Int(shift.into()),
                    Value::Int(1),
                ],
            };
            Ok(send(
                &self.shared,
                parent,
                self.kind.name(),
                args,
                completion,
            ))
        } else {
            completion.resume(cx, Value::Void)
        }
    }
}
struct AfterDefault {
    result: Value,
    completion: Box<dyn NativeContinuation>,
}
impl Trace for AfterDefault {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.result.trace(visit);
        self.completion.trace(visit);
    }
}
impl NativeContinuation for AfterDefault {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.completion.resume(cx, self.result)
    }
}

impl bindings::State {
    pub(in crate::layer) fn key_event(
        &self,
        cx: &mut NativeCx<'_>,
        kind: Kind,
        args: &[Value],
    ) -> NativeResult<NativeStep> {
        if args.len() < kind.fields().len() {
            return Err(NativeError::Missing(args.len() + 1));
        }
        let lease = self.lease()?;
        let task = Box::new(DefaultKey {
            shared: lease.shared.clone(),
            id: lease.id,
            kind,
            args: args[..kind.fields().len()].to_vec(),
            completion: Box::new(Returned),
        });
        self.event_then(cx, kind.name(), kind.fields(), args, task)
    }
}
