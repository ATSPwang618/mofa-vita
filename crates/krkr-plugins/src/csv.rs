//! CSVParser from krkr2@dca72645, cpp/plugins/csvParser.cpp.
//! Input and callback work are owned continuations on the original VM.
mod reader;

use crate::exports::arg;
use krkr_engine::storages;
use parser::with_state as state;
use std::{io::Read, sync::Arc};
use tjs_bind::{Array, IntoTjs, RestArgs, Utf16, flow};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef, ObjectKind,
    Trace, Value, value,
};

const TEXT_BYTES: usize = 16 * 1024 * 1024;
const FIELD_LIMIT: usize = 65536;

krkr_engine::native_plugin! {
    pub(crate) Csv {
        names: ["csvParser.dll", "csvParser.tpm"],
        classes: [parser],
        extensions: [],
    }
}

struct Input {
    text: Arc<[u16]>,
    position: usize,
}

#[tjs_bind::class(name = "CSVParser")]
mod parser {
    use super::*;
    pub struct State {
        pub(super) target: Option<ObjId>,
        pub(super) input: Option<Input>,
        pub(super) separator: u16,
        pub(super) newline: Arc<[u16]>,
        pub(super) line: i32,
        pub(super) generation: u64,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                target: None,
                input: None,
                separator: 44,
                newline: Arc::from([13, 10]),
                line: 0,
                generation: 0,
            }
        }
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            self.target.trace(visit);
        }
    }
    impl State {
        #[tjs::constructor]
        fn new(
            cx: &mut NativeCx<'_>,
            #[tjs(default = Value::Obj(ObjRef { object: None, this: None }))] target: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<Self> {
            let mut state = Self::default();
            let Value::Obj(reference) = target else {
                return Err(NativeError::Type("an object or null callback target"));
            };
            // AsObject discards a supplied closure's bound receiver.
            state.target = reference.object;
            if let Some(&separator) = args.first() {
                state.separator = value::to_integer(cx.heap(), separator)? as u16;
            }
            if let Some(&newline) = args.get(1) {
                if let Value::Str(id) = newline {
                    check_text(cx.heap().string(id)?)?;
                }
                if matches!(newline, Value::Octet(_)) {
                    return Err(NativeError::Type("a string-convertible newline"));
                }
                let text = value::to_string_units(cx.heap(), newline)?;
                check_text(&text)?;
                state.newline = text.into();
            }
            Ok(state)
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.input = None;
            self.target = None;
            self.newline = Arc::from([]);
            self.generation = self.generation.wrapping_add(1);
        }
        #[tjs::getter(name = "currentLineNumber")]
        fn line(&self) -> i64 {
            self.line.into()
        }
        #[tjs::method]
        fn init(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<()> {
            initialize(cx, arg(args, 0)?)
        }
        #[tjs::method(name = "initStorage", resumable = true)]
        fn init_storage(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            load(cx, arg(args, 0)?, args.get(1).copied(), false)
        }
        #[tjs::method(name = "getNextLine", resumable = true)]
        fn next(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            next_row(cx, cx.this(), None)
        }
        #[tjs::method(resumable = true)]
        fn parse(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if let Some(&text) = args.first() {
                initialize(cx, text)?;
            }
            start_parse(cx, cx.this())
        }
        #[tjs::method(name = "parseStorage", resumable = true)]
        fn parse_storage(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if let Some(&file) = args.first() {
                load(cx, file, args.get(1).copied(), true)
            } else {
                start_parse(cx, cx.this())
            }
        }
    }
}

fn check_text(text: &[u16]) -> NativeResult<()> {
    if text.len() > TEXT_BYTES / 2 {
        Err(NativeError::Message("CSV exceeds text size limit"))
    } else {
        Ok(())
    }
}
fn string(cx: &NativeCx<'_>, input: Value) -> NativeResult<Vec<u16>> {
    match input {
        Value::Void => Ok(Vec::new()), // AsStringNoAddRef accepts void as empty.
        Value::Str(id) => {
            let text = cx.heap().string(id)?;
            check_text(text)?;
            Ok(text.to_vec())
        }
        _ => Err(NativeError::Type("a String or void")),
    }
}
fn set_input(s: &mut parser::State, text: Vec<u16>) {
    s.input = Some(Input {
        text: text.into(),
        position: 0,
    });
    s.line = 0;
    s.generation = s.generation.wrapping_add(1);
}
fn initialize(cx: &mut NativeCx<'_>, text: Value) -> NativeResult<()> {
    let text = string(cx, text)?;
    state(cx, cx.this(), |s| set_input(s, text))
}
fn error(error: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(error.to_string())
}

struct Load {
    owner: ObjId,
    generation: u64,
    stream: Box<dyn krkr_engine::assets::Stream>,
    bytes: Vec<u8>,
    remaining: usize,
    utf8: bool,
    parse: bool,
}
impl Trace for Load {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
    }
}
fn load(
    cx: &mut NativeCx<'_>,
    name: Value,
    utf8: Option<Value>,
    parse: bool,
) -> NativeResult<NativeStep> {
    let name = string(cx, name)?;
    let utf8 = utf8
        .map(|v| value::to_integer(cx.heap(), v))
        .transpose()?
        .unwrap_or(0) as i32
        != 0;
    let owner = cx.this();
    let generation = state(cx, owner, |s| {
        // A failed replacement closes the previous source but keeps its line number.
        s.input = None;
        s.generation = s.generation.wrapping_add(1);
        s.generation
    })?;
    storages::managed::plans(
        cx,
        vec![(name, true)],
        ((owner, generation), (utf8, parse)),
        |((owner, generation), (utf8, parse)), _, mut plans| {
            let plan = plans.pop().flatten().expect("required CSV plan");
            if plan.bytes > TEXT_BYTES as u64 {
                return Err(NativeError::Message("CSV exceeds input size limit"));
            }
            let remaining = plan.bytes as usize;
            let stream = plan.open().map_err(error)?;
            Ok(NativeStep::Continue(Box::new(Load {
                owner,
                generation,
                stream,
                bytes: Vec::with_capacity(remaining),
                remaining,
                utf8,
                parse,
            })))
        },
    )
}
impl NativeContinuation for Load {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if state(cx, self.owner, |s| s.generation)? != self.generation {
            return Err(NativeError::Message("CSV source changed during loading"));
        }
        if self.remaining != 0 {
            let mut chunk = [0; 64 * 1024];
            let count = self.remaining.min(chunk.len());
            self.stream.read_exact(&mut chunk[..count]).map_err(error)?;
            self.bytes.extend_from_slice(&chunk[..count]);
            self.remaining -= count;
            return Ok(NativeStep::Continue(self));
        }
        let text = if self.utf8 {
            // The explicit binary path converts a NUL-terminated UTF-8 buffer;
            // it neither consumes BOMs nor interprets Kirikiri text-stream modes.
            let end = self
                .bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(self.bytes.len());
            let text = std::str::from_utf8(&self.bytes[..end]).map_err(error)?;
            if text.encode_utf16().count() > TEXT_BYTES / 2 {
                return Err(NativeError::Message("CSV exceeds text size limit"));
            }
            text.encode_utf16().collect()
        } else {
            krkr_engine::assets::text::decode(&self.bytes, &[117, 116, 102, 45, 56], TEXT_BYTES)
                .map_err(error)?
        };
        check_text(&text)?;
        state(cx, self.owner, |s| set_input(s, text))?;
        if self.parse {
            start_parse(cx, self.owner)
        } else {
            Ok(NativeStep::Return(Value::Void))
        }
    }
}

struct Capture {
    owner: ObjId,
    target: ObjId,
    key: Value,
    missing: Value,
    checked: bool,
}
impl Trace for Capture {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.target.trace(visit);
        self.key.trace(visit);
        self.missing.trace(visit);
    }
}
fn start_parse(cx: &mut NativeCx<'_>, owner: ObjId) -> NativeResult<NativeStep> {
    let (loaded, target) = state(cx, owner, |s| {
        (s.input.is_some(), s.target.unwrap_or(owner))
    })?;
    if !loaded {
        return Ok(NativeStep::Return(Value::Void));
    }
    let key = Value::Str(
        cx.heap_mut()
            .alloc_string("doLine".encode_utf16().collect::<Vec<_>>()),
    );
    let missing = Value::Obj(cx.heap_mut().alloc_dictionary().into());
    Box::new(Capture {
        owner,
        target,
        key,
        missing,
        checked: false,
    })
    .read()
}
impl Capture {
    fn read(self: Box<Self>) -> NativeResult<NativeStep> {
        Ok(NativeStep::GetOr {
            object: Value::Obj(ObjRef::bound(self.target)),
            key: self.key,
            raw: true,
            fallback: self.missing,
            continuation: self,
        })
    }
}
impl NativeContinuation for Capture {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if !self.checked {
            if value::strict_equal(cx.heap(), result, self.missing)? {
                return Ok(NativeStep::Return(Value::Void));
            }
            if let Value::Obj(reference) = result
                && let Some(object) = reference.object
                && !cx.heap().is_valid(object)?
            {
                return Ok(NativeStep::Return(Value::Void));
            }
            // Dictionary returns void for an absent ordinary PropGet. Its
            // IsValid(name), unlike that read, still reports no member.
            if matches!(result, Value::Void) {
                let Value::Str(key) = self.key else {
                    unreachable!()
                };
                let units = cx.heap().string(key)?.to_vec();
                let key = cx.heap_mut().intern(&units);
                if cx.heap().member(self.target, key)?.is_none() {
                    return Ok(NativeStep::Return(Value::Void));
                }
            }
            self.checked = true;
            return self.read();
        }
        let Value::Obj(reference) = result else {
            return Err(NativeError::Type("a doLine object"));
        };
        let Some(object) = reference.object else {
            return Err(NativeError::Type("a non-null doLine object"));
        };
        // AsObject captures the object, not the property's bound this. The
        // reference explicitly calls it with the target for all rows.
        let callback = Value::Obj(ObjRef {
            object: Some(object),
            this: Some(self.target),
        });
        next_row(cx, self.owner, Some(callback))
    }
}

struct Row {
    owner: ObjId,
    generation: u64,
    callback: Option<Value>,
    reader: reader::Row,
}
impl Trace for Row {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.callback.trace(visit);
    }
}
fn next_row(
    cx: &mut NativeCx<'_>,
    owner: ObjId,
    callback: Option<Value>,
) -> NativeResult<NativeStep> {
    let pending = state(cx, owner, |s| {
        let input = s.input.as_ref()?;
        if input.position == input.text.len() {
            s.input = None;
            return None;
        }
        Some((
            s.generation,
            reader::Row::new(
                input.text.clone(),
                input.position,
                s.separator,
                s.newline.clone(),
            ),
        ))
    })?;
    match pending {
        Some((generation, reader)) => Ok(NativeStep::Continue(Box::new(Row {
            owner,
            generation,
            callback,
            reader,
        }))),
        None => Ok(NativeStep::Return(Value::Void)),
    }
}
impl NativeContinuation for Row {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if state(cx, self.owner, |s| s.generation)? != self.generation {
            return Err(NativeError::Message(
                "CSV source changed during row parsing",
            ));
        }
        if !self.reader.advance()? {
            return Ok(NativeStep::Continue(self));
        }
        let line = state(cx, self.owner, |s| {
            s.input.as_mut().expect("current CSV input").position = self.reader.position;
            s.line = s.line.wrapping_add(1);
            s.generation = s.generation.wrapping_add(1);
            s.line
        })?;
        let row = Array(self.reader.fields.into_iter().map(Utf16)).into_tjs(cx.heap_mut())?;
        if let Some(callback) = self.callback {
            let Value::Obj(reference) = callback else {
                unreachable!()
            };
            let object = reference.object.expect("captured callback");
            // A non-callable object/invalidated function returns a failed
            // FuncCall status in the reference; that status is ignored.
            let callable = cx.heap().is_valid(object)?
                && matches!(
                    cx.heap().object(object)?.kind(),
                    ObjectKind::Function
                        | ObjectKind::NativeFunction
                        | ObjectKind::Class
                        | ObjectKind::NativeClass
                );
            let next = flow::callback(
                ParseNext {
                    owner: self.owner,
                    callback,
                },
                |s, cx, _| next_row(cx, s.owner, Some(s.callback)),
            );
            if callable {
                Ok(NativeStep::CallDiscard {
                    function: callback,
                    arguments: vec![row, Value::Int(line.into())],
                    continuation: next,
                })
            } else {
                Ok(NativeStep::Continue(next))
            }
        } else {
            Ok(NativeStep::Return(row))
        }
    }
}
#[derive(tjs_bind::Trace)]
struct ParseNext {
    owner: ObjId,
    callback: Value,
}
