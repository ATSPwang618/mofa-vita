use super::*;
use crate::NativeError;
use tjs_core::ObjRef;

const HEADER: &[u8] = b"KBAD100\0";
pub(crate) fn encode(heap: &Heap, root: ObjId) -> NativeResult<Vec<u8>> {
    let mut output = HEADER.to_vec();
    let mut work = vec![Event::Value(Value::Obj(ObjRef::bound(root)))];
    let mut ancestors = Vec::new();
    while let Some(event) = work.pop() {
        let value = match event {
            Event::End => {
                ancestors.pop();
                continue;
            }
            Event::Key(key) => {
                string(&mut output, heap.symbol(key)?)?;
                continue;
            }
            Event::Value(value) => value,
        };
        match value {
            Value::Void => output.push(0xc1),
            Value::Int(n) => integer(&mut output, n),
            Value::Real(n) => {
                output.push(0xcb);
                output.extend(n.to_le_bytes());
            }
            Value::Str(id) => string(&mut output, heap.string(id)?)?,
            Value::Octet(id) => {
                let bytes = heap.octet(id)?;
                length(&mut output, bytes.len(), 0xd4, 5, None, 0xda, 0xdb)?;
                output.extend(bytes);
            }
            Value::Obj(reference) => {
                let object = reference.this.or(reference.object);
                if let Some(id) = object {
                    let kind = heap.container_kind(id)?;
                    if matches!(kind, ObjectKind::Array | ObjectKind::Dictionary)
                        && !ancestors.contains(&id)
                    {
                        let count = children(heap, id, &mut work)?;
                        ancestors.push(id);
                        if kind == ObjectKind::Array {
                            length(&mut output, count, 0x90, 15, None, 0xdc, 0xdd)?;
                        } else {
                            length(&mut output, count, 0x80, 15, None, 0xde, 0xdf)?;
                        }
                        continue;
                    }
                }
                output.push(0xc0);
            }
        }
    }
    Ok(output)
}
fn length(
    out: &mut Vec<u8>,
    n: usize,
    base: u8,
    max: usize,
    short: Option<u8>,
    medium: u8,
    long: u8,
) -> NativeResult<()> {
    let n = u32::try_from(n).map_err(|_| bad())?;
    if n as usize <= max {
        out.push(base + n as u8);
    } else if let Some(tag) = short.filter(|_| n <= 255) {
        out.extend([tag, n as u8]);
    } else if n <= 65535 {
        out.push(medium);
        out.extend((n as u16).to_le_bytes());
    } else {
        out.push(long);
        out.extend(n.to_le_bytes());
    }
    Ok(())
}
fn string(out: &mut Vec<u8>, units: &[u16]) -> NativeResult<()> {
    length(out, units.len(), 0xa0, 31, Some(0xc4), 0xc5, 0xc6)?;
    out.extend(units.iter().flat_map(|u| u.to_le_bytes()));
    Ok(())
}
fn integer(out: &mut Vec<u8>, n: i64) {
    if (0..=127).contains(&n) {
        out.push(n as u8);
    } else if n >= 0 {
        let (tag, size) = if n <= 255 {
            (0xcc, 1)
        } else if n <= 65535 {
            (0xcd, 2)
        } else if n <= u32::MAX as i64 {
            (0xce, 4)
        } else {
            (0xcf, 8)
        };
        out.push(tag);
        out.extend(&n.to_le_bytes()[..size]);
    } else {
        // The reference writer uses signed tags even for negative fixnums.
        let (tag, size) = if n >= i8::MIN as i64 {
            (0xd0, 1)
        } else if n >= i16::MIN as i64 {
            (0xd1, 2)
        } else if n >= i32::MIN as i64 {
            (0xd2, 4)
        } else {
            (0xd3, 8)
        };
        out.push(tag);
        out.extend(&n.to_le_bytes()[..size]);
    }
}
fn bad() -> NativeError {
    NativeError::Message("invalid structured binary data")
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl Reader<'_> {
    fn take(&mut self, length: usize) -> NativeResult<&[u8]> {
        let end = self.pos.checked_add(length).ok_or_else(bad)?;
        let bytes = self.bytes.get(self.pos..end).ok_or_else(bad)?;
        self.pos = end;
        Ok(bytes)
    }
    fn number(&mut self, size: usize) -> NativeResult<u64> {
        let mut bytes = [0; 8];
        bytes[..size].copy_from_slice(self.take(size)?);
        Ok(u64::from_le_bytes(bytes))
    }
    fn string_bytes(&mut self, tag: u8) -> NativeResult<&[u8]> {
        let length = match tag {
            0xa0..=0xbf => usize::from(tag - 0xa0),
            0xc4..=0xc6 => self.number(1 << (tag - 0xc4))? as usize,
            _ => return Err(bad()),
        };
        self.take(length.checked_mul(2).ok_or_else(bad)?)
    }
    fn value(
        &mut self,
        heap: &mut Heap,
        root: &mut Option<ObjId>,
    ) -> NativeResult<(Value, usize, bool)> {
        let tag = self.number(1)? as u8;
        let value = match tag {
            0..=0x7f => Value::Int(i64::from(tag)),
            0xe0..=0xff => Value::Int(i64::from(tag as i8)),
            0xc0 => Value::Obj(ObjRef::default()),
            0xc1 => Value::Void,
            0xc2 => Value::Int(1),
            0xc3 => Value::Int(0),
            0xcc..=0xcf => Value::Int(self.number(1 << (tag - 0xcc))? as i64),
            0xd0..=0xd3 => {
                let size = 1 << (tag - 0xd0);
                let n = self.number(size)? as i64;
                Value::Int((n << (64 - size * 8)) >> (64 - size * 8))
            }
            0xca => Value::Real(f32::from_bits(self.number(4)? as u32).into()),
            0xcb => Value::Real(f64::from_bits(self.number(8)?)),
            0xa0..=0xbf | 0xc4..=0xc6 => {
                let bytes = self.string_bytes(tag)?;
                Value::Str(
                    heap.alloc_string(
                        bytes
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|b| u16::from_le_bytes([b[0], b[1]]))
                            .collect::<Vec<_>>(),
                    ),
                )
            }
            0xd4..=0xdb => {
                let length = if tag <= 0xd9 {
                    usize::from(tag - 0xd4)
                } else {
                    self.number(if tag == 0xda { 2 } else { 4 })? as usize
                };
                Value::Octet(heap.alloc_octet(self.take(length)?))
            }
            0x80..=0x9f | 0xdc..=0xdf => {
                let map = matches!(tag, 0x80..=0x8f | 0xde | 0xdf);
                let length = if tag <= 0x9f {
                    usize::from(tag & 15)
                } else {
                    self.number(if tag & 1 == 0 { 2 } else { 4 })? as usize
                };
                // Every element needs at least one tag. Validate before allocating.
                if length > self.bytes.len() - self.pos {
                    return Err(bad());
                }
                let id = if let Some(id) = root.take() {
                    let expected = if map {
                        ObjectKind::Dictionary
                    } else {
                        ObjectKind::Array
                    };
                    if heap.container_kind(id)? != expected {
                        return Err(bad());
                    }
                    id
                } else if map {
                    heap.alloc_dictionary()
                } else {
                    heap.alloc_array()
                };
                return Ok((Value::Obj(ObjRef::bound(id)), length, map));
            }
            _ => return Err(bad()),
        };
        Ok((value, 0, false))
    }
}
pub(crate) fn decode(heap: &mut Heap, bytes: &[u8], target: ObjId) -> NativeResult<Value> {
    let bytes = bytes.strip_prefix(HEADER).ok_or_else(bad)?;
    let mut reader = Reader { bytes, pos: 0 };
    let (root, count, map) = reader.value(heap, &mut Some(target))?;
    let mut stack = if count == 0 {
        Vec::new()
    } else {
        vec![(target, count, map)]
    };
    // Dictionary names become symbols directly. A temporary managed string
    // per key otherwise adds both allocation and later GC work to each load.
    let mut key_units = Vec::new();
    while let Some((id, remaining, map)) = stack.last_mut() {
        if *remaining == 0 {
            stack.pop();
            continue;
        }
        let key = if *map {
            let tag = reader.number(1)? as u8;
            let bytes = reader.string_bytes(tag)?;
            key_units.clear();
            key_units.extend(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| u16::from_le_bytes(*b))
                    .take_while(|&u| u != 0),
            );
            Some(heap.intern(&key_units))
        } else {
            None
        };
        let (value, count, map) = reader.value(heap, &mut None)?;
        if let Some(key) = key {
            heap.set_member(*id, key, value)?;
        } else {
            heap.array_push(*id, value)?;
        }
        *remaining -= 1;
        if count != 0 {
            let Value::Obj(reference) = value else {
                unreachable!()
            };
            stack.push((reference.object.expect("container"), count, map));
        }
    }
    Ok(root)
}
