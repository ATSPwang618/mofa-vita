//! Regex compilation and text matching, independent of VM objects.
use std::ops::Range;

use crate::{Heap, NativeError, NativeResult, Value};
use fancy_regex::{Regex, RegexBuilder};
use tjs_core::value;

type Captures = smallvec::SmallVec<[Option<Range<usize>>; 4]>;

pub(super) struct Engine {
    // A valid zero-width-only pattern cannot match under TJS FIND_NOT_EMPTY.
    regex: Option<Regex>,
    pub global: bool,
}

pub(super) fn text(heap: &mut Heap, value: Value) -> NativeResult<String> {
    let owned;
    let units = if let Value::Str(id) = value {
        heap.string(id)?
    } else {
        owned = value::to_string_units(heap, value)?;
        &owned
    };
    String::from_utf16(units)
        .map_err(|_| NativeError::Message("RegExp requires well-formed UTF-16"))
}

pub(super) fn string(heap: &mut Heap, text: &str) -> Value {
    Value::Str(heap.alloc_string(text.encode_utf16().collect::<Vec<_>>()))
}

/// TJSAllocVariantString(pointer, length) treats a leading NUL as empty,
/// while preserving embedded NUL and the explicit length otherwise.
pub(super) fn substring(heap: &mut Heap, text: &str) -> Value {
    string(heap, if text.starts_with('\0') { "" } else { text })
}

pub(super) fn array<'a>(
    heap: &mut Heap,
    parts: impl IntoIterator<Item = &'a str>,
) -> NativeResult<Value> {
    let array = heap.alloc_array();
    for part in parts {
        let value = substring(heap, part);
        heap.array_push(array, value)?;
    }
    Ok(Value::Obj(tjs_core::ObjRef::bound(array)))
}

/// TJS start offsets count UTF-16 code units, never UTF-8 bytes.
pub(super) fn byte_offset(text: &str, start: u32) -> NativeResult<usize> {
    let mut units = 0_u32;
    for (byte, ch) in text.char_indices() {
        if units == start {
            return Ok(byte);
        }
        units += ch.len_utf16() as u32;
        if units > start {
            return Err(NativeError::Message(
                "RegExp.start splits a UTF-16 surrogate pair",
            ));
        }
    }
    Ok(text.len())
}

impl Engine {
    pub fn compile(pattern: &[u16], flags: &[u16]) -> NativeResult<Self> {
        let pattern = String::from_utf16(pattern)
            .map_err(|_| NativeError::Message("RegExp requires well-formed UTF-16"))?;
        let pattern = octal_escapes(&pattern);
        let flags = &flags[..flags
            .iter()
            .position(|&unit| unit == 0 || unit == 47)
            .unwrap_or(flags.len())];
        let built = RegexBuilder::new(&pattern)
            .oniguruma_mode(true)
            .find_not_empty(true)
            .ignore_numbered_groups_when_named_groups_exist(false)
            .case_insensitive(flags.contains(&u16::from(b'i')))
            .build();
        let regex = match built {
            Ok(regex) => Some(regex),
            Err(fancy_regex::Error::CompileError(error))
                if matches!(*error, fancy_regex::CompileError::PatternCanNeverMatch) =>
            {
                None
            }
            Err(error) => return Err(NativeError::Detail(format!("RegExp: {error}"))),
        };
        Ok(Self {
            regex,
            global: flags.contains(&u16::from(b'g')),
        })
    }

    pub fn captures(&self, text: &str) -> NativeResult<Option<Captures>> {
        let Some(regex) = &self.regex else {
            return Ok(None);
        };
        regex
            .captures(text)
            .map(|captures| {
                captures.map(|captures| {
                    (0..captures.len())
                        .map(|index| captures.get(index).map(|m| m.start()..m.end()))
                        .collect()
                })
            })
            .map_err(|error| NativeError::Detail(format!("RegExp: {error}")))
    }

    /// Splitting and literal replacement only consume group zero. Avoid a
    /// second allocation and copying every subgroup for each match.
    pub fn find(&self, text: &str) -> NativeResult<Option<Range<usize>>> {
        let Some(regex) = &self.regex else {
            return Ok(None);
        };
        regex
            .find(text)
            .map(|found| found.map(|m| m.start()..m.end()))
            .map_err(|error| NativeError::Detail(format!("RegExp: {error}")))
    }
}

/// Oniguruma accepts \0-prefixed octal character escapes, including ranges
/// such as [\000-\037]. Preserve numbered backreferences and escaped slashes.
fn octal_escapes(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        if chars.peek() != Some(&'0') {
            out.push(ch);
            if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        let mut code = 0;
        for _ in 0..3 {
            let Some(digit) = chars.peek().and_then(|ch| ch.to_digit(8)) else {
                break;
            };
            code = code * 8 + digit;
            chars.next();
        }
        out.push_str(&format!("\\x{{{code:x}}}"));
    }
    out
}
