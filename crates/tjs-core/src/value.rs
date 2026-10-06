//! Copyable values contain managed IDs, not owning Rust references.

mod numeric;
mod text;
mod types;
mod update;
pub use types::instance_of;
pub use update::UpdateOp;

pub use numeric::*;
pub use text::{
    add_in, append_string_units, character_code, character_from, equal, strict_equal, to_string,
    to_string_units,
};

#[inline(never)]
pub(crate) fn character(
    heap: &mut crate::Heap,
    value: Value,
    from: bool,
) -> Result<Value, ArithmeticError> {
    if from {
        character_from(heap, value)
    } else {
        character_code(heap, value)
    }
}

use crate::{Heap, HeapError, ObjRef, OctetId, StrId};

#[derive(Clone, Copy, Debug, Default)]
pub enum Value {
    #[default]
    Void,
    Int(i64),
    Real(f64),
    Str(StrId),
    Obj(ObjRef),
    Octet(OctetId),
}

#[derive(Debug, thiserror::Error)]
pub enum ArithmeticError {
    #[error("value buffer allocation failed")]
    Allocation,
    #[error("this operation is not implemented for these value kinds")]
    UnsupportedOperands,
    #[error("integer division overflow")]
    DivisionOverflow,
    #[error("integer division by zero")]
    DivideByZero,
    #[error("shift counts outside 0..63 are not defined by this implementation")]
    ShiftCount,
    #[error(transparent)]
    Heap(#[from] HeapError),
}

// Keep the two truth-conversion paths outside the VM dispatch loop.
#[inline(never)]
pub(crate) fn logical(
    heap: &Heap,
    lhs: Value,
    rhs: Value,
    and: bool,
) -> Result<Value, ArithmeticError> {
    let left = lhs.truthy(heap)?;
    let result = if and {
        left && rhs.truthy(heap)?
    } else {
        left || rhs.truthy(heap)?
    };
    Ok(Value::Int(i64::from(result)))
}

impl Value {
    #[inline]
    pub fn truthy(self, heap: &Heap) -> Result<bool, ArithmeticError> {
        Ok(match self {
            Self::Void => false,
            Self::Int(value) => value != 0,
            Self::Real(value) => value != 0.0,
            // TJS strings use numeric conversion, not nonempty-string truth.
            Self::Str(_) => to_integer(heap, self)? != 0,
            Self::Obj(reference) => reference.object.is_some(),
            Self::Octet(id) => !heap.octet(id)?.is_empty(),
        })
    }

    pub fn as_integer(self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(value),
            _ => None,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Void => f.write_str("void"),
            Self::Int(value) => value.fmt(f),
            Self::Real(value) => value.fmt(f),
            Self::Str(id) => write!(f, "<string {id:?}>"),
            Self::Obj(reference) if reference.object.is_none() => f.write_str("null"),
            Self::Obj(reference) => write!(f, "<object {reference:?}>"),
            Self::Octet(id) => write!(f, "<octet {id:?}>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truth_is_not_integer_truncation() {
        let heap = Heap::new();
        assert!(Value::Real(0.5).truthy(&heap).unwrap());
        assert!(Value::Real(f64::NAN).truthy(&heap).unwrap());
        assert!(!Value::Real(-0.0).truthy(&heap).unwrap());
        assert!(!Value::Void.truthy(&heap).unwrap());
    }

    #[test]
    fn integer_word_operations_wrap_without_changing_operand_errors() {
        assert_eq!(
            add(Value::Int(i64::MAX), Value::Int(1))
                .unwrap()
                .as_integer(),
            Some(i64::MIN)
        );
        assert_eq!(
            negate(Value::Int(i64::MIN)).unwrap().as_integer(),
            Some(i64::MIN)
        );
        assert!(matches!(
            multiply(Value::Obj(ObjRef::default()), Value::Int(1)).unwrap_err(),
            ArithmeticError::UnsupportedOperands
        ));
    }

    #[test]
    fn integer_division_and_remainder_keep_their_distinct_error_order() {
        let heap = Heap::new();
        let null = Value::Obj(ObjRef::default());
        assert!(matches!(
            int_divide(&heap, null, Value::Int(0)),
            Err(ArithmeticError::UnsupportedOperands)
        ));
        assert!(matches!(
            remainder(&heap, null, Value::Int(0)),
            Err(ArithmeticError::DivideByZero)
        ));
        assert!(matches!(
            int_divide(&heap, Value::Int(i64::MIN), Value::Int(-1)),
            Err(ArithmeticError::DivisionOverflow)
        ));
        assert!(matches!(
            remainder(&heap, Value::Int(i64::MIN), Value::Int(-1)),
            Err(ArithmeticError::DivisionOverflow)
        ));
    }
}
