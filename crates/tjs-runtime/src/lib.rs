//! Host-independent VM driving and dynamic compilation. The host owns active
//! VMs and supplies their roots together when collecting the shared heap.
use tjs_core::{
    CollectionStats, CollectionStep, Diagnostic, Heap, Module, RunBudget, ScriptException,
    SourceId, SourceMap, Value, Vm, VmExit, WeakModule,
};
use tjs_front::Preprocessor;
pub mod bytecode;
pub mod clock;
mod compilation_cache;
mod scheduler;
pub use scheduler::{
    CancelledContext, ContextId, Scheduler, SchedulerEvent, SchedulerLimits, WaitId,
};

#[derive(Clone, Debug)]
pub enum RuntimeExit {
    Finished(Value),
    Fault(Diagnostic),
    Thrown(ScriptException),
    Yielded,
    Waiting(tjs_core::WaitRequest),
}

pub struct Runtime {
    pub heap: Heap,
    pub sources: SourceMap,
    pub preprocessor: Preprocessor,
    compiled_sources: Vec<(SourceId, WeakModule)>,
    failed_source: Option<SourceId>,
    compilation_debt: usize,
    compilation_cache: compilation_cache::Cache,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::with_sources(SourceMap::new(), Preprocessor::default())
    }
}

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_sources(sources: SourceMap, preprocessor: Preprocessor) -> Self {
        Self {
            heap: tjs_bind::new_heap(),
            sources,
            preprocessor,
            compiled_sources: Vec::new(),
            failed_source: None,
            compilation_debt: 0,
            compilation_cache: Default::default(),
        }
    }

    /// A compile request is serviced once and then yields to the host. Compiled
    /// code executes in subsequent slices on the original VM stack and budget.
    /// Compilation itself is synchronous until the compiler gains work quotas.
    pub fn run_slice(&mut self, vm: &mut Vm, budget: RunBudget) -> RuntimeExit {
        match vm.run_slice(&mut self.heap, budget) {
            VmExit::Finished(value) => RuntimeExit::Finished(value),
            VmExit::Fault(error) => RuntimeExit::Fault(error),
            VmExit::Thrown(exception) => RuntimeExit::Thrown(exception),
            VmExit::Yielded => RuntimeExit::Yielded,
            VmExit::Waiting(request) => RuntimeExit::Waiting(request),
            VmExit::Inspecting(kind) => {
                let text = match kind {
                    tjs_core::Inspection::Dump => {
                        format!("heap: {:?}\n{}", self.heap.counts(), vm.dump_code())
                    }
                    tjs_core::Inspection::StackTrace { limit } => vm
                        .inspection_trace()
                        .iter()
                        .take(if limit == 0 {
                            usize::MAX
                        } else {
                            limit.max(1) as usize
                        })
                        .map(|frame| {
                            let location = frame
                                .span
                                .and_then(|span| {
                                    let file = self.sources.get(span.source())?;
                                    let (line, _) = file.line_column(span.start())?;
                                    Some(format!("{}({line})", file.name()))
                                })
                                .unwrap_or_else(|| "(unknown)".into());
                            format!("{location} [{}]", frame.function)
                        })
                        .collect::<Vec<_>>()
                        .join(" <-- "),
                };
                let value = Value::Str(
                    self.heap
                        .alloc_string(text.encode_utf16().collect::<Vec<_>>()),
                );
                match vm.resume_inspection(value) {
                    Ok(()) => RuntimeExit::Yielded,
                    Err(error) => RuntimeExit::Fault(error),
                }
            }
            VmExit::CompileRequest(request) => {
                let result = self.compile_request(vm, &request);

                match vm.resume_compile(&mut self.heap, result) {
                    Ok(()) => RuntimeExit::Yielded,
                    Err(error) => RuntimeExit::Fault(error),
                }
            }
        }
    }

    fn compile_request(
        &mut self,
        vm: &Vm,
        request: &tjs_core::CompileRequest,
    ) -> Result<Module, Diagnostic> {
        let tjs_core::ScriptSource::Text(text) = &request.source else {
            let tjs_core::ScriptSource::Bytecode(bytes) = &request.source else {
                unreachable!()
            };
            let (module, source) = bytecode::decode_with_preprocessor(
                bytes,
                &request.name,
                &mut self.sources,
                &mut self.preprocessor,
            )?;
            if let Some(source) = source {
                self.compiled_sources.push((source, module.downgrade()));
            }
            self.compilation_debt = self.compilation_debt.saturating_add(bytes.len());
            if let Some(output) = &request.output {
                let text = source
                    .and_then(|id| self.sources.get(id))
                    .map(|file| file.units());
                let bytes =
                    bytecode::reencode(bytes, &module, if output.debug { text } else { None })?;
                self.heap
                    .storage()
                    .and_then(|storage| storage.write_binary(&output.name, &[], &bytes))
                    .map_err(|e| vm.diagnostic(e.to_string()))?;
            }
            return Ok(module);
        };
        let revision = self.preprocessor.revision();
        let candidate = self.compilation_cache.fingerprint(request, revision);
        let mut admit = false;
        if let Some(fingerprint) = candidate {
            let (cached, retired) =
                self.compilation_cache
                    .get(request, revision, fingerprint, &self.sources);
            if retired {
                self.release_completed_sources();
            }
            if let Some(module) = cached {
                return Ok(module);
            }
            admit = self.compilation_cache.repeated(fingerprint);
        }
        // SourceId and the VM trace identify the caller. Embedding the parent
        // name here would grow source names quadratically under nested eval.
        let source = self
            .sources
            .add_utf16(&request.name, text.to_vec())
            .map_err(|error| vm.diagnostic(error.to_string()))?;
        self.sources
            .set_line_offset(source, request.line_offset)
            .expect("registered source");
        self.compilation_debt = self.compilation_debt.saturating_add(text.len() * 2);
        if request.output.is_some() {
            self.preprocessor.begin_trace();
        }
        let result = tjs_front::compile_storage(
            &self.sources,
            source,
            &mut self.preprocessor,
            request.expression,
            request.result_needed,
        );
        let trace = request
            .output
            .as_ref()
            .map(|_| self.preprocessor.end_trace());
        match result {
            Ok(module) => {
                self.compiled_sources.push((source, module.downgrade()));
                // Preprocessor side effects must execute on every compilation.
                // Reuse only when compiling left the revision unchanged.
                if admit
                    && self.preprocessor.revision() == revision
                    && self.compilation_cache.insert(
                        request,
                        revision,
                        candidate.unwrap(),
                        source,
                        &module,
                        &self.sources,
                    )
                {
                    self.release_completed_sources();
                }
                if let Some(output) = &request.output {
                    let bytes = bytecode::encode_preprocessed(
                        &module,
                        text,
                        output.debug,
                        request.expression,
                        request.result_needed,
                        trace.expect("recorded compile output"),
                    )?;
                    self.heap
                        .storage()
                        .and_then(|storage| storage.write_binary(&output.name, &[], &bytes))
                        .map_err(|e| vm.diagnostic(e.to_string()))?;
                }
                Ok(module)
            }
            Err(mut error) => {
                if let Some(previous) = self.failed_source.replace(source) {
                    self.sources.remove(previous);
                }
                if let Some(span) = error.span {
                    let file = self.sources.get(source).expect("registered eval source");
                    if let Some((line, column)) = file.line_column(span.start()) {
                        error.message =
                            format!("{} ({}:{line}:{column})", error.message, file.name());
                    }
                }
                Err(error)
            }
        }
    }

    /// Current retained compilation/source charge, including cache entries.
    pub fn compile_cache_bytes(&self) -> usize {
        self.compilation_cache.bytes()
    }

    /// Reclaim optional compiled code under host pressure, or disable caching
    /// with zero. Active VMs and escaping functions retain their own code.
    pub fn set_compile_cache_limit(&mut self, bytes: usize) {
        self.compilation_cache.set_limit(bytes);
        self.release_completed_sources();
    }

    /// Include source allocations in the host's existing collection policy.
    pub fn allocation_debt(&self) -> usize {
        self.heap
            .allocation_debt()
            .saturating_add(self.compilation_debt)
    }

    /// Collect only at a host boundary with roots from every active VM/context.
    /// Sources follow code lifetime; diagnostics keep the most recent failed
    /// compilation available until another compile error replaces it.
    pub fn collect(&mut self, roots: impl IntoIterator<Item = Value>) -> CollectionStats {
        let stats = self.heap.collect(roots);
        self.release_completed_sources();
        self.compilation_debt = 0;
        stats
    }

    /// Advance GC with every active VM's current roots. New compilation debt
    /// survives cycle completion, just like managed allocations during marking.
    pub fn collect_step(
        &mut self,
        roots: impl IntoIterator<Item = Value>,
        budget: usize,
    ) -> CollectionStep {
        if budget != 0 && !self.heap.is_collecting() {
            self.compilation_debt = 0;
        }
        let step = self.heap.collect_step(roots, budget);
        if step.completed.is_some() {
            self.release_completed_sources();
        }
        step
    }

    /// Host policy after a collection trigger: small heaps use the full fast
    /// path, larger heaps advance incrementally. A 4 MiB allocation backlog
    /// forces completion so a fast allocator cannot indefinitely outrun GC.
    /// Returns Some only when a cycle completes. Supply all current VM roots.
    pub fn collect_auto(
        &mut self,
        roots: impl IntoIterator<Item = Value>,
    ) -> Option<CollectionStats> {
        let counts = self.heap.counts();
        let entries = counts.objects + counts.strings + counts.octets + counts.symbols;
        let debt = self.allocation_debt();
        if (!self.heap.is_collecting() && entries <= 4096) || debt >= 4 * 1024 * 1024 {
            return Some(self.collect(roots));
        }
        let budget = 512 + (debt / 256).min(3584);
        self.collect_step(roots, budget).completed
    }

    fn release_completed_sources(&mut self) {
        self.compiled_sources.retain(|(source, module)| {
            if module.is_alive() {
                true
            } else {
                self.sources.remove(*source);
                false
            }
        });
    }
}

#[cfg(test)]
#[path = "../tests/internal/compilation_cache.rs"]
mod compilation_cache_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_policy_keeps_small_heaps_fast_and_finishes_allocation_backlogs() {
        let mut runtime = Runtime::new();
        assert!(runtime.collect_auto([]).is_some());
        for _ in 0..8000 {
            runtime.heap.alloc_object();
        }
        assert!(runtime.collect_auto([]).is_none());
        assert!(runtime.heap.is_collecting());
        let held = runtime.heap.alloc_string(vec![42; 3 * 1024 * 1024]);
        let stats = runtime
            .collect_auto([Value::Str(held)])
            .expect("pressure forces collection");
        assert!(!runtime.heap.is_collecting());
        assert!(stats.after.objects < 8000);
        assert_eq!(runtime.heap.string(held).unwrap().len(), 3 * 1024 * 1024);
    }

    #[test]
    fn repeated_evaluation_releases_completed_code_and_source_text() {
        let mut runtime = Runtime::new();
        let source = runtime
            .sources
            .add_utf8(
                "loop.tjs",
                r#"
            var total=0;
            for(var i=0;i<200;i++) { var n=(string(i))!; total+=n; }
            try { var n='function(){return "null.x"!;}()'!; } catch(e) { total++; }
            total;
        "#,
            )
            .unwrap();
        let module = tjs_front::compile(&runtime.sources, source).unwrap();
        let mut vm = Vm::new(&module);
        let mut completed = false;
        for _ in 0..20_000 {
            let exit = runtime.run_slice(&mut vm, RunBudget::new(1).unwrap());
            runtime.collect(vm.roots());
            assert!(
                runtime.compiled_sources.len() <= 2,
                "completed evals stay rooted"
            );
            match exit {
                RuntimeExit::Finished(value) => {
                    assert_eq!(value.as_integer(), Some(19_901));
                    completed = true;
                    break;
                }
                RuntimeExit::Yielded => {}
                exit => panic!("{exit:?}"),
            }
        }
        assert!(completed);
        assert!(runtime.compiled_sources.is_empty());
        assert!(runtime.sources.get(source).is_some());
    }
}
