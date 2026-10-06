//! Unhandled errors pass through System.exceptionHandler at the owning context
//! boundary. The scheduler retains both executions until the host takes it.
use super::*;
use tjs_core::{NativeContinuation, NativeCx, NativeStep, Trace};

enum Stage {
    Start,
    System,
    Handler,
    Class,
    Constructed,
    Trace,
    Returned,
}
struct Handler {
    global: ObjId,
    stage: Stage,
    function: Value,
    exception: Value,
    message: Value,
    trace: Value,
    build: bool,
}
impl Trace for Handler {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(Value::Obj(self.global.into()));
        for value in [self.function, self.exception, self.message, self.trace] {
            visit(value);
        }
    }
}
fn object(value: Value) -> bool {
    matches!(
        value,
        Value::Obj(ObjRef {
            object: Some(_),
            ..
        })
    )
}
fn key(cx: &mut NativeCx<'_>, name: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    )
}
impl Handler {
    fn call(self: Box<Self>) -> NativeStep {
        NativeStep::Call {
            function: self.function,
            arguments: vec![self.exception],
            continuation: self,
        }
    }
}
impl NativeContinuation for Handler {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        Ok(match self.stage {
            Stage::Start => {
                self.stage = Stage::System;
                NativeStep::GetOptional {
                    object: Value::Obj(ObjRef::bound(self.global)),
                    key: key(cx, "System"),
                    continuation: self,
                }
            }
            Stage::System => {
                if !object(result) {
                    return Ok(NativeStep::Return(Value::Int(0)));
                }
                self.stage = Stage::Handler;
                NativeStep::GetOptional {
                    object: result,
                    key: key(cx, "exceptionHandler"),
                    continuation: self,
                }
            }
            Stage::Handler => {
                if !object(result) {
                    return Ok(NativeStep::Return(Value::Int(0)));
                }
                self.function = result;
                if self.build {
                    self.stage = Stage::Class;
                    NativeStep::Get {
                        object: Value::Obj(ObjRef::bound(self.global)),
                        key: key(cx, "Exception"),
                        continuation: self,
                    }
                } else {
                    self.stage = Stage::Returned;
                    self.call()
                }
            }
            Stage::Class => {
                self.stage = Stage::Constructed;
                NativeStep::Construct {
                    class: result,
                    arguments: vec![self.message],
                    continuation: self,
                }
            }
            Stage::Constructed => {
                self.exception = result;
                self.stage = Stage::Trace;
                NativeStep::Set {
                    object: result,
                    key: key(cx, "trace"),
                    value: self.trace,
                    continuation: self,
                }
            }
            Stage::Trace => {
                self.stage = Stage::Returned;
                self.call()
            }
            Stage::Returned => NativeStep::Return(Value::Int(i64::from(result.truthy(cx.heap())?))),
        })
    }
}
fn diagnostic(result: &RuntimeExit) -> &tjs_core::Diagnostic {
    match result {
        RuntimeExit::Fault(error) => error,
        RuntimeExit::Thrown(error) => &error.diagnostic,
        _ => unreachable!("completed exception"),
    }
}
impl<C: Clock + 'static> Engine<C> {
    /// None means the completed context is now running its exception handler.
    pub(super) fn handle_exception(
        &mut self,
        context: ContextId,
        result: RuntimeExit,
    ) -> Option<RuntimeExit> {
        if let Some(original) = self.driver.original_result(context) {
            let resolved = match result {
                RuntimeExit::Finished(Value::Int(1)) => RuntimeExit::Finished(Value::Void),
                RuntimeExit::Finished(_) => original.clone(),
                _ => {
                    // A failing handler must not replace the source and stack
                    // of the fault that caused it to run.
                    let secondary = diagnostic(&result);
                    let location = secondary
                        .span
                        .and_then(|span| {
                            let file = self.runtime().sources.get(span.source())?;
                            let (line, column) = file.line_column(span.start())?;
                            Some(format!(" ({}:{line}:{column})", file.name()))
                        })
                        .unwrap_or_default();
                    let mut error = diagnostic(original).clone();
                    error.message = format!(
                        "{}\nSystem.exceptionHandler failed: {}{}",
                        error.message, secondary.message, location,
                    );
                    RuntimeExit::Fault(error)
                }
            };
            assert!(self.driver.resolve_completion(context, resolved.clone()));
            return Some(resolved);
        }
        if matches!(result, RuntimeExit::Finished(_)) {
            return Some(result);
        }
        // The reference removes a failing continuous handler before calling
        // System.exceptionHandler, even when that handler accepts the error.
        if let Some(CallbackState {
            kind: CallbackKind::Continuous(function),
            ..
        }) = self.callbacks.get(&context)
        {
            self.system.borrow_mut().abort_continuous(*function);
        }
        let mut handler = Handler {
            global: self.global,
            stage: Stage::Start,
            function: Value::Void,
            exception: Value::Void,
            message: Value::Void,
            trace: Value::Void,
            build: false,
        };
        match &result {
            RuntimeExit::Thrown(error) => handler.exception = error.value,
            RuntimeExit::Fault(error) => {
                handler.build = true;
                let runtime = self.driver.runtime_mut();
                handler.message = Value::Str(
                    runtime
                        .heap
                        .alloc_string(error.message.encode_utf16().collect::<Vec<_>>()),
                );
                let trace = error
                    .trace
                    .iter()
                    .map(|frame| {
                        let location = frame
                            .span
                            .and_then(|span| {
                                let source = runtime.sources.get(span.source())?;
                                let (line, _) = source.line_column(span.start())?;
                                Some(format!("{}({line})", source.name()))
                            })
                            .unwrap_or_else(|| "(unknown)".into());
                        format!("{location} [{}]", frame.function)
                    })
                    .collect::<Vec<_>>()
                    .join(" <-- ");
                handler.trace = Value::Str(
                    runtime
                        .heap
                        .alloc_string(trace.encode_utf16().collect::<Vec<_>>()),
                );
            }
            _ => unreachable!("terminal context"),
        }
        let vm = Vm::task(self.global, Box::new(handler));
        self.driver
            .resume_completed(context, vm)
            .unwrap_or_else(|_| panic!("completed context can resume"));
        None
    }
}
