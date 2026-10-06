//! Instruction decoding for member and property-body access.
use super::{
    CallError, Vm,
    calls::ReturnTo,
    dispatch::{Access, ReadMode},
};
use crate::{
    Heap, Instruction, Value,
    member::{self, MemberError},
};

impl Vm {
    pub(super) fn access_instruction(
        &mut self,
        heap: &mut Heap,
        instruction: Instruction,
    ) -> Result<(), CallError> {
        let read =
            |register: crate::Register| self.registers[self.frame.base + register.0 as usize];
        let destination =
            |register: crate::Register| ReturnTo::Register(self.frame.base + register.0 as usize);
        let (object, key, access) = match instruction {
            Instruction::UpdateMember {
                object,
                key,
                value,
                op,
            } => (
                Some(read(object)),
                read(key),
                Access::Update {
                    destination: destination(value),
                    key: read(key),
                    value: if op.unary() { Value::Void } else { read(value) },
                    op,
                },
            ),
            Instruction::UpdateName { key, value, op } => (
                None,
                read(key),
                Access::Update {
                    destination: destination(value),
                    key: read(key),
                    value: if op.unary() { Value::Void } else { read(value) },
                    op,
                },
            ),
            Instruction::UpdateProperty {
                property,
                value,
                op,
            } => (
                Some(read(property)),
                Value::Void,
                Access::Update {
                    destination: destination(value),
                    key: Value::Void,
                    value: if op.unary() { Value::Void } else { read(value) },
                    op,
                },
            ),
            Instruction::StoreMember {
                object,
                key,
                value,
                mode,
            } => (
                Some(read(object)),
                read(key),
                Access::Write {
                    destination: ReturnTo::Discard,
                    value: read(value),
                    ensure: mode != crate::ir::StoreMode::Existing,
                    raw: mode == crate::ir::StoreMode::Raw,
                    hidden: mode == crate::ir::StoreMode::Hidden,
                    class_only: false,
                    ignore_invalid: false,
                },
            ),
            Instruction::StoreName { key, value, mode } => (
                None,
                read(key),
                Access::Write {
                    destination: ReturnTo::Discard,
                    value: read(value),
                    ensure: mode != crate::ir::StoreMode::Existing,
                    raw: mode == crate::ir::StoreMode::Raw,
                    hidden: mode == crate::ir::StoreMode::Hidden,
                    class_only: false,
                    ignore_invalid: false,
                },
            ),
            Instruction::GetProperty { dst, src } => {
                let property = read(src);
                let destination = destination(dst);
                return self.property_body(heap, property, None, destination);
            }
            Instruction::SetProperty { property, value } => {
                let (property, value) = (read(property), read(value));
                return self.property_body(heap, property, Some(value), ReturnTo::Discard);
            }
            Instruction::GetName { dst, key } | Instruction::GetRawName { dst, key } => (
                None,
                read(key),
                Access::Read {
                    destination: destination(dst),
                    mode: if matches!(instruction, Instruction::GetRawName { .. }) {
                        ReadMode::Raw
                    } else {
                        ReadMode::Value
                    },
                },
            ),
            Instruction::SetName { key, value } | Instruction::SetRawName { key, value } => {
                let raw = matches!(instruction, Instruction::SetRawName { .. });
                (
                    None,
                    read(key),
                    Access::Write {
                        destination: ReturnTo::Discard,
                        value: read(value),
                        ensure: raw,
                        raw,
                        hidden: false,
                        class_only: false,
                        ignore_invalid: false,
                    },
                )
            }
            Instruction::GetMember { dst, object, key }
            | Instruction::GetRawMember { dst, object, key } => (
                Some(read(object)),
                read(key),
                Access::Read {
                    destination: destination(dst),
                    mode: if matches!(instruction, Instruction::GetRawMember { .. }) {
                        ReadMode::Raw
                    } else {
                        ReadMode::Value
                    },
                },
            ),
            Instruction::TypeOfMember {
                dst, object, key, ..
            } => (
                Some(read(object)),
                read(key),
                Access::Read {
                    destination: destination(dst),
                    mode: ReadMode::TypeOf,
                },
            ),
            Instruction::SetMember { object, key, value }
            | Instruction::SetRawMember { object, key, value } => (
                Some(read(object)),
                read(key),
                Access::Write {
                    destination: ReturnTo::Discard,
                    value: read(value),
                    ensure: true,
                    raw: matches!(instruction, Instruction::SetRawMember { .. }),
                    hidden: false,
                    class_only: false,
                    ignore_invalid: false,
                },
            ),
            Instruction::ContainsMember { dst, object, key } => (
                Some(read(object)),
                read(key),
                Access::Contains {
                    destination: destination(dst),
                },
            ),
            Instruction::DeleteMember { dst, object, key } => (
                Some(read(object)),
                read(key),
                Access::Delete {
                    destination: destination(dst),
                },
            ),
            Instruction::DeleteName { dst, key } => (
                None,
                read(key),
                Access::Delete {
                    destination: destination(dst),
                },
            ),
            _ => unreachable!("member instruction"),
        };
        let (receiver, fallback) = match object {
            Some(object) => (self.member_receiver(heap, object), None),
            None => self.name_receivers(heap),
        };
        self.access(heap, receiver, key, access, fallback, true)
    }

    fn property_body(
        &mut self,
        heap: &mut Heap,
        property: Value,
        argument: Option<Value>,
        destination: ReturnTo,
    ) -> Result<(), CallError> {
        let context = self.this(heap);
        match member::dereference(heap, property, context, argument) {
            Ok(value) => {
                self.deliver(destination, value);
                self.frame.pc += 1;
                Ok(())
            }
            Err(MemberError::Invoke { function, argument }) => {
                self.invoke_accessor(heap, function, argument, destination, true)
            }
            Err(error) => Err(error.into()),
        }
    }
}
