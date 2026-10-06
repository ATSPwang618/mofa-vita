use super::*;
use krkr_engine::{
    assets::{Stream, name, text},
    storages,
};
use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};
use tjs_core::{CompileRequest, NativeTryContinuation, ScriptSource};

fn string(cx: &NativeCx<'_>, value: Value) -> NativeResult<Vec<u16>> {
    if matches!(value, Value::Octet(_)) {
        return Err(NativeError::Type(
            "a string-convertible storage name or mode",
        ));
    }
    Ok(tjs_core::string::c_string(&value::to_string_units(cx.heap(), value)?).to_vec())
}
pub(super) fn start(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let name = string(cx, arg(args, 0)?)?;
    let mode = args
        .get(1)
        .copied()
        .filter(|v| !matches!(v, Value::Void))
        .map(|v| string(cx, v))
        .transpose()?
        .unwrap_or_default();
    let context = args
        .get(2)
        .copied()
        .filter(|v| !matches!(v, Value::Void))
        .map(closure)
        .transpose()?
        .and_then(|r| r.object);
    storages::managed::plans(
        cx,
        vec![(name.clone(), true)],
        ((context, cx.result_needed()), (name, mode)),
        |((context, needed), (name, mode)), cx, mut plans| {
            let limit = storages::service(cx)?
                .borrow()
                .limits()
                .max_read_bytes
                .min(BYTES);
            let plan = plans.pop().flatten().expect("required ScriptsEx plan");
            if plan.bytes > limit as u64 {
                return Err(NativeError::Message("ScriptsEx storage exceeds size limit"));
            }
            let offset = text::offset(&mode).map_err(error)?.unwrap_or(0);
            let mut stream = plan.open().map_err(error)?;
            stream.seek(SeekFrom::Start(offset)).map_err(error)?;
            let encoding = krkr_engine::scripts::text_encoding(cx.heap_mut())?;
            Ok(NativeStep::Continue(Box::new(Load {
                stream,
                bytes: vec![],
                encoding,
                limit,
                name: String::from_utf16_lossy(name::split_name(&name).1),
                context,
                needed,
            })))
        },
    )
}
struct Load {
    stream: Box<dyn Stream>,
    bytes: Vec<u8>,
    encoding: Vec<u16>,
    limit: usize,
    name: String,
    context: Option<ObjId>,
    needed: bool,
}
impl Trace for Load {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.context.trace(v);
    }
}
impl NativeContinuation for Load {
    fn resume(mut self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let mut chunk = [0u8; 64 * 1024];
        let n = self.stream.read(&mut chunk).map_err(error)?;
        if n != 0 {
            if n > self.limit.saturating_sub(self.bytes.len()) {
                return Err(NativeError::Message("ScriptsEx storage exceeds size limit"));
            }
            self.bytes.extend_from_slice(&chunk[..n]);
            return Ok(NativeStep::Continue(self));
        }
        let decoded = text::decode(&self.bytes, &self.encoding, self.limit).map_err(error)?;
        if decoded.len() > self.limit.saturating_sub(18) / 2 {
            return Err(NativeError::Message(
                "ScriptsEx wrapped source exceeds size limit",
            ));
        }
        let mut source = Vec::with_capacity(decoded.len() + 9);
        source.extend("(const)[".encode_utf16());
        source.extend(decoded);
        source.push(93);
        // Match the original constant wrapper and expression-mode return.
        // Nested containers require (const); this is not a separate sandbox
        // for arbitrary TJS source or its preprocessor.
        Ok(NativeStep::Try {
            task: Box::new(Eval {
                request: CompileRequest {
                    source: ScriptSource::Text(Arc::from(source)),
                    output: None,
                    expression: true,
                    result_needed: true,
                    name: self.name,
                    line_offset: 0,
                },
                context: self.context,
            }),
            continuation: Box::new(First {
                needed: self.needed,
            }),
        })
    }
}
struct Eval {
    request: CompileRequest,
    context: Option<ObjId>,
}
impl Trace for Eval {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.context.trace(v);
    }
}
impl NativeContinuation for Eval {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Evaluate {
            request: self.request,
            context: self.context,
        })
    }
}
struct First {
    needed: bool,
}
impl Trace for First {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeTryContinuation for First {
    fn resume(
        self: Box<Self>,
        _: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        Ok(match result {
            Err(e) => NativeStep::Throw(e),
            Ok(object) if self.needed => NativeStep::GetOr {
                object,
                key: Value::Int(0),
                raw: true,
                fallback: Value::Void,
                continuation: tjs_bind::flow::identity(),
            },
            Ok(_) => NativeStep::Return(Value::Void),
        })
    }
}
