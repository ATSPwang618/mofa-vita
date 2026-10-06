//! Reference author/license notices: reference-notices.txt.
//! Kirikiri2 expat/Main.cpp: SAX callbacks execute on the caller's resumable VM.
mod reader;
use reader::{Data, LIMIT, Parser, Position};
use std::{cell::Cell, io::Read, rc::Rc};
use tjs_bind::{RestArgs, flow};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef, Trace,
    Value,
};
const HANDLERS: [&str; 9] = [
    "startElement",
    "endElement",
    "characterData",
    "processingInstruction",
    "comment",
    "startCdataSection",
    "endCdataSection",
    "defaultHandler",
    "defaultHandlerExpand",
];
krkr_engine::native_plugin! {
    pub(crate) Expat { names: ["expat.dll","expat.tpm"], classes: [bindings], extensions: [], }
}
#[tjs_bind::class(name = "XMLParser")]
mod bindings {
    use super::*;
    #[derive(tjs_bind::Trace)]
    pub struct State {
        pub(super) target: Option<ObjId>,
        #[trace(skip = "Parser status has no VM references")]
        pub(super) position: Position,
        pub(super) error: i64,
        pub(super) started: bool,
        #[trace(skip = "Cancellation-safe parse lease contains no VM references")]
        pub(super) busy: Rc<Cell<bool>>,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                target: None,
                position: Position {
                    line: 1,
                    ..Default::default()
                },
                error: 0,
                started: false,
                busy: Rc::new(Cell::new(false)),
            }
        }
    }
    impl State {
        #[tjs::constructor]
        fn new(#[tjs(default = Value::Void)] target: Value) -> NativeResult<Self> {
            let target = match target {
                Value::Void => None,
                Value::Obj(r) => r.object,
                _ => return Err(NativeError::Type("an XML callback target")),
            };
            Ok(Self {
                target,
                ..Self::default()
            })
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.target = None;
        }
        #[tjs::getter(name = "errorCode")]
        fn error_code(&self) -> i64 {
            self.error
        }
        #[tjs::getter(name = "errorString")]
        fn error_string(&self) -> String {
            reader::error_string(self.error).to_owned()
        }
        #[tjs::getter(name = "currentByteIndex")]
        fn byte_index(&self) -> i64 {
            if self.started {
                self.position.index as i64
            } else {
                -1
            }
        }
        #[tjs::getter(name = "currentByteCount")]
        fn byte_count(&self) -> i64 {
            self.position.count as i64
        }
        #[tjs::getter(name = "currentLineNumber")]
        fn line(&self) -> i64 {
            self.position.line as i64
        }
        #[tjs::getter(name = "currentColumnNumber")]
        fn column(&self) -> i64 {
            self.position.column as i64
        }
        #[tjs::method(resumable = true)]
        fn parse(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let value = crate::exports::arg(args, 0)?;
            let units = units(cx, value)?;
            let text = String::from_utf16(&units)
                .map_err(|_| NativeError::Message("invalid XML UTF-16"))?;
            if text.len() > LIMIT {
                return Err(NativeError::Message("XML input exceeds limit"));
            }
            let job = begin(cx, args.get(1).copied())?;
            job.start(cx, text.into_bytes())
        }
        #[tjs::method(name = "parseStorage", resumable = true)]
        fn storage(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let name = units(cx, crate::exports::arg(args, 0)?)?;
            let job = begin(cx, args.get(1).copied())?;
            krkr_engine::storages::managed::plans(
                cx,
                vec![(name, true)],
                job,
                |job, cx, mut plans| {
                    let plan = plans.pop().flatten().expect("required XML source");
                    if plan.bytes > LIMIT as u64 {
                        return Err(NativeError::Message("XML input exceeds limit"));
                    }
                    krkr_engine::extensions::run_work(
                        cx,
                        move |cancel| {
                            let mut stream = plan.open().map_err(detail)?;
                            let mut data = Vec::new();
                            let mut buf = [0; 8192];
                            loop {
                                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                    return Err(NativeError::Message("XML load cancelled"));
                                }
                                let n = stream.read(&mut buf).map_err(detail)?;
                                if n == 0 {
                                    break;
                                }
                                if data.len() + n > LIMIT {
                                    return Err(NativeError::Message("XML input exceeds limit"));
                                }
                                data.extend_from_slice(&buf[..n]);
                            }
                            Ok(data)
                        },
                        Box::new(job),
                    )
                },
            )
        }
    }
}
fn detail(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
fn units(cx: &NativeCx<'_>, value: Value) -> NativeResult<Vec<u16>> {
    match value {
        Value::Void => Ok(Vec::new()),
        Value::Str(id) => {
            let units = cx.heap().string(id)?;
            if units.len() > LIMIT / 2 {
                return Err(NativeError::Message("XML input exceeds limit"));
            }
            Ok(units.to_vec())
        }
        _ => Err(NativeError::Type("an XML String")),
    }
}
struct Lease(Rc<Cell<bool>>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.set(false)
    }
}
struct Start {
    owner: ObjId,
    target: ObjId,
    _lease: Lease,
}
impl Trace for Start {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.target.trace(visit)
    }
}
fn begin(cx: &mut NativeCx<'_>, target: Option<Value>) -> NativeResult<Start> {
    let owner = cx.this();
    let target = target
        .map(crate::exports::object)
        .transpose()?
        .unwrap_or(owner);
    let (target, busy) = bindings::with_state(cx, owner, |s| {
        if s.busy.replace(true) {
            return Err(NativeError::Message("XMLParser is already parsing"));
        }
        s.error = 0;
        s.started = true;
        s.position = Position {
            line: 1,
            ..Default::default()
        };
        Ok((s.target.unwrap_or(target), s.busy.clone()))
    })??;
    Ok(Start {
        owner,
        target,
        _lease: Lease(busy),
    })
}
impl krkr_engine::extensions::WorkContinuation<Vec<u8>> for Start {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, data: Vec<u8>) -> NativeResult<NativeStep> {
        self.start(cx, data)
    }
}
impl Start {
    fn start(self, cx: &mut NativeCx<'_>, data: Vec<u8>) -> NativeResult<NativeStep> {
        let job = Box::new(Parse {
            start: self,
            parser: Parser::new(data),
            enabled: [false; 9],
            capture: 0,
        });
        job.capture(cx)
    }
}
struct Parse {
    start: Start,
    parser: Parser,
    enabled: [bool; 9],
    capture: usize,
}
impl Trace for Parse {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.start.trace(visit)
    }
}
fn string(cx: &mut NativeCx<'_>, text: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(text.encode_utf16().collect::<Vec<_>>()),
    )
}
impl Parse {
    fn capture(self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let key = string(cx, HANDLERS[self.capture]);
        Ok(NativeStep::GetOr {
            object: Value::Obj(self.start.target.into()),
            key,
            raw: true,
            fallback: Value::Void,
            continuation: self,
        })
    }
    fn next(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        // Accessing state validates that a callback did not invalidate the parser.
        bindings::with_state(cx, self.start.owner, |_| ())?;
        let message = match self.parser.next(self.enabled[8] || !self.enabled[7]) {
            Ok(Some(m)) => m,
            result => {
                let (code, position) = match result {
                    Ok(None) => (0, self.parser.end()),
                    Err(e) => (e.code, e.position),
                    _ => unreachable!(),
                };
                bindings::with_state(cx, self.start.owner, |s| {
                    s.error = code;
                    s.position = position;
                })?;
                return Ok(NativeStep::Return(Value::Int(i64::from(code == 0))));
            }
        };
        bindings::with_state(cx, self.start.owner, |s| {
            s.position = message.position.clone()
        })?;
        let mut handler = message.handler;
        let data = if handler < 7 && self.enabled[handler] {
            message.data
        } else {
            handler = if self.enabled[8] { 8 } else { 7 };
            if !self.enabled[handler] || message.raw.is_empty() {
                return Ok(NativeStep::Continue(flow::callback(self, |s, cx, _| {
                    s.next(cx)
                })));
            }
            Data::Text(message.raw)
        };
        let mut args = Vec::new();
        match data {
            Data::None => {}
            Data::Text(s) => args.push(string(cx, &s)),
            Data::Pair(a, b) => {
                args.push(string(cx, &a));
                args.push(string(cx, &b));
            }
            Data::Attributes(name, attrs) => {
                args.push(string(cx, &name));
                let dict = cx.heap_mut().alloc_dictionary();
                for (key, value) in attrs {
                    let value = string(cx, &value);
                    let key = cx
                        .heap_mut()
                        .intern(&key.encode_utf16().collect::<Vec<_>>());
                    cx.heap_mut().set_member(dict, key, value)?;
                }
                args.push(Value::Obj(dict.into()));
            }
        }
        let key = string(cx, HANDLERS[handler]);
        Ok(NativeStep::GetProperty {
            object: Value::Obj(self.start.target.into()),
            key,
            flags: tjs_core::MemberFlags {
                ignore_property: true,
                ..Default::default()
            },
            continuation: flow::callback((self, args), |(s, args), _cx, method| {
                let method = crate::exports::object(method)?;
                Ok(NativeStep::Call {
                    function: Value::Obj(ObjRef {
                        object: Some(method),
                        this: Some(s.start.target),
                    }),
                    arguments: args,
                    continuation: flow::callback(s, |s, cx, _| s.next(cx)),
                })
            }),
        })
    }
}
impl NativeContinuation for Parse {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        self.enabled[self.capture] = match value {
            Value::Obj(r) => r
                .object
                .map(|o| cx.heap().is_valid(o))
                .transpose()?
                .unwrap_or(false),
            _ => false,
        };
        self.capture += 1;
        if self.capture < 9 {
            self.capture(cx)
        } else {
            self.next(cx)
        }
    }
}
