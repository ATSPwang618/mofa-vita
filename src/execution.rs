use tjs_core::{Diagnostic, RunBudget, Value, Vm};

use crate::Execution;
use tjs_runtime::{Runtime, RuntimeExit as VmExit};

/// One budget shared by a submission and every finalizer it triggers.
#[derive(Default)]
pub(crate) struct Usage {
    pub instructions: u64,
    pub work: u64,
}

impl Usage {
    fn step(
        &mut self,
        vm: &mut Vm,
        runtime: &mut Runtime,
        options: &Execution,
    ) -> Result<VmExit, Diagnostic> {
        let remaining = options.remaining(self.work);
        if remaining == 0 {
            return Err(vm.diagnostic(
                "CLI instruction limit reached; adjust --max-instructions to allow more execution",
            ));
        }
        let budget = RunBudget::new(remaining).expect("positive remainder");
        let instructions = vm.instructions_executed();
        let work = vm.work_executed();
        let exit = runtime.run_slice(vm, budget);
        self.instructions += vm.instructions_executed() - instructions;
        self.work += vm.work_executed() - work;
        match exit {
            VmExit::Fault(error) => Err(error),
            VmExit::Waiting(_) => {
                Err(vm.diagnostic("external native wait requires a host scheduler"))
            }
            VmExit::Thrown(exception) => Err(exception.diagnostic),
            other => Ok(other),
        }
    }
}

pub(crate) fn execute(
    vm: &mut Vm,
    runtime: &mut Runtime,
    options: &Execution,
    usage: &mut Usage,
) -> Result<Value, Diagnostic> {
    loop {
        drain_finalizers(runtime, options, usage, Some(vm), &[])?;
        match usage.step(vm, runtime, options)? {
            VmExit::Finished(value) => {
                runtime.collect(vm.roots());
                drain_finalizers(runtime, options, usage, Some(vm), &[])?;
                return Ok(value);
            }
            VmExit::Yielded => {
                // Advance only at a boundary with all current VM roots.
                if runtime.heap.is_collecting() || runtime.allocation_debt() >= 256 * 1024 {
                    runtime.collect_auto(vm.roots());
                }
            }
            VmExit::Fault(_) | VmExit::Thrown(_) | VmExit::Waiting(_) => {
                unreachable!("step returns errors")
            }
        }
    }
}

pub(crate) fn drain_finalizers(
    runtime: &mut Runtime,
    options: &Execution,
    usage: &mut Usage,
    kept_vm: Option<&Vm>,
    kept_values: &[Value],
) -> Result<(), Diagnostic> {
    while runtime.heap.pending_finalizers() != 0 {
        while let Some(mut finalizer) = Vm::take_finalizer(&mut runtime.heap) {
            loop {
                match usage.step(&mut finalizer, runtime, options)? {
                    VmExit::Finished(_) => break,
                    VmExit::Yielded => {
                        if runtime.heap.is_collecting() || runtime.allocation_debt() >= 256 * 1024 {
                            runtime.collect_auto(
                                kept_vm
                                    .into_iter()
                                    .flat_map(Vm::roots)
                                    .chain(kept_values.iter().copied())
                                    .chain(finalizer.roots()),
                            );
                        }
                    }
                    VmExit::Fault(_) | VmExit::Thrown(_) | VmExit::Waiting(_) => {
                        unreachable!("step returns errors")
                    }
                }
            }
        }
        // Re-mark resurrection and discover garbage created by callbacks once
        // per batch, rather than scanning the entire heap after each object.
        runtime.collect(
            kept_vm
                .into_iter()
                .flat_map(Vm::roots)
                .chain(kept_values.iter().copied()),
        );
    }
    Ok(())
}
