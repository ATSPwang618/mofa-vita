//! Immutable, validated register code. Runtime handles are never serialized here.

use std::fmt;

use crate::{CallSite, FunctionId, value::UpdateOp};

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreMode {
    Existing,
    Ensure,
    Hidden,
    Raw,
}

pub const MAX_REGISTERS: u32 = 65_536;
pub const MAX_INSTRUCTIONS: usize = 100_000;

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Register(pub u32);

impl fmt::Display for Register {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instruction {
    IsValid {
        dst: Register,
        src: Register,
    },
    Invalidate {
        dst: Register,
        src: Register,
    },
    AddClassInfo,
    TypeOf {
        dst: Register,
        src: Register,
    },
    InstanceOf {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    TypeOfMember {
        dst: Register,
        object: Register,
        key: Register,
        computed: bool,
    },
    GetRawName {
        dst: Register,
        key: Register,
    },
    SetRawName {
        key: Register,
        value: Register,
    },
    GetRawMember {
        dst: Register,
        object: Register,
        key: Register,
    },
    SetRawMember {
        object: Register,
        key: Register,
        value: Register,
    },
    GetProperty {
        dst: Register,
        src: Register,
    },
    SetProperty {
        property: Register,
        value: Register,
    },
    RegisterMembers,
    DefineMember {
        object: Register,
        key: Register,
        value: Register,
    },
    LoadReal {
        dst: Register,
        bits: u64,
    },
    Divide {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    IntDivide {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Remainder {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    BitAnd {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    BitOr {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    BitXor {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    ShiftLeft {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    ShiftRight {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    ShiftRightUnsigned {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    StrictEqual {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    StrictNotEqual {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    ToNumber {
        dst: Register,
        src: Register,
    },
    ToInteger {
        dst: Register,
        src: Register,
    },
    ToReal {
        dst: Register,
        src: Register,
    },
    ToString {
        dst: Register,
        src: Register,
    },
    BitNot {
        dst: Register,
        src: Register,
    },
    LoadInt {
        dst: Register,
        value: i64,
    },
    LoadVoid {
        dst: Register,
    },
    LoadNull {
        dst: Register,
    },
    LoadConstant {
        dst: Register,
        constant: u32,
    },
    /// Load the cached function object without binding a this context.
    LoadFunction {
        dst: Register,
        function: FunctionId,
    },
    LoadThis {
        dst: Register,
    },
    LoadGlobal {
        dst: Register,
    },
    GetName {
        dst: Register,
        key: Register,
    },
    SetName {
        key: Register,
        value: Register,
    },
    DefineThis {
        key: Register,
        value: Register,
    },
    BindContext {
        dst: Register,
        object: Register,
        context: Register,
    },
    NewDictionary {
        dst: Register,
    },
    NewArray {
        dst: Register,
    },
    ArrayPush {
        array: Register,
        value: Register,
    },
    CollectArguments {
        dst: Register,
        start: u32,
    },
    GetMember {
        dst: Register,
        object: Register,
        key: Register,
    },
    SetMember {
        object: Register,
        key: Register,
        value: Register,
    },
    DeleteMember {
        dst: Register,
        object: Register,
        key: Register,
    },
    Move {
        dst: Register,
        src: Register,
    },
    Add {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Subtract {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Multiply {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Negate {
        dst: Register,
        src: Register,
    },
    Not {
        dst: Register,
        src: Register,
    },
    IsVoid {
        dst: Register,
        src: Register,
    },
    Equal {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    NotEqual {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Less {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    LessEqual {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Greater {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    GreaterEqual {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Jump {
        target: u32,
    },
    JumpIfFalse {
        condition: Register,
        target: u32,
    },
    Call {
        site: u32,
    },
    Return {
        src: Register,
    },
    Throw {
        src: Register,
    },
    /// Combine already-evaluated operands using TJS truth conversion.
    LogicalAnd {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    LogicalOr {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    /// UTF-16 code-unit conversions (# and $).
    CharacterCode {
        dst: Register,
        src: Register,
    },
    CharacterFrom {
        dst: Register,
        src: Register,
    },
    ContainsMember {
        dst: Register,
        object: Register,
        key: Register,
    },
    /// Postfix evaluation. None selects the reference's statement-only mode.
    Eval {
        dst: Option<Register>,
        src: Register,
    },
    DeleteName {
        dst: Register,
        key: Register,
    },
    /// The value slot holds the operand on entry and the updated result on return.
    /// Keeping three registers preserves the compact instruction representation.
    UpdateMember {
        object: Register,
        key: Register,
        value: Register,
        op: UpdateOp,
    },
    UpdateName {
        key: Register,
        value: Register,
        op: UpdateOp,
    },
    UpdateProperty {
        property: Register,
        value: Register,
        op: UpdateOp,
    },
    StoreMember {
        object: Register,
        key: Register,
        value: Register,
        mode: StoreMode,
    },
    StoreName {
        key: Register,
        value: Register,
        mode: StoreMode,
    },
    ToOctet {
        dst: Register,
        src: Register,
    },
    ClassInfo {
        object: Register,
        name: Register,
    },
    LoadScope {
        dst: Register,
    },
}

impl Instruction {
    pub(crate) fn reads(self) -> [Option<Register>; 3] {
        match self {
            Self::UpdateMember {
                object,
                key,
                value,
                op,
            } => [Some(object), Some(key), (!op.unary()).then_some(value)],
            Self::UpdateName { key, value, op } => {
                [Some(key), (!op.unary()).then_some(value), None]
            }
            Self::UpdateProperty {
                property,
                value,
                op,
            } => [Some(property), (!op.unary()).then_some(value), None],
            Self::StoreMember {
                object, key, value, ..
            } => [Some(object), Some(key), Some(value)],
            Self::StoreName { key, value, .. } => [Some(key), Some(value), None],
            Self::ToOctet { src, .. } => [Some(src), None, None],
            Self::ClassInfo { object, name } => [Some(object), Some(name), None],
            Self::LoadScope { .. } => [None, None, None],
            Self::AddClassInfo
            | Self::RegisterMembers
            | Self::LoadReal { .. }
            | Self::LoadInt { .. }
            | Self::LoadFunction { .. }
            | Self::LoadThis { .. }
            | Self::LoadGlobal { .. }
            | Self::LoadConstant { .. }
            | Self::LoadNull { .. }
            | Self::LoadVoid { .. }
            | Self::NewDictionary { .. }
            | Self::NewArray { .. }
            | Self::CollectArguments { .. }
            | Self::Jump { .. }
            | Self::Call { .. } => [None, None, None],
            Self::Eval { src, .. }
            | Self::IsValid { src, .. }
            | Self::Invalidate { src, .. }
            | Self::TypeOf { src, .. }
            | Self::GetProperty { src, .. }
            | Self::ToNumber { src, .. }
            | Self::ToInteger { src, .. }
            | Self::ToReal { src, .. }
            | Self::CharacterCode { src, .. }
            | Self::CharacterFrom { src, .. }
            | Self::ToString { src, .. }
            | Self::BitNot { src, .. }
            | Self::Move { src, .. }
            | Self::Negate { src, .. }
            | Self::Not { src, .. }
            | Self::IsVoid { src, .. }
            | Self::Throw { src }
            | Self::Return { src } => [Some(src), None, None],
            Self::JumpIfFalse { condition, .. } => [Some(condition), None, None],
            Self::GetRawName { key, .. }
            | Self::GetName { key, .. }
            | Self::DeleteName { key, .. } => [Some(key), None, None],
            Self::SetRawName { key, value }
            | Self::SetName { key, value }
            | Self::DefineThis { key, value } => [Some(key), Some(value), None],
            Self::SetProperty { property, value } => [Some(property), Some(value), None],
            Self::ArrayPush { array, value } => [Some(array), Some(value), None],
            Self::BindContext {
                object, context, ..
            } => [Some(object), Some(context), None],
            Self::TypeOfMember { object, key, .. }
            | Self::GetRawMember { object, key, .. }
            | Self::GetMember { object, key, .. }
            | Self::ContainsMember { object, key, .. }
            | Self::DeleteMember { object, key, .. } => [Some(object), Some(key), None],
            Self::SetRawMember { object, key, value }
            | Self::SetMember { object, key, value }
            | Self::DefineMember { object, key, value } => [Some(object), Some(key), Some(value)],
            Self::InstanceOf { lhs, rhs, .. }
            | Self::Divide { lhs, rhs, .. }
            | Self::IntDivide { lhs, rhs, .. }
            | Self::Remainder { lhs, rhs, .. }
            | Self::LogicalAnd { lhs, rhs, .. }
            | Self::LogicalOr { lhs, rhs, .. }
            | Self::BitAnd { lhs, rhs, .. }
            | Self::BitOr { lhs, rhs, .. }
            | Self::BitXor { lhs, rhs, .. }
            | Self::ShiftLeft { lhs, rhs, .. }
            | Self::ShiftRight { lhs, rhs, .. }
            | Self::ShiftRightUnsigned { lhs, rhs, .. }
            | Self::StrictEqual { lhs, rhs, .. }
            | Self::StrictNotEqual { lhs, rhs, .. }
            | Self::Add { lhs, rhs, .. }
            | Self::Subtract { lhs, rhs, .. }
            | Self::Multiply { lhs, rhs, .. }
            | Self::Equal { lhs, rhs, .. }
            | Self::NotEqual { lhs, rhs, .. }
            | Self::Less { lhs, rhs, .. }
            | Self::LessEqual { lhs, rhs, .. }
            | Self::Greater { lhs, rhs, .. }
            | Self::GreaterEqual { lhs, rhs, .. } => [Some(lhs), Some(rhs), None],
        }
    }

    pub(crate) fn writes(self, calls: &[CallSite]) -> Option<Register> {
        match self {
            Self::UpdateMember { value, .. }
            | Self::UpdateName { value, .. }
            | Self::UpdateProperty { value, .. } => Some(value),
            Self::ToOctet { dst, .. } | Self::LoadScope { dst } => Some(dst),
            Self::StoreMember { .. } | Self::StoreName { .. } | Self::ClassInfo { .. } => None,
            Self::IsValid { dst, .. }
            | Self::Invalidate { dst, .. }
            | Self::TypeOf { dst, .. }
            | Self::InstanceOf { dst, .. }
            | Self::TypeOfMember { dst, .. }
            | Self::GetRawName { dst, .. }
            | Self::GetRawMember { dst, .. }
            | Self::GetProperty { dst, .. }
            | Self::LoadReal { dst, .. }
            | Self::Divide { dst, .. }
            | Self::IntDivide { dst, .. }
            | Self::Remainder { dst, .. }
            | Self::LogicalAnd { dst, .. }
            | Self::LogicalOr { dst, .. }
            | Self::BitAnd { dst, .. }
            | Self::BitOr { dst, .. }
            | Self::BitXor { dst, .. }
            | Self::ShiftLeft { dst, .. }
            | Self::ShiftRight { dst, .. }
            | Self::ShiftRightUnsigned { dst, .. }
            | Self::StrictEqual { dst, .. }
            | Self::StrictNotEqual { dst, .. }
            | Self::ToNumber { dst, .. }
            | Self::ToInteger { dst, .. }
            | Self::ToReal { dst, .. }
            | Self::CharacterCode { dst, .. }
            | Self::CharacterFrom { dst, .. }
            | Self::ToString { dst, .. }
            | Self::BitNot { dst, .. }
            | Self::LoadInt { dst, .. }
            | Self::LoadFunction { dst, .. }
            | Self::LoadThis { dst }
            | Self::LoadGlobal { dst }
            | Self::GetName { dst, .. }
            | Self::DeleteName { dst, .. }
            | Self::BindContext { dst, .. }
            | Self::LoadConstant { dst, .. }
            | Self::LoadNull { dst }
            | Self::LoadVoid { dst }
            | Self::NewDictionary { dst }
            | Self::NewArray { dst }
            | Self::CollectArguments { dst, .. }
            | Self::GetMember { dst, .. }
            | Self::ContainsMember { dst, .. }
            | Self::DeleteMember { dst, .. }
            | Self::Move { dst, .. }
            | Self::Add { dst, .. }
            | Self::Subtract { dst, .. }
            | Self::Multiply { dst, .. }
            | Self::Not { dst, .. }
            | Self::IsVoid { dst, .. }
            | Self::Equal { dst, .. }
            | Self::NotEqual { dst, .. }
            | Self::Less { dst, .. }
            | Self::LessEqual { dst, .. }
            | Self::Greater { dst, .. }
            | Self::GreaterEqual { dst, .. }
            | Self::Negate { dst, .. } => Some(dst),
            Self::Call { site } => calls[site as usize].dst,
            Self::Eval { dst, .. } => dst,
            Self::AddClassInfo
            | Self::RegisterMembers
            | Self::SetRawName { .. }
            | Self::SetRawMember { .. }
            | Self::SetProperty { .. }
            | Self::DefineMember { .. }
            | Self::Return { .. }
            | Self::SetName { .. }
            | Self::DefineThis { .. }
            | Self::SetMember { .. }
            | Self::ArrayPush { .. }
            | Self::Throw { .. }
            | Self::Jump { .. }
            | Self::JumpIfFalse { .. } => None,
        }
    }
}

impl fmt::Display for Instruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::UpdateMember {
                object,
                key,
                value,
                op,
            } => write!(f, "update_member {op:?} {value}, {object}[{key}]"),
            Self::UpdateName { key, value, op } => write!(f, "update_name {op:?} {value}, {key}"),
            Self::UpdateProperty {
                property,
                value,
                op,
            } => write!(f, "update_property {op:?} {value}, {property}"),
            Self::StoreMember {
                object,
                key,
                value,
                mode,
            } => write!(f, "store_member {mode:?} {object}[{key}], {value}"),
            Self::StoreName { key, value, mode } => write!(f, "store_name {mode:?} {key}, {value}"),
            Self::ToOctet { dst, src } => write!(f, "octet {dst}, {src}"),
            Self::ClassInfo { object, name } => write!(f, "class_info {object}, {name}"),
            Self::LoadScope { dst } => write!(f, "load_scope {dst}"),
            Self::IsValid { dst, src } => write!(f, "isvalid {dst}, {src}"),
            Self::Invalidate { dst, src } => write!(f, "invalidate {dst}, {src}"),
            Self::AddClassInfo => write!(f, "add_class_info"),
            Self::TypeOf { dst, src } => write!(f, "typeof {dst}, {src}"),
            Self::InstanceOf { dst, lhs, rhs } => write!(f, "instanceof {dst}, {lhs}, {rhs}"),
            Self::TypeOfMember {
                dst,
                object,
                key,
                computed,
            } => write!(
                f,
                "typeof_member {dst}, {object}, {key}, computed={computed}"
            ),
            Self::GetRawName { dst, key } => write!(f, "get_raw_name {dst}, {key}"),
            Self::SetRawName { key, value } => write!(f, "set_raw_name {key}, {value}"),
            Self::GetRawMember { dst, object, key } => {
                write!(f, "get_raw_member {dst}, {object}, {key}")
            }
            Self::SetRawMember { object, key, value } => {
                write!(f, "set_raw_member {object}, {key}, {value}")
            }
            Self::GetProperty { dst, src } => write!(f, "get_property {dst}, {src}"),
            Self::SetProperty { property, value } => write!(f, "set_property {property}, {value}"),
            Self::RegisterMembers => write!(f, "register_members"),
            Self::DefineMember { object, key, value } => {
                write!(f, "define_member {object}, {key}, {value}")
            }
            Self::LoadReal { dst, bits } => write!(f, "load_real {dst}, {}", f64::from_bits(bits)),
            Self::Divide { dst, lhs, rhs } => write!(f, "div {dst}, {lhs}, {rhs}"),
            Self::IntDivide { dst, lhs, rhs } => write!(f, "idiv {dst}, {lhs}, {rhs}"),
            Self::Remainder { dst, lhs, rhs } => write!(f, "mod {dst}, {lhs}, {rhs}"),
            Self::LogicalAnd { dst, lhs, rhs } => write!(f, "logical_and {dst}, {lhs}, {rhs}"),
            Self::LogicalOr { dst, lhs, rhs } => write!(f, "logical_or {dst}, {lhs}, {rhs}"),
            Self::BitAnd { dst, lhs, rhs } => write!(f, "bit_and {dst}, {lhs}, {rhs}"),
            Self::BitOr { dst, lhs, rhs } => write!(f, "bit_or {dst}, {lhs}, {rhs}"),
            Self::BitXor { dst, lhs, rhs } => write!(f, "bit_xor {dst}, {lhs}, {rhs}"),
            Self::ShiftLeft { dst, lhs, rhs } => write!(f, "shl {dst}, {lhs}, {rhs}"),
            Self::ShiftRight { dst, lhs, rhs } => write!(f, "shr {dst}, {lhs}, {rhs}"),
            Self::ShiftRightUnsigned { dst, lhs, rhs } => write!(f, "ushr {dst}, {lhs}, {rhs}"),
            Self::StrictEqual { dst, lhs, rhs } => write!(f, "seq {dst}, {lhs}, {rhs}"),
            Self::StrictNotEqual { dst, lhs, rhs } => write!(f, "sne {dst}, {lhs}, {rhs}"),
            Self::ToNumber { dst, src } => write!(f, "number {dst}, {src}"),
            Self::ToInteger { dst, src } => write!(f, "int {dst}, {src}"),
            Self::ToReal { dst, src } => write!(f, "real {dst}, {src}"),
            Self::CharacterCode { dst, src } => write!(f, "char_code {dst}, {src}"),
            Self::CharacterFrom { dst, src } => write!(f, "char_from {dst}, {src}"),
            Self::ToString { dst, src } => write!(f, "string {dst}, {src}"),
            Self::BitNot { dst, src } => write!(f, "bit_not {dst}, {src}"),
            Self::LoadInt { dst, value } => write!(f, "load_int {dst}, {value}"),
            Self::LoadFunction { dst, function } => {
                write!(f, "load_function {dst}, f{}", function.0)
            }
            Self::LoadThis { dst } => write!(f, "load_this {dst}"),
            Self::LoadGlobal { dst } => write!(f, "load_global {dst}"),
            Self::GetName { dst, key } => write!(f, "get_name {dst}, {key}"),
            Self::ContainsMember { dst, object, key } => {
                write!(f, "contains {dst}, {object}, {key}")
            }
            Self::DeleteName { dst, key } => write!(f, "delete_name {dst}, {key}"),
            Self::SetName { key, value } => write!(f, "set_name {key}, {value}"),
            Self::DefineThis { key, value } => write!(f, "define_this {key}, {value}"),
            Self::BindContext {
                dst,
                object,
                context,
            } => write!(f, "bind_context {dst}, {object}, {context}"),
            Self::LoadVoid { dst } => write!(f, "load_void {dst}"),
            Self::LoadNull { dst } => write!(f, "load_null {dst}"),
            Self::LoadConstant { dst, constant } => write!(f, "load_const {dst}, c{constant}"),
            Self::NewDictionary { dst } => write!(f, "new_dictionary {dst}"),
            Self::NewArray { dst } => write!(f, "new_array {dst}"),
            Self::ArrayPush { array, value } => write!(f, "array_push {array}, {value}"),
            Self::CollectArguments { dst, start } => write!(f, "collect_arguments {dst}, {start}"),
            Self::GetMember { dst, object, key } => write!(f, "get_member {dst}, {object}, {key}"),
            Self::SetMember { object, key, value } => {
                write!(f, "set_member {object}, {key}, {value}")
            }
            Self::DeleteMember { dst, object, key } => {
                write!(f, "delete_member {dst}, {object}, {key}")
            }
            Self::Move { dst, src } => write!(f, "move {dst}, {src}"),
            Self::Add { dst, lhs, rhs } => write!(f, "add {dst}, {lhs}, {rhs}"),
            Self::Subtract { dst, lhs, rhs } => write!(f, "sub {dst}, {lhs}, {rhs}"),
            Self::Multiply { dst, lhs, rhs } => write!(f, "mul {dst}, {lhs}, {rhs}"),
            Self::Negate { dst, src } => write!(f, "neg {dst}, {src}"),
            Self::Not { dst, src } => write!(f, "not {dst}, {src}"),
            Self::IsVoid { dst, src } => write!(f, "is_void {dst}, {src}"),
            Self::Equal { dst, lhs, rhs } => write!(f, "eq {dst}, {lhs}, {rhs}"),
            Self::NotEqual { dst, lhs, rhs } => write!(f, "ne {dst}, {lhs}, {rhs}"),
            Self::Less { dst, lhs, rhs } => write!(f, "lt {dst}, {lhs}, {rhs}"),
            Self::LessEqual { dst, lhs, rhs } => write!(f, "le {dst}, {lhs}, {rhs}"),
            Self::Greater { dst, lhs, rhs } => write!(f, "gt {dst}, {lhs}, {rhs}"),
            Self::GreaterEqual { dst, lhs, rhs } => write!(f, "ge {dst}, {lhs}, {rhs}"),
            Self::Jump { target } => write!(f, "jump {target:04}"),
            Self::JumpIfFalse { condition, target } => {
                write!(f, "jump_false {condition}, {target:04}")
            }
            Self::Eval {
                dst: Some(dst),
                src,
            } => write!(f, "eval {dst}, {src}"),
            Self::Eval { dst: None, src } => write!(f, "eval_discard {src}"),
            Self::Call { site } => write!(f, "call site{site}"),
            Self::Return { src } => write!(f, "return {src}"),
            Self::Throw { src } => write!(f, "throw {src}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Module;

    #[test]
    fn validation_rejects_bad_indices_initialization_and_termination() {
        let cases = [
            vec![Instruction::Return { src: Register(0) }],
            vec![
                Instruction::LoadInt {
                    dst: Register(1),
                    value: 1,
                },
                Instruction::Return { src: Register(0) },
            ],
            vec![Instruction::LoadVoid { dst: Register(0) }],
            vec![
                Instruction::LoadVoid { dst: Register(0) },
                Instruction::Jump { target: 3 },
                Instruction::Return { src: Register(0) },
            ],
        ];
        for code in cases {
            assert!(Module::new(1, code.clone(), vec![None; code.len()]).is_err());
        }
        assert!(
            Module::new(
                1,
                vec![
                    Instruction::LoadVoid { dst: Register(0) },
                    Instruction::Return { src: Register(0) }
                ],
                vec![]
            )
            .is_err()
        );
    }
}
