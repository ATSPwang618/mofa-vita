//! The old frame and control-flow conventions are translated once. Execution
//! uses the same operations, property continuations and verifier as source code.
use super::{
    Limits,
    instruction::{Form, Opcode, Operation},
    read::{self, Context, File, Object, Value},
};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use tjs_core::{
    ArgumentSource, CallArguments, CallSite, CallTarget, CatchHandler, CodeOrigin, Constant,
    Diagnostic, Function, FunctionId, FunctionKind, FunctionMember, Instruction as I, Module,
    OriginEntry, Register as R, StoreMode, value::UpdateOp,
};

struct Constants<'a, 'b> {
    data: &'a read::Data<'b>,
    values: Vec<Constant>,
    strings: Vec<Option<u32>>,
    octets: Vec<Option<u32>>,
}
impl Constants<'_, '_> {
    fn string(&mut self, index: usize) -> u32 {
        if let Some(id) = self.strings[index] {
            return id;
        }
        let id = self.values.len() as u32;
        self.values
            .push(Constant::String(Arc::from(self.data.string(index))));
        self.strings[index] = Some(id);
        id
    }
    fn octet(&mut self, index: usize) -> u32 {
        if let Some(id) = self.octets[index] {
            return id;
        }
        let id = self.values.len() as u32;
        self.values
            .push(Constant::Octet(self.data.octets[index].into()));
        self.octets[index] = Some(id);
        id
    }
    fn name(&mut self, index: Option<usize>) -> u32 {
        if let Some(index) = index {
            return self.string(index);
        }
        let id = self.values.len() as u32;
        self.values.push(Constant::String(Arc::from([])));
        id
    }
}

struct TryRegion {
    target: u32,
    exception: i16,
    parent: Option<usize>,
}
struct Flow {
    states: Vec<Option<Option<usize>>>,
    tries: Vec<TryRegion>,
    entries: Vec<Option<usize>>,
}

fn flow(object: &Object, start: u32) -> Result<Flow, Diagnostic> {
    let instructions = &object.instructions;
    let mut result = Flow {
        states: vec![None; instructions.len()],
        tries: Vec::new(),
        entries: vec![None; instructions.len()],
    };
    if instructions.is_empty() {
        return Ok(result);
    }
    let index = |pc| {
        instructions
            .binary_search_by_key(&pc, |i| i.pc)
            .map_err(|_| {
                read::error(
                    object.code_offset + pc as usize * 2,
                    "invalid execution entry",
                )
            })
    };
    let mut pending = VecDeque::from([(index(start)?, None)]);
    while let Some((i, state)) = pending.pop_front() {
        if let Some(previous) = result.states[i] {
            if previous != state {
                return Err(read::error(
                    object.code_offset + instructions[i].pc as usize * 2,
                    "branch joins incompatible try contexts",
                ));
            }
            continue;
        }
        result.states[i] = Some(state);
        let inst = instructions[i];
        let words = &object.code[inst.pc as usize..];
        // Runtime errors, not just THROW, enter the active handler. Catch code
        // resumes the parent's condition flag, just like ExecuteCodeInTryBlock.
        if let Some(active) = state {
            let region: &TryRegion = &result.tries[active];
            pending.push_back((index(region.target)?, region.parent));
        }
        let next = |pending: &mut VecDeque<_>, state| -> Result<(), Diagnostic> {
            if i + 1 == instructions.len() {
                return Err(read::error(
                    object.code_offset + inst.pc as usize * 2,
                    "execution falls beyond the code area",
                ));
            }
            pending.push_back((i + 1, state));
            Ok(())
        };
        match inst.op {
            Opcode::Entry => {
                let entry = result.tries.len();
                result.tries.push(TryRegion {
                    target: (inst.pc as i64 + i64::from(words[1])) as u32,
                    exception: words[2],
                    parent: state,
                });
                result.entries[i] = Some(entry);
                next(&mut pending, Some(entry))?;
            }
            Opcode::Ret | Opcode::Extry => {
                if let Some(active) = state {
                    next(&mut pending, result.tries[active].parent)?;
                }
            }
            Opcode::Throw => {}
            Opcode::Jmp | Opcode::Jf | Opcode::Jnf => {
                let target = (inst.pc as i64 + i64::from(words[1])) as u32;
                pending.push_back((index(target)?, state));
                if inst.op != Opcode::Jmp {
                    next(&mut pending, state)?;
                }
            }
            _ => next(&mut pending, state)?,
        }
    }
    Ok(result)
}

fn function<'a, 'b>(
    object: &'a Object,
    constants: &mut Constants<'a, 'b>,
    name: &str,
    object_index: usize,
    start: u32,
    limit: usize,
) -> Result<Function, Diagnostic> {
    let flow = flow(object, start)?;
    let locals = (object.variables + object.reserve).saturating_sub(2);
    let this = locals + object.frames + 2;
    let registers = this + 6 + flow.tries.len() as u32;
    if registers > tjs_core::ir::MAX_REGISTERS {
        return Err(read::error(
            object.offset,
            "translated frame exceeds register limit",
        ));
    }
    let mut lower = Lower {
        object,
        constants,
        code: Vec::new(),
        calls: Vec::new(),
        map: vec![0; object.code.len() + 1],
        jumps: Vec::new(),
        locals,
        this: R(this),
        scope: R(this + 1),
        flag: R(this + 2),
        key: R(this + 3),
        value: R(this + 4),
        temporary: R(this + 5),
        saves: this + 6,
        needs_scope: false,
    };
    for (index, inst) in object.instructions.iter().enumerate() {
        lower.map[inst.pc as usize] = lower.code.len() as u32;
        lower.instruction(index, &flow);
        lower.map[(inst.pc + inst.length) as usize] = lower.code.len() as u32;
        if lower.code.len() > limit {
            return Err(read::error(
                object.code_offset + inst.pc as usize * 2,
                "translated code exceeds instruction limit",
            ));
        }
    }
    lower.emit(I::Return { src: R(0) });
    let mut targets = Vec::with_capacity(flow.tries.len());
    for (index, region) in flow.tries.iter().enumerate() {
        targets.push(lower.code.len() as u32);
        lower.emit(I::Move {
            dst: lower.flag,
            src: R(lower.saves + index as u32),
        });
        lower.jump(I::Jump { target: 0 }, region.target);
    }
    lower.emit(I::Return { src: R(0) });
    // Old frames start cleared, including the independent SRV result slot.
    // Initialization remains explicit so the ordinary IR verifier stays strict.
    let mut prefix = Vec::new();
    for register in 0..registers {
        if register == 0 || register > object.arguments {
            prefix.push(I::LoadVoid { dst: R(register) });
        }
    }
    prefix.push(I::LoadThis { dst: lower.this });
    if lower.needs_scope {
        prefix.push(I::LoadScope { dst: lower.scope });
    }
    prefix.push(I::LoadInt {
        dst: lower.flag,
        value: 0,
    });
    if let Some(base) = object.collapse {
        prefix.push(I::CollectArguments {
            dst: R(base + 1),
            start: base,
        });
    }
    let jump_index = prefix.len();
    prefix.push(I::Jump { target: 0 });
    let offset = prefix.len() as u32;
    prefix[jump_index] = I::Jump {
        target: offset + lower.map[start as usize],
    };
    if prefix.len() + lower.code.len() > limit {
        return Err(read::error(
            object.offset,
            "translated code exceeds instruction limit",
        ));
    }
    for &(pc, target) in &lower.jumps {
        match &mut lower.code[pc] {
            I::Jump { target: value } | I::JumpIfFalse { target: value, .. } => {
                *value = offset + lower.map[target as usize]
            }
            _ => unreachable!("recorded branch"),
        }
    }
    let mut handlers: Vec<CatchHandler> = Vec::new();
    for (index, inst) in object.instructions.iter().enumerate() {
        let Some(Some(region)) = flow.states[index] else {
            continue;
        };
        let start = offset + lower.map[inst.pc as usize];
        let end = offset + lower.map[(inst.pc + inst.length) as usize];
        if start == end {
            continue;
        }
        let target = offset + targets[region];
        let exception = lower.register(flow.tries[region].exception);
        if let Some(previous) = handlers
            .last_mut()
            .filter(|previous| previous.end == start && previous.target == target)
        {
            previous.end = end;
        } else {
            handlers.push(CatchHandler {
                start,
                end,
                target,
                exception,
            });
        }
    }
    let origin = CodeOrigin {
        storage: name.into(),
        object: object_index as u32,
        entries: object
            .instructions
            .iter()
            .map(|inst| {
                let source = object
                    .source_positions
                    .partition_point(|&(pc, _)| pc <= inst.pc);
                OriginEntry {
                    legacy_pc: inst.pc,
                    ir_pc: offset + lower.map[inst.pc as usize],
                    file_offset: (object.code_offset + inst.pc as usize * 2) as u32,
                    source_offset: source.checked_sub(1).map(|i| object.source_positions[i].1),
                }
            })
            .collect(),
    };
    prefix.extend(lower.code);
    let spans = vec![None; prefix.len()];
    let display_name = object
        .name
        .map(|index| String::from_utf16_lossy(&lower.constants.data.string(index)))
        .unwrap_or_else(|| format!("<object {object_index}>"));
    Function::with_handlers(
        display_name,
        object.arguments,
        registers,
        prefix,
        spans,
        lower.calls,
        handlers,
    )?
    .with_origin(origin)
}

pub(super) fn lower(file: File<'_>, name: &str, limits: &Limits) -> Result<Module, Diagnostic> {
    let mut constants = Constants {
        data: &file.data,
        values: Vec::new(),
        strings: vec![None; file.data.strings.len()],
        octets: vec![None; file.data.octets.len()],
    };
    let mut entry_code = vec![I::LoadVoid { dst: R(0) }];
    let mut calls = Vec::new();
    if let Some(top) = file.top {
        calls.push(CallSite {
            target: CallTarget::Direct(FunctionId(top as u32 + 1)),
            dst: Some(R(0)),
            arguments: CallArguments::Registers {
                start: R(0),
                count: 0,
            },
        });
        entry_code.push(I::Call { site: 0 });
    }
    entry_code.push(I::Return { src: R(0) });
    let mut remaining = limits
        .max_output_instructions
        .checked_sub(entry_code.len())
        .ok_or_else(|| read::error(0, "translated code exceeds instruction limit"))?;
    let spans = vec![None; entry_code.len()];
    let mut functions = vec![Function::new(name, 0, 1, entry_code, spans, calls)?];
    let mut members = vec![Vec::new(); file.objects.len()];
    for object in &file.objects {
        if let Some(parent) = object.parent {
            for &(member, target) in &object.members {
                members[parent].push(FunctionMember {
                    name: constants.string(member),
                    function: FunctionId(target as u32 + 1),
                });
            }
        }
    }
    let mut helpers = Vec::new();
    let mut helper_ids = HashMap::new();
    for (index, object) in file.objects.iter().enumerate() {
        let kind = match object.context {
            Context::Property => FunctionKind::Property {
                getter: object.getter.map(|i| FunctionId(i as u32 + 1)),
                setter: object.setter.map(|i| FunctionId(i as u32 + 1)),
            },
            Context::Class => {
                let mut bases = Vec::new();
                if let Some(super_index) = object.super_getter {
                    for &start in &file.objects[super_index].super_entries {
                        let id = *helper_ids.entry((super_index, start)).or_insert_with(|| {
                            let id = FunctionId((file.objects.len() + 1 + helpers.len()) as u32);
                            helpers.push((super_index, start));
                            id
                        });
                        bases.push(id);
                    }
                }
                FunctionKind::Class {
                    constructor: constants.name(object.name),
                    bases,
                }
            }
            Context::Function | Context::Expression => FunctionKind::Function,
            Context::Super => FunctionKind::SuperResolver,
            Context::Top | Context::Setter | Context::Getter => FunctionKind::Internal,
        };
        let function = function(object, &mut constants, name, index, 0, remaining)?
            .with_kind(kind)
            .with_members(std::mem::take(&mut members[index]));
        remaining -= function.instructions().len();
        functions.push(function);
    }
    for (index, start) in helpers {
        let function = function(
            &file.objects[index],
            &mut constants,
            name,
            index,
            start,
            remaining,
        )?
        .with_kind(FunctionKind::Internal);
        remaining -= function.instructions().len();
        functions.push(function);
    }
    Module::with_constants(functions, constants.values)
}

struct Lower<'a, 'b, 'c> {
    object: &'a Object,
    constants: &'b mut Constants<'a, 'c>,
    code: Vec<I>,
    calls: Vec<CallSite>,
    map: Vec<u32>,
    jumps: Vec<(usize, u32)>,
    locals: u32,
    this: R,
    scope: R,
    flag: R,
    key: R,
    value: R,
    temporary: R,
    saves: u32,
    needs_scope: bool,
}

impl Lower<'_, '_, '_> {
    fn instruction(&mut self, index: usize, flow: &Flow) {
        let inst = self.object.instructions[index];
        let words = &self.object.code[inst.pc as usize..];
        if let Some((op, form)) = inst.op.operation() {
            self.operation(op, form, words);
            return;
        }
        use Opcode::*;
        match inst.op {
            Nop | Debugger => {
                // VM_DEBUGGER has no effect in the reference without ENABLE_DEBUGGER.
                self.emit(I::Move {
                    dst: self.temporary,
                    src: self.temporary,
                });
            }
            Const => {
                let dst = self.register(words[1]);
                self.constant(words[2], dst);
            }
            Cp | Getp | Setp | Chgthis | Chkins | Addci => {
                let a = self.register(words[1]);
                let b = self.register(words[2]);
                self.emit(match inst.op {
                    Cp => I::Move { dst: a, src: b },
                    Getp => I::GetProperty { dst: a, src: b },
                    Setp => I::SetProperty {
                        property: a,
                        value: b,
                    },
                    Chgthis => I::BindContext {
                        dst: a,
                        object: a,
                        context: b,
                    },
                    Chkins => I::InstanceOf {
                        dst: a,
                        lhs: a,
                        rhs: b,
                    },
                    Addci => I::ClassInfo { object: a, name: b },
                    _ => unreachable!(),
                });
            }
            Cl => {
                let dst = self.register(words[1]);
                self.emit(I::LoadVoid { dst });
            }
            Ccl => {
                for old in i32::from(words[1])..i32::from(words[1]) + i32::from(words[2]) {
                    let dst = self.register(old as i16);
                    self.emit(I::LoadVoid { dst });
                }
            }
            Tt | Tf => {
                let src = self.register(words[1]);
                self.emit(I::Not {
                    dst: self.flag,
                    src,
                });
                if inst.op == Tt {
                    self.emit(I::Not {
                        dst: self.flag,
                        src: self.flag,
                    });
                }
            }
            Ceq | Cdeq | Clt | Cgt => {
                let lhs = self.register(words[1]);
                let rhs = self.register(words[2]);
                let dst = self.flag;
                self.emit(match inst.op {
                    Ceq => I::Equal { dst, lhs, rhs },
                    Cdeq => I::StrictEqual { dst, lhs, rhs },
                    Clt => I::Less { dst, lhs, rhs },
                    Cgt => I::Greater { dst, lhs, rhs },
                    _ => unreachable!(),
                });
            }
            Setf | Setnf => {
                let dst = self.register(words[1]);
                self.emit(if inst.op == Setf {
                    I::Move {
                        dst,
                        src: self.flag,
                    }
                } else {
                    I::Not {
                        dst,
                        src: self.flag,
                    }
                });
            }
            Nf => self.emit(I::Not {
                dst: self.flag,
                src: self.flag,
            }),
            Jf | Jnf | Jmp => {
                let target = (inst.pc as i64 + i64::from(words[1])) as u32;
                let jump = match inst.op {
                    Jmp => I::Jump { target: 0 },
                    Jnf => I::JumpIfFalse {
                        condition: self.flag,
                        target: 0,
                    },
                    Jf => {
                        self.emit(I::Not {
                            dst: self.temporary,
                            src: self.flag,
                        });
                        I::JumpIfFalse {
                            condition: self.temporary,
                            target: 0,
                        }
                    }
                    _ => unreachable!(),
                };
                self.jump(jump, target);
            }
            Typeofd | Typeofi | Gpd | Gpi | Gpds | Gpis | Deld | Deli => {
                let dst = if words[1] == 0 && matches!(inst.op, Deld | Deli) {
                    self.temporary
                } else {
                    self.register(words[1])
                };
                let key = self.member_key(words[3], matches!(inst.op, Typeofd | Gpd | Gpds | Deld));
                let raw = matches!(inst.op, Gpds | Gpis);
                let instruction = if matches!(inst.op, Typeofd | Typeofi) {
                    I::TypeOfMember {
                        dst,
                        object: self.register(words[2]),
                        key,
                        computed: inst.op == Typeofi,
                    }
                } else if words[2] == -2 {
                    if matches!(inst.op, Deld | Deli) {
                        I::DeleteName { dst, key }
                    } else if raw {
                        I::GetRawName { dst, key }
                    } else {
                        I::GetName { dst, key }
                    }
                } else {
                    let object = self.register(words[2]);
                    if matches!(inst.op, Deld | Deli) {
                        I::DeleteMember { dst, object, key }
                    } else if raw {
                        I::GetRawMember { dst, object, key }
                    } else {
                        I::GetMember { dst, object, key }
                    }
                };
                self.emit(instruction);
            }
            Spd | Spde | Spdeh | Spds | Spi | Spie | Spis => {
                let key = self.member_key(words[2], matches!(inst.op, Spd | Spde | Spdeh | Spds));
                let value = self.register(words[3]);
                let mode = match inst.op {
                    Spd | Spi => StoreMode::Existing,
                    Spde | Spie => StoreMode::Ensure,
                    Spdeh => StoreMode::Hidden,
                    Spds | Spis => StoreMode::Raw,
                    _ => unreachable!(),
                };
                let instruction = if words[1] == -2 {
                    I::StoreName { key, value, mode }
                } else {
                    I::StoreMember {
                        object: self.register(words[1]),
                        key,
                        value,
                        mode,
                    }
                };
                self.emit(instruction);
            }
            Call | Calld | Calli | New => self.call(inst.op, words),
            Entry => {
                if let Some(region) = flow.entries[index] {
                    self.emit(I::Move {
                        dst: R(self.saves + region as u32),
                        src: self.flag,
                    });
                    self.emit(I::LoadInt {
                        dst: self.flag,
                        value: 0,
                    });
                }
            }
            Ret | Extry => {
                if let Some(Some(region)) = flow.states[index] {
                    self.emit(I::Move {
                        dst: self.flag,
                        src: R(self.saves + region as u32),
                    });
                } else {
                    self.emit(I::Return { src: R(0) });
                }
            }
            Regmember => self.emit(I::RegisterMembers),
            Global => {
                let dst = self.register(words[1]);
                self.emit(I::LoadGlobal { dst });
                self.emit(I::LoadNull {
                    dst: self.temporary,
                });
                self.emit(I::BindContext {
                    dst,
                    object: dst,
                    context: self.temporary,
                });
            }
            Lnot | Bnot | Typeof | Eval | Eexp | Asc | Chr | Num | Chs | Inv | Chkinv | Int
            | Real | Str | Octet | Srv | Throw => {
                let dst = self.register(words[1]);
                let src = dst;
                self.emit(match inst.op {
                    Lnot => I::Not { dst, src },
                    Bnot => I::BitNot { dst, src },
                    Typeof => I::TypeOf { dst, src },
                    Eval => I::Eval {
                        dst: Some(dst),
                        src,
                    },
                    Eexp => I::Eval { dst: None, src },
                    Asc => I::CharacterCode { dst, src },
                    Chr => I::CharacterFrom { dst, src },
                    Num => I::ToNumber { dst, src },
                    Chs => I::Negate { dst, src },
                    Inv => I::Invalidate { dst, src },
                    Chkinv => I::IsValid { dst, src },
                    Int => I::ToInteger { dst, src },
                    Real => I::ToReal { dst, src },
                    Str => I::ToString { dst, src },
                    Octet => I::ToOctet { dst, src },
                    Srv => I::Move { dst: R(0), src },
                    Throw => I::Throw { src },
                    _ => unreachable!(),
                });
            }
            _ => unreachable!("arithmetic family handled above"),
        }
    }

    fn call(&mut self, op: Opcode, words: &[i16]) {
        let member = matches!(op, Opcode::Calld | Opcode::Calli);
        let count_index = if member { 4 } else { 3 };
        let arguments = match words[count_index] {
            -1 => CallArguments::ForwardOriginal,
            -2 => {
                let mut args = Vec::with_capacity(words[count_index + 1] as usize);
                for pair in words[count_index + 2..]
                    .chunks_exact(2)
                    .take(words[count_index + 1] as usize)
                {
                    args.push(match pair[0] {
                        0 => ArgumentSource::Value(self.register(pair[1])),
                        1 => ArgumentSource::Array(self.register(pair[1])),
                        2 => ArgumentSource::Original {
                            start: self.object.unnamed.expect("validated unnamed arguments"),
                        },
                        _ => unreachable!("validated expansion kind"),
                    });
                }
                CallArguments::Expanded(args.into_boxed_slice())
            }
            count => CallArguments::Expanded(
                words[count_index + 1..count_index + 1 + count as usize]
                    .iter()
                    .map(|&old| ArgumentSource::Value(self.register(old)))
                    .collect(),
            ),
        };
        let target = if member {
            let key = self.member_key(words[3], op == Opcode::Calld);
            if words[2] == -2 && op == Opcode::Calld {
                CallTarget::Name { key }
            } else {
                CallTarget::Member {
                    object: self.register(words[2]),
                    key,
                    computed: op == Opcode::Calli,
                }
            }
        } else if op == Opcode::New {
            CallTarget::Construct(self.register(words[2]))
        } else {
            CallTarget::Value(self.register(words[2]))
        };
        let dst = (words[1] != 0).then(|| self.register(words[1]));
        let site = self.calls.len() as u32;
        self.calls.push(CallSite {
            target,
            dst,
            arguments,
        });
        self.emit(I::Call { site });
    }
    fn register(&mut self, old: i16) -> R {
        match old {
            -1 => self.this,
            -2 => {
                self.needs_scope = true;
                self.scope
            }
            old if old < -2 => R((-i32::from(old) - 2) as u32),
            old => R(self.locals + 1 + old as u32),
        }
    }
    fn emit(&mut self, instruction: I) {
        self.code.push(instruction);
    }
    fn constant(&mut self, index: i16, dst: R) {
        let instruction = match self.object.values[index as usize] {
            Value::Void => I::LoadVoid { dst },
            Value::Null => I::LoadNull { dst },
            Value::Int(value) => I::LoadInt { dst, value },
            Value::Real(bits) => I::LoadReal { dst, bits },
            Value::Function(function) => I::LoadFunction {
                dst,
                function: FunctionId(function as u32 + 1),
            },
            Value::String(index) => I::LoadConstant {
                dst,
                constant: self.constants.string(index),
            },
            Value::Octet(index) => I::LoadConstant {
                dst,
                constant: self.constants.octet(index),
            },
        };
        self.emit(instruction);
    }
    fn member_key(&mut self, old: i16, direct: bool) -> R {
        if direct {
            self.constant(old, self.key);
            self.key
        } else {
            self.register(old)
        }
    }
    fn jump(&mut self, instruction: I, target: u32) {
        self.jumps.push((self.code.len(), target));
        self.emit(instruction);
    }
    fn update_op(operation: Operation) -> UpdateOp {
        match operation {
            Operation::Inc => UpdateOp::Increment,
            Operation::Dec => UpdateOp::Decrement,
            Operation::Lor => UpdateOp::LogicalOr,
            Operation::Land => UpdateOp::LogicalAnd,
            Operation::Bor => UpdateOp::BitOr,
            Operation::Bxor => UpdateOp::BitXor,
            Operation::Band => UpdateOp::BitAnd,
            Operation::Sar => UpdateOp::ShiftRight,
            Operation::Sal => UpdateOp::ShiftLeft,
            Operation::Sr => UpdateOp::ShiftRightUnsigned,
            Operation::Add => UpdateOp::Add,
            Operation::Sub => UpdateOp::Subtract,
            Operation::Mod => UpdateOp::Remainder,
            Operation::Div => UpdateOp::Divide,
            Operation::Idiv => UpdateOp::IntDivide,
            Operation::Mul => UpdateOp::Multiply,
        }
    }
    fn operation(&mut self, operation: Operation, form: Form, words: &[i16]) {
        let op = Self::update_op(operation);
        if matches!(form, Form::Register) {
            let dst = self.register(words[1]);
            let rhs = if op.unary() {
                self.temporary
            } else {
                self.register(words[2])
            };
            let instruction = match op {
                UpdateOp::Increment | UpdateOp::Decrement => {
                    self.emit(I::ToNumber { dst, src: dst });
                    self.emit(I::LoadInt {
                        dst: rhs,
                        value: if op == UpdateOp::Increment { 1 } else { -1 },
                    });
                    I::Add { dst, lhs: dst, rhs }
                }
                UpdateOp::LogicalOr => I::LogicalOr { dst, lhs: dst, rhs },
                UpdateOp::LogicalAnd => I::LogicalAnd { dst, lhs: dst, rhs },
                UpdateOp::BitOr => I::BitOr { dst, lhs: dst, rhs },
                UpdateOp::BitXor => I::BitXor { dst, lhs: dst, rhs },
                UpdateOp::BitAnd => I::BitAnd { dst, lhs: dst, rhs },
                UpdateOp::ShiftRight => I::ShiftRight { dst, lhs: dst, rhs },
                UpdateOp::ShiftLeft => I::ShiftLeft { dst, lhs: dst, rhs },
                UpdateOp::ShiftRightUnsigned => I::ShiftRightUnsigned { dst, lhs: dst, rhs },
                UpdateOp::Add => I::Add { dst, lhs: dst, rhs },
                UpdateOp::Subtract => I::Subtract { dst, lhs: dst, rhs },
                UpdateOp::Remainder => I::Remainder { dst, lhs: dst, rhs },
                UpdateOp::Divide => I::Divide { dst, lhs: dst, rhs },
                UpdateOp::IntDivide => I::IntDivide { dst, lhs: dst, rhs },
                UpdateOp::Multiply => I::Multiply { dst, lhs: dst, rhs },
            };
            self.emit(instruction);
            return;
        }
        if !op.unary() {
            let source = self.register(words[if matches!(form, Form::Property) { 3 } else { 4 }]);
            self.emit(I::Move {
                dst: self.value,
                src: source,
            });
        }
        let instruction = if matches!(form, Form::Property) {
            let property = self.register(words[2]);
            I::UpdateProperty {
                property,
                value: self.value,
                op,
            }
        } else {
            let key = self.member_key(words[3], matches!(form, Form::Direct));
            if words[2] == -2 {
                I::UpdateName {
                    key,
                    value: self.value,
                    op,
                }
            } else {
                I::UpdateMember {
                    object: self.register(words[2]),
                    key,
                    value: self.value,
                    op,
                }
            }
        };
        self.emit(instruction);
        if words[1] != 0 {
            let dst = self.register(words[1]);
            self.emit(I::Move {
                dst,
                src: self.value,
            });
        }
    }
}
