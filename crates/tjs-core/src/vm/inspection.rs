use super::{State, Vm};
use crate::{Diagnostic, TraceFrame, Value};

impl Vm {
    pub fn inspection_trace(&self) -> Vec<TraceFrame> {
        let mut frames = self.diagnostic("").trace;
        if let Some(frame) = frames.first_mut() {
            let function = &self.modules[self.frame.module].module.functions()[self.frame.function];
            let pc = self.frame.pc.saturating_sub(1);
            (frame.function, frame.pc) = function.trace_location(pc);
            frame.span = function.span_at(pc);
        }
        frames
    }

    pub fn dump_code(&self) -> String {
        self.modules
            .iter()
            .map(|loaded| loaded.module.disassemble())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn resume_inspection(&mut self, value: Value) -> Result<(), Diagnostic> {
        if !matches!(self.state, State::Inspecting(_)) {
            return Err(self.diagnostic("VM has no outstanding inspection request"));
        }
        self.state = State::Runnable;
        self.resume_value = Some(value);
        Ok(())
    }
}
