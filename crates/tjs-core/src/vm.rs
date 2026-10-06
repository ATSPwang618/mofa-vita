//! Execution slices retain explicit frames and registers; script recursion never
//! recurses through the Rust call stack.

mod calls;
mod dispatch;
mod entry;
mod errors;
mod evaluation;
mod inspection;
pub use entry::Callback;
pub use evaluation::{CompileOutput, CompileRequest, ScriptSource};
mod functions;
mod lifecycle;
mod missing;
mod native;
mod properties;
mod strings;
mod types;
mod updates;
use calls::ReturnTo;
use dispatch::Continuation;

use std::num::NonZeroU32;

use crate::{
    Diagnostic, Heap, HeapError, Instruction, Module, NativeError, ObjId, ObjRef, Phase,
    ScriptException, Span, TraceFrame, Value,
    member::{self, MemberError},
    value,
};

#[derive(Clone, Copy, Debug)]
pub struct RunBudget {
    // Exception-frame searches and suspended dispatch resumptions also consume
    // one work allowance.
    max_instructions: NonZeroU32,
}

impl RunBudget {
    pub fn new(max_instructions: u32) -> Option<Self> {
        NonZeroU32::new(max_instructions).map(|max_instructions| Self { max_instructions })
    }

    pub fn max_instructions(self) -> u32 {
        self.max_instructions.get()
    }
}

/// Host policy, not a bytecode-format limit. Includes the root frame and original
/// argument copies in the stack accounting.
#[derive(Clone, Copy, Debug)]
pub struct VmLimits {
    pub max_call_depth: usize,
    pub max_stack_values: usize,
}

impl Default for VmLimits {
    fn default() -> Self {
        Self {
            max_call_depth: 1_024,
            max_stack_values: 1_048_576,
        }
    }
}

#[derive(Clone, Debug)]
pub enum VmExit {
    Finished(Value),
    Fault(Diagnostic),
    Thrown(ScriptException),
    Yielded,
    CompileRequest(CompileRequest),
    Waiting(crate::WaitRequest),
    Inspecting(crate::Inspection),
}

#[derive(Clone, Debug)]
enum State {
    Runnable,
    Compiling,
    Inspecting(crate::Inspection),
    Waiting(crate::WaitRequest),
    Finished(Value),
    Fault(Diagnostic),
    Thrown(ScriptException),
}

#[derive(Clone, Copy)]
struct PendingException {
    value: Value,
    runtime: bool,
    depth: usize,
}

#[derive(Clone, Copy)]
struct Frame {
    module: usize,
    function: usize,
    this: Option<ObjId>,
    callee: Option<ObjId>,
    pc: usize,
    base: usize,
    argument_start: usize,
    return_destination: ReturnTo,
}

impl Frame {
    fn root() -> Self {
        Self {
            module: 0,
            function: 0,
            this: None,
            callee: None,
            pc: 0,
            base: 0,
            argument_start: 0,
            return_destination: ReturnTo::Discard,
        }
    }
}

struct LoadedModule {
    module: Module,
    global: Option<ObjId>,
    function_pool: Option<ObjId>,
    constant_pool: Option<ObjId>,
}

impl LoadedModule {
    fn new(module: &Module, global: Option<ObjId>) -> Self {
        Self {
            module: module.clone(),
            global,
            function_pool: None,
            constant_pool: None,
        }
    }

    fn roots(&self) -> impl Iterator<Item = Value> + '_ {
        self.function_pool
            .map(|id| Value::Obj(id.into()))
            .into_iter()
            .chain(self.global.map(|id| Value::Obj(id.into())))
            .chain(self.constant_pool.map(|id| Value::Obj(id.into())))
    }

    fn constants(&mut self, heap: &mut Heap) -> ObjId {
        *self
            .constant_pool
            .get_or_insert_with(|| match self.function_pool {
                Some(pool) => heap.function_constants(pool),
                None => heap.alloc_constant_pool(&self.module),
            })
    }
}

#[derive(Debug, thiserror::Error)]
enum CallError {
    #[error(transparent)]
    Arithmetic(#[from] crate::value::ArithmeticError),
    #[error("VM call depth limit reached")]
    Depth,
    #[error("VM value stack limit reached")]
    Stack,
    #[error("value does not support this call or construction")]
    NotCallable,
    #[error(transparent)]
    Heap(#[from] HeapError),
    #[error(transparent)]
    Member(#[from] MemberError),
    #[error(transparent)]
    Native(#[from] NativeError),
}

pub struct Vm {
    modules: Vec<LoadedModule>,
    external_global: Option<ObjId>,
    registers: Vec<Value>,
    callers: Vec<Frame>,
    frame: Frame,
    limits: VmLimits,
    executed: u64,
    unwind_steps: u64,
    resume_steps: u64,
    pending_exception: Option<PendingException>,
    pending_compile: Option<evaluation::PendingCompile>,
    resume_error: Option<Diagnostic>,
    continuations: Vec<Continuation>,
    resume_value: Option<Value>,
    type_names: [Value; 7],
    finalize_name: Value,
    state: State,
}

impl Vm {
    pub fn new(module: &Module) -> Self {
        // The default stack budget exceeds the maximum verified entry window.
        Self::with_limits(module, VmLimits::default()).expect("entry fits default VM limits")
    }

    /// Use an existing global object from the same heap. Reset keeps this host
    /// environment; a VM created with new gets a fresh environment after reset.
    pub fn with_global(module: &Module, global: ObjId) -> Self {
        let mut vm = Self::new(module);
        vm.external_global = Some(global);
        vm.modules[0].global = Some(global);
        vm
    }

    pub fn with_limits(module: &Module, limits: VmLimits) -> Result<Self, Diagnostic> {
        if limits.max_call_depth == 0 || module.register_count() as usize > limits.max_stack_values
        {
            return Err(Diagnostic::new(
                Phase::Runtime,
                module.span_at(0),
                "entry does not fit VM limits",
            ));
        }
        Ok(Self {
            modules: vec![LoadedModule::new(module, None)],
            external_global: None,
            registers: vec![Value::Void; module.register_count() as usize],
            callers: Vec::new(),
            frame: Frame::root(),
            limits,
            executed: 0,
            unwind_steps: 0,
            resume_steps: 0,
            pending_exception: None,
            pending_compile: None,
            resume_error: None,
            continuations: Vec::new(),
            resume_value: None,
            type_names: [Value::Void; 7],
            finalize_name: Value::Void,
            state: State::Runnable,
        })
    }

    /// Retains allocated register/frame capacity for subsequent execution.
    pub fn reset(&mut self) {
        self.registers
            .truncate(self.modules[0].module.register_count() as usize);
        self.modules.truncate(1);
        self.modules[0].global = self.external_global;
        if self.external_global.is_none() {
            self.modules[0].function_pool = None;
        }
        self.registers.fill(Value::Void);
        self.clear_execution();
    }

    fn clear_execution(&mut self) {
        self.callers.clear();
        self.continuations.clear();
        self.resume_value = None;
        self.frame = Frame::root();
        self.executed = 0;
        self.unwind_steps = 0;
        self.resume_steps = 0;
        self.pending_exception = None;
        self.pending_compile = None;
        self.resume_error = None;
        self.state = State::Runnable;
    }

    pub fn pc(&self) -> usize {
        self.frame.pc
    }
    pub fn instructions_executed(&self) -> u64 {
        self.executed
    }
    pub fn call_depth(&self) -> usize {
        self.callers.len() + 1
    }
    pub fn work_executed(&self) -> u64 {
        self.executed + self.unwind_steps + self.resume_steps
    }
    pub fn is_unwinding(&self) -> bool {
        self.pending_exception.is_some()
    }
    pub fn stack_value_count(&self) -> usize {
        self.registers.len()
    }
    pub fn current_function(&self) -> &str {
        self.modules[self.frame.module].module.functions()[self.frame.function].name()
    }
    pub fn global(&self) -> Option<ObjId> {
        self.modules[0].global
    }
    pub fn registers(&self) -> &[Value] {
        &self.registers[self.frame.base..]
    }

    /// Original argument count and contents, unaffected by defaults or assignment.
    pub fn original_arguments(&self) -> &[Value] {
        &self.registers[self.frame.argument_start..self.frame.base]
    }

    /// Trace registers, original arguments, frame contexts, module environments
    /// and caches, and values retained by terminal/pending exception states.
    /// Combine roots from ALL VMs sharing the same heap before collecting.
    pub fn roots(&self) -> impl Iterator<Item = Value> + '_ {
        let terminal = match &self.state {
            State::Finished(value) => Some(*value),
            State::Thrown(exception) => Some(exception.value),
            _ => None,
        };
        self.registers
            .iter()
            .copied()
            .chain(self.modules.iter().flat_map(LoadedModule::roots))
            .chain(
                self.callers
                    .iter()
                    .chain(std::iter::once(&self.frame))
                    .flat_map(|frame| {
                        [frame.this, frame.callee]
                            .into_iter()
                            .flatten()
                            .map(|id| Value::Obj(id.into()))
                    }),
            )
            .chain(self.pending_exception.map(|pending| pending.value))
            .chain([self.finalize_name])
            .chain(self.type_names)
            .chain(self.resume_value)
            .chain(self.continuations.iter().flat_map(|c| c.action.roots()))
            .chain(
                self.pending_compile
                    .as_ref()
                    .and_then(|pending| pending.invocation.this)
                    .map(|id| Value::Obj(id.into())),
            )
            .chain(terminal)
    }

    pub fn current_span(&self) -> Option<Span> {
        self.modules[self.frame.module].module.functions()[self.frame.function]
            .span_at(self.frame.pc)
    }

    /// Capture only when reporting a fault or an explicit host stop. Execution
    /// does not allocate diagnostic strings or stack traces per instruction.
    pub fn diagnostic(&self, message: impl Into<String>) -> Diagnostic {
        let mut error = Diagnostic::new(Phase::Runtime, self.current_span(), message);
        let location = |frame: Frame, pc| {
            let function = &self.modules[frame.module].module.functions()[frame.function];
            let (name, original_pc) = function.trace_location(pc);
            TraceFrame {
                function: name,
                pc: original_pc,
                span: function.span_at(pc),
            }
        };
        error.trace.push(location(self.frame, self.frame.pc));
        for &caller in self.callers.iter().rev() {
            // A saved caller's PC is already advanced past its Call instruction.
            error.trace.push(location(caller, caller.pc - 1));
        }
        error
    }

    fn load_constant(&mut self, heap: &mut Heap, module: usize, constant: u32) -> Value {
        let loaded = &mut self.modules[module];
        let pool = loaded.constants(heap);
        heap.constant_value(pool, constant)
    }

    fn ensure_global(&mut self, heap: &mut Heap) -> ObjId {
        *self.modules[self.frame.module]
            .global
            .get_or_insert_with(|| heap.alloc_global())
    }

    fn this(&mut self, heap: &mut Heap) -> ObjId {
        self.frame.this.unwrap_or_else(|| self.ensure_global(heap))
    }

    fn member_receiver(&mut self, heap: &mut Heap, value: Value) -> Value {
        match value {
            Value::Obj(mut reference) if reference.this.is_none() => {
                reference.this = Some(self.this(heap));
                Value::Obj(reference)
            }
            _ => value,
        }
    }

    fn unwind_one(&mut self, heap: &mut Heap) -> Option<VmExit> {
        let pending = self.pending_exception.expect("exception is pending");
        // A native boundary belongs between its caller and the callbacks it
        // invoked. Inner script catch blocks are searched at deeper frames;
        // here it wins over a catch surrounding the native call itself.
        if let Some(index) = self.continuations.iter().rposition(|c| {
            c.depth == pending.depth + 1 && matches!(c.action, dispatch::Action::TryNative(_))
        }) {
            self.continuations.truncate(index + 1);
            let dispatch::Action::TryNative(caught) = &mut self.continuations[index].action else {
                unreachable!()
            };
            caught.error = Some(pending.value);
            self.frame = caught.frame;
            self.registers.truncate(caught.stack_len);
            self.callers.truncate(pending.depth);
            self.pending_exception = None;
            self.resume_value = Some(Value::Void);
            self.release_inactive_modules();
            return None;
        }
        let current = pending.depth == self.callers.len();
        let frame = if current {
            self.frame
        } else {
            self.callers[pending.depth]
        };
        let pc = if current { frame.pc } else { frame.pc - 1 };
        let function = &self.modules[frame.module].module.functions()[frame.function];
        if let Some(handler) = function.handler_at(pc) {
            self.registers
                .truncate(frame.base + function.register_count() as usize);
            self.registers[frame.base + handler.exception.0 as usize] = pending.value;
            self.callers.truncate(pending.depth);
            self.continuations.retain(|c| c.depth <= pending.depth);
            self.resume_value = None;
            self.frame = Frame {
                pc: handler.target as usize,
                ..frame
            };
            self.pending_exception = None;
            self.release_inactive_modules();
            if pending.runtime {
                let destination = frame.base + handler.exception.0 as usize;
                // The saved caller PC points past the catch entry, so errors
                // in a custom Exception constructor escape this protected range.
                self.frame.pc += 1;
                if let Err(error) = self.build_exception(heap, pending.value, destination) {
                    self.frame.pc -= 1;
                    return self.runtime_error(heap, error.to_string());
                }
            }
        } else if pending.depth > 0 {
            self.pending_exception = Some(PendingException {
                depth: pending.depth - 1,
                ..pending
            });
        } else {
            // Keep frames intact while searching, so an uncaught exception can
            // report its original throw site and callers without eager tracing.
            let exception = ScriptException {
                value: pending.value,
                diagnostic: self.diagnostic(format!(
                    "uncaught script exception: {}",
                    crate::exception::describe(heap, pending.value)
                )),
            };
            self.pending_exception = None;
            self.continuations.clear();
            self.resume_value = None;
            self.state = State::Thrown(exception.clone());
            return Some(VmExit::Thrown(exception));
        }
        None
    }

    /// Every slice for this VM must use the same originating heap. No heap borrow
    /// survives a slice; the host may collect using Vm::roots before resuming.
    pub fn run_slice(&mut self, heap: &mut Heap, budget: RunBudget) -> VmExit {
        match &self.state {
            State::Finished(value) => return VmExit::Finished(*value),
            State::Fault(error) => return VmExit::Fault(error.clone()),
            State::Thrown(exception) => return VmExit::Thrown(exception.clone()),
            State::Compiling => {
                return VmExit::CompileRequest(
                    self.pending_compile
                        .as_ref()
                        .expect("pending compilation")
                        .request
                        .clone(),
                );
            }
            State::Waiting(request) => return VmExit::Waiting(*request),
            State::Inspecting(kind) => return VmExit::Inspecting(*kind),
            State::Runnable => {}
        }
        // Compilation completes between slices. Handle its failure once here,
        // without adding an eval-specific branch to every ordinary instruction.
        if let Some(error) = self.resume_error.take() {
            self.resume_steps += 1;
            return self
                .runtime_diagnostic(heap, error)
                .unwrap_or(VmExit::Yielded);
        }
        // The active function changes only at call/return boundaries. Avoid a
        // module-table lookup for every arithmetic instruction in the same frame.
        let mut active_module = self.frame.module;
        let mut module = self.modules[active_module].module.clone();
        let mut function = &module.functions()[self.frame.function];
        macro_rules! runtime_error {
            ($message:expr) => {{
                if let Some(exit) = self.runtime_error(heap, $message) {
                    return exit;
                }
                if let Some(exit) = self.suspension() {
                    return exit;
                }
                continue;
            }};
        }
        for _ in 0..budget.max_instructions.get() {
            if self.pending_exception.is_some() {
                self.unwind_steps += 1;
                if let Some(exit) = self.unwind_one(heap) {
                    return exit;
                }
                active_module = self.frame.module;
                module = self.modules[active_module].module.clone();
                function = &module.functions()[self.frame.function];
                continue;
            }
            if let Some(value) = self.resume_value.take() {
                self.resume_steps += 1;
                if let Err(error) = self.resume(heap, value) {
                    self.frame.pc -= 1;
                    runtime_error!(error.to_string());
                }
                if let Some(exit) = self.suspension() {
                    return exit;
                }
                if active_module != self.frame.module {
                    active_module = self.frame.module;
                    module = self.modules[active_module].module.clone();
                }
                function = &module.functions()[self.frame.function];
                continue;
            }
            // Construction validates PC, registers, calls and initialization once.
            let instruction = function.instructions()[self.frame.pc];
            self.executed += 1;
            let registers = &self.registers[self.frame.base..];
            let operation = match instruction {
                Instruction::Eval { dst, src } => {
                    let source = registers[src.0 as usize];
                    let destination = dst.map_or(ReturnTo::Discard, |dst| {
                        ReturnTo::Register(self.frame.base + dst.0 as usize)
                    });
                    match self.evaluate(heap, source, destination) {
                        Ok(Some(request)) => return VmExit::CompileRequest(request),
                        Ok(None) => continue,
                        Err(error) => runtime_error!(error.to_string()),
                    }
                }
                Instruction::LoadReal { dst, bits } => Ok((dst, Value::Real(f64::from_bits(bits)))),
                Instruction::ToNumber { dst, src } => {
                    value::to_number(heap, registers[src.0 as usize]).map(|value| (dst, value))
                }
                Instruction::ToInteger { dst, src } => {
                    value::to_integer(heap, registers[src.0 as usize])
                        .map(|value| (dst, Value::Int(value)))
                }
                Instruction::ToReal { dst, src } => value::to_real(heap, registers[src.0 as usize])
                    .map(|value| (dst, Value::Real(value))),
                Instruction::IsValid { dst, src } => {
                    let value = registers[src.0 as usize];
                    match value {
                        Value::Obj(reference) => match reference.object {
                            Some(object) => heap
                                .is_valid(object)
                                .map(|valid| (dst, Value::Int(i64::from(valid))))
                                .map_err(Into::into),
                            None => runtime_error!("isvalid on null"),
                        },
                        _ => Ok((dst, Value::Int(1))),
                    }
                }
                Instruction::Invalidate { dst, src } => {
                    let value = registers[src.0 as usize];
                    let destination = ReturnTo::Register(self.frame.base + dst.0 as usize);
                    if let Err(error) = self.invalidate(heap, value, destination) {
                        runtime_error!(error.to_string());
                    }
                    if let Some(exit) = self.suspension() {
                        return exit;
                    }
                    if active_module != self.frame.module {
                        active_module = self.frame.module;
                        module = self.modules[active_module].module.clone();
                    }
                    function = &module.functions()[self.frame.function];
                    continue;
                }
                Instruction::TypeOf { dst, src } => {
                    let value = registers[src.0 as usize];
                    Ok((dst, self.type_name(heap, Some(value))))
                }
                Instruction::InstanceOf { dst, lhs, rhs } => {
                    value::instance_of(heap, registers[lhs.0 as usize], registers[rhs.0 as usize])
                        .map(|result| (dst, Value::Int(i64::from(result))))
                }
                Instruction::CharacterCode { dst, src }
                | Instruction::CharacterFrom { dst, src } => value::character(
                    heap,
                    registers[src.0 as usize],
                    matches!(instruction, Instruction::CharacterFrom { .. }),
                )
                .map(|value| (dst, value)),
                Instruction::ToString { dst, src } => {
                    value::to_string(heap, registers[src.0 as usize]).map(|value| (dst, value))
                }
                Instruction::ToOctet { dst, src } => match registers[src.0 as usize] {
                    value @ Value::Octet(_) => Ok((dst, value)),
                    Value::Void => Ok((dst, Value::Octet(heap.alloc_octet(Vec::new())))),
                    _ => Err(value::ArithmeticError::UnsupportedOperands),
                },
                Instruction::LoadScope { dst } => {
                    let global = self.ensure_global(heap);
                    let this = self.this(heap);
                    let scope = if this == global {
                        ObjRef::bound(global)
                    } else {
                        ObjRef {
                            object: Some(heap.alloc_scope(this, global)),
                            this: None,
                        }
                    };
                    self.registers[self.frame.base + dst.0 as usize] = Value::Obj(scope);
                    self.frame.pc += 1;
                    continue;
                }
                Instruction::BitNot { dst, src } => {
                    value::to_integer(heap, registers[src.0 as usize])
                        .map(|value| (dst, Value::Int(!value)))
                }
                Instruction::Divide { dst, lhs, rhs } => {
                    value::divide(heap, registers[lhs.0 as usize], registers[rhs.0 as usize])
                        .map(|value| (dst, value))
                }
                Instruction::IntDivide { dst, lhs, rhs } => {
                    value::int_divide(heap, registers[lhs.0 as usize], registers[rhs.0 as usize])
                        .map(|value| (dst, value))
                }
                Instruction::Remainder { dst, lhs, rhs } => {
                    value::remainder(heap, registers[lhs.0 as usize], registers[rhs.0 as usize])
                        .map(|value| (dst, value))
                }
                Instruction::LogicalAnd { dst, lhs, rhs }
                | Instruction::LogicalOr { dst, lhs, rhs } => value::logical(
                    heap,
                    registers[lhs.0 as usize],
                    registers[rhs.0 as usize],
                    matches!(instruction, Instruction::LogicalAnd { .. }),
                )
                .map(|value| (dst, value)),
                Instruction::BitAnd { dst, lhs, rhs }
                | Instruction::BitOr { dst, lhs, rhs }
                | Instruction::BitXor { dst, lhs, rhs } => {
                    value::to_integer(heap, registers[lhs.0 as usize]).and_then(|lhs| {
                        let rhs = value::to_integer(heap, registers[rhs.0 as usize])?;
                        let result = match instruction {
                            Instruction::BitAnd { .. } => lhs & rhs,
                            Instruction::BitOr { .. } => lhs | rhs,
                            _ => lhs ^ rhs,
                        };
                        Ok((dst, Value::Int(result)))
                    })
                }
                Instruction::ShiftLeft { dst, lhs, rhs }
                | Instruction::ShiftRight { dst, lhs, rhs }
                | Instruction::ShiftRightUnsigned { dst, lhs, rhs } => {
                    value::to_integer(heap, registers[lhs.0 as usize]).and_then(|lhs| {
                        let rhs = value::shift_count(heap, registers[rhs.0 as usize])?;
                        let result = match instruction {
                            Instruction::ShiftLeft { .. } => lhs.wrapping_shl(rhs),
                            Instruction::ShiftRight { .. } => lhs >> rhs,
                            _ => ((lhs as u64) >> rhs) as i64,
                        };
                        Ok((dst, Value::Int(result)))
                    })
                }
                Instruction::StrictEqual { dst, lhs, rhs }
                | Instruction::StrictNotEqual { dst, lhs, rhs } => {
                    value::strict_equal(heap, registers[lhs.0 as usize], registers[rhs.0 as usize])
                        .map(|equal| {
                            (
                                dst,
                                Value::Int(i64::from(
                                    equal
                                        ^ matches!(instruction, Instruction::StrictNotEqual { .. }),
                                )),
                            )
                        })
                }
                Instruction::LoadInt { dst, value } => Ok((dst, Value::Int(value))),
                Instruction::LoadVoid { dst } => Ok((dst, Value::Void)),
                Instruction::LoadNull { dst } => Ok((dst, Value::Obj(ObjRef::default()))),
                Instruction::LoadFunction { dst, function } => {
                    Ok((dst, self.load_function(heap, function)))
                }
                Instruction::LoadThis { dst } => {
                    Ok((dst, Value::Obj(ObjRef::bound(self.this(heap)))))
                }
                Instruction::LoadGlobal { dst } => {
                    Ok((dst, Value::Obj(ObjRef::bound(self.ensure_global(heap)))))
                }
                Instruction::GetRawName { .. }
                | Instruction::SetRawName { .. }
                | Instruction::GetRawMember { .. }
                | Instruction::SetRawMember { .. }
                | Instruction::TypeOfMember { .. }
                | Instruction::GetProperty { .. }
                | Instruction::SetProperty { .. }
                | Instruction::GetName { .. }
                | Instruction::SetName { .. }
                | Instruction::GetMember { .. }
                | Instruction::SetMember { .. }
                | Instruction::ContainsMember { .. }
                | Instruction::DeleteMember { .. }
                | Instruction::DeleteName { .. }
                | Instruction::UpdateMember { .. }
                | Instruction::UpdateName { .. }
                | Instruction::UpdateProperty { .. }
                | Instruction::StoreMember { .. }
                | Instruction::StoreName { .. } => {
                    if let Err(error) = self.access_instruction(heap, instruction) {
                        runtime_error!(error.to_string());
                    }
                    if let Some(exit) = self.suspension() {
                        return exit;
                    }
                    if active_module != self.frame.module {
                        active_module = self.frame.module;
                        module = self.modules[active_module].module.clone();
                    }
                    function = &module.functions()[self.frame.function];
                    continue;
                }
                Instruction::DefineThis { key, value } => {
                    let key = registers[key.0 as usize];
                    let value = registers[value.0 as usize];
                    let context = self.this(heap);
                    if let Err(error) =
                        member::define(heap, Value::Obj(ObjRef::bound(context)), key, value)
                    {
                        runtime_error!(error.to_string());
                    }
                    self.frame.pc += 1;
                    continue;
                }
                Instruction::DefineMember { object, key, value } => {
                    let result = member::define(
                        heap,
                        registers[object.0 as usize],
                        registers[key.0 as usize],
                        registers[value.0 as usize],
                    );
                    if let Err(error) = result {
                        runtime_error!(error.to_string());
                    }
                    self.frame.pc += 1;
                    continue;
                }
                Instruction::AddClassInfo => {
                    if let Err(error) = self.add_class_info(heap) {
                        runtime_error!(error.to_string());
                    }
                    self.frame.pc += 1;
                    continue;
                }
                Instruction::ClassInfo { object, name } => {
                    let object = registers[object.0 as usize];
                    let name = registers[name.0 as usize];
                    if let Err(error) = self.add_class_info_value(heap, object, name) {
                        runtime_error!(error.to_string());
                    }
                    self.frame.pc += 1;
                    continue;
                }
                Instruction::RegisterMembers => {
                    if let Err(error) = self.register_members(heap) {
                        runtime_error!(error.to_string());
                    }
                    self.frame.pc += 1;
                    continue;
                }
                Instruction::BindContext {
                    dst,
                    object,
                    context,
                } => {
                    let (Value::Obj(mut reference), Value::Obj(context)) =
                        (registers[object.0 as usize], registers[context.0 as usize])
                    else {
                        runtime_error!("incontextof requires object values");
                    };
                    reference.this = context.object;
                    Ok((dst, Value::Obj(reference)))
                }
                Instruction::NewDictionary { dst } => {
                    Ok((dst, Value::Obj(ObjRef::bound(heap.alloc_dictionary()))))
                }
                Instruction::NewArray { dst } => {
                    Ok((dst, Value::Obj(ObjRef::bound(heap.alloc_array()))))
                }
                Instruction::ArrayPush { array, value } => {
                    let Value::Obj(reference) = registers[array.0 as usize] else {
                        runtime_error!("array construction requires an array");
                    };
                    let result = reference
                        .object
                        .ok_or(HeapError::NotArray)
                        .and_then(|id| heap.array_push(id, registers[value.0 as usize]));
                    if let Err(error) = result {
                        runtime_error!(error.to_string());
                    }
                    self.frame.pc += 1;
                    continue;
                }
                Instruction::CollectArguments { dst, start } => {
                    let values = self.original_arguments();
                    match heap.alloc_array_from(&values[(start as usize).min(values.len())..]) {
                        Ok(array) => Ok((dst, Value::Obj(ObjRef::bound(array)))),
                        Err(error) => runtime_error!(error.to_string()),
                    }
                }
                Instruction::LoadConstant { dst, constant } => {
                    Ok((dst, self.load_constant(heap, self.frame.module, constant)))
                }
                Instruction::Move { dst, src } => Ok((dst, registers[src.0 as usize])),
                Instruction::Add { dst, lhs, rhs } => {
                    value::add_in(heap, registers[lhs.0 as usize], registers[rhs.0 as usize])
                        .map(|value| (dst, value))
                }
                Instruction::Subtract { dst, lhs, rhs } => {
                    value::subtract_in(heap, registers[lhs.0 as usize], registers[rhs.0 as usize])
                        .map(|value| (dst, value))
                }
                Instruction::Multiply { dst, lhs, rhs } => {
                    value::multiply_in(heap, registers[lhs.0 as usize], registers[rhs.0 as usize])
                        .map(|value| (dst, value))
                }
                Instruction::Negate { dst, src } => {
                    value::negate_in(heap, registers[src.0 as usize]).map(|value| (dst, value))
                }
                Instruction::Not { dst, src } => registers[src.0 as usize]
                    .truthy(heap)
                    .map(|truth| (dst, Value::Int(i64::from(!truth)))),
                Instruction::IsVoid { dst, src } => Ok((
                    dst,
                    Value::Int(i64::from(matches!(registers[src.0 as usize], Value::Void))),
                )),
                Instruction::Equal { dst, lhs, rhs } | Instruction::NotEqual { dst, lhs, rhs } => {
                    value::equal(heap, registers[lhs.0 as usize], registers[rhs.0 as usize]).map(
                        |equal| {
                            (
                                dst,
                                Value::Int(i64::from(
                                    equal ^ matches!(instruction, Instruction::NotEqual { .. }),
                                )),
                            )
                        },
                    )
                }
                Instruction::Less { dst, lhs, rhs }
                | Instruction::LessEqual { dst, lhs, rhs }
                | Instruction::Greater { dst, lhs, rhs }
                | Instruction::GreaterEqual { dst, lhs, rhs } => {
                    value::compare_in(heap, registers[lhs.0 as usize], registers[rhs.0 as usize])
                        .map(|ordering| {
                            let result = match instruction {
                                Instruction::Less { .. } => ordering.is_some_and(|o| o.is_lt()),
                                Instruction::LessEqual { .. } => {
                                    !ordering.is_some_and(|o| o.is_gt())
                                }
                                Instruction::Greater { .. } => ordering.is_some_and(|o| o.is_gt()),
                                Instruction::GreaterEqual { .. } => {
                                    !ordering.is_some_and(|o| o.is_lt())
                                }
                                _ => unreachable!("comparison instruction"),
                            };
                            (dst, Value::Int(i64::from(result)))
                        })
                }
                Instruction::Jump { target } => {
                    self.frame.pc = target as usize;
                    continue;
                }
                Instruction::JumpIfFalse { condition, target } => {
                    let truth = match registers[condition.0 as usize].truthy(heap) {
                        Ok(truth) => truth,
                        Err(error) => runtime_error!(error.to_string()),
                    };
                    self.frame.pc = if truth {
                        self.frame.pc + 1
                    } else {
                        target as usize
                    };
                    continue;
                }
                Instruction::Call { site } => {
                    if let Err(error) = self.call(heap, &function.calls()[site as usize]) {
                        runtime_error!(error.to_string());
                    }
                    if let Some(exit) = self.suspension() {
                        return exit;
                    }
                    if active_module != self.frame.module {
                        active_module = self.frame.module;
                        module = self.modules[active_module].module.clone();
                    }
                    function = &module.functions()[self.frame.function];
                    continue;
                }
                Instruction::Throw { src } => {
                    self.pending_exception = Some(PendingException {
                        value: registers[src.0 as usize],
                        runtime: false,
                        depth: self.callers.len(),
                    });
                    continue;
                }
                Instruction::Return { src } => {
                    let result = registers[src.0 as usize];
                    if let Some(caller) = self.callers.pop() {
                        let destination = self.frame.return_destination;
                        self.registers.truncate(self.frame.argument_start);
                        let previous_module = self.frame.module;
                        self.frame = caller;
                        if previous_module != caller.module {
                            self.release_module(previous_module);
                            // Removal may move another module into the same index.
                            active_module = self.frame.module;
                            module = self.modules[active_module].module.clone();
                        }
                        self.deliver(destination, result);
                        if active_module != self.frame.module {
                            active_module = self.frame.module;
                            module = self.modules[active_module].module.clone();
                        }
                        function = &module.functions()[self.frame.function];
                        continue;
                    }
                    self.frame.pc += 1;
                    // Only the returned value remains live after the root frame.
                    // Retaining scratch registers delays automatic finalizers.
                    self.registers.fill(Value::Void);
                    self.state = State::Finished(result);
                    return VmExit::Finished(result);
                }
            };
            match operation {
                Ok((dst, value)) => self.registers[self.frame.base + dst.0 as usize] = value,
                Err(error) => runtime_error!(error.to_string()),
            }
            self.frame.pc += 1;
        }
        VmExit::Yielded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallArguments, CallTarget, Register};

    #[test]
    fn repeated_calls_reuse_frame_and_value_capacity() {
        use crate::{CallSite, Function, FunctionId};
        let entry = Function::new(
            "entry",
            0,
            2,
            vec![
                Instruction::LoadInt {
                    dst: Register(1),
                    value: 7,
                },
                Instruction::Call { site: 0 },
                Instruction::Return { src: Register(1) },
            ],
            vec![None; 3],
            vec![CallSite {
                target: CallTarget::Direct(FunctionId(1)),
                dst: Some(Register(1)),
                arguments: CallArguments::Registers {
                    start: Register(1),
                    count: 1,
                },
            }],
        )
        .unwrap();
        let callee = Function::new(
            "identity",
            1,
            2,
            vec![Instruction::Return { src: Register(1) }],
            vec![None],
            vec![],
        )
        .unwrap();
        let module = Module::from_functions(vec![entry, callee]).unwrap();
        let mut heap = Heap::new();
        let mut vm = Vm::new(&module);
        let budget = RunBudget::new(100).unwrap();
        assert!(matches!(
            vm.run_slice(&mut heap, budget),
            VmExit::Finished(Value::Int(7))
        ));
        let capacity = (vm.registers.capacity(), vm.callers.capacity());
        for _ in 0..100 {
            vm.reset();
            assert!(matches!(
                vm.run_slice(&mut heap, budget),
                VmExit::Finished(Value::Int(7))
            ));
            assert_eq!((vm.registers.capacity(), vm.callers.capacity()), capacity);
        }
    }

    #[test]
    fn slices_preserve_pc_and_aliasing_operands() {
        let code = vec![
            Instruction::LoadInt {
                dst: Register(0),
                value: 3,
            },
            Instruction::Multiply {
                dst: Register(0),
                lhs: Register(0),
                rhs: Register(0),
            },
            Instruction::Return { src: Register(0) },
        ];
        let module = Module::new(1, code, vec![None; 3]).unwrap();
        let mut heap = Heap::new();
        let mut vm = Vm::new(&module);
        let budget = RunBudget::new(1).unwrap();
        assert!(matches!(vm.run_slice(&mut heap, budget), VmExit::Yielded));
        assert_eq!(vm.pc(), 1);
        assert!(matches!(vm.run_slice(&mut heap, budget), VmExit::Yielded));
        assert_eq!(vm.registers()[0].as_integer(), Some(9));
        assert!(matches!(
            vm.run_slice(&mut heap, budget),
            VmExit::Finished(Value::Int(9))
        ));
        assert!(matches!(
            vm.run_slice(&mut heap, budget),
            VmExit::Finished(Value::Int(9))
        ));
        assert_eq!(vm.instructions_executed(), 3);
        vm.reset();
        assert_eq!(vm.instructions_executed(), 0);
        assert!(matches!(
            vm.run_slice(&mut heap, RunBudget::new(100).unwrap()),
            VmExit::Finished(Value::Int(9))
        ));
        assert!(RunBudget::new(0).is_none());
    }
}
