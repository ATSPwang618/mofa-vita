//! TJS pack/unpack templates. Numeric byte encoding uses Rust's endian APIs;
//! Base64 encoding uses the base64 crate; decoding follows TJS's lookup rules.
mod encoding;
mod template;
use crate::{Heap, NativeError, NativeResult, ObjRef, OctetId, Value, value};
use base64::{Engine, engine::general_purpose::STANDARD};
use template::{Directive, parse, width};

pub fn pack(heap: &mut Heap, items: &[Value], template: Value) -> NativeResult<Value> {
    let bytes = pack_bytes(heap, items, template)?;
    Ok(packed(heap, bytes))
}

/// Pack a managed array without duplicating its elements during conversion.
pub fn pack_array(heap: &mut Heap, array: crate::ObjId, template: Value) -> NativeResult<Value> {
    let bytes = pack_bytes(heap, heap.array(array)?, template)?;
    Ok(packed(heap, bytes))
}

fn packed(heap: &mut Heap, bytes: Vec<u8>) -> Value {
    if bytes.is_empty() {
        Value::Obj(ObjRef::default())
    } else {
        Value::Octet(heap.alloc_octet(bytes))
    }
}

fn pack_bytes(heap: &Heap, items: &[Value], template: Value) -> NativeResult<Vec<u8>> {
    let directives = parse(heap, template)?;
    let mut bytes = Vec::new();
    let mut arg = 0;
    let mut index = 0;
    while let Some(&Directive { kind, count }) = directives.get(index) {
        if arg >= items.len() {
            break;
        }
        if let Some(size) = width(kind) {
            let end = arg + count.unwrap_or(items.len() - arg).min(items.len() - arg);
            bytes.reserve((end - arg).saturating_mul(size).min(64 * 1024));
            for &item in &items[arg..end] {
                let bits = match kind {
                    b'd' => value::to_real(heap, item)?.to_bits(),
                    b'f' => u64::from((value::to_real(heap, item)? as f32).to_bits()),
                    _ => value::to_integer(heap, item)? as u64,
                };
                let big = matches!(kind, b'n' | b'N');
                let buffer = if big {
                    bits.to_be_bytes()
                } else {
                    bits.to_le_bytes()
                };
                bytes.extend_from_slice(if big {
                    &buffer[8 - size..]
                } else {
                    &buffer[..size]
                });
            }
            arg = end;
        } else {
            match kind {
                b'x' => bytes.resize(bytes.len() + count.unwrap_or(0), 0),
                b'X' => {
                    let count = count.unwrap_or(0);
                    if count > bytes.len() {
                        return Err(NativeError::Message("pack cursor before start"));
                    }
                    bytes.truncate(bytes.len() - count);
                }
                b'@' => bytes.resize(
                    bytes
                        .len()
                        .max(count.ok_or(NativeError::Message("pack @ requires a position"))?),
                    0,
                ),
                b'u' | b'w' => {
                    return Err(NativeError::Message(
                        "uuencode and BER are not supported by TJS",
                    ));
                }
                b'p' | b'P' => {
                    arg += 1;
                }
                _ => {
                    let owned;
                    let units = if let Value::Str(id) = items[arg] {
                        heap.string(id)?
                    } else {
                        owned = crate::string::units(heap, items[arg])?;
                        &owned
                    };
                    let text = crate::string::c_string(units);
                    match kind {
                        b'a' | b'A' => {
                            let length = count.unwrap_or(units.len());
                            let start = bytes.len();
                            bytes.extend(text.iter().take(length).map(|&u| u as u8));
                            bytes.resize(start + length, if kind == b'A' { 32 } else { 0 });
                        }
                        b'b' | b'B' | b'h' | b'H' => {
                            let bits = matches!(kind, b'b' | b'B');
                            let group = if bits { 8 } else { 2 };
                            for chunk in
                                text[..count.unwrap_or(text.len()).min(text.len())].chunks(group)
                            {
                                let mut byte = 0;
                                for (i, &unit) in chunk.iter().enumerate() {
                                    let digit = match unit {
                                        48..=49 if bits => unit - 48,
                                        48..=57 if !bits => unit - 48,
                                        97..=102 if !bits => unit - 87,
                                        // The reference HexToBin accepts A..E, but not F.
                                        65..=69 if !bits => unit - 55,
                                        _ => {
                                            return Err(NativeError::Message(
                                                "invalid bit or hex string",
                                            ));
                                        }
                                    };
                                    let shift = if matches!(kind, b'B' | b'H') {
                                        group - i - 1
                                    } else {
                                        i
                                    };
                                    byte |= (digit as u8) << (shift * if bits { 1 } else { 4 });
                                }
                                bytes.push(byte);
                            }
                        }
                        b'm' => {
                            encoding::decode_base64(text, &mut bytes)?;
                        }
                        _ => unreachable!("validated template"),
                    }
                    arg += 1;
                }
            }
        }
        // In the reference a star directive consumes the remaining arguments
        // and prevents later directives from running.
        if count.is_some() {
            index += 1;
        } else if matches!(kind, b'x' | b'X') {
            return Err(NativeError::Message("pack padding requires a count"));
        }
    }
    Ok(bytes)
}

pub fn unpack(heap: &mut Heap, octet: OctetId, template: Value) -> NativeResult<Value> {
    let directives = parse(heap, template)?;
    heap.octet(octet)?;
    let mut pos = 0;
    let mut items = Vec::new();
    for Directive { kind, count } in directives {
        // Each directive finishes borrowing before allocating its result.
        // Octets are immutable and allocation never initiates collection.
        let bytes = heap.octet(octet)?;
        if pos >= bytes.len() {
            break;
        }
        let rest = &bytes[pos..];
        if let Some(size) = width(kind) {
            let count = count.unwrap_or(rest.len() / size);
            let end = rest.len().min(count.saturating_mul(size));
            items.reserve(end.div_ceil(size).min(4096));
            for chunk in rest[..end].chunks(size) {
                let mut buffer = [0_u8; 8];
                let big = matches!(kind, b'n' | b'N');
                if big {
                    buffer[8 - size..8 - size + chunk.len()].copy_from_slice(chunk);
                } else {
                    buffer[..chunk.len()].copy_from_slice(chunk);
                }
                let n = if big {
                    u64::from_be_bytes(buffer)
                } else {
                    u64::from_le_bytes(buffer)
                };
                items.push(match kind {
                    b'd' => Value::Real(f64::from_bits(n)),
                    b'f' => Value::Real(f32::from_bits(n as u32).into()),
                    b'c' => Value::Int(i64::from(n as i8)),
                    b's' => Value::Int(i64::from(n as i16)),
                    b'i' | b'l' => Value::Int(i64::from(n as i32)),
                    _ => Value::Int(n as i64),
                });
            }
            pos = pos.saturating_add(count.saturating_mul(size));
            continue;
        }
        let output = match kind {
            b'a' | b'A' => {
                let length = count.unwrap_or(rest.len());
                let text = encoding::decode_text(&rest[..length.min(rest.len())])?;
                pos = pos.saturating_add(length);
                text
            }
            b'b' | b'B' | b'h' | b'H' => {
                let bits = matches!(kind, b'b' | b'B');
                let group = if bits { 8 } else { 2 };
                let length = count.unwrap_or(rest.len() * group);
                let mut text = Vec::with_capacity(length.min(rest.len() * group));
                for i in 0..length.min(rest.len() * group) {
                    let shift = if matches!(kind, b'B' | b'H') {
                        group - i % group - 1
                    } else {
                        i % group
                    };
                    let digit = (rest[i / group] >> (shift * if bits { 1 } else { 4 }))
                        & if bits { 1 } else { 15 };
                    text.push(u16::from(if digit < 10 {
                        b'0' + digit
                    } else {
                        b'A' + digit - 10
                    }));
                }
                pos = pos.saturating_add(length.div_ceil(group));
                text
            }
            b'm' => {
                pos = bytes.len();
                STANDARD.encode(rest).encode_utf16().collect()
            }
            b'x' => {
                pos = pos.saturating_add(count.unwrap_or(rest.len()));
                continue;
            }
            b'X' => {
                pos = pos.saturating_sub(count.unwrap_or(pos));
                continue;
            }
            b'@' => {
                pos = count.unwrap_or(rest.len());
                continue;
            }
            b'p' | b'P' | b'u' | b'w' => {
                return Err(NativeError::Message(
                    "unpack template is not supported by TJS",
                ));
            }
            _ => unreachable!("validated template"),
        };
        items.push(Value::Str(heap.alloc_string(output)));
    }
    // Transfer the finished buffer through the normal array write barrier.
    // The intrinsic factory still supplies Array's native methods and state.
    if items.capacity() > items.len().saturating_add(items.len() / 4) {
        items.shrink_to_fit();
    }
    let array = heap.alloc_array();
    heap.array_replace(array, items)?;
    Ok(Value::Obj(ObjRef::bound(array)))
}
