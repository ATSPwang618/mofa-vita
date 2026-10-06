use super::Compiler;
use tjs_core::{CallArguments, CallSite, CallTarget, Diagnostic, Instruction, Register, Span};

impl Compiler<'_> {
    pub(super) fn regexp(
        &mut self,
        pattern: u32,
        dst: Register,
        span: Span,
    ) -> Result<(), Diagnostic> {
        // Match T_REGEXP: global.RegExp(), then instance._compile('//flags/pattern').
        // Use ordinary calls so script overrides, exceptions and slicing still work.
        let object = self.temporary(span)?;
        let class = self.temporary(span)?;
        let key = self.temporary(span)?;
        let encoded = self.temporary(span)?;
        let name = self
            .constants
            .name(&"RegExp".encode_utf16().collect::<Vec<_>>());
        self.emit(Instruction::LoadGlobal { dst: object }, span)?;
        self.emit(
            Instruction::LoadConstant {
                dst: key,
                constant: name,
            },
            span,
        )?;
        self.emit(
            Instruction::GetMember {
                dst: class,
                object,
                key,
            },
            span,
        )?;
        self.emit(
            Instruction::LoadConstant {
                dst: encoded,
                constant: pattern,
            },
            span,
        )?;
        let site = self.calls.len() as u32;
        self.calls.push(CallSite {
            target: CallTarget::Construct(class),
            dst: Some(object),
            arguments: CallArguments::Registers {
                start: encoded,
                count: 0,
            },
        });
        self.emit(Instruction::Call { site }, span)?;
        let name = self
            .constants
            .name(&"_compile".encode_utf16().collect::<Vec<_>>());
        self.emit(
            Instruction::LoadConstant {
                dst: key,
                constant: name,
            },
            span,
        )?;
        let site = self.calls.len() as u32;
        self.calls.push(CallSite {
            target: CallTarget::Member {
                object,
                key,
                computed: false,
            },
            dst: None,
            arguments: CallArguments::Registers {
                start: encoded,
                count: 1,
            },
        });
        self.emit(Instruction::Call { site }, span)?;
        self.emit(Instruction::Move { dst, src: object }, span)
    }
}
