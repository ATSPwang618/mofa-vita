use super::*;

pub fn encode(heap: &mut Heap, root: ObjId) -> NativeResult<Vec<u16>> {
    let mut output = Vec::new();
    let mut ancestors = Vec::new();
    let mut counts: Vec<(usize, bool)> = Vec::new();
    let mut work = vec![Event::Value(Value::Obj(tjs_core::ObjRef::bound(root)))];
    while let Some(event) = work.pop() {
        let value = match event {
            Event::End => {
                ancestors.pop();
                if counts.pop().expect("container count").0 != 0 {
                    output.extend("\r\n".encode_utf16());
                }
                output.extend(std::iter::repeat_n(32, ancestors.len()));
                output.push(93);
                continue;
            }
            Event::Key(key) => {
                separator(&mut counts, &mut output);
                quote(heap.symbol(key)?, &mut output);
                output.extend(" => ".encode_utf16());
                continue;
            }
            Event::Value(value) => value,
        };
        if counts.last().is_some_and(|&(_, array)| array) {
            separator(&mut counts, &mut output);
        }
        match value {
            Value::Obj(reference) => {
                if let Some(id) = reference.this.or(reference.object) {
                    let kind = heap.container_kind(id)?;
                    if matches!(kind, ObjectKind::Array | ObjectKind::Dictionary)
                        && !ancestors.contains(&id)
                    {
                        children(heap, id, &mut work)?;
                        ancestors.push(id);
                        counts.push((0, kind == ObjectKind::Array));
                        output.extend(
                            if kind == ObjectKind::Array {
                                "(const) [\r\n"
                            } else {
                                "(const) %[\r\n"
                            }
                            .encode_utf16(),
                        );
                        continue;
                    }
                }
                output.extend("null".encode_utf16());
                if let Some(id) = reference.this.or(reference.object) {
                    if ancestors.contains(&id) {
                        output.extend(" /* object recursion detected */".encode_utf16());
                    } else {
                        output.extend(" /* (object) ".encode_utf16());
                        quote(
                            &heap
                                .object_text(tjs_core::ObjRef::bound(id))
                                .encode_utf16()
                                .collect::<Vec<_>>(),
                            &mut output,
                        );
                        output.extend(" */".encode_utf16());
                    }
                }
            }
            Value::Str(id) => quote(heap.string(id)?, &mut output),
            Value::Void => output.extend("void".encode_utf16()),
            Value::Octet(id) => {
                output.extend("<%".encode_utf16());
                const HEX: &[u8; 16] = b"0123456789abcdef";
                for byte in heap.octet(id)? {
                    output.extend([
                        u16::from(HEX[(byte >> 4) as usize]),
                        u16::from(HEX[(byte & 15) as usize]),
                    ]);
                }
                output.extend("%>".encode_utf16());
            }
            Value::Real(n) => {
                let mut text = n.to_string();
                if n.is_infinite() {
                    text = if n.is_sign_negative() {
                        "-Infinity"
                    } else {
                        "Infinity"
                    }
                    .into();
                } else if n.is_finite() && !text.contains(['.', 'e', 'E']) {
                    text.push_str(".0");
                }
                output.extend(text.encode_utf16());
            }
            Value::Int(n) => {
                tjs_core::value::append_string_units(heap, Value::Int(n), &mut output)?;
            }
        }
    }
    Ok(output)
}
fn quote(units: &[u16], out: &mut Vec<u16>) {
    out.push(34);
    tjs_core::string::escape_into(units, out);
    out.push(34);
}
fn separator(counts: &mut [(usize, bool)], output: &mut Vec<u16>) {
    if let Some((count, _)) = counts.last_mut() {
        if *count != 0 {
            output.extend(",\r\n".encode_utf16());
        }
        *count += 1;
    }
    output.extend(std::iter::repeat_n(32, counts.len()));
}
