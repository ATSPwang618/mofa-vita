//! Replacement owns its buffers; callbacks resume on the same VM.
use super::engine::{Engine, array};
use crate::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value,
};
use std::rc::Rc;
use tjs_core::{ObjRef, value};

pub(super) enum Substitution {
    Literal(Vec<u16>),
    Callback(Value),
}
impl Substitution {
    pub fn new(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Self> {
        Ok(if let Value::Obj(mut reference) = value {
            reference.this = reference.this.or(Some(cx.this()));
            Self::Callback(Value::Obj(reference))
        } else {
            let mut text = value::to_string_units(cx.heap(), value)?;
            text.truncate(tjs_core::string::c_string(&text).len());
            Self::Literal(text)
        })
    }
}
pub(super) struct Replacement {
    owner: ObjId,
    engine: Rc<Engine>,
    global: bool,
    start: u32,
    target: String,
    // Materialize once only if a callback requests shifted UTF-16 captures.
    units: Option<(Vec<u16>, usize)>,
    offset: usize,
    output: Vec<u16>,
    substitution: Substitution,
}
impl Trace for Replacement {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        if let Substitution::Callback(callback) = self.substitution {
            visit(callback);
        }
    }
}
impl Replacement {
    pub fn start(
        cx: &mut NativeCx<'_>,
        engine: Rc<Engine>,
        start: u32,
        target: String,
        substitution: Substitution,
    ) -> NativeResult<NativeStep> {
        Self {
            owner: cx.this(),
            global: engine.global,
            engine,
            start,
            output: Vec::with_capacity(target.len()),
            target,
            units: None,
            offset: 0,
            substitution,
        }
        .advance(cx)
    }
    fn finish(mut self, cx: &mut NativeCx<'_>) -> NativeStep {
        self.output.extend(
            self.target[self.offset..]
                .split('\0')
                .next()
                .unwrap_or_default()
                .encode_utf16(),
        );
        NativeStep::Return(if cx.result_needed() {
            Value::Str(cx.heap_mut().alloc_string(self.output))
        } else {
            Value::Void
        })
    }
    fn advance(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        // The library search itself is synchronous. Bound the number of
        // searches and writes per native continuation, not its backtracking.
        for _ in 0..64 {
            let suffix = &self.target[self.offset..];
            match &self.substitution {
                Substitution::Literal(replacement) => {
                    let Some(whole) = self.engine.find(suffix)? else {
                        return Ok(self.finish(cx));
                    };
                    self.output.extend(
                        suffix[..whole.start]
                            .split('\0')
                            .next()
                            .unwrap_or_default()
                            .encode_utf16(),
                    );
                    self.output.extend_from_slice(replacement);
                    self.offset += whole.end;
                }
                Substitution::Callback(callback) => {
                    let Some(captures) = self.engine.captures(suffix)? else {
                        return Ok(self.finish(cx));
                    };
                    let whole = captures[0].as_ref().expect("matched expression");
                    self.output.extend(
                        suffix[..whole.start]
                            .split('\0')
                            .next()
                            .unwrap_or_default()
                            .encode_utf16(),
                    );
                    // GetResultArray applies Start even though replace's search
                    // ignores it. Shift in code units, including into a pair.
                    let argument = if self.start == 0 {
                        array(
                            cx.heap_mut(),
                            captures.iter().map(|range| {
                                range.as_ref().map_or("", |range| &suffix[range.clone()])
                            }),
                        )?
                    } else {
                        let (units, offset) = self.units.get_or_insert_with(|| {
                            (
                                self.target.encode_utf16().collect(),
                                self.target[..self.offset].encode_utf16().count(),
                            )
                        });
                        let output = cx.heap_mut().alloc_array();
                        for range in &captures {
                            let part = if let Some(range) = range {
                                let begin = *offset
                                    + self.start as usize
                                    + suffix[..range.start].encode_utf16().count();
                                let length = suffix[range.clone()].encode_utf16().count();
                                if length == 0 {
                                    &[]
                                } else {
                                    units
                                        .get(begin..begin + length)
                                        .ok_or(NativeError::Message(
                                            "RegExp replacement capture exceeds target",
                                        ))?
                                }
                            } else {
                                &[]
                            };
                            let part = if part.first() == Some(&0) { &[] } else { part };
                            let value = Value::Str(cx.heap_mut().alloc_string(part));
                            cx.heap_mut().array_push(output, value)?;
                        }
                        Value::Obj(ObjRef::bound(output))
                    };
                    if let Some((_, offset)) = &mut self.units {
                        *offset += suffix[..whole.end].encode_utf16().count();
                    }
                    self.offset += whole.end;
                    return Ok(NativeStep::Call {
                        function: *callback,
                        arguments: vec![argument],
                        continuation: Box::new(self),
                    });
                }
            }
            if !self.global || self.offset == self.target.len() {
                return Ok(self.finish(cx));
            }
        }
        Ok(NativeStep::Continue(Box::new(self)))
    }
}
impl NativeContinuation for Replacement {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if matches!(self.substitution, Substitution::Callback(_)) {
            if let Value::Str(id) = result {
                // GetString and ttstr::operator+= both append C-strings.
                self.output
                    .extend_from_slice(tjs_core::string::c_string(cx.heap().string(id)?));
            } else {
                self.output
                    .extend(value::to_string_units(cx.heap(), result)?);
            }
            if !self.global || self.offset == self.target.len() {
                return Ok(self.finish(cx));
            }
            // Global is captured once by replace_regex, while RegEx and Start
            // are read again after callbacks. Recompilation is observable.
            (self.engine, self.start) = super::snapshot(cx.heap_mut(), self.owner)?;
        }
        self.advance(cx)
    }
}
