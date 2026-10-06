//! Bounded immutable compilation results. Execution pools and runtime values
//! are recreated by the VM; a hit never skips evaluation or reuses its result.
use std::{
    collections::{VecDeque, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    mem::size_of,
    sync::Arc,
};
use tjs_core::{CompileRequest, Module, ScriptSource, SourceId, SourceMap};

pub(crate) const MAX_BYTES: usize = 512 * 1024;
const MAX_ENTRIES: usize = 128;
const MAX_UNITS: usize = 4096;

struct Entry {
    fingerprint: u64,
    revision: u64,
    text: Arc<[u16]>,
    name: String,
    line: i32,
    expression: bool,
    result: bool,
    source: SourceId,
    module: Module,
    bytes: usize,
}
pub(crate) struct Cache {
    entries: VecDeque<Entry>,
    bytes: usize,
    limit: usize,
    seen: [u64; MAX_ENTRIES],
    seen_len: usize,
    seen_next: usize,
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            limit: MAX_BYTES,
            seen: [0; MAX_ENTRIES],
            seen_len: 0,
            seen_next: 0,
        }
    }
}
impl Cache {
    pub fn bytes(&self) -> usize {
        self.bytes + self.entries.capacity() * size_of::<Entry>()
    }
    pub fn set_limit(&mut self, bytes: usize) {
        self.limit = bytes.min(MAX_BYTES);
        while self.bytes() > self.limit && self.evict() {}
        if self.bytes() > self.limit {
            self.entries.shrink_to_fit();
        }
        if self.limit == 0 {
            self.seen_len = 0;
            self.seen_next = 0;
        }
    }
    fn evict(&mut self) -> bool {
        if let Some(entry) = self.entries.pop_front() {
            self.bytes -= entry.bytes;
            true
        } else {
            false
        }
    }
    pub fn fingerprint(&self, request: &CompileRequest, revision: u64) -> Option<u64> {
        let ScriptSource::Text(text) = &request.source else {
            return None;
        };
        if self.limit <= MAX_ENTRIES * size_of::<Entry>()
            || request.output.is_some()
            || text.len() > MAX_UNITS
            || request.name.len() > 1024
        {
            return None;
        }
        let mut hash = DefaultHasher::new();
        (
            revision,
            text.as_ref(),
            &request.name,
            request.line_offset,
            request.expression,
            request.result_needed,
        )
            .hash(&mut hash);
        Some(hash.finish())
    }
    pub fn get(
        &mut self,
        request: &CompileRequest,
        revision: u64,
        fingerprint: u64,
        sources: &SourceMap,
    ) -> (Option<Module>, bool) {
        let ScriptSource::Text(text) = &request.source else {
            return (None, false);
        };
        let Some(i) = self.entries.iter().rposition(|e| {
            e.fingerprint == fingerprint
                && e.revision == revision
                && e.text == *text
                && e.name == request.name
                && e.line == request.line_offset
                && e.expression == request.expression
                && e.result == request.result_needed
        }) else {
            return (None, false);
        };
        let entry = self.entries.remove(i).unwrap();
        // SourceMap is public: explicit removal or a changed diagnostic offset
        // must not leave a reusable module with a stale source location.
        if !sources.get(entry.source).is_some_and(|f| {
            f.name() == entry.name
                && f.units() == entry.text.as_ref()
                && f.line_offset() == entry.line
        }) {
            self.bytes -= entry.bytes;
            return (None, true);
        }
        let module = entry.module.clone();
        self.entries.push_back(entry);
        (Some(module), false)
    }
    pub fn repeated(&mut self, fingerprint: u64) -> bool {
        // One-off generated strings (notably deserializers and numeric eval)
        // must retain neither code nor source text. Hash collisions only alter
        // admission; every actual cache hit compares the complete key above.
        if self.seen[..self.seen_len].contains(&fingerprint) {
            return true;
        }
        self.seen[self.seen_next] = fingerprint;
        self.seen_next = (self.seen_next + 1) % MAX_ENTRIES;
        self.seen_len = (self.seen_len + 1).min(MAX_ENTRIES);
        false
    }
    pub fn insert(
        &mut self,
        request: &CompileRequest,
        revision: u64,
        fingerprint: u64,
        source: SourceId,
        module: &Module,
        sources: &SourceMap,
    ) -> bool {
        let ScriptSource::Text(text) = &request.source else {
            return false;
        };
        let Some(file) = sources.get(source) else {
            return false;
        };
        let mut entry = Entry {
            fingerprint,
            revision,
            text: text.clone(),
            name: request.name.clone(),
            line: request.line_offset,
            expression: request.expression,
            result: request.result_needed,
            source,
            module: module.clone(),
            bytes: 0,
        };
        entry.bytes = module
            .retained_bytes()
            .saturating_add(file.retained_bytes())
            .saturating_add(text.len() * 2 + 2 * size_of::<usize>())
            .saturating_add(entry.name.capacity());
        if entry.bytes.saturating_add(MAX_ENTRIES * size_of::<Entry>()) > self.limit {
            return false;
        }
        self.entries
            .reserve_exact(MAX_ENTRIES.saturating_sub(self.entries.len()));
        let mut retired = false;
        while self.bytes().saturating_add(entry.bytes) > self.limit
            || self.entries.len() >= MAX_ENTRIES
        {
            if !self.evict() {
                return retired;
            }
            retired = true;
        }
        self.bytes += entry.bytes;
        self.entries.push_back(entry);
        retired
    }
}
