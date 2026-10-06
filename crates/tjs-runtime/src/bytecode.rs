//! Versioned krkr-rs bytecode. No heap handles or process-local source IDs are
//! serialized. Decoding rebuilds functions through the ordinary verifier.
use serde::{Deserialize, Serialize};
use tjs_core::{
    CallSite, CatchHandler, CodeOrigin, Constant, Diagnostic, Function, FunctionKind,
    FunctionMember, Instruction, Module, Phase, SourceId, SourceMap,
};

const MAGIC: &[u8; 8] = b"KRRSBC\x03\0";
const PREPROCESSED_MAGIC: &[u8; 8] = b"KRRSBC\x04\0";

// This version covers the enum ordering of Instruction and its metadata too.
// Changes to that schema require a version bump, even if Rust layout is stable.

pub fn is_bytecode(bytes: &[u8]) -> bool {
    bytes.starts_with(b"KRRSBC") || bytes.starts_with(b"TJS2")
}

#[derive(Serialize, Deserialize)]
struct Archive {
    debug: Option<Vec<u16>>,
    functions: Vec<Code>,
    constants: Vec<Constant>,
}
#[derive(Serialize, Deserialize)]
struct ArchiveWithOrigins {
    archive: Archive,
    origins: Vec<Option<CodeOrigin>>,
}

#[derive(Serialize, Deserialize)]
struct PreprocessedArchive {
    module: Vec<u8>,
    // Dynamic conditional compilation must retain its original input. This
    // source is only compiled when incoming definitions differ from the build.
    fallback: Option<Vec<u16>>,
    inputs: Vec<(Vec<u16>, i32)>,
    outputs: Vec<(Vec<u16>, i32)>,
    expression: bool,
    result_needed: bool,
}

/// Compile a storage unit for offline packaging. Record only definitions that
/// this unit actually reads, so unrelated session definitions do not invalidate
/// it. Assignments are replayed only after the cached module passes verification.
pub fn compile_preprocessed(
    source: &[u16],
    name: &str,
    preprocessor: &mut tjs_front::Preprocessor,
    debug: bool,
) -> Result<Vec<u8>, Diagnostic> {
    compile_preprocessed_storage(source, name, preprocessor, debug, false, false)
}

pub fn compile_preprocessed_storage(
    text: &[u16],
    name: &str,
    preprocessor: &mut tjs_front::Preprocessor,
    debug: bool,
    expression: bool,
    result_needed: bool,
) -> Result<Vec<u8>, Diagnostic> {
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf16(name, text.to_vec())
        .map_err(|e| error(e.to_string()))?;
    preprocessor.begin_trace();
    let result =
        tjs_front::compile_storage(&sources, source, preprocessor, expression, result_needed);
    let trace = preprocessor.end_trace();
    let module = result?;
    encode_preprocessed(&module, text, debug, expression, result_needed, trace)
}

pub fn encode_preprocessed(
    module: &Module,
    text: &[u16],
    debug: bool,
    expression: bool,
    result_needed: bool,
    trace: tjs_front::preprocessor::PreprocessorTrace,
) -> Result<Vec<u8>, Diagnostic> {
    let module = encode(module, debug.then_some(text))?;
    if trace.inputs.is_empty() && trace.outputs.is_empty() {
        return Ok(module);
    }
    let archive = PreprocessedArchive {
        module,
        fallback: (!trace.inputs.is_empty()).then(|| text.to_vec()),
        inputs: trace.inputs,
        outputs: trace.outputs,
        expression,
        result_needed,
    };
    let mut bytes = PREPROCESSED_MAGIC.to_vec();
    bytes.extend(postcard::to_allocvec(&archive).map_err(|e| error(e.to_string()))?);
    Ok(bytes)
}

/// Change debug storage without dropping a loaded unit's conditional contract.
pub fn reencode(
    original: &[u8],
    module: &Module,
    source: Option<&[u16]>,
) -> Result<Vec<u8>, Diagnostic> {
    let Some(payload) = original.strip_prefix(PREPROCESSED_MAGIC) else {
        return encode(module, source);
    };
    let (mut archive, trailing) = postcard::take_from_bytes::<PreprocessedArchive>(payload)
        .map_err(|e| error(e.to_string()))?;
    if !trailing.is_empty() {
        return Err(error("trailing bytes after preprocessed bytecode module"));
    }
    // The incoming definitions may have forced a different branch at load.
    // Preserve the original compiled branch and its matching dependency/delta;
    // updating it alone would make this archive's conditional contract false.
    let mut original_sources = SourceMap::new();
    let (original_module, original_source) =
        decode(&archive.module, "<cached>", &mut original_sources)?;
    let debug_source = if source.is_some() {
        original_source
            .and_then(|id| original_sources.get(id))
            .map(|file| file.units())
            .or(archive.fallback.as_deref())
    } else {
        None
    };
    archive.module = encode(&original_module, debug_source)?;
    let mut bytes = PREPROCESSED_MAGIC.to_vec();
    bytes.extend(postcard::to_allocvec(&archive).map_err(|e| error(e.to_string()))?);
    Ok(bytes)
}

/// Load an offline storage unit using the current session's conditional state.
/// A changed guard or DEBUG branch compiles the retained source instead of
/// freezing the branch selected on the PC. Legacy modules remain supported.
pub fn decode_with_preprocessor(
    bytes: &[u8],
    name: &str,
    sources: &mut SourceMap,
    preprocessor: &mut tjs_front::Preprocessor,
) -> Result<(Module, Option<SourceId>), Diagnostic> {
    let Some(payload) = bytes.strip_prefix(PREPROCESSED_MAGIC) else {
        return decode(bytes, name, sources);
    };
    let (archive, trailing) = postcard::take_from_bytes::<PreprocessedArchive>(payload)
        .map_err(|e| error(e.to_string()))?;
    if !trailing.is_empty() {
        return Err(error("trailing bytes after preprocessed bytecode module"));
    }
    let trace = tjs_front::preprocessor::PreprocessorTrace {
        inputs: archive.inputs,
        outputs: archive.outputs,
    };
    if preprocessor.matches_trace(&trace) {
        let result = decode(&archive.module, name, sources)?;
        preprocessor.apply_trace(&trace);
        return Ok(result);
    }
    let text = archive
        .fallback
        .ok_or_else(|| error("preprocessed bytecode has no fallback source"))?;
    let source = sources
        .add_utf16(name, text)
        .map_err(|e| error(e.to_string()))?;
    match tjs_front::compile_storage(
        sources,
        source,
        preprocessor,
        archive.expression,
        archive.result_needed,
    ) {
        Ok(module) => Ok((module, Some(source))),
        Err(mut diagnostic) => {
            if let Some(span) = diagnostic.span
                && let Some(file) = sources.get(source)
                && let Some((line, column)) = file.line_column(span.start())
            {
                diagnostic.message = format!("{} ({name}:{line}:{column})", diagnostic.message);
            }
            diagnostic.span = None;
            sources.remove(source);
            Err(diagnostic)
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Code {
    name: String,
    parameters: u32,
    registers: u32,
    instructions: Vec<Instruction>,
    spans: Vec<Option<(u32, u32)>>,
    calls: Vec<CallSite>,
    handlers: Vec<CatchHandler>,
    members: Vec<FunctionMember>,
    kind: FunctionKind,
}
fn error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Phase::Verify, None, message)
}

pub fn encode(module: &Module, source: Option<&[u16]>) -> Result<Vec<u8>, Diagnostic> {
    let archive = Archive {
        debug: source.map(<[u16]>::to_vec),
        constants: module.constants().to_vec(),
        functions: module
            .functions()
            .iter()
            .map(|function| Code {
                name: function.name().into(),
                parameters: function.parameter_count(),
                registers: function.register_count(),
                instructions: function.instructions().to_vec(),
                spans: if source.is_some() {
                    (0..function.instructions().len())
                        .map(|pc| {
                            function
                                .span_at(pc)
                                .map(|span| (span.start().get(), span.end().get()))
                        })
                        .collect()
                } else {
                    Vec::new()
                },
                calls: function.calls().to_vec(),
                handlers: function.handlers().to_vec(),
                members: function.members().to_vec(),
                kind: function.kind().clone(),
            })
            .collect(),
    };
    let archive = ArchiveWithOrigins {
        archive,
        origins: module
            .functions()
            .iter()
            .map(|function| function.origin().cloned())
            .collect(),
    };
    let payload = postcard::to_allocvec(&archive).map_err(|e| error(e.to_string()))?;
    let mut bytes = Vec::with_capacity(MAGIC.len() + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend(payload);
    Ok(bytes)
}

pub fn decode(
    bytes: &[u8],
    name: &str,
    sources: &mut SourceMap,
) -> Result<(Module, Option<SourceId>), Diagnostic> {
    if bytes.starts_with(PREPROCESSED_MAGIC) {
        return Err(error("preprocessed bytecode requires session definitions"));
    }
    if bytes.starts_with(b"TJS2") {
        return tjs_front::bytecode::import(bytes, name).map(|module| (module, None));
    }
    let payload = bytes
        .strip_prefix(MAGIC)
        .ok_or_else(|| error("unsupported krkr-rs bytecode version"))?;
    let (encoded, trailing) = postcard::take_from_bytes::<ArchiveWithOrigins>(payload)
        .map_err(|e| error(e.to_string()))?;
    let ArchiveWithOrigins { archive, origins } = encoded;
    if origins.len() != archive.functions.len() {
        return Err(error(
            "original location table has the wrong function count",
        ));
    }
    if !trailing.is_empty() {
        return Err(error("trailing bytes after bytecode module"));
    }
    let source = archive
        .debug
        .map(|text| {
            sources
                .add_utf16(name, text)
                .map_err(|e| error(e.to_string()))
        })
        .transpose()?;
    let result = (|| {
        let functions = archive
            .functions
            .into_iter()
            .zip(origins)
            .map(|(code, origin)| {
                let spans = if let Some(source) = source {
                    code.spans
                        .into_iter()
                        .map(|span| {
                            span.map(|(start, end)| {
                                sources
                                    .span(source, start as usize..end as usize)
                                    .map_err(|e| error(e.to_string()))
                            })
                            .transpose()
                        })
                        .collect::<Result<Vec<_>, _>>()?
                } else {
                    if !code.spans.is_empty() {
                        return Err(error("source spans without debug source"));
                    }
                    vec![None; code.instructions.len()]
                };
                let function = Function::with_handlers(
                    code.name,
                    code.parameters,
                    code.registers,
                    code.instructions,
                    spans,
                    code.calls,
                    code.handlers,
                )?
                .with_members(code.members)
                .with_kind(code.kind);
                if let Some(origin) = origin {
                    function.with_origin(origin)
                } else {
                    Ok(function)
                }
            })
            .collect::<Result<Vec<_>, Diagnostic>>()?;
        Module::with_constants(functions, archive.constants)
    })();
    match result {
        Ok(module) => Ok((module, source)),
        Err(mut error) => {
            error.span = None;
            if let Some(source) = source {
                sources.remove(source);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(module: &Module) -> i64 {
        let mut heap = tjs_bind::new_heap();
        let mut vm = tjs_core::Vm::new(module);
        match vm.run_slice(&mut heap, tjs_core::RunBudget::new(10000).unwrap()) {
            tjs_core::VmExit::Finished(tjs_core::Value::Int(value)) => value,
            _ => panic!("fixture did not finish"),
        }
    }

    #[test]
    fn offline_units_replay_assignments_and_honor_changed_session_branches() {
        let text: Vec<_> = "@if(__loaded==0)\n@set(__loaded=1)\n@if(DEBUG)\nreturn 42;\n@endif\n@if(DEBUG==0)\nreturn 7;\n@endif\n@endif\nreturn 99;".encode_utf16().collect();
        let mut build = tjs_front::Preprocessor::default();
        let bytes = compile_preprocessed(&text, "unit", &mut build, false).unwrap();
        assert!(bytes.starts_with(PREPROCESSED_MAGIC));
        assert_eq!(build.get("__loaded"), 1);
        let mut session = tjs_front::Preprocessor::default();
        session.set("unrelated", 123);
        let mut sources = SourceMap::new();
        let (module, source) =
            decode_with_preprocessor(&bytes, "unit", &mut sources, &mut session).unwrap();
        assert!(
            source.is_none(),
            "unrelated flags must not force compilation"
        );
        assert_eq!(run(&module), 7);
        assert_eq!(session.get("__loaded"), 1);
        let (module, source) =
            decode_with_preprocessor(&bytes, "unit", &mut sources, &mut session).unwrap();
        assert!(
            source.is_some(),
            "the changed load guard must select a new branch"
        );
        assert_eq!(run(&module), 99);
        session.set("__loaded", 0);
        session.set("DEBUG", 1);
        let (module, _) =
            decode_with_preprocessor(&bytes, "unit", &mut sources, &mut session).unwrap();
        assert_eq!(run(&module), 42);
        assert_eq!(session.get("__loaded"), 1);
    }

    #[test]
    fn rewritten_bytecode_keeps_guard_contract_and_invalid_code_cannot_set_flags() {
        let text: Vec<_> = "@if(FLAG==0)\n@set(FLAG=1)\nreturn 7;\n@endif\nreturn 99;"
            .encode_utf16()
            .collect();
        let bytes =
            compile_preprocessed(&text, "unit", &mut tjs_front::Preprocessor::default(), true)
                .unwrap();
        let mut sources = SourceMap::new();
        let mut different = tjs_front::Preprocessor::default();
        different.set("FLAG", 1);
        let (fallback, _) =
            decode_with_preprocessor(&bytes, "unit", &mut sources, &mut different).unwrap();
        assert_eq!(run(&fallback), 99);
        let rewritten = reencode(&bytes, &fallback, None).unwrap();
        let mut fresh = tjs_front::Preprocessor::default();
        let (module, _) =
            decode_with_preprocessor(&rewritten, "unit", &mut sources, &mut fresh).unwrap();
        assert_eq!(run(&module), 7);
        assert_eq!(fresh.get("FLAG"), 1);
        let (mut archive, _) =
            postcard::take_from_bytes::<PreprocessedArchive>(&bytes[PREPROCESSED_MAGIC.len()..])
                .unwrap();
        archive.module.truncate(8);
        let mut invalid = PREPROCESSED_MAGIC.to_vec();
        invalid.extend(postcard::to_allocvec(&archive).unwrap());
        let mut fresh = tjs_front::Preprocessor::default();
        assert!(decode_with_preprocessor(&invalid, "unit", &mut sources, &mut fresh).is_err());
        assert_eq!(fresh.get("FLAG"), 0);
    }
    #[test]
    fn current_cache_preserves_discarded_native_calls() {
        use tjs_core::{RunBudget, Value, Vm, VmExit};
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("discard", "Math.abs(null); 7;").unwrap();
        let module = tjs_front::compile(&sources, source).unwrap();
        assert!(module.functions()[0].calls()[0].dst.is_none());
        let encoded = encode(&module, None).unwrap();
        assert_eq!(&encoded[..8], MAGIC);
        for version in [1, 2] {
            let mut stale = encoded.clone();
            stale[6] = version;
            assert!(
                decode(&stale, "stale-cache", &mut sources)
                    .unwrap_err()
                    .message
                    .contains("unsupported krkr-rs bytecode version")
            );
        }
        let (restored, _) = decode(&encoded, "discard-cache", &mut sources).unwrap();
        assert!(restored.functions()[0].calls()[0].dst.is_none());
        let mut vm = Vm::new(&restored);
        let mut heap = tjs_bind::new_heap();
        assert!(matches!(
            vm.run_slice(&mut heap, RunBudget::new(1000).unwrap()),
            VmExit::Finished(Value::Int(7))
        ));
    }

    #[test]
    fn malformed_code_is_verified_and_debug_sources_are_not_leaked() {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("input", "42;").unwrap();
        let module = tjs_front::compile(&sources, source).unwrap();
        let bytes = encode(&module, Some(&[52, 50, 59])).unwrap();
        let (mut archive, _) =
            postcard::take_from_bytes::<ArchiveWithOrigins>(&bytes[MAGIC.len()..]).unwrap();
        archive.archive.functions[0].instructions[0] = Instruction::Jump { target: u32::MAX };
        let mut invalid = MAGIC.to_vec();
        invalid.extend(postcard::to_allocvec(&archive).unwrap());
        assert!(
            decode(&invalid, "broken", &mut sources)
                .unwrap_err()
                .message
                .contains("jump target")
        );
        assert!(decode(&bytes[..bytes.len() - 1], "truncated", &mut sources).is_err());
        let plain = encode(&module, None).unwrap();
        let (restored, source) = decode(&plain, "stripped", &mut sources).unwrap();
        assert!(source.is_none());
        assert!(
            restored
                .functions()
                .iter()
                .all(|f| (0..f.instructions().len()).all(|pc| f.span_at(pc).is_none()))
        );
    }
}
