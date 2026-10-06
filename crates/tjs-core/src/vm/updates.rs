//! Property operations retain the selected accessor across callbacks. Resolving
//! the member again after its getter could call a different setter.
use super::{
    CallError, Vm,
    calls::ReturnTo,
    dispatch::{Access, Action},
};
use crate::{
    Heap, ObjectKind, Value,
    member::{self, MemberError},
    value::UpdateOp,
};

enum Write {
    Property,
    Member,
    Missing,
}
enum Stage {
    Read,
    Write,
}

pub(super) struct PendingUpdate {
    receiver: Value,
    key: Value,
    old: Value,
    rhs: Value,
    result: Value,
    destination: ReturnTo,
    op: UpdateOp,
    write: Write,
    stage: Stage,
}

impl PendingUpdate {
    pub(super) fn roots(&self) -> impl Iterator<Item = Value> {
        [self.receiver, self.key, self.old, self.rhs, self.result].into_iter()
    }
    fn context(&self) -> crate::ObjId {
        let Value::Obj(reference) = self.receiver else {
            unreachable!("validated update receiver")
        };
        reference
            .this
            .or(reference.object)
            .expect("update receiver")
    }
}

impl Vm {
    pub(super) fn update_result(
        &mut self,
        heap: &mut Heap,
        receiver: Value,
        access: Access,
        old: Value,
        missing: bool,
        advance: bool,
    ) -> Result<(), CallError> {
        let Access::Update {
            destination,
            key,
            value: rhs,
            op,
        } = access
        else {
            unreachable!("update access")
        };
        if let (Value::Obj(reference), Value::Int(index)) = (receiver, key)
            && let Some(id) = reference.object
            && let Some(dispatch) = heap.native_index(id)?
        {
            let result = (dispatch.update)(
                &mut crate::NativeCx::new(heap, id, true),
                index as i32,
                op,
                rhs,
            )?;
            self.deliver(destination, result);
            if advance {
                self.frame.pc += 1;
            }
            return Ok(());
        }
        let property = match old {
            Value::Obj(reference) => reference.object.is_some_and(|id| {
                heap.is_valid(id).unwrap_or(false)
                    && heap.object(id).is_ok_and(|object| {
                        matches!(
                            object.kind(),
                            ObjectKind::Property | ObjectKind::NativeProperty
                        )
                    })
            }),
            _ => false,
        };
        let pending = PendingUpdate {
            receiver,
            key,
            old,
            rhs,
            result: Value::Void,
            destination,
            op,
            write: if missing {
                Write::Missing
            } else if property {
                Write::Property
            } else {
                Write::Member
            },
            stage: Stage::Read,
        };
        if property {
            match member::dereference(heap, old, pending.context(), None) {
                Ok(value) => self.finish_update(heap, pending, value, advance),
                Err(MemberError::Invoke { function, argument }) => {
                    self.push_action(Action::Update(Box::new(pending)));
                    self.invoke_accessor(heap, function, argument, ReturnTo::Resume, advance)
                }
                Err(error) => Err(error.into()),
            }
        } else {
            self.finish_update(heap, pending, old, advance)
        }
    }

    fn finish_update(
        &mut self,
        heap: &mut Heap,
        mut pending: PendingUpdate,
        value: Value,
        advance: bool,
    ) -> Result<(), CallError> {
        pending.result = pending.op.apply(heap, value, pending.rhs)?;
        pending.stage = Stage::Write;
        match pending.write {
            Write::Member => {
                member::commit_update(heap, pending.receiver, pending.key, pending.result)?
            }
            Write::Property => match member::dereference(
                heap,
                pending.old,
                pending.context(),
                Some(pending.result),
            ) {
                Ok(_) => {}
                Err(MemberError::Invoke { function, argument }) => {
                    self.push_action(Action::Update(Box::new(pending)));
                    return self.invoke_accessor(
                        heap,
                        function,
                        argument,
                        ReturnTo::Resume,
                        advance,
                    );
                }
                Err(error) => return Err(error.into()),
            },
            Write::Missing => {
                let (receiver, key, value) = (pending.receiver, pending.key, pending.result);
                self.push_action(Action::Update(Box::new(pending)));
                return self.access(
                    heap,
                    receiver,
                    key,
                    Access::Write {
                        destination: ReturnTo::Resume,
                        value,
                        ensure: true,
                        raw: false,
                        hidden: false,
                        class_only: false,
                        ignore_invalid: false,
                    },
                    None,
                    advance,
                );
            }
        }
        self.deliver(pending.destination, pending.result);
        if advance {
            self.frame.pc += 1;
        }
        Ok(())
    }

    pub(super) fn resume_update(
        &mut self,
        heap: &mut Heap,
        pending: PendingUpdate,
        value: Value,
    ) -> Result<(), CallError> {
        match pending.stage {
            Stage::Read => self.finish_update(heap, pending, value, false),
            Stage::Write => {
                self.deliver(pending.destination, pending.result);
                Ok(())
            }
        }
    }
}
