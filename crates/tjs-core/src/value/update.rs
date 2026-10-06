//! Variant operations shared by source and imported property updates.
use super::{ArithmeticError, Value};
use crate::{Heap, value};

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateOp {
    Increment,
    Decrement,
    LogicalOr,
    LogicalAnd,
    BitOr,
    BitXor,
    BitAnd,
    ShiftRight,
    ShiftLeft,
    ShiftRightUnsigned,
    Add,
    Subtract,
    Remainder,
    Divide,
    IntDivide,
    Multiply,
}

impl UpdateOp {
    pub fn unary(self) -> bool {
        matches!(self, Self::Increment | Self::Decrement)
    }

    pub(crate) fn apply(
        self,
        heap: &mut Heap,
        lhs: Value,
        rhs: Value,
    ) -> Result<Value, ArithmeticError> {
        use UpdateOp::*;
        match self {
            Increment | Decrement => value::add(
                value::to_number(heap, lhs)?,
                Value::Int(if self == Increment { 1 } else { -1 }),
            ),
            LogicalOr | LogicalAnd => value::logical(heap, lhs, rhs, self == LogicalAnd),
            Add => value::add_in(heap, lhs, rhs),
            Subtract => value::subtract_in(heap, lhs, rhs),
            Multiply => value::multiply_in(heap, lhs, rhs),
            Divide => value::divide(heap, lhs, rhs),
            IntDivide => value::int_divide(heap, lhs, rhs),
            Remainder => value::remainder(heap, lhs, rhs),
            BitOr | BitXor | BitAnd => {
                let lhs = value::to_integer(heap, lhs)?;
                let rhs = value::to_integer(heap, rhs)?;
                Ok(Value::Int(match self {
                    BitOr => lhs | rhs,
                    BitXor => lhs ^ rhs,
                    _ => lhs & rhs,
                }))
            }
            ShiftRight | ShiftLeft | ShiftRightUnsigned => {
                let lhs = value::to_integer(heap, lhs)?;
                let rhs = value::shift_count(heap, rhs)?;
                Ok(Value::Int(match self {
                    ShiftRight => lhs >> rhs,
                    ShiftLeft => lhs.wrapping_shl(rhs),
                    _ => ((lhs as u64) >> rhs) as i64,
                }))
            }
        }
    }
}
