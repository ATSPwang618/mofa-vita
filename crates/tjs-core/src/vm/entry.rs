//! Host callbacks enter the same continuation/exception/budget path as script
//! calls. The reusable entry module contains no heap handles.
use super::{
    Vm,
    calls::{Invocation, ReturnTo},
    dispatch::Action,
    native::PendingNative,
};
use crate::{
    Instruction, Module, NativeContinuation, NativeCx, NativeResult, NativeStep, ObjId, Register,
    Trace, Value,
};

pub enum Callback {
    Function(Value),
    Member { object: Value, key: Value },
}
struct Entry {
    callback: Callback,
    arguments: Vec<Value>,
}
impl Trace for Entry {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        match self.callback {
            Callback::Function(function) => visit(function),
            Callback::Member { object, key } => {
                visit(object);
                visit(key);
            }
        }
        self.arguments.trace(visit);
    }
}
struct Returned;
impl Trace for Returned {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Returned {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(value))
    }
}
impl NativeContinuation for Entry {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Entry {
            callback,
            arguments,
        } = *self;
        Ok(match callback {
            Callback::Function(function) => NativeStep::Call {
                function,
                arguments,
                continuation: Box::new(Returned),
            },
            Callback::Member { object, key } => NativeStep::CallMember {
                object,
                key,
                arguments,
                continuation: Box::new(Returned),
            },
        })
    }
}
impl Vm {
    /// Replace a finished task, reusing only its Rust execution buffers.
    pub fn restart_callback(&mut self, global: ObjId, callback: Callback, arguments: Vec<Value>) {
        self.restart_task(
            global,
            Box::new(Entry {
                callback,
                arguments,
            }),
        );
    }

    /// Replace owned native work with default task limits and no previous roots.
    pub fn restart_task(&mut self, global: ObjId, task: Box<dyn NativeContinuation>) {
        self.clear_execution();
        self.limits = super::VmLimits::default();
        self.registers.clear();
        self.registers.push(Value::Void);
        self.modules.clear();
        self.modules
            .push(super::LoadedModule::new(entry_module(), Some(global)));
        self.external_global = Some(global);
        self.type_names.fill(Value::Void);
        self.finalize_name = Value::Void;
        self.start_task(global, task);
    }

    /// Keep at most 64 KiB of completed callback buffers, with no heap roots.
    pub fn into_idle_task(mut self) -> Option<Self> {
        let bytes = self.registers.capacity() * size_of::<Value>()
            + self.modules.capacity() * size_of::<super::LoadedModule>()
            + self.callers.capacity() * size_of::<super::Frame>()
            + self.continuations.capacity() * size_of::<super::dispatch::Continuation>();
        if bytes > 64 * 1024 {
            return None;
        }
        self.clear_execution();
        self.registers.clear();
        self.modules.clear();
        self.modules
            .push(super::LoadedModule::new(entry_module(), None));
        self.registers.push(Value::Void);
        self.external_global = None;
        self.type_names.fill(Value::Void);
        self.finalize_name = Value::Void;
        self.state = super::State::Finished(Value::Void);
        Some(self)
    }

    pub fn callback(global: ObjId, callback: Callback, arguments: Vec<Value>) -> Self {
        Self::task(
            global,
            Box::new(Entry {
                callback,
                arguments,
            }),
        )
    }

    /// Start owned native work under normal VM budgets, waits and roots.
    /// The first resume receives void and runs only when the VM is driven.
    pub fn task(global: ObjId, task: Box<dyn NativeContinuation>) -> Self {
        let mut vm = Self::with_global(entry_module(), global);
        vm.start_task(global, task);
        vm
    }

    fn start_task(&mut self, global: ObjId, task: Box<dyn NativeContinuation>) {
        self.frame.pc = 1;
        self.push_action(Action::Native(Box::new(PendingNative {
            context: global,
            invocation: Invocation {
                start: 1,
                end: 1,
                this: Some(global),
                destination: ReturnTo::Register(0),
            },
            continuation: task,
        })));
        self.resume_value = Some(Value::Void);
    }
}

fn entry_module() -> &'static Module {
    static MODULE: std::sync::OnceLock<Module> = std::sync::OnceLock::new();
    MODULE.get_or_init(|| {
        Module::new(
            1,
            vec![
                Instruction::LoadVoid { dst: Register(0) },
                Instruction::Return { src: Register(0) },
            ],
            vec![None; 2],
        )
        .expect("host callback entry")
    })
}
