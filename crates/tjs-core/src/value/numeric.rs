use std::cmp::Ordering;

use super::{ArithmeticError, Value};
use crate::{
    Heap,
    number::{self, Number},
};

pub fn to_number(heap: &Heap, value: Value) -> Result<Value, ArithmeticError> {
    Ok(match value {
        Value::Int(_) | Value::Real(_) => value,
        Value::Void => Value::Int(0),
        Value::Str(id) => match number::parse(heap.string(id)?) {
            Some((Number::Int(value), _)) => Value::Int(value),
            Some((Number::Real(value), _)) => Value::Real(value),
            None => Value::Int(0),
        },
        _ => return Err(ArithmeticError::UnsupportedOperands),
    })
}

pub fn to_integer(heap: &Heap, value: Value) -> Result<i64, ArithmeticError> {
    match to_number(heap, value)? {
        Value::Int(value) => Ok(value),
        Value::Real(value) => Ok(real_to_integer(value)),
        _ => unreachable!("number conversion"),
    }
}

/// Legacy Windows/x86 floating conversion: truncate, returning the integer
/// indefinite word for NaN or overflow with floating exceptions masked.
/// Spell out that word so Rust's saturating cast does not change it on other
/// hosts. The preprocessor must narrow this same i64 result to i32 afterwards.
pub fn real_to_integer(value: f64) -> i64 {
    if (-9223372036854775808.0..9223372036854775808.0).contains(&value) {
        value as i64
    } else {
        i64::MIN
    }
}

pub fn to_real(heap: &Heap, value: Value) -> Result<f64, ArithmeticError> {
    real(to_number(heap, value)?)
}

fn real(value: Value) -> Result<f64, ArithmeticError> {
    match value {
        Value::Int(value) => Ok(value as f64),
        Value::Real(value) => Ok(value),
        Value::Void => Ok(0.0),
        _ => Err(ArithmeticError::UnsupportedOperands),
    }
}

pub fn add(lhs: Value, rhs: Value) -> Result<Value, ArithmeticError> {
    match (lhs, rhs) {
        (Value::Int(lhs), Value::Int(rhs)) => Ok(Value::Int(lhs.wrapping_add(rhs))),
        (Value::Void, value @ (Value::Int(_) | Value::Real(_)))
        | (value @ (Value::Int(_) | Value::Real(_)), Value::Void) => Ok(value),
        _ => Ok(Value::Real(real(lhs)? + real(rhs)?)),
    }
}

pub fn subtract(lhs: Value, rhs: Value) -> Result<Value, ArithmeticError> {
    let lhs = if matches!(lhs, Value::Void) {
        Value::Int(0)
    } else {
        lhs
    };
    let rhs = if matches!(rhs, Value::Void) {
        Value::Int(0)
    } else {
        rhs
    };
    match (lhs, rhs) {
        (Value::Int(lhs), Value::Int(rhs)) => Ok(Value::Int(lhs.wrapping_sub(rhs))),
        _ => Ok(Value::Real(real(lhs)? - real(rhs)?)),
    }
}

pub fn multiply(lhs: Value, rhs: Value) -> Result<Value, ArithmeticError> {
    let lhs = if matches!(lhs, Value::Void) {
        Value::Int(0)
    } else {
        lhs
    };
    let rhs = if matches!(rhs, Value::Void) {
        Value::Int(0)
    } else {
        rhs
    };
    match (lhs, rhs) {
        (Value::Int(lhs), Value::Int(rhs)) => Ok(Value::Int(lhs.wrapping_mul(rhs))),
        _ => Ok(Value::Real(real(lhs)? * real(rhs)?)),
    }
}

pub fn negate(value: Value) -> Result<Value, ArithmeticError> {
    match value {
        Value::Int(value) => Ok(Value::Int(value.wrapping_neg())),
        Value::Real(value) => Ok(Value::Real(-value)),
        Value::Void => Ok(Value::Int(0)),
        _ => Err(ArithmeticError::UnsupportedOperands),
    }
}

#[inline]
pub fn subtract_in(heap: &Heap, lhs: Value, rhs: Value) -> Result<Value, ArithmeticError> {
    if let (Value::Int(lhs), Value::Int(rhs)) = (lhs, rhs) {
        return Ok(Value::Int(lhs.wrapping_sub(rhs)));
    }
    subtract(to_number(heap, lhs)?, to_number(heap, rhs)?)
}

#[inline]
pub fn multiply_in(heap: &Heap, lhs: Value, rhs: Value) -> Result<Value, ArithmeticError> {
    if let (Value::Int(lhs), Value::Int(rhs)) = (lhs, rhs) {
        return Ok(Value::Int(lhs.wrapping_mul(rhs)));
    }
    multiply(to_number(heap, lhs)?, to_number(heap, rhs)?)
}

pub fn negate_in(heap: &Heap, value: Value) -> Result<Value, ArithmeticError> {
    negate(to_number(heap, value)?)
}

pub fn divide(heap: &Heap, lhs: Value, rhs: Value) -> Result<Value, ArithmeticError> {
    Ok(Value::Real(to_real(heap, lhs)? / to_real(heap, rhs)?))
}

pub fn int_divide(heap: &Heap, lhs: Value, rhs: Value) -> Result<Value, ArithmeticError> {
    // The VM uses Variant::idivequal: both conversions precede zero checking.
    let right = to_integer(heap, rhs)?;
    let left = to_integer(heap, lhs)?;
    if right == 0 {
        return Err(ArithmeticError::DivideByZero);
    }
    left.checked_div(right)
        .map(Value::Int)
        .ok_or(ArithmeticError::DivisionOverflow)
}

pub fn remainder(heap: &Heap, lhs: Value, rhs: Value) -> Result<Value, ArithmeticError> {
    let right = to_integer(heap, rhs)?;
    if right == 0 {
        return Err(ArithmeticError::DivideByZero);
    }
    to_integer(heap, lhs)?
        .checked_rem(right)
        .map(Value::Int)
        .ok_or(ArithmeticError::DivisionOverflow)
}

pub fn shift_count(heap: &Heap, value: Value) -> Result<u32, ArithmeticError> {
    let count = to_integer(heap, value)?;
    if !(0..64).contains(&count) {
        return Err(ArithmeticError::ShiftCount);
    }
    Ok(count as u32)
}

/// Integer pairs stay exact above 2^53; NaN is unordered. The compiler/VM
/// implement TJS <= and >= as negated > and <, including their NaN behavior.
#[inline]
pub fn compare_in(
    heap: &Heap,
    lhs: Value,
    rhs: Value,
) -> Result<Option<Ordering>, ArithmeticError> {
    if let (Value::Int(lhs), Value::Int(rhs)) = (lhs, rhs) {
        return Ok(Some(lhs.cmp(&rhs)));
    }
    compare_other(heap, lhs, rhs)
}

fn compare_other(heap: &Heap, lhs: Value, rhs: Value) -> Result<Option<Ordering>, ArithmeticError> {
    match (lhs, rhs) {
        (Value::Str(lhs), Value::Str(rhs)) => Ok(Some(
            heap.string(lhs)?
                .iter()
                .take_while(|&&u| u != 0)
                .cmp(heap.string(rhs)?.iter().take_while(|&&u| u != 0)),
        )),
        _ => Ok(to_real(heap, lhs)?.partial_cmp(&to_real(heap, rhs)?)),
    }
}
