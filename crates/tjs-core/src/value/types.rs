use super::{ArithmeticError, Value};
use crate::Heap;

pub fn instance_of(heap: &mut Heap, value: Value, name: Value) -> Result<bool, ArithmeticError> {
    // Object-to-string produces a diagnostic representation, never a class name.
    if matches!(name, Value::Obj(_)) {
        return Ok(false);
    }
    let Value::Str(name) = super::to_string(heap, name)? else {
        unreachable!("string conversion")
    };
    let units = heap.string(name)?;
    let name = &units[..units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len())];
    let equal = |text: &str| text.encode_utf16().eq(name.iter().copied());
    if matches!(value, Value::Void) {
        return Ok(false);
    }
    if equal("Object") {
        return Ok(true);
    }
    Ok(match value {
        Value::Void => false,
        Value::Int(_) | Value::Real(_) => equal("Number"),
        Value::Str(_) => equal("String"),
        Value::Octet(_) => equal("Octet"),
        Value::Obj(reference) => match reference.object {
            Some(object) => heap.instance_of(object, name)?,
            None => false,
        },
    })
}
