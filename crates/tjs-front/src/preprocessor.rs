//! TJS conditional compilation has a separate, eager signed 32-bit language.
//! Definitions belong to the host/session, not the script's runtime globals.
mod expression;

use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
};

static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct Preprocessor {
    values: HashMap<Vec<u16>, i32>,
    revision: u64,
    recording: Option<Recording>,
}

#[derive(Clone, Debug, Default)]
struct Recording {
    reads: RefCell<BTreeMap<Vec<u16>, i32>>,
    writes: HashSet<Vec<u16>>,
}

/// Definitions read from the incoming session and assignments made by a unit.
#[derive(Clone, Debug, Default)]
pub struct PreprocessorTrace {
    pub inputs: Vec<(Vec<u16>, i32)>,
    pub outputs: Vec<(Vec<u16>, i32)>,
}

impl Default for Preprocessor {
    fn default() -> Self {
        let mut result = Self {
            values: HashMap::new(),
            revision: 0,
            recording: None,
        };
        // The selected reference language profile is TJS 2.4.28. Hosts may
        // override it; this is not a claim of complete engine compatibility.
        result.set("version", 0x0204_001c);
        result
    }
}

impl Preprocessor {
    pub fn begin_trace(&mut self) {
        self.recording = Some(Recording::default());
    }

    pub fn end_trace(&mut self) -> PreprocessorTrace {
        let Some(recording) = self.recording.take() else {
            return PreprocessorTrace::default();
        };
        let mut outputs: Vec<_> = recording
            .writes
            .into_iter()
            .map(|name| {
                let value = self.values[&name];
                (name, value)
            })
            .collect();
        outputs.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        PreprocessorTrace {
            inputs: recording.reads.into_inner().into_iter().collect(),
            outputs,
        }
    }

    pub fn matches_trace(&self, trace: &PreprocessorTrace) -> bool {
        trace
            .inputs
            .iter()
            .all(|(name, expected)| self.values.get(name).copied().unwrap_or(0) == *expected)
    }

    pub fn apply_trace(&mut self, trace: &PreprocessorTrace) {
        for (name, value) in &trace.outputs {
            self.assign(name.clone(), *value);
        }
    }
    /// Supply a host default without replacing explicit session definitions.
    pub fn set_default(&mut self, name: &str, value: i32) {
        let name = name.encode_utf16().collect::<Vec<_>>();
        if !self.values.contains_key(&name) {
            self.assign(name, value);
        }
    }

    pub fn set(&mut self, name: &str, value: i32) {
        self.assign(name.encode_utf16().collect(), value);
    }

    /// Opaque identity of the current definitions. Clones share an identity
    /// until a write; replacing a preprocessor cannot reuse an old cache key.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn assign(&mut self, name: Vec<u16>, value: i32) {
        if let Some(recording) = &mut self.recording {
            recording.writes.insert(name.clone());
        }
        if self.values.get(&name) != Some(&value) {
            self.values.insert(name, value);
            self.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn get(&self, name: &str) -> i32 {
        self.value(&name.encode_utf16().collect::<Vec<_>>())
    }

    fn value(&self, name: &[u16]) -> i32 {
        let value = self.values.get(name).copied().unwrap_or(0);
        if let Some(recording) = &self.recording
            && !recording.writes.contains(name)
        {
            recording
                .reads
                .borrow_mut()
                .entry(name.to_vec())
                .or_insert(value);
        }
        value
    }

    pub(crate) fn evaluate(&mut self, units: &[u16]) -> Result<i32, expression::Error> {
        expression::evaluate(self, units)
    }
}
