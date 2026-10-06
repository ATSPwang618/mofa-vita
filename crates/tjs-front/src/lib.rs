//! UTF-16 tokens → arena AST → register code. No script execution happens here.

pub mod ast;
pub mod bytecode;
pub mod compiler;
pub mod lexer;
pub mod parser;
pub mod preprocessor;

pub use preprocessor::Preprocessor;

use tjs_core::{Diagnostic, Module, SourceId, SourceMap};

pub fn compile(sources: &SourceMap, source: SourceId) -> Result<Module, Diagnostic> {
    compile_with_preprocessor(sources, source, &mut Preprocessor::default())
}

/// Reuse definitions for successive scripts in the same engine/session.
/// Like TJS, directives take effect during lexing, even if compilation later fails.
pub fn compile_with_preprocessor(
    sources: &SourceMap,
    source: SourceId,
    preprocessor: &mut Preprocessor,
) -> Result<Module, Diagnostic> {
    let (program, _) = parser::parse_source_with_preprocessor(sources, source, preprocessor)?;
    compiler::compile(sources, &program)
}

/// Compile the postfix evaluation operator's two modes. Value mode prepends a
/// return; discarded mode executes the entire script. Neither captures locals.
pub fn compile_eval(
    sources: &SourceMap,
    source: SourceId,
    preprocessor: &mut Preprocessor,
    result_needed: bool,
) -> Result<Module, Diagnostic> {
    let (program, _) = parser::parse_eval(sources, source, preprocessor, result_needed)?;
    compiler::compile_discard(sources, &program)
}

/// Compile a storage unit, independently selecting expression parsing and result
/// retention. Script mode returns only explicit return statements.
pub fn compile_storage(
    sources: &SourceMap,
    source: SourceId,
    preprocessor: &mut Preprocessor,
    expression: bool,
    result_needed: bool,
) -> Result<Module, Diagnostic> {
    if expression {
        compile_eval(sources, source, preprocessor, result_needed)
    } else {
        let (program, _) = parser::parse_source_with_preprocessor(sources, source, preprocessor)?;
        compiler::compile_discard(sources, &program)
    }
}
