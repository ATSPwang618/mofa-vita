use crate::{Heap, NativeError, NativeResult, Value};

#[derive(Clone, Copy)]
pub(super) struct Directive {
    pub kind: u8,
    pub count: Option<usize>,
}

pub(super) fn parse(
    heap: &Heap,
    template: Value,
) -> NativeResult<smallvec::SmallVec<[Directive; 8]>> {
    let Value::Str(id) = template else {
        return Err(NativeError::Type("a pack/unpack template string"));
    };
    let units = crate::string::c_string(heap.string(id)?);
    let mut input = units.iter().copied().peekable();
    let mut output = smallvec::SmallVec::new();
    while let Some(unit) = input.next() {
        let kind = u8::try_from(unit)
            .ok()
            .filter(|c| b"aAbBcCdfhHiIlLnNpPsSvVuwxX@m".contains(c))
            .ok_or(NativeError::Message(
                "unknown pack/unpack template character",
            ))?;
        let count = if input.peek() == Some(&42) {
            input.next();
            None
        } else if matches!(input.peek(), Some(48..=57)) {
            let mut n = 0_usize;
            while let Some(ch @ 48..=57) = input.peek().copied() {
                n = n
                    .checked_mul(10)
                    .and_then(|n| n.checked_add(usize::from(ch - 48)))
                    .ok_or(NativeError::Message("pack/unpack count overflow"))?;
                input.next();
            }
            Some(n)
        } else {
            Some(1)
        };
        output.push(Directive { kind, count });
    }
    Ok(output)
}
pub(super) fn width(kind: u8) -> Option<usize> {
    Some(match kind {
        b'c' | b'C' => 1,
        b's' | b'S' | b'v' | b'n' => 2,
        b'i' | b'I' | b'l' | b'L' | b'N' | b'V' | b'f' => 4,
        b'd' => 8,
        _ => return None,
    })
}
