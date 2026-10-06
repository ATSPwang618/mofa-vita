//! Import the original TJS2100 container into the shared, validated KrIR.
//! The reader borrows file tables; lowering owns all data that survives import.
mod instruction;
mod lower;
mod read;

use tjs_core::{Diagnostic, Module};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_objects: usize,
    pub max_data_entries: usize,
    pub max_code_words: usize,
    pub max_output_instructions: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 12 * 1024 * 1024,
            max_objects: 16_384,
            max_data_entries: 262_144,
            max_code_words: 100_000,
            max_output_instructions: 100_000,
        }
    }
}

pub fn import(bytes: &[u8], name: &str) -> Result<Module, Diagnostic> {
    import_with_limits(bytes, name, &Limits::default())
}

pub fn import_with_limits(bytes: &[u8], name: &str, limits: &Limits) -> Result<Module, Diagnostic> {
    let file = read::read(bytes, limits)?;
    lower::lower(file, name, limits)
}
