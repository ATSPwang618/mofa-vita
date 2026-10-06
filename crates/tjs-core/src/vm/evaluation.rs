//! The VM suspends at eval; its host compiles code and resumes the same stack.
use std::sync::Arc;

use super::{
    CallError, LoadedModule, State, Vm,
    calls::{Invocation, ReturnTo},
};
use crate::{Diagnostic, FunctionId, Heap, Module, TraceFrame, Value};

#[derive(Clone, Debug)]
pub struct CompileRequest {
    pub source: ScriptSource,
    pub output: Option<CompileOutput>,
    /// Expression mode is independent of whether the caller consumes a result.
    pub expression: bool,
    pub result_needed: bool,
    pub name: String,
    pub line_offset: i32,
}

#[derive(Clone, Debug)]
pub enum ScriptSource {
    Text(Arc<[u16]>),
    Bytecode(Arc<[u8]>),
}
#[derive(Clone, Debug)]
pub struct CompileOutput {
    pub name: Vec<u16>,
    pub debug: bool,
}

pub(super) struct PendingCompile {
    pub request: CompileRequest,
    pub(super) invocation: Invocation,
    advance: bool,
}

impl Vm {
    pub(super) fn evaluate_native(
        &mut self,
        heap: &mut Heap,
        request: CompileRequest,
        context: Option<crate::ObjId>,
        mut invocation: Invocation,
        advance: bool,
    ) {
        // Arguments have already been consumed by the native adapter.
        self.registers.truncate(invocation.start);
        invocation.end = invocation.start;
        invocation.this = Some(context.unwrap_or_else(|| self.ensure_global(heap)));
        self.pending_compile = Some(PendingCompile {
            request,
            invocation,
            advance,
        });
        self.state = State::Compiling;
    }

    pub(super) fn evaluate(
        &mut self,
        heap: &mut Heap,
        source: Value,
        destination: ReturnTo,
    ) -> Result<Option<CompileRequest>, CallError> {
        let Value::Str(source) = crate::value::to_string(heap, source)? else {
            unreachable!("string conversion")
        };
        let units = crate::string::c_string(heap.string(source)?);
        if units.is_empty() {
            self.deliver(destination, Value::Void);
            self.frame.pc += 1;
            return Ok(None);
        }
        let request = CompileRequest {
            source: ScriptSource::Text(Arc::from(units)),
            output: None,
            expression: true,
            result_needed: destination.result_needed(),
            name: "<eval>".into(),
            line_offset: 0,
        };
        self.pending_compile = Some(PendingCompile {
            request: request.clone(),
            invocation: Invocation {
                start: self.registers.len(),
                end: self.registers.len(),
                this: Some(self.this(heap)),
                destination,
            },
            advance: true,
        });
        self.state = State::Compiling;
        Ok(Some(request))
    }

    /// Supply the result of the outstanding synchronous compile request. The
    /// caller must not reset/reuse this VM between requesting and completing it.
    /// Script execution and catch dispatch resume in the next budgeted slice.
    pub fn resume_compile(
        &mut self,
        heap: &mut Heap,
        result: Result<Module, Diagnostic>,
    ) -> Result<(), Diagnostic> {
        let pending = self
            .pending_compile
            .take()
            .ok_or_else(|| self.diagnostic("VM has no outstanding compile request"))?;
        self.state = State::Runnable;
        let result = result.and_then(|module| {
            if pending.request.output.is_some() {
                self.complete(pending.invocation, Value::Void, pending.advance);
                return Ok(());
            }
            let global = self.ensure_global(heap);
            let index = self.modules.len();
            self.modules.push(LoadedModule::new(&module, Some(global)));
            if let Err(error) = self.enter(
                heap,
                index,
                FunctionId(0),
                None,
                pending.invocation,
                pending.advance,
            ) {
                self.modules.pop();
                return Err(self.diagnostic(error.to_string()));
            }
            Ok(())
        });
        if let Err(mut error) = result {
            // A native continuation resumes after its original call instruction.
            // Compilation can fail before entering a child frame, so restore
            // that call site for catch-range lookup just as resume_wait does.
            if !pending.advance {
                self.frame.pc = self.frame.pc.saturating_sub(1);
            }
            if error.trace.is_empty() {
                error.trace.push(TraceFrame {
                    function: "<eval>".into(),
                    pc: 0,
                    span: error.span,
                });
                error.trace.extend(self.diagnostic(&error.message).trace);
            }
            self.resume_error = Some(error);
        }
        Ok(())
    }

    /// Active frames own loaded pools. Escaping functions own their code and
    /// pool through the heap, so completed evals need no permanent VM root.
    pub(super) fn release_module(&mut self, index: usize) {
        if index == 0
            || self.frame.module == index
            || self.callers.iter().any(|frame| frame.module == index)
        {
            return;
        }
        self.modules.swap_remove(index);
        let moved = self.modules.len();
        if self.frame.module == moved {
            self.frame.module = index;
        }
        for frame in &mut self.callers {
            if frame.module == moved {
                frame.module = index;
            }
        }
    }

    pub(super) fn release_inactive_modules(&mut self) {
        for index in (1..self.modules.len()).rev() {
            self.release_module(index);
        }
    }
}
