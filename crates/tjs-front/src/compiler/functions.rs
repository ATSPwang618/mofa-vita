use std::collections::HashMap;

use tjs_core::{
    CallArguments, CallSite, CallTarget, Constant, Diagnostic, FunctionId,
    FunctionKind as RuntimeKind, FunctionMember, Instruction, Module, Phase, Register, SourceMap,
};

use super::{Binding, Compiler, Constants, Context};
use crate::ast::{FunctionKind, Program, RestParameter, Statement};

pub fn compile(sources: &SourceMap, program: &Program) -> Result<Module, Diagnostic> {
    compile_mode(sources, program, true)
}

pub(crate) fn compile_discard(
    sources: &SourceMap,
    program: &Program,
) -> Result<Module, Diagnostic> {
    compile_mode(sources, program, false)
}

fn compile_mode(
    sources: &SourceMap,
    program: &Program,
    capture_completion: bool,
) -> Result<Module, Diagnostic> {
    let mut constants = Constants {
        values: program
            .strings
            .iter()
            .cloned()
            .map(Constant::String)
            .chain(program.octets.iter().cloned().map(Constant::Octet))
            .collect(),
        names: HashMap::new(),
        containers: HashMap::new(),
    };
    constants.containers(program)?;
    let mut members = vec![Vec::new(); program.functions().len()];
    let mut globals = Vec::new();
    for (index, function) in program.functions().iter().enumerate() {
        let Some(name) = function.name else { continue };
        let id = FunctionId(index as u32 + 1);
        if let Some(parent) = function.parent {
            // RegisterFunction only registers children of named functions.
            if matches!(
                program.function(parent).kind,
                FunctionKind::Function | FunctionKind::Class { .. }
            ) {
                let units = sources.slice(name).ok_or_else(|| {
                    Diagnostic::new(Phase::Compile, name, "source handle is no longer valid")
                })?;
                members[parent.0].push(FunctionMember {
                    name: constants.name(units),
                    function: id,
                });
            }
        } else {
            globals.push((
                name,
                id,
                !matches!(function.kind, FunctionKind::Class { .. }),
            ));
        }
    }
    let mut compiler = Compiler::new(sources, program, &mut constants, &[], Context::Script)?;
    compiler.capture_completion = capture_completion;
    compiler.emit(Instruction::LoadVoid { dst: Register(0) }, program.span())?;
    // All declarations in the script context register before its body, even in
    // blocks/branches. Repeated names are overwritten in source order.
    for (name, function, bind) in globals {
        let value = compiler.temporary(name)?;
        let context = compiler.temporary(name)?;
        let key = compiler.temporary(name)?;
        compiler.emit(
            Instruction::LoadFunction {
                dst: value,
                function,
            },
            name,
        )?;
        if bind {
            compiler.emit(Instruction::LoadThis { dst: context }, name)?;
            compiler.emit(
                Instruction::BindContext {
                    dst: value,
                    object: value,
                    context,
                },
                name,
            )?;
        }
        compiler.load_name(name, key)?;
        compiler.emit(Instruction::DefineThis { key, value }, name)?;
        compiler.next_temp = compiler.next_local;
    }
    compiler.statement_list(program.roots())?;
    compiler.emit(Instruction::Return { src: Register(0) }, program.span())?;
    let mut functions = vec![compiler.finish("<script>", 0)?];
    let mut resolvers = Vec::new();
    for (index, definition) in program.functions().iter().enumerate() {
        let context = match definition.kind {
            FunctionKind::Function => Context::NamedFunction,
            FunctionKind::Class { .. } => Context::Class,
            FunctionKind::Accessor | FunctionKind::Property { .. } => Context::Accessor,
            FunctionKind::Expression => Context::FunctionExpression,
        };
        let mut compiler = Compiler::new(
            sources,
            program,
            &mut constants,
            &definition.parameters,
            context,
        )?;
        let parent = definition.parent.map(|id| program.function(id));
        let owner = match parent {
            Some(parent) if matches!(parent.kind, FunctionKind::Property { .. }) => {
                parent.parent.map(|id| program.function(id))
            }
            other => other,
        };
        if let Some(owner) = owner {
            if let FunctionKind::Class { bases } = &owner.kind {
                if bases.len() == 1 {
                    compiler.super_expression = Some(bases[0]);
                }
            }
        }
        let name = if let Some(name) = definition.name {
            String::from_utf16_lossy(compiler.name(name)?)
        } else {
            if matches!(definition.kind, FunctionKind::Accessor) {
                "(accessor)"
            } else {
                "(anonymous)"
            }
            .to_owned()
        };
        compiler.emit(Instruction::LoadVoid { dst: Register(0) }, definition.span)?;
        let named_rest = match definition.rest {
            Some(RestParameter::Named(name)) => {
                let dst = compiler.temporary(name)?;
                compiler.next_local = compiler.next_temp;
                compiler.emit(
                    Instruction::CollectArguments {
                        dst,
                        start: definition.parameters.len() as u32,
                    },
                    name,
                )?;
                Some(name)
            }
            Some(RestParameter::Unnamed) => {
                compiler.unnamed_rest_start = definition.parameters.len() as u32;
                None
            }
            None => None,
        };
        compiler.parameters(&definition.parameters)?;
        if let Some(name) = named_rest {
            let units = compiler.name(name)?.to_vec();
            let binding = Register(compiler.scopes[0].bindings.len() as u32 + 1);
            compiler.scopes[0]
                .bindings
                .entry(units)
                .or_insert_with(|| Binding::active(binding, name));
        }
        let kind = match &definition.kind {
            FunctionKind::Class { bases } => {
                compiler.emit(Instruction::AddClassInfo, definition.span)?;
                for &base in bases {
                    let span = program.expression(base).span;
                    let value = compiler.operand(base)?;
                    let context = compiler.temporary(span)?;
                    compiler.emit(Instruction::LoadThis { dst: context }, span)?;
                    compiler.emit(
                        Instruction::BindContext {
                            dst: value,
                            object: value,
                            context,
                        },
                        span,
                    )?;
                    let site = compiler.calls.len() as u32;
                    compiler.calls.push(CallSite {
                        target: CallTarget::Value(value),
                        dst: None,
                        arguments: CallArguments::Registers {
                            start: Register(0),
                            count: 0,
                        },
                    });
                    compiler.emit(Instruction::Call { site }, span)?;
                    compiler.next_temp = compiler.next_local;
                }
                compiler.emit(Instruction::RegisterMembers, definition.span)?;
                let Statement::Block { statements, .. } = program.statement(definition.body) else {
                    unreachable!("class body")
                };
                for &statement in statements {
                    compiler.statement(statement)?;
                }
                let mut getters = Vec::new();
                for &base in bases {
                    getters.push(FunctionId(
                        (program.functions().len() + 1 + resolvers.len()) as u32,
                    ));
                    resolvers.push(base);
                }
                RuntimeKind::Class {
                    constructor: compiler.constants.name(
                        sources
                            .slice(definition.name.expect("class name"))
                            .expect("class source"),
                    ),
                    bases: getters,
                }
            }
            FunctionKind::Property { getter, setter } => RuntimeKind::Property {
                getter: getter.map(|id| FunctionId(id.0 as u32 + 1)),
                setter: setter.map(|id| FunctionId(id.0 as u32 + 1)),
            },
            _ => {
                compiler.statement(definition.body)?;
                RuntimeKind::Function
            }
        };
        compiler.emit(Instruction::LoadVoid { dst: Register(0) }, definition.span)?;
        compiler.emit(Instruction::Return { src: Register(0) }, definition.span)?;
        functions.push(
            compiler
                .finish(&name, definition.parameters.len() as u32)?
                .with_members(std::mem::take(&mut members[index]))
                .with_kind(kind),
        );
    }
    for expression in resolvers {
        let mut compiler = Compiler::new(
            sources,
            program,
            &mut constants,
            &[],
            Context::FunctionExpression,
        )?;
        compiler.global_context = true;
        compiler.expression(expression, Register(0))?;
        compiler.emit(
            Instruction::Return { src: Register(0) },
            program.expression(expression).span,
        )?;
        functions.push(compiler.finish("(base)", 0)?);
    }
    Module::with_constants(functions, constants.values)
}
