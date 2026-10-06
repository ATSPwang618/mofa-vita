//! A module owns immutable functions and their call-site metadata.

use std::{
    cmp::Reverse,
    fmt::Write,
    sync::{Arc, Weak},
};

use crate::{
    Diagnostic, Instruction, Phase, Register, Span,
    ir::{MAX_INSTRUCTIONS, MAX_REGISTERS},
};

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FunctionId(pub u32);

/// A named child exposed as a member of its enclosing function object.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug)]
pub struct FunctionMember {
    /// Index of a string constant in the same module.
    pub name: u32,
    pub function: FunctionId,
}

/// Original code locations survive import and caching without inventing source
/// text or storing a process-local SourceId. PCs are 16-bit code-word offsets.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct CodeOrigin {
    pub storage: String,
    pub object: u32,
    pub entries: Vec<OriginEntry>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug)]
pub struct OriginEntry {
    pub legacy_pc: u32,
    pub ir_pc: u32,
    pub file_offset: u32,
    pub source_offset: Option<u32>,
}

/// Script definition kind. Accessors and base resolvers are ordinary functions.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub enum FunctionKind {
    #[default]
    Function,
    Class {
        constructor: u32,
        bases: Vec<FunctionId>,
    },
    Property {
        getter: Option<FunctionId>,
        setter: Option<FunctionId>,
    },
    /// Executable top/accessor/base-resolver contexts are not instances of Function.
    Internal,
    /// Original ctSuperClassGetter: only its explicit resolver entries execute;
    /// ordinary FuncCall succeeds without touching the caller's result.
    SuperResolver,
}

/// Portable module data, never runtime heap handles.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub enum Constant {
    String(Arc<[u16]>),
    Octet(Box<[u8]>),
    Array(Box<[ConstantValue]>),
    Dictionary(Box<[(Arc<[u16]>, ConstantValue)]>),
}

/// Container edges point backwards in the constant table. This keeps module
/// validation, instantiation and destruction iterative, even for nested data.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug)]
pub enum ConstantValue {
    Void,
    Int(i64),
    Real(f64),
    Null,
    Reference(u32),
}

/// Arguments occupy a contiguous snapshot in the caller's registers. Keeping
/// this metadata outside Instruction avoids enlarging every arithmetic opcode.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct CallSite {
    pub target: CallTarget,
    /// None passes a null result to the callee; it is not a scratch register.
    pub dst: Option<Register>,
    pub arguments: CallArguments,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug)]
pub enum CallTarget {
    Direct(FunctionId),
    Value(Register),
    Construct(Register),
    Name {
        key: Register,
    },
    Member {
        object: Register,
        key: Register,
        computed: bool,
    },
}

impl CallTarget {
    fn registers(self) -> [Option<Register>; 2] {
        match self {
            Self::Direct(_) => [None, None],
            Self::Value(register) | Self::Construct(register) | Self::Name { key: register } => {
                [Some(register), None]
            }
            Self::Member { object, key, .. } => [Some(object), Some(key)],
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub enum CallArguments {
    Registers { start: Register, count: u32 },
    ForwardOriginal,
    Expanded(Box<[ArgumentSource]>),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug)]
pub enum ArgumentSource {
    Value(Register),
    Array(Register),
    Original { start: u32 },
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug)]
pub struct CatchHandler {
    /// Protected instruction range, end excluded.
    pub start: u32,
    pub end: u32,
    pub target: u32,
    pub exception: Register,
}

pub(crate) fn find_handler(handlers: &[CatchHandler], pc: usize) -> Option<CatchHandler> {
    handlers
        .iter()
        .rev()
        .copied()
        .find(|handler| handler.start as usize <= pc && pc < handler.end as usize)
}

impl CallSite {
    pub(crate) fn arguments(&self) -> impl Iterator<Item = Register> + '_ {
        let range = match self.arguments {
            CallArguments::Registers { start, count } => start.0..start.0 + count,
            CallArguments::ForwardOriginal | CallArguments::Expanded(_) => 0..0,
        };
        let expanded: &[ArgumentSource] = match &self.arguments {
            CallArguments::Expanded(sources) => sources,
            _ => &[],
        };
        range
            .map(Register)
            .chain(expanded.iter().filter_map(|source| match source {
                ArgumentSource::Value(register) | ArgumentSource::Array(register) => {
                    Some(*register)
                }
                ArgumentSource::Original { .. } => None,
            }))
    }

    pub(crate) fn reads(&self) -> impl Iterator<Item = Register> + '_ {
        self.arguments()
            .chain(self.target.registers().into_iter().flatten())
    }
}

#[derive(Debug)]
pub struct Function {
    name: String,
    parameters: u32,
    registers: u32,
    code: Vec<Instruction>,
    spans: Vec<Option<Span>>,
    calls: Vec<CallSite>,
    handlers: Vec<CatchHandler>,
    members: Vec<FunctionMember>,
    kind: FunctionKind,
    origin: Option<CodeOrigin>,
}

impl Function {
    /// r0 is the result slot; parameters occupy r1 through rN.
    pub fn new(
        name: impl Into<String>,
        parameters: u32,
        registers: u32,
        code: Vec<Instruction>,
        spans: Vec<Option<Span>>,
        calls: Vec<CallSite>,
    ) -> Result<Self, Diagnostic> {
        Self::with_handlers(name, parameters, registers, code, spans, calls, Vec::new())
    }

    pub fn with_handlers(
        name: impl Into<String>,
        parameters: u32,
        registers: u32,
        code: Vec<Instruction>,
        spans: Vec<Option<Span>>,
        calls: Vec<CallSite>,
        mut handlers: Vec<CatchHandler>,
    ) -> Result<Self, Diagnostic> {
        let error = |span, message| Diagnostic::new(Phase::Verify, span, message);
        if registers == 0 || registers > MAX_REGISTERS || parameters >= registers {
            return Err(error(
                None,
                "register/parameter count exceeds the supported range",
            ));
        }
        if code.is_empty() || code.len() > MAX_INSTRUCTIONS {
            return Err(error(None, "instruction count exceeds the supported range"));
        }
        if spans.len() != code.len() {
            return Err(error(None, "instruction/source map lengths differ"));
        }
        for call in &calls {
            if call.dst.is_some_and(|dst| dst.0 >= registers) {
                return Err(error(None, "call destination is out of bounds"));
            }
            if call
                .target
                .registers()
                .into_iter()
                .flatten()
                .any(|r| r.0 >= registers)
            {
                return Err(error(None, "call target register is out of bounds"));
            }
            if let CallArguments::Registers { start, count } = call.arguments {
                if start.0 > registers || count > registers - start.0 {
                    return Err(error(None, "call argument window is out of bounds"));
                }
            }
            if call.arguments().any(|r| r.0 >= registers) {
                return Err(error(None, "expanded argument register is out of bounds"));
            }
        }
        for (pc, &instruction) in code.iter().enumerate() {
            if let Instruction::Call { site } = instruction {
                if site as usize >= calls.len() {
                    return Err(error(spans[pc], "call site is out of bounds"));
                }
            }
            for register in instruction.reads().into_iter().flatten() {
                if register.0 >= registers {
                    return Err(error(spans[pc], "source register is out of bounds"));
                }
            }
            if let Some(register) = instruction.writes(&calls) {
                if register.0 >= registers {
                    return Err(error(spans[pc], "destination register is out of bounds"));
                }
            }
            if let Instruction::Jump { target } | Instruction::JumpIfFalse { target, .. } =
                instruction
            {
                if target as usize >= code.len() {
                    return Err(error(spans[pc], "jump target is out of bounds"));
                }
            }
        }
        if !matches!(code.last(), Some(Instruction::Return { .. })) {
            return Err(error(
                spans.last().copied().flatten(),
                "function must end with return",
            ));
        }
        // Outermost first for equal starts; reverse lookup selects the innermost
        // protected region. Structured ranges nest or are disjoint.
        handlers.sort_by_key(|handler| (handler.start, Reverse(handler.end)));
        let mut enclosing: Vec<CatchHandler> = Vec::new();
        for &handler in &handlers {
            if handler.start >= handler.end
                || handler.end > handler.target
                || handler.target as usize >= code.len()
                || handler.exception.0 >= registers
            {
                return Err(error(
                    None,
                    "invalid catch handler range, target or destination",
                ));
            }
            while enclosing
                .last()
                .is_some_and(|parent| handler.start >= parent.end)
            {
                enclosing.pop();
            }
            if let Some(parent) = enclosing.last() {
                if handler.end > parent.end
                    || (handler.start == parent.start && handler.end == parent.end)
                {
                    return Err(error(None, "catch ranges must nest or be disjoint"));
                }
            }
            enclosing.push(handler);
        }
        crate::verify::definite_initialization(
            registers, parameters, &code, &spans, &calls, &handlers,
        )?;
        Ok(Self {
            name: name.into(),
            parameters,
            registers,
            code,
            spans,
            calls,
            handlers,
            members: Vec::new(),
            kind: FunctionKind::Function,
            origin: None,
        })
    }

    /// References are checked when this function is linked into a Module.
    pub fn with_members(mut self, members: Vec<FunctionMember>) -> Self {
        self.members = members;
        self
    }

    pub fn with_kind(mut self, kind: FunctionKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn with_origin(mut self, origin: CodeOrigin) -> Result<Self, Diagnostic> {
        if origin
            .entries
            .iter()
            .any(|entry| entry.ir_pc as usize >= self.code.len())
            || origin.entries.windows(2).any(|pair| {
                pair[0].legacy_pc >= pair[1].legacy_pc
                    || pair[0].ir_pc > pair[1].ir_pc
                    || pair[0].file_offset >= pair[1].file_offset
            })
        {
            return Err(Diagnostic::new(
                Phase::Verify,
                None,
                "invalid original code location map",
            ));
        }
        self.origin = Some(origin);
        Ok(self)
    }

    pub fn origin(&self) -> Option<&CodeOrigin> {
        self.origin.as_ref()
    }

    pub fn original_location(&self, pc: usize) -> Option<OriginEntry> {
        let entries = &self.origin.as_ref()?.entries;
        let index = entries
            .partition_point(|entry| entry.ir_pc as usize <= pc)
            .checked_sub(1)?;
        entries.get(index).copied()
    }

    pub(crate) fn trace_location(&self, pc: usize) -> (String, usize) {
        match (&self.origin, self.original_location(pc)) {
            (Some(origin), Some(entry)) => (
                format!("{}#{}:{}", origin.storage, origin.object, self.name),
                entry.legacy_pc as usize,
            ),
            _ => (self.name.clone(), pc),
        }
    }

    pub fn kind(&self) -> &FunctionKind {
        &self.kind
    }

    pub fn members(&self) -> &[FunctionMember] {
        &self.members
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn parameter_count(&self) -> u32 {
        self.parameters
    }
    pub fn register_count(&self) -> u32 {
        self.registers
    }
    pub fn instructions(&self) -> &[Instruction] {
        &self.code
    }
    pub fn calls(&self) -> &[CallSite] {
        &self.calls
    }
    pub fn handlers(&self) -> &[CatchHandler] {
        &self.handlers
    }
    pub(crate) fn handler_at(&self, pc: usize) -> Option<CatchHandler> {
        find_handler(&self.handlers, pc)
    }
    pub fn span_at(&self, pc: usize) -> Option<Span> {
        self.spans.get(pc).copied().flatten()
    }
}

#[derive(Debug)]
struct ModuleData {
    functions: Vec<Function>,
    constants: Vec<Constant>,
}

/// Immutable code is shared by VMs and escaping function objects.
#[derive(Clone, Debug)]
pub struct Module(Arc<ModuleData>);

/// A code-lifetime observer for source maps and tooling; never roots VM objects.
#[derive(Clone, Debug)]
pub struct WeakModule(Weak<ModuleData>);

impl WeakModule {
    pub fn is_alive(&self) -> bool {
        self.0.strong_count() != 0
    }
}

impl Module {
    pub fn downgrade(&self) -> WeakModule {
        WeakModule(Arc::downgrade(&self.0))
    }

    pub fn new(
        registers: u32,
        code: Vec<Instruction>,
        spans: Vec<Option<Span>>,
    ) -> Result<Self, Diagnostic> {
        Self::from_functions(vec![Function::new(
            "<script>",
            0,
            registers,
            code,
            spans,
            Vec::new(),
        )?])
    }

    /// Function 0 is the parameterless script entry. Linking checks callees once.
    pub fn from_functions(functions: Vec<Function>) -> Result<Self, Diagnostic> {
        Self::with_constants(functions, Vec::new())
    }

    pub fn with_constants(
        functions: Vec<Function>,
        constants: Vec<Constant>,
    ) -> Result<Self, Diagnostic> {
        for (index, constant) in constants.iter().enumerate() {
            let valid = |value: &ConstantValue| !matches!(value, ConstantValue::Reference(target) if *target as usize >= index);
            let valid = match constant {
                Constant::Array(values) => values.iter().all(valid),
                Constant::Dictionary(entries) => entries.iter().all(|(_, value)| valid(value)),
                _ => true,
            };
            if !valid {
                return Err(Diagnostic::new(
                    Phase::Verify,
                    None,
                    "constant containers may only reference earlier constants",
                ));
            }
        }
        if functions.is_empty()
            || functions[0].parameters != 0
            || !matches!(functions[0].kind, FunctionKind::Function)
        {
            return Err(Diagnostic::new(
                Phase::Verify,
                None,
                "module requires a parameterless function as its script entry",
            ));
        }
        for function in &functions {
            let references: Vec<_> = match &function.kind {
                FunctionKind::Function | FunctionKind::Internal | FunctionKind::SuperResolver => {
                    Vec::new()
                }
                FunctionKind::Class { bases, .. } => bases.clone(),
                FunctionKind::Property { getter, setter } => {
                    getter.iter().chain(setter).copied().collect()
                }
            };
            if references.iter().any(|id| id.0 as usize >= functions.len()) {
                return Err(Diagnostic::new(
                    Phase::Verify,
                    None,
                    "definition reference is out of bounds",
                ));
            }
            if let FunctionKind::Class { constructor, .. } = function.kind {
                if !matches!(
                    constants.get(constructor as usize),
                    Some(Constant::String(_))
                ) {
                    return Err(Diagnostic::new(
                        Phase::Verify,
                        None,
                        "class constructor name must be a string constant",
                    ));
                }
            }
            if references.iter().any(|id| {
                !matches!(
                    functions[id.0 as usize].kind,
                    FunctionKind::Function | FunctionKind::Internal
                )
            }) {
                return Err(Diagnostic::new(
                    Phase::Verify,
                    None,
                    "accessors and base resolvers must be functions",
                ));
            }
            for member in &function.members {
                if member.function.0 as usize >= functions.len()
                    || !matches!(
                        constants.get(member.name as usize),
                        Some(Constant::String(_))
                    )
                {
                    return Err(Diagnostic::new(
                        Phase::Verify,
                        None,
                        "invalid function member name or target",
                    ));
                }
            }
            for (pc, instruction) in function.instructions().iter().enumerate() {
                if matches!(
                    instruction,
                    Instruction::RegisterMembers | Instruction::AddClassInfo
                ) && !matches!(function.kind, FunctionKind::Class { .. })
                {
                    return Err(Diagnostic::new(
                        Phase::Verify,
                        function.span_at(pc),
                        "class registration requires a class initializer",
                    ));
                }
                if let Instruction::LoadConstant { constant, .. } = instruction {
                    if *constant as usize >= constants.len() {
                        return Err(Diagnostic::new(
                            Phase::Verify,
                            function.span_at(pc),
                            "constant is out of bounds",
                        ));
                    }
                }
                if let Instruction::LoadFunction {
                    function: callee, ..
                } = instruction
                {
                    if callee.0 as usize >= functions.len() {
                        return Err(Diagnostic::new(
                            Phase::Verify,
                            function.span_at(pc),
                            "function value is out of bounds",
                        ));
                    }
                }
            }
            for call in &function.calls {
                if matches!(call.target, CallTarget::Direct(callee) if callee.0 as usize >= functions.len())
                {
                    return Err(Diagnostic::new(
                        Phase::Verify,
                        None,
                        "callee is out of bounds",
                    ));
                }
                if matches!(call.target, CallTarget::Direct(callee) if matches!(functions[callee.0 as usize].kind, FunctionKind::Property { .. }))
                {
                    return Err(Diagnostic::new(
                        Phase::Verify,
                        None,
                        "a property cannot be a direct call target",
                    ));
                }
            }
        }
        Ok(Self(Arc::new(ModuleData {
            functions,
            constants,
        })))
    }

    pub fn constants(&self) -> &[Constant] {
        &self.0.constants
    }

    /// Capacity-based charge for retaining immutable code and its metadata.
    /// Shared constant strings are charged in full; allocator headers are not
    /// included. This does not include a separately owned source-text map.
    pub fn retained_bytes(&self) -> usize {
        use std::mem::{size_of, size_of_val};
        let mut bytes = size_of::<ModuleData>()
            + 2 * size_of::<usize>()
            + self.0.functions.capacity() * size_of::<Function>()
            + self.0.constants.capacity() * size_of::<Constant>();
        for function in &self.0.functions {
            bytes += function.name.capacity()
                + function.code.capacity() * size_of::<Instruction>()
                + function.spans.capacity() * size_of::<Option<Span>>()
                + function.calls.capacity() * size_of::<CallSite>()
                + function.handlers.capacity() * size_of::<CatchHandler>()
                + function.members.capacity() * size_of::<FunctionMember>();
            for call in &function.calls {
                if let CallArguments::Expanded(arguments) = &call.arguments {
                    bytes += size_of_val(arguments.as_ref());
                }
            }
            if let FunctionKind::Class { bases, .. } = &function.kind {
                bytes += bases.capacity() * size_of::<FunctionId>();
            }
            if let Some(origin) = &function.origin {
                bytes += origin.storage.capacity()
                    + origin.entries.capacity() * size_of::<OriginEntry>();
            }
        }
        for constant in &self.0.constants {
            bytes += match constant {
                Constant::String(text) => size_of_val(text.as_ref()) + 2 * size_of::<usize>(),
                Constant::Octet(data) => data.len(),
                Constant::Array(values) => size_of_val(values.as_ref()),
                Constant::Dictionary(entries) => {
                    size_of_val(entries.as_ref())
                        + entries
                            .iter()
                            .map(|(key, _)| size_of_val(key.as_ref()) + 2 * size_of::<usize>())
                            .sum::<usize>()
                }
            };
        }
        bytes
    }

    pub fn functions(&self) -> &[Function] {
        &self.0.functions
    }
    pub fn entry(&self) -> &Function {
        &self.0.functions[0]
    }
    pub fn register_count(&self) -> u32 {
        self.entry().register_count()
    }
    pub fn instructions(&self) -> &[Instruction] {
        self.entry().instructions()
    }
    pub fn span_at(&self, pc: usize) -> Option<Span> {
        self.entry().span_at(pc)
    }

    pub fn disassemble(&self) -> String {
        let mut text = String::new();
        for (index, constant) in self.constants().iter().enumerate() {
            match constant {
                Constant::String(units) => writeln!(
                    &mut text,
                    "constant c{index} string utf16_units={}",
                    units.len()
                ),
                Constant::Octet(bytes) => {
                    writeln!(&mut text, "constant c{index} octet bytes={}", bytes.len())
                }
                Constant::Array(values) => {
                    writeln!(&mut text, "constant c{index} array {values:?}")
                }
                Constant::Dictionary(entries) => {
                    writeln!(
                        &mut text,
                        "constant c{index} dictionary entries={}",
                        entries.len()
                    )
                }
            }
            .expect("String formatting");
        }
        for (index, function) in self.functions().iter().enumerate() {
            writeln!(
                &mut text,
                "function {index} {} parameters={} registers={} instructions={}",
                function.name,
                function.parameters,
                function.registers,
                function.code.len()
            )
            .expect("String formatting");
            if !matches!(function.kind, FunctionKind::Function) {
                writeln!(&mut text, "  kind {:?}", function.kind).expect("String formatting");
            }
            for member in &function.members {
                writeln!(
                    &mut text,
                    "  member c{} = function {}",
                    member.name, member.function.0
                )
                .expect("String formatting");
            }
            for (pc, instruction) in function.code.iter().enumerate() {
                write!(&mut text, "{pc:04}  {instruction}").expect("String formatting");
                if let Instruction::Call { site } = instruction {
                    let call = &function.calls[*site as usize];
                    write!(
                        &mut text,
                        " -> {} = {:?}({:?})",
                        call.dst
                            .map_or_else(|| "discard".into(), |dst| dst.to_string()),
                        call.target,
                        call.arguments
                    )
                    .expect("String formatting");
                }
                if let Some(span) = function.span_at(pc) {
                    write!(
                        &mut text,
                        "  ; utf16 {}..{}",
                        span.start().get(),
                        span.end().get()
                    )
                    .expect("String formatting");
                }
                if let Some(location) = function.original_location(pc) {
                    write!(
                        &mut text,
                        "  ; TJS2100 pc={} byte={} source={:?}",
                        location.legacy_pc, location.file_offset, location.source_offset
                    )
                    .expect("String formatting");
                }
                text.push('\n');
            }
            for handler in &function.handlers {
                writeln!(
                    &mut text,
                    "catch {}..{} -> {:04}, {}",
                    handler.start, handler.end, handler.target, handler.exception
                )
                .expect("String formatting");
            }
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(code: Vec<Instruction>, calls: Vec<CallSite>) -> Result<Function, Diagnostic> {
        let spans = vec![None; code.len()];
        Function::new("caller", 0, 2, code, spans, calls)
    }

    #[test]
    fn call_metadata_is_validated_before_execution() {
        let call = CallSite {
            target: CallTarget::Direct(FunctionId(2)),
            dst: Some(Register(0)),
            arguments: CallArguments::Registers {
                start: Register(1),
                count: 1,
            },
        };
        let code = vec![
            Instruction::LoadInt {
                dst: Register(1),
                value: 3,
            },
            Instruction::Call { site: 0 },
            Instruction::Return { src: Register(0) },
        ];
        assert!(caller(code.clone(), vec![]).is_err());
        assert!(
            caller(
                code.clone(),
                vec![CallSite {
                    arguments: CallArguments::Registers {
                        start: Register(1),
                        count: u32::MAX
                    },
                    ..call
                }]
            )
            .is_err()
        );
        let function = caller(code, vec![call]).unwrap();
        assert!(
            Module::from_functions(vec![function])
                .unwrap_err()
                .message
                .contains("callee")
        );
    }

    #[test]
    fn argument_reads_require_initialization_but_parameters_start_initialized() {
        let call = CallSite {
            target: CallTarget::Direct(FunctionId(0)),
            dst: Some(Register(0)),
            arguments: CallArguments::Registers {
                start: Register(1),
                count: 1,
            },
        };
        assert!(
            caller(
                vec![
                    Instruction::Call { site: 0 },
                    Instruction::Return { src: Register(0) }
                ],
                vec![call]
            )
            .unwrap_err()
            .message
            .contains("operand is not initialized")
        );
        assert!(
            Function::new(
                "identity",
                1,
                2,
                vec![Instruction::Return { src: Register(1) }],
                vec![None],
                vec![]
            )
            .is_ok()
        );
    }
}
