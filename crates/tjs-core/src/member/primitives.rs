//! Read-only String/Octet properties handled directly by the reference VM.
use super::MemberError;
use crate::{Heap, Value, value};

enum Key {
    Length,
    Index(i32),
}

fn key(heap: &Heap, value: Value) -> Result<Key, MemberError> {
    match value {
        Value::Int(index) => Ok(Key::Index(index as i32)),
        Value::Real(_) => Ok(Key::Index(value::to_integer(heap, value)? as i32)),
        Value::Str(id) => {
            let units = heap.string(id)?;
            let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
            let units = &units[..end];
            if units == [108, 101, 110, 103, 116, 104] {
                return Ok(Key::Length);
            }
            // Unlike Array names, only an initial decimal digit selects indexing.
            // TJS_atoi accepts a numeric prefix: "1suffix" addresses index 1.
            if !units.first().is_some_and(|u| (48..=57).contains(u)) {
                return Err(MemberError::Missing);
            }
            let mut index = 0_i32;
            for &digit in units.iter().take_while(|&&u| (48..=57).contains(&u)) {
                index = index.wrapping_mul(10).wrapping_add(i32::from(digit - 48));
            }
            Ok(Key::Index(index))
        }
        _ => Err(MemberError::Name),
    }
}

pub(super) fn get(heap: &mut Heap, receiver: Value, name: Value) -> Result<Value, MemberError> {
    let key = key(heap, name)?;
    match receiver {
        Value::Str(id) => {
            let units = heap.string(id)?;
            let Key::Index(index) = key else {
                return Ok(Value::Int(units.len() as i64));
            };
            let index = usize::try_from(index).map_err(|_| MemberError::Range)?;
            // The terminator is readable for String, but not for Octet.
            let unit = if index == units.len() {
                0
            } else {
                *units.get(index).ok_or(MemberError::Range)?
            };
            let units = [unit];
            Ok(Value::Str(heap.alloc_string(if unit == 0 {
                &[][..]
            } else {
                &units
            })))
        }
        Value::Octet(id) => {
            let bytes = heap.octet(id)?;
            let Key::Index(index) = key else {
                return Ok(Value::Int(bytes.len() as i64));
            };
            let index = usize::try_from(index).map_err(|_| MemberError::Range)?;
            Ok(Value::Int(i64::from(
                *bytes.get(index).ok_or(MemberError::Range)?,
            )))
        }
        _ => unreachable!("primitive receiver"),
    }
}

pub(super) fn set(heap: &Heap, name: Value) -> Result<(), MemberError> {
    if !matches!(name, Value::Int(_) | Value::Real(_)) {
        key(heap, name)?;
    }
    Err(MemberError::AccessDenied)
}
