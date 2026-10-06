use super::{
    CallError, Vm,
    calls::{Invocation, ReturnTo},
    dispatch::{Access, Action, Lookup},
};
use crate::{Heap, ObjId, ObjRef, Value, member};
use std::rc::Rc;

pub(super) struct PendingMissing {
    pub(super) lookup: Lookup,
    pub(super) property: Value,
    _guard: Rc<()>,
}
impl Vm {
    pub(super) fn call_missing(
        &mut self,
        heap: &mut Heap,
        mut lookup: Lookup,
        object: ObjId,
        name: Value,
        value: Option<Value>,
    ) -> Result<(), CallError> {
        let guard = heap.enter_missing(object)?;
        let name_units = (*b"missing").map(u16::from);
        let receiver = Value::Obj(ObjRef::bound(object));
        // Failure to find a handler means the ordinary lookup should proceed.
        let symbol = heap.find_symbol(&name_units);
        let handler = symbol
            .map(|symbol| heap.lookup_member(object, symbol))
            .transpose()?
            .flatten();
        let callable = match handler {
            Some(Value::Obj(reference)) => reference.object.is_some_and(|id| {
                heap.object(id).is_ok_and(|object| {
                    matches!(
                        object.kind(),
                        crate::ObjectKind::Function
                            | crate::ObjectKind::NativeFunction
                            | crate::ObjectKind::Class
                            | crate::ObjectKind::NativeClass
                            | crate::ObjectKind::Property
                            | crate::ObjectKind::NativeProperty
                    )
                })
            }),
            _ => false,
        };
        if !callable {
            return self.lookup(heap, lookup);
        }
        let key = Value::Str(heap.alloc_string(name_units.to_vec()));
        let property = heap.missing_property(value.unwrap_or(Value::Void));
        let start = self.registers.len();
        let end = start.checked_add(3).ok_or(CallError::Stack)?;
        if end > self.limits.max_stack_values {
            return Err(CallError::Stack);
        }
        self.registers
            .extend([Value::Int(i64::from(value.is_some())), name, property]);
        let advance = lookup.advance;
        lookup.advance = false;
        self.push_action(Action::Missing(Box::new(PendingMissing {
            lookup,
            property,
            _guard: guard,
        })));
        self.access(
            heap,
            receiver,
            key,
            Access::Call {
                invocation: Invocation {
                    start,
                    end,
                    this: Some(object),
                    destination: ReturnTo::Resume,
                },
                instance: None,
            },
            None,
            advance,
        )
    }

    pub(super) fn resume_missing(
        &mut self,
        heap: &mut Heap,
        pending: PendingMissing,
        accepted: Value,
    ) -> Result<(), CallError> {
        if crate::value::to_integer(heap, accepted)? == 0 {
            return self.lookup(heap, pending.lookup);
        }
        let Value::Obj(property) = pending.property else {
            unreachable!()
        };
        let value = heap.missing_value(property.object.expect("property"))?;
        let lookup = pending.lookup;
        drop(pending._guard);
        if matches!(lookup.access, Access::Update { .. }) {
            return self.update_result(heap, lookup.receiver, lookup.access, value, true, false);
        }
        let mode = match lookup.access {
            Access::Read { mode, .. } => mode.member_mode(),
            Access::Call { .. } => member::GetMode::Value,
            _ => return self.access_result(heap, lookup.access, lookup.receiver, value, false),
        };
        let Value::Obj(receiver) = lookup.receiver else {
            unreachable!()
        };
        let result = if matches!(lookup.access, Access::Call { .. }) {
            member::read_callable_value(
                heap,
                lookup.receiver,
                receiver.object.expect("receiver"),
                value,
            )
        } else {
            member::read_value(
                heap,
                lookup.receiver,
                receiver.object.expect("receiver"),
                value,
                mode,
            )
        };
        match result {
            Ok(value) => self.access_result(heap, lookup.access, lookup.receiver, value, false),
            Err(member::MemberError::Invoke { function, argument }) => self.accessor_result(
                heap,
                lookup.access,
                lookup.receiver,
                function,
                argument,
                false,
            ),
            Err(error) if self.ignore_member_status(lookup.access, &error, false) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}
