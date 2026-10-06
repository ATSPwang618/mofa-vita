//! UTF-16 primitive operations shared by the VM and native containers.
mod format;
#[cfg(test)]
#[path = "../tests/internal/string_reuse.rs"]
mod reuse_tests;
mod work;

use crate::{Heap, NativeError, NativeResult, NativeStep, StrId, Value, value};
use work::{Find, Operation, Text};

pub fn c_string(units: &[u16]) -> &[u16] {
    &units[..units.iter().position(|&u| u == 0).unwrap_or(units.len())]
}
/// The pointer+length string constructor treats a leading NUL as empty,
/// without discarding embedded NUL in a nonempty prefix.
pub fn slice_string(units: &[u16]) -> &[u16] {
    if units.first() == Some(&0) {
        &[]
    } else {
        units
    }
}
pub fn units(heap: &Heap, value: Value) -> NativeResult<Vec<u16>> {
    Ok(value::to_string_units(heap, value)?)
}

fn escape_unit(output: &mut Vec<u16>, hex: &mut bool, unit: u16) {
    let escaped = match unit {
        7 => Some(b'a'),
        8 => Some(b'b'),
        12 => Some(b'f'),
        10 => Some(b'n'),
        13 => Some(b'r'),
        9 => Some(b't'),
        11 => Some(b'v'),
        34 | 39 | 92 => Some(unit as u8),
        _ => None,
    };
    if let Some(ch) = escaped {
        output.extend([92, u16::from(ch)]);
        *hex = false;
    } else if unit < 32 || (*hex && matches!(unit, 48..=57 | 65..=70 | 97..=102)) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        output.extend([
            92,
            120,
            u16::from(HEX[(unit >> 4) as usize]),
            u16::from(HEX[(unit & 15) as usize]),
        ]);
        *hex = true;
    } else {
        output.push(unit);
        *hex = false;
    }
}
pub fn escape(units: &[u16]) -> Vec<u16> {
    let mut output = Vec::new();
    escape_into(units, &mut output);
    output
}
/// Append a quoted-string body without an intermediate allocation.
pub fn escape_into(units: &[u16], output: &mut Vec<u16>) {
    let mut hex = false;
    let mut remaining = c_string(units);
    while !remaining.is_empty() {
        if hex {
            escape_unit(output, &mut hex, remaining[0]);
            remaining = &remaining[1..];
            continue;
        }
        let plain = remaining
            .iter()
            .position(|&u| u < 32 || matches!(u, 34 | 39 | 92))
            .unwrap_or(remaining.len());
        output.extend_from_slice(&remaining[..plain]);
        remaining = &remaining[plain..];
        if let Some((&unit, tail)) = remaining.split_first() {
            escape_unit(output, &mut hex, unit);
            remaining = tail;
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Method {
    CharAt,
    IndexOf,
    Upper,
    Lower,
    Substring,
    Trim,
    Reverse,
    Repeat,
    Escape,
    Sprintf,
}
impl Method {
    pub fn from_name(name: &[u16]) -> Option<Self> {
        Some(match name {
            [99, 104, 97, 114, 65, 116] => Self::CharAt,
            [105, 110, 100, 101, 120, 79, 102] => Self::IndexOf,
            [116, 111, 85, 112, 112, 101, 114, 67, 97, 115, 101] => Self::Upper,
            [116, 111, 76, 111, 119, 101, 114, 67, 97, 115, 101] => Self::Lower,
            [115, 117, 98, 115, 116, 114, 105, 110, 103] | [115, 117, 98, 115, 116, 114] => {
                Self::Substring
            }
            [116, 114, 105, 109] => Self::Trim,
            [114, 101, 118, 101, 114, 115, 101] => Self::Reverse,
            [114, 101, 112, 101, 97, 116] => Self::Repeat,
            [101, 115, 99, 97, 112, 101] => Self::Escape,
            [115, 112, 114, 105, 110, 116, 102] => Self::Sprintf,
            _ => return None,
        })
    }
}
pub(crate) fn call(
    heap: &mut Heap,
    target: StrId,
    method: Method,
    args: &[Value],
    result_needed: bool,
) -> NativeResult<NativeStep> {
    let exact = |count| {
        if args.len() == count {
            Ok(())
        } else {
            Err(NativeError::Message("invalid string method argument count"))
        }
    };
    let integer = |heap: &Heap, index: usize| -> NativeResult<i32> {
        Ok(value::to_integer(
            heap,
            *args.get(index).ok_or(NativeError::Missing(index + 1))?,
        )? as i32)
    };
    let empty = |heap: &mut Heap| {
        NativeStep::Return(if result_needed {
            Value::Str(heap.alloc_string(Vec::<u16>::new()))
        } else {
            Value::Void
        })
    };
    match method {
        Method::CharAt => {
            exact(1)?;
            if heap.string(target)?.is_empty() {
                return Ok(empty(heap));
            }
            let index = integer(heap, 0)? as usize;
            if !result_needed {
                return Ok(NativeStep::Return(Value::Void));
            }
            let unit = heap
                .string(target)?
                .get(index)
                .copied()
                .filter(|&unit| unit != 0);
            Ok(NativeStep::Return(Value::Str(
                heap.alloc_string(unit.into_iter().collect::<Vec<_>>()),
            )))
        }
        Method::IndexOf => {
            if args.len() != 1 && args.len() != 2 {
                exact(1)?;
            }
            let Value::Str(needle) = value::to_string(heap, args[0])? else {
                unreachable!()
            };
            // The reference checks the string object, not strlen(pattern).
            // A nonempty native buffer starting with NUL is an empty needle
            // for strstr, and still performs the start conversion.
            if heap.string(needle)?.is_empty() {
                return Ok(NativeStep::Return(Value::Int(-1)));
            }
            let start = if args.len() == 2 {
                integer(heap, 1)?
            } else {
                0
            };
            let start = start as usize;
            if start >= heap.string(target)?.len() {
                return Ok(NativeStep::Return(Value::Int(-1)));
            }
            Find::start(heap, target, needle, start)
        }
        Method::Substring => {
            if args.len() != 1 && args.len() != 2 {
                exact(1)?;
            }
            let start = integer(heap, 0)? as usize;
            let length = heap.string(target)?.len();
            if start >= length {
                return Ok(empty(heap));
            }
            // Explicit count converts even if the result is discarded.
            let count = if args.len() == 2 {
                Some(integer(heap, 1)?.max(0) as usize)
            } else {
                None
            };
            if !result_needed {
                return Ok(NativeStep::Return(Value::Void));
            }
            if slice_string(&heap.string(target)?[start..]).is_empty() {
                return Ok(empty(heap));
            }
            Text::start(
                heap,
                target,
                start,
                Operation::Copy {
                    end: start + count.unwrap_or(length - start).min(length - start),
                    stop_at_nul: count.is_none(),
                },
            )
        }
        Method::Upper | Method::Lower | Method::Trim | Method::Reverse => {
            exact(0)?;
            if !result_needed {
                return Ok(NativeStep::Return(Value::Void));
            }
            let operation = match method {
                Method::Upper | Method::Lower => Operation::Case {
                    upper: matches!(method, Method::Upper),
                },
                Method::Trim => Operation::Trim {
                    end: heap.string(target)?.len(),
                    trailing: true,
                },
                _ => Operation::Reverse { visible: None },
            };
            Text::start(heap, target, 0, operation)
        }
        Method::Repeat => {
            exact(1)?;
            if !result_needed {
                return Ok(NativeStep::Return(Value::Void));
            }
            let count = integer(heap, 0)?.max(0) as usize;
            let length = heap.string(target)?.len();
            if count == 1 {
                return Ok(NativeStep::Return(Value::Str(target)));
            }
            let total = length
                .checked_mul(count)
                .ok_or(NativeError::Message("repeat length overflow"))?;
            Text::start(heap, target, 0, Operation::Repeat { total })
        }
        Method::Escape => {
            if !result_needed {
                return Ok(NativeStep::Return(Value::Void));
            }
            Text::start(heap, target, 0, Operation::Escape { hex: false })
        }
        Method::Sprintf => {
            if !result_needed {
                return Ok(NativeStep::Return(Value::Void));
            }
            format::start(heap, target, args)
        }
    }
}
