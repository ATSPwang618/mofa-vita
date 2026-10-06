//! Optional windowEx callbacks use the event VM and never run on the host thread.
use super::*;
use krkr_protocol::window::ExtendedEvent;
use tjs_core::{NativeContinuation, NativeCx, NativeStep, ObjRef, value};

pub(super) fn callback(heap: &mut Heap, owner: ObjId, input: Input) -> Box<dyn NativeContinuation> {
    let (name, arguments) = match input {
        Input::Move(g) => (
            "onMove",
            vec![
                Value::Int(i64::from(g.client_left as u16)),
                Value::Int(i64::from(g.client_top as u16)),
            ],
        ),
        Input::Extended(ExtendedEvent::Minimize) => ("onMinimize", vec![]),
        Input::Extended(ExtendedEvent::Maximize) => ("onMaximize", vec![]),
        Input::Extended(ExtendedEvent::Show) => ("onShow", vec![]),
        Input::Extended(ExtendedEvent::Hide) => ("onHide", vec![]),
        Input::Extended(ExtendedEvent::DisplayChanged) => ("onDisplayChanged", vec![]),
        Input::Extended(ExtendedEvent::DpiChanged(x, y)) => (
            "onDPIChanged",
            vec![Value::Int(x.into()), Value::Int(y.into())],
        ),
        Input::Extended(ExtendedEvent::ActivateChanged(active, minimized)) => (
            "onActivateChanged",
            vec![
                Value::Int(active.into()),
                minimized
                    .map(|v| Value::Int(v.into()))
                    .unwrap_or(Value::Void),
            ],
        ),
        _ => unreachable!(),
    };
    Box::new(Optional {
        owner: Value::Obj(ObjRef::bound(owner)),
        key: Value::Str(heap.alloc_string(name.encode_utf16().collect::<Vec<_>>())),
        arguments,
        phase: false,
        completion: Box::new(tasks::Returned),
    })
}
struct Optional {
    owner: Value,
    key: Value,
    arguments: Vec<Value>,
    phase: bool,
    completion: Box<dyn NativeContinuation>,
}
impl Trace for Optional {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.key.trace(visit);
        self.arguments.trace(visit);
        self.completion.trace(visit);
    }
}
impl NativeContinuation for Optional {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        mut value: Value,
    ) -> NativeResult<NativeStep> {
        if !self.phase {
            self.phase = true;
            return Ok(NativeStep::GetOptional {
                object: self.owner,
                key: self.key,
                continuation: self,
            });
        }
        if let Value::Obj(ref mut function) = value
            && function.object.is_some()
        {
            if function.this.is_none() {
                let Value::Obj(owner) = self.owner else {
                    unreachable!()
                };
                function.this = owner.object;
            }
            return Ok(NativeStep::Call {
                function: value,
                arguments: self.arguments,
                continuation: self.completion,
            });
        }
        self.completion.resume(cx, Value::Void)
    }
}
pub(super) fn query_maximize(
    cx: &mut NativeCx<'_>,
    shared: Shared,
    id: WindowId,
    control: krkr_protocol::window::Control,
) -> NativeResult<NativeStep> {
    let owner = shared.borrow().record(id)?.owner;
    Box::new(Optional {
        owner: Value::Obj(ObjRef::bound(owner)),
        key: Value::Str(
            cx.heap_mut()
                .alloc_string("onMaximizeQuery".encode_utf16().collect::<Vec<_>>()),
        ),
        arguments: vec![],
        phase: false,
        completion: Box::new(Maximize {
            shared,
            id,
            control,
        }),
    })
    .resume(cx, Value::Void)
}
struct Maximize {
    shared: Shared,
    id: WindowId,
    control: krkr_protocol::window::Control,
}
impl Trace for Maximize {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(owner) = self.shared.borrow().owner(self.id) {
            owner.trace(visit);
        }
    }
}
impl NativeContinuation for Maximize {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, result: Value) -> NativeResult<NativeStep> {
        if value::to_integer(cx.heap(), result)? != 0 {
            return Ok(NativeStep::Return(Value::Void));
        }
        tasks::request(
            &self.shared,
            self.id,
            Command::Control(self.control),
            tasks::Change::None,
            None,
        )
    }
}
