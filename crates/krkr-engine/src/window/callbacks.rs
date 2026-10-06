use super::*;
use tjs_core::{NativeContinuation, NativeCx, NativeStep, ObjRef};
pub(super) const NAMES: &[&str] = &[
    "onResize",
    "onActivate",
    "onDeactivate",
    "onCloseQuery",
    "onMouseEnter",
    "onMouseLeave",
    "onMouseMove",
    "onMouseDown",
    "onMouseUp",
    "onClick",
    "onDoubleClick",
    "onMouseWheel",
    "onKeyDown",
    "onKeyUp",
    "onKeyPress",
    "action",
    "onHintChanged",
    "onMove",
];
pub(super) fn arguments(input: Input, heap: &mut Heap) -> (usize, Vec<Value>) {
    let (index, arguments) = match input {
        Input::Extended(_) => unreachable!("extended callbacks use optional dispatch"),
        Input::Resize(_) => (0, ints([])),
        Input::Focus(true) => (1, ints([])),
        Input::Focus(false) => (2, ints([])),
        Input::Close => (3, ints([1])),
        Input::MouseEnter => (4, ints([])),
        Input::MouseLeave => (5, ints([])),
        Input::MouseMove { x, y, shift } => (6, ints([x.into(), y.into(), shift.into()])),
        Input::MouseDown {
            x,
            y,
            button,
            shift,
        } => (7, ints([x.into(), y.into(), button.into(), shift.into()])),
        Input::MouseUp {
            x,
            y,
            button,
            shift,
        } => (8, ints([x.into(), y.into(), button.into(), shift.into()])),
        Input::Click { x, y } => (9, ints([x.into(), y.into()])),
        Input::DoubleClick { x, y } => (10, ints([x.into(), y.into()])),
        Input::Wheel { shift, delta, x, y } => {
            (11, ints([shift.into(), delta.into(), x.into(), y.into()]))
        }
        Input::KeyDown { key, shift } => (12, ints([key.into(), shift.into()])),
        Input::KeyUp { key, shift } => (13, ints([key.into(), shift.into()])),
        Input::KeyPress(key) => {
            return (
                14,
                vec![Value::Str(heap.alloc_string(if key == 0 {
                    vec![]
                } else {
                    vec![key]
                }))],
            );
        }
        Input::Move(_) => (17, ints([])),
    };
    (index, arguments)
}
fn ints<const N: usize>(values: [i64; N]) -> Vec<Value> {
    values.into_iter().map(Value::Int).collect()
}
pub(crate) fn action(
    cx: &mut NativeCx<'_>,
    event_type: Value,
    action: Value,
    keys: &[tjs_core::SymbolId],
    fields: &[usize],
    args: &[Value],
) -> NativeResult<NativeStep> {
    action_for(
        cx,
        Value::Obj(ObjRef::bound(cx.this())),
        event_type,
        action,
        keys,
        fields,
        args,
    )
}
pub(crate) fn action_for(
    cx: &mut NativeCx<'_>,
    owner: Value,
    event_type: Value,
    action: Value,
    keys: &[tjs_core::SymbolId],
    fields: &[usize],
    args: &[Value],
) -> NativeResult<NativeStep> {
    action_then(
        cx,
        owner,
        event_type,
        action,
        keys,
        fields,
        args,
        Box::new(super::tasks::Returned),
    )
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn action_then(
    cx: &mut NativeCx<'_>,
    owner: Value,
    event_type: Value,
    action: Value,
    keys: &[tjs_core::SymbolId],
    fields: &[usize],
    args: &[Value],
    completion: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    if args.len() < fields.len() {
        return Err(NativeError::Missing(args.len() + 1));
    }
    let target = Value::Obj(ObjRef::bound(cx.this()));
    let event = cx.heap_mut().alloc_dictionary();
    for (key, value) in [(keys[0], event_type), (keys[1], target)]
        .into_iter()
        .chain(
            fields
                .iter()
                .map(|&index| keys[index])
                .zip(args.iter().copied()),
        )
    {
        cx.heap_mut().set_member(event, key, value)?;
    }
    Ok(NativeStep::GetOptional {
        object: owner,
        key: action,
        continuation: Box::new(Action {
            target: owner,
            event: Value::Obj(ObjRef::bound(event)),
            completion,
        }),
    })
}
struct Action {
    target: Value,
    event: Value,
    completion: Box<dyn NativeContinuation>,
}
impl Trace for Action {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(self.target);
        visit(self.event);
        self.completion.trace(visit);
    }
}
impl NativeContinuation for Action {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        mut function: Value,
    ) -> NativeResult<NativeStep> {
        let Value::Obj(ref mut reference) = function else {
            return self.completion.resume(cx, Value::Void);
        };
        if reference.object.is_none() {
            return self.completion.resume(cx, Value::Void);
        }
        if reference.this.is_none()
            && let Value::Obj(target) = self.target
        {
            reference.this = target.object;
        }
        Ok(NativeStep::Call {
            function,
            arguments: vec![self.event],
            continuation: self.completion,
        })
    }
}
