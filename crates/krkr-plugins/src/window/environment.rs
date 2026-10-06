//! windowEx 88c9be22 System.readEnvValue/expandEnvString. The
//! host process environment is queried by name, without shell evaluation.
use crate::exports::{Exports, arg, class};
use krkr_engine::plugins::Context;
use std::ffi::OsString;
use tjs_core::{
    NativeCallable, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Trace,
    Value, string::c_string,
};

const LIMIT: usize = 16 * 1024 * 1024;
const SLICE: usize = 4096;

pub(super) fn install(cx: &mut Context<'_>, exports: &mut Exports) -> NativeResult<()> {
    let system = class(cx, "System")?;
    exports.function(cx, system, "readEnvValue", NativeCallable::Leaf(read))?;
    exports.function(
        cx,
        system,
        "expandEnvString",
        NativeCallable::Resumable(expand),
    )
}

fn read(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let Value::Str(id) = arg(args, 0)? else {
        return Err(NativeError::Type(
            "a nonempty environment variable name string",
        ));
    };
    let name = cx.heap().string(id)?;
    // ttstr == L"" checks whether its managed string is empty. A nonempty
    // managed string starting with NUL passes, then queries an empty OS name.
    if name.is_empty() {
        return Err(NativeError::Message(
            "environment variable name cannot be empty",
        ));
    }
    if !cx.result_needed() {
        return Ok(Value::Void);
    }
    let Some(value) = lookup(c_string(name))? else {
        return Ok(Value::Void);
    };
    // The reference first calls GetEnvironmentVariableW(name, NULL, 0):
    // modern Windows returns 1 for a defined empty value (space for NUL).
    // The second call returns 0 characters, but its result is not checked;
    // the plugin still assigns the empty buffer as a String. Only a failed
    // first capacity query leaves void. Do not copy the old Windows XP quirk.
    Ok(Value::Str(cx.heap_mut().alloc_string(value)))
}

fn expand(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let input = arg(args, 0)?;
    if !cx.result_needed() {
        return Ok(NativeStep::Return(Value::Void));
    }
    // AsStringNoAddRef accepts String and Void, unlike AsString/to_string.
    let input = match input {
        Value::Void => &[][..],
        Value::Str(id) => c_string(cx.heap().string(id)?),
        _ => return Err(NativeError::Type("a string or void")),
    };
    let mut source = Vec::new();
    append(&mut source, input)?;
    Ok(NativeStep::Continue(Box::new(Expansion {
        source,
        output: Vec::new(),
        cursor: 0,
        literal: 0,
        opening: None,
    })))
}

fn append(output: &mut Vec<u16>, text: &[u16]) -> NativeResult<()> {
    let required = output
        .len()
        .checked_add(text.len())
        .filter(|&n| n <= LIMIT)
        .ok_or(NativeError::Message(
            "environment text exceeds 16 Mi UTF-16 units",
        ))?;
    if output.capacity() < required {
        let capacity = required
            .max(output.capacity().saturating_mul(2))
            .clamp(256, LIMIT);
        output
            .try_reserve_exact(capacity - output.len())
            .map_err(|_| NativeError::Message("cannot allocate environment text"))?;
    }
    output.extend_from_slice(text);
    Ok(())
}

struct Expansion {
    source: Vec<u16>,
    output: Vec<u16>,
    cursor: usize,
    literal: usize,
    opening: Option<usize>,
}
impl Trace for Expansion {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Expansion {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let end = self.source.len().min(self.cursor + SLICE);
        let initial_output = self.output.len();
        while self.cursor < end {
            let index = self.cursor;
            self.cursor += 1;
            if self.source[index] != u16::from(b'%') {
                continue;
            }
            if let Some(start) = self.opening.take() {
                if let Some(value) = lookup(&self.source[start + 1..index])? {
                    // Never rescan a replacement: values containing %NAME%
                    // remain literal and cmd.exe substring syntax is absent.
                    append(&mut self.output, &value)?;
                } else {
                    let Self { source, output, .. } = self.as_mut();
                    append(output, &source[start..=index])?;
                }
                self.literal = index + 1;
            } else {
                let start = self.literal;
                let Self { source, output, .. } = self.as_mut();
                append(output, &source[start..index])?;
                self.literal = index;
                self.opening = Some(index);
            }
            if self.output.len() - initial_output >= SLICE {
                break;
            }
        }
        if self.cursor != self.source.len() {
            return Ok(NativeStep::Continue(self));
        }
        let start = self.literal;
        let Self { source, output, .. } = self.as_mut();
        append(output, &source[start..])?;
        Ok(NativeStep::Return(Value::Str(
            cx.heap_mut().alloc_string(self.output),
        )))
    }
}

fn lookup(name: &[u16]) -> NativeResult<Option<Vec<u16>>> {
    if name.is_empty() {
        return Ok(None);
    }
    if name.len() > LIMIT {
        return Err(NativeError::Message(
            "environment name exceeds 16 Mi UTF-16 units",
        ));
    }
    let Some(name) = os_name(name) else {
        return Ok(None);
    };
    std::env::var_os(name).map(os_value).transpose()
}

#[cfg(windows)]
fn os_name(name: &[u16]) -> Option<OsString> {
    use std::os::windows::ffi::OsStringExt;
    // Windows performs the native case-insensitive lookup and retains UTF-16
    // names, including unpaired surrogates, without lossy Unicode conversion.
    Some(OsString::from_wide(name))
}
#[cfg(not(windows))]
fn os_name(name: &[u16]) -> Option<OsString> {
    // POSIX names are case-sensitive. A non-Unicode UTF-16 name cannot name
    // a UTF-8 environment entry, so it is absent rather than normalized.
    String::from_utf16(name).ok().map(OsString::from)
}
#[cfg(windows)]
fn os_value(value: OsString) -> NativeResult<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    bounded_units(value.encode_wide())
}
#[cfg(not(windows))]
fn os_value(value: OsString) -> NativeResult<Vec<u16>> {
    let value = value
        .to_str()
        .ok_or(NativeError::Message("environment value is not UTF-8"))?;
    bounded_units(value.encode_utf16())
}
fn bounded_units(units: impl Iterator<Item = u16>) -> NativeResult<Vec<u16>> {
    let mut value = Vec::new();
    for unit in units {
        append(&mut value, &[unit])?;
    }
    Ok(value)
}
