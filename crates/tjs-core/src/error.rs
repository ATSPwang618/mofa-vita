use crate::source::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Lex,
    Parse,
    Compile,
    Verify,
    Runtime,
}

#[derive(Clone, Debug)]
pub struct TraceFrame {
    pub function: String,
    pub pc: usize,
    pub span: Option<Span>,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{phase:?}: {message}")]
pub struct Diagnostic {
    pub phase: Phase,
    pub span: Option<Span>,
    pub message: String,
    /// Innermost frame first. Names are owned so diagnostics outlive the module.
    pub trace: Vec<TraceFrame>,
}

impl Diagnostic {
    pub fn new(phase: Phase, span: impl Into<Option<Span>>, message: impl Into<String>) -> Self {
        Self {
            phase,
            span: span.into(),
            message: message.into(),
            trace: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScriptException {
    pub value: crate::Value,
    pub diagnostic: Diagnostic,
}
