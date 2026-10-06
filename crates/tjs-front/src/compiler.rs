mod calls;
mod constants;
mod effects;
mod expressions;
mod functions;
mod places;
mod regexp;
mod scopes;
mod statements;

use std::{collections::HashMap, sync::Arc};

use tjs_core::{
    CallSite, CatchHandler, Constant, Diagnostic, Function, Instruction, Phase, Register,
    SourceMap, Span,
    ir::{MAX_INSTRUCTIONS, MAX_REGISTERS},
};

use crate::ast::{ExprId, Parameter, Program, Variable};
use scopes::{Binding, Scope};

struct Constants {
    values: Vec<Constant>,
    names: HashMap<Arc<[u16]>, u32>,
    containers: HashMap<usize, u32>,
}

impl Constants {
    fn name(&mut self, units: &[u16]) -> u32 {
        if let Some(&index) = self.names.get(units) {
            return index;
        }
        let index = self.values.len() as u32;
        let units: Arc<[u16]> = units.into();
        self.values.push(Constant::String(units.clone()));
        self.names.insert(units, index);
        index
    }
}

pub use functions::compile;
pub(crate) use functions::compile_discard;

struct Compiler<'source> {
    sources: &'source SourceMap,
    program: &'source Program,
    constants: &'source mut Constants,
    context: Context,
    super_expression: Option<ExprId>,
    global_context: bool,
    capture_completion: bool,
    scopes: Vec<Scope>,
    controls: Vec<Control>,
    with_objects: Vec<Register>,
    next_local: u32,
    next_temp: u32,
    high_water: u32,
    code: Vec<Instruction>,
    spans: Vec<Option<Span>>,
    calls: Vec<CallSite>,
    handlers: Vec<CatchHandler>,
    unnamed_rest_start: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Script,
    NamedFunction,
    FunctionExpression,
    Class,
    Accessor,
}

#[derive(Default)]
struct Control {
    // None denotes a switch: continue searches past it for an enclosing loop.
    continues: Option<Vec<usize>>,
    breaks: Vec<usize>,
}

impl Control {
    fn loop_scope() -> Self {
        Self {
            continues: Some(Vec::new()),
            ..Self::default()
        }
    }
}

impl<'source> Compiler<'source> {
    fn new(
        sources: &'source SourceMap,
        program: &'source Program,
        constants: &'source mut Constants,
        parameters: &[Parameter],
        context: Context,
    ) -> Result<Self, Diagnostic> {
        if parameters.len() >= MAX_REGISTERS as usize {
            return Err(Diagnostic::new(
                Phase::Compile,
                program.span(),
                "parameter register limit exceeded",
            ));
        }
        let next = parameters.len() as u32 + 1;
        Ok(Self {
            sources,
            program,
            constants,
            context,
            super_expression: None,
            global_context: false,
            capture_completion: true,
            scopes: vec![Scope::default()],
            controls: Vec::new(),
            with_objects: Vec::new(),
            next_local: next,
            next_temp: next,
            high_water: next,
            code: Vec::new(),
            spans: Vec::new(),
            calls: Vec::new(),
            handlers: Vec::new(),
            unnamed_rest_start: 0,
        })
    }

    fn parameters(&mut self, parameters: &[Parameter]) -> Result<(), Diagnostic> {
        for (index, parameter) in parameters.iter().enumerate() {
            let name = self.name(parameter.name)?.to_vec();
            let register = Register(index as u32 + 1);
            let binding = Register(self.scopes[0].bindings.len() as u32 + 1);
            // Namespace.Add keeps the first binding of a repeated name; defaults
            // still address the physical argument slot by its original position.
            self.scopes[0]
                .bindings
                .entry(name)
                .or_insert_with(|| Binding::active(binding, parameter.name));
            if let Some(default) = parameter.default {
                // The reference registers each parameter before its initializer.
                // Later parameters are not in scope until their turn is reached.
                let span = self.program.expression(default).span;
                let condition = self.temporary(span)?;
                self.emit(
                    Instruction::IsVoid {
                        dst: condition,
                        src: register,
                    },
                    parameter.name,
                )?;
                let skip = self.code.len();
                self.emit(
                    Instruction::JumpIfFalse {
                        condition,
                        target: 0,
                    },
                    span,
                )?;
                self.next_temp = self.next_local;
                self.expression(default, register)?;
                self.patch_here(skip);
                self.next_temp = self.next_local;
            }
        }
        Ok(())
    }

    fn finish(self, name: &str, parameters: u32) -> Result<Function, Diagnostic> {
        Function::with_handlers(
            name,
            parameters,
            self.high_water,
            self.code,
            self.spans,
            self.calls,
            self.handlers,
        )
    }

    fn name(&self, span: Span) -> Result<&[u16], Diagnostic> {
        self.sources.slice(span).ok_or_else(|| {
            Diagnostic::new(Phase::Compile, span, "source handle is no longer valid")
        })
    }

    fn lookup(&self, span: Span) -> Result<Option<Register>, Diagnostic> {
        if self.global_context {
            return Ok(None);
        }
        let name = self.name(span)?;
        Ok(self.scopes.iter().rev().find_map(|scope| {
            scope
                .bindings
                .get(name)
                .filter(|binding| binding.active)
                .map(|binding| binding.register)
        }))
    }

    fn declare(&mut self, variable: Variable, span: Span) -> Result<(), Diagnostic> {
        let Variable { name, initializer } = variable;
        // Evaluate before installing the binding; redeclarations read the old value.
        let value = if let Some(initializer) = initializer {
            self.operand(initializer)?
        } else {
            let value = self.temporary(name)?;
            self.emit(Instruction::LoadVoid { dst: value }, name)?;
            value
        };
        self.bind(name, value, span)
    }

    fn bind(&mut self, name: Span, value: Register, span: Span) -> Result<(), Diagnostic> {
        if self.context == Context::Script && self.scopes.len() == 1 {
            let key = self.temporary(name)?;
            self.load_name(name, key)?;
            self.emit(Instruction::DefineThis { key, value }, span)?;
        } else if self.context == Context::Class && self.scopes.len() == 1 {
            let key = self.temporary(name)?;
            let object = self.temporary(name)?;
            self.load_name(name, key)?;
            self.emit(Instruction::LoadThis { dst: object }, span)?;
            self.emit(Instruction::DefineMember { object, key, value }, span)?;
        } else {
            let units = self.name(name)?.to_vec();
            let binding = self
                .scopes
                .last_mut()
                .expect("scope")
                .bindings
                .get_mut(&units)
                .expect("reserved declaration");
            binding.active = true;
            let local = binding.register;
            if local != value {
                self.emit(
                    Instruction::Move {
                        dst: local,
                        src: value,
                    },
                    span,
                )?;
            }
        }
        self.next_temp = self.next_local;
        Ok(())
    }

    fn condition_jump(&mut self, condition: ExprId) -> Result<usize, Diagnostic> {
        let mark = self.next_temp;
        let span = self.program.expression(condition).span;
        let register = self.temporary(span)?;
        self.condition_value(condition, register)?;
        let jump = self.code.len();
        self.emit(
            Instruction::JumpIfFalse {
                condition: register,
                target: 0,
            },
            span,
        )?;
        self.next_temp = mark;
        Ok(jump)
    }

    fn jump(&mut self, span: Span) -> Result<usize, Diagnostic> {
        let pc = self.code.len();
        self.emit(Instruction::Jump { target: 0 }, span)?;
        Ok(pc)
    }

    fn patch_here(&mut self, pc: usize) {
        let destination = self.code.len() as u32;
        self.patch_to(pc, destination);
    }

    fn patch_to(&mut self, pc: usize, destination: u32) {
        match &mut self.code[pc] {
            Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } => {
                *target = destination
            }
            _ => unreachable!("only jump sites are patched"),
        }
    }

    fn temporary(&mut self, span: Span) -> Result<Register, Diagnostic> {
        if self.next_temp >= MAX_REGISTERS {
            return Err(Diagnostic::new(
                Phase::Compile,
                span,
                "register limit exceeded",
            ));
        }
        let result = Register(self.next_temp);
        self.next_temp += 1;
        self.high_water = self.high_water.max(self.next_temp);
        Ok(result)
    }

    fn emit(&mut self, instruction: Instruction, span: Span) -> Result<(), Diagnostic> {
        if self.code.len() >= MAX_INSTRUCTIONS {
            return Err(Diagnostic::new(
                Phase::Compile,
                span,
                "instruction limit exceeded",
            ));
        }
        self.code.push(instruction);
        self.spans.push(Some(span));
        Ok(())
    }
}
