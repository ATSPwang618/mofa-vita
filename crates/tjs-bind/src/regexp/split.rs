//! Shared, bounded split traversal. Output is written directly to its array.
use super::engine::{Engine, substring};
use crate::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, RestArgs, Trace,
    Value,
};
use std::{collections::HashSet, rc::Rc};
use tjs_core::{ObjRef, value};

enum Delimiters {
    Bytes([u64; 4]),
    Short(Vec<u16>),
    Wide(HashSet<u16>),
}
impl Delimiters {
    fn new(units: Vec<u16>) -> Self {
        if units.iter().all(|&unit| unit < 256) {
            let mut bits = [0; 4];
            for unit in units {
                bits[unit as usize / 64] |= 1 << (unit % 64);
            }
            Self::Bytes(bits)
        } else if units.len() <= 8 {
            Self::Short(units)
        } else {
            Self::Wide(units.into_iter().collect())
        }
    }
    fn contains(&self, unit: u16) -> bool {
        match self {
            Self::Bytes(bits) => unit < 256 && bits[unit as usize / 64] & (1 << (unit % 64)) != 0,
            Self::Short(units) => units.contains(&unit),
            Self::Wide(units) => units.contains(&unit),
        }
    }
}

enum Input {
    Regex {
        engine: Rc<Engine>,
        text: String,
        matched: bool,
    },
    Characters {
        text: Vec<u16>,
        delimiter: Delimiters,
        begin: usize,
    },
}
struct Split {
    input: Input,
    offset: usize,
    output: ObjId,
    purge: bool,
}
impl Trace for Split {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.output.trace(visit);
    }
}
impl NativeContinuation for Split {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.advance(cx)
    }
}
pub(super) fn regex(
    cx: &mut NativeCx<'_>,
    engine: Rc<Engine>,
    text: String,
    output: ObjId,
    purge: bool,
) -> NativeResult<NativeStep> {
    Split {
        input: Input::Regex {
            engine,
            text,
            matched: false,
        },
        offset: 0,
        output,
        purge,
    }
    .advance(cx)
}
pub(crate) fn array(
    cx: &mut NativeCx<'_>,
    pattern: Value,
    target: Value,
    args: RestArgs<'_>,
) -> NativeResult<NativeStep> {
    let output = cx.this();
    cx.heap_mut().array_resize(output, 0)?;
    // Array.split clears first, then converts the target, purge and delimiter.
    let mut text = value::to_string_units(cx.heap(), target)?;
    let purge = args
        .get(1)
        .copied()
        .unwrap_or(Value::Void)
        .truthy(cx.heap())?;
    if let Value::Obj(reference) = pattern
        && let Some(object) = reference.object
    {
        let state = cx
            .heap_mut()
            .with_native_state::<super::implementation::State, _>(object, |state| {
                state.engine.clone()
            });
        match state {
            Ok(engine) => {
                let engine = engine.ok_or(NativeError::Message("RegExp has not been compiled"))?;
                let text = String::from_utf16(&text)
                    .map_err(|_| NativeError::Message("RegExp requires well-formed UTF-16"))?;
                return regex(cx, engine, text, output, purge);
            }
            Err(NativeError::This) => {} // Non-RegExp objects are string delimiters.
            Err(error) => return Err(error),
        }
    }
    let mut delimiter = value::to_string_units(cx.heap(), pattern)?;
    text.truncate(
        text.iter()
            .position(|&unit| unit == 0)
            .unwrap_or(text.len()),
    );
    delimiter.truncate(
        delimiter
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(delimiter.len()),
    );
    Split {
        input: Input::Characters {
            text,
            delimiter: Delimiters::new(delimiter),
            begin: 0,
        },
        offset: 0,
        output,
        purge,
    }
    .advance(cx)
}
impl Split {
    fn done(self) -> NativeStep {
        NativeStep::Return(Value::Obj(ObjRef::bound(self.output)))
    }
    fn advance(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        match &mut self.input {
            Input::Regex {
                engine,
                text,
                matched,
            } => {
                for _ in 0..64 {
                    let suffix = &text[self.offset..];
                    let Some(whole) = engine.find(suffix)? else {
                        if !*matched || !self.purge || !suffix.is_empty() {
                            let value = substring(cx.heap_mut(), suffix);
                            cx.heap_mut().array_push(self.output, value)?;
                        }
                        return Ok(self.done());
                    };
                    *matched = true;
                    if !self.purge || whole.start > 0 {
                        let value = substring(cx.heap_mut(), &suffix[..whole.start]);
                        cx.heap_mut().array_push(self.output, value)?;
                    }
                    self.offset += whole.end;
                }
            }
            Input::Characters {
                text,
                delimiter,
                begin,
            } => {
                let end = self.offset.saturating_add(1024).min(text.len());
                while self.offset < end {
                    let unit = text[self.offset];
                    let found = delimiter.contains(unit);
                    if found {
                        if !self.purge || *begin < self.offset {
                            let value =
                                Value::Str(cx.heap_mut().alloc_string(&text[*begin..self.offset]));
                            cx.heap_mut().array_push(self.output, value)?;
                        }
                        *begin = self.offset + 1;
                    }
                    self.offset += 1;
                }
                if self.offset == text.len() {
                    if !self.purge || *begin < text.len() {
                        let value = Value::Str(cx.heap_mut().alloc_string(&text[*begin..]));
                        cx.heap_mut().array_push(self.output, value)?;
                    }
                    return Ok(self.done());
                }
            }
        }
        Ok(NativeStep::Continue(Box::new(self)))
    }
}
