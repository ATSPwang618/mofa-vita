use super::{DEPTH, LIMIT, string_arg};
use tjs_core::{Heap, NativeError, NativeResult, ObjRef, Value};

const ERROR: &str = "JSONファイル のパースに失敗しました";

pub(super) fn parse(heap: &mut Heap, text: &[u16]) -> NativeResult<Value> {
    if text.len() > LIMIT {
        return Err(NativeError::Message("JSON exceeds input size limit"));
    }
    Reader { text, at: 0 }.value(heap, 0)
}
struct Reader<'a> {
    text: &'a [u16],
    at: usize,
}
impl Reader<'_> {
    fn take(&mut self) -> Option<u16> {
        let value = self.text.get(self.at).copied()?;
        self.at += 1;
        Some(value)
    }
    fn next(&mut self) -> NativeResult<Option<u16>> {
        loop {
            let Some(unit) = self.take() else {
                return Ok(None);
            };
            match unit {
                35 => self.line(),
                47 if self.text.get(self.at) == Some(&47) => self.line(),
                47 if self.text.get(self.at) == Some(&42) => {
                    self.at += 1;
                    loop {
                        match self.take() {
                            Some(42) if self.text.get(self.at) == Some(&47) => {
                                self.at += 1;
                                break;
                            }
                            None => return Err(NativeError::Message(ERROR)),
                            _ => {}
                        }
                    }
                }
                0..=32 => {}
                _ => return Ok(Some(unit)),
            }
        }
    }
    fn line(&mut self) {
        while let Some(unit) = self.take() {
            if matches!(unit, 10 | 13) {
                break;
            }
        }
    }
    fn value(&mut self, heap: &mut Heap, depth: usize) -> NativeResult<Value> {
        if depth > DEPTH {
            return Err(NativeError::Message("JSON structure is too deep"));
        }
        Ok(match self.next()? {
            Some(quote @ (34 | 39)) => self.quoted(heap, quote)?,
            Some(123) => self.container(heap, depth, false)?,
            Some(91) => self.container(heap, depth, true)?,
            Some(43 | 45 | 46 | 48..=57) => self.number(),
            Some(97..=122) => {
                let start = self.at - 1;
                while self
                    .text
                    .get(self.at)
                    .is_some_and(|u| (97..=122).contains(u))
                {
                    self.at += 1;
                }
                match &self.text[start..self.at] {
                    [116, 114, 117, 101] => Value::Int(1),
                    [102, 97, 108, 115, 101] => Value::Int(0),
                    [110, 117, 108, 108] | [118, 111, 105, 100] => Value::Void,
                    _ => return Err(NativeError::Message(ERROR)),
                }
            }
            _ => return Err(NativeError::Message(ERROR)),
        })
    }
    fn container(&mut self, heap: &mut Heap, depth: usize, array: bool) -> NativeResult<Value> {
        let object = if array {
            heap.alloc_array()
        } else {
            heap.alloc_dictionary()
        };
        let close = if array { 93 } else { 125 };
        loop {
            let Some(unit) = self.next()? else {
                return Err(NativeError::Message(ERROR));
            };
            if unit == close {
                break;
            }
            self.at -= 1;
            if matches!(unit, 44 | 59) {
                if array {
                    heap.array_push(object, Value::Void)?;
                }
            } else if array {
                let value = self.value(heap, depth + 1)?;
                heap.array_push(object, value)?;
            } else {
                let key = self.value(heap, depth + 1)?;
                match self.next()? {
                    Some(61) => {
                        if self.text.get(self.at) == Some(&62) {
                            self.at += 1;
                        }
                    }
                    Some(58) => {}
                    _ => return Err(NativeError::Message(ERROR)),
                }
                let value = self.value(heap, depth + 1)?;
                // key.GetString() is a strict conversion, after parsing value.
                let key = string_arg(heap, key)?;
                if !key.is_empty() {
                    let key = heap.intern(&key);
                    heap.set_member(object, key, value)?;
                }
            }
            match self.next()? {
                Some(44 | 59) => {}
                Some(unit) if unit == close => break,
                _ => return Err(NativeError::Message(ERROR)),
            }
        }
        Ok(Value::Obj(ObjRef::bound(object)))
    }
    fn quoted(&mut self, heap: &mut Heap, quote: u16) -> NativeResult<Value> {
        let mut text = Vec::new();
        loop {
            let unit = match self.take() {
                Some(unit) if unit == quote => break,
                // The old EOF loop never terminates. Report a parse failure
                // rather than reproducing unbounded allocation on bad input.
                None | Some(0 | 10 | 13) => return Err(NativeError::Message(ERROR)),
                Some(92) => match self.take() {
                    Some(98) => 8,
                    Some(102) => 12,
                    Some(116) => 9,
                    Some(114) => 13,
                    Some(110) => 10,
                    Some(kind @ (117 | 120)) => {
                        let length = if kind == 117 { 4 } else { 2 };
                        let end = (self.at + length).min(self.text.len());
                        let text = &self.text[self.at..end];
                        self.at = end;
                        hex_prefix(text)
                    }
                    Some(unit) => unit,
                    None => return Err(NativeError::Message(ERROR)),
                },
                Some(unit) => unit,
            };
            // ttstr += tjs_char appends a two-unit C string: U+0000 is omitted.
            if unit != 0 {
                text.push(unit);
            }
        }
        Ok(Value::Str(heap.alloc_string(text)))
    }
    fn number(&mut self) -> Value {
        let start = self.at - 1;
        while self
            .text
            .get(self.at)
            .is_some_and(|u| matches!(*u, 43 | 45 | 46 | 48..=57 | 69 | 101))
        {
            self.at += 1;
        }
        let text = &self.text[start..self.at];
        // A decimal point alone selects wcstod. Thus 1e3 is integer 1,
        // 010 is octal 8, and 1.0e3 is real 1000 in the original plugin.
        if text.contains(&46) {
            Value::Real(real_prefix(text))
        } else {
            Value::Int(integer_prefix(text))
        }
    }
}
fn sign(text: &[u16]) -> (bool, &[u16]) {
    match text.first() {
        Some(45) => (true, &text[1..]),
        Some(43) => (false, &text[1..]),
        _ => (false, text),
    }
}
fn integer_prefix(text: &[u16]) -> i64 {
    let (negative, text) = sign(text);
    let radix = if text.first() == Some(&48) { 8 } else { 10 };
    let limit = i64::MAX as u64 + u64::from(negative);
    let mut number = 0_u64;
    for &unit in text {
        let Some(digit) = unit.checked_sub(48).filter(|d| *d < radix) else {
            break;
        };
        number = number
            .saturating_mul(u64::from(radix))
            .saturating_add(u64::from(digit))
            .min(limit);
    }
    if negative {
        number.wrapping_neg() as i64
    } else {
        number as i64
    }
}
fn real_prefix(text: &[u16]) -> f64 {
    let (_, digits) = sign(text);
    let mut at = text.len() - digits.len();
    let mut count = 0;
    while text.get(at).is_some_and(|u| (48..=57).contains(u)) {
        at += 1;
        count += 1;
    }
    if text.get(at) == Some(&46) {
        at += 1;
        while text.get(at).is_some_and(|u| (48..=57).contains(u)) {
            at += 1;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    if text.get(at).is_some_and(|u| matches!(*u, 69 | 101)) {
        let exponent = at;
        at += 1;
        if text.get(at).is_some_and(|u| matches!(*u, 43 | 45)) {
            at += 1;
        }
        let start = at;
        while text.get(at).is_some_and(|u| (48..=57).contains(u)) {
            at += 1;
        }
        if start == at {
            at = exponent;
        }
    }
    String::from_utf16_lossy(&text[..at]).parse().unwrap_or(0.0)
}
fn hex_prefix(text: &[u16]) -> u16 {
    let text = &text[text
        .iter()
        .take_while(|u| matches!(**u, 9..=13 | 32))
        .count()..];
    let (negative, mut text) = sign(text);
    if text.starts_with(&[48, 120]) || text.starts_with(&[48, 88]) {
        text = &text[2..];
    }
    let mut number = 0_u16;
    for &unit in text {
        let digit = match unit {
            48..=57 => unit - 48,
            65..=70 => unit - 55,
            97..=102 => unit - 87,
            _ => break,
        };
        number = number.wrapping_mul(16).wrapping_add(digit);
    }
    if negative {
        number.wrapping_neg()
    } else {
        number
    }
}
