//! Native classes declared with ordinary Rust state and methods.
//!
//! `#[class]` generates metadata, adapters, `install`, `install_with_state`
//! (explicitly replaces class state), and `with_state` (accesses an instance).
//! Methods use `#[tjs::method]`, properties use getter/setter, and associated
//! constants use `#[tjs::constant(name = "scriptName")]`.
//!
//! Fixed parameters use the established FromTjs rules. Opt into shared TJS
//! conversion with `#[tjs(coerce)]`; `#[tjs(default = expression)]` applies only
//! when omitted, never to an explicit void. Conversions run in declaration order.
//! Defaults are Rust expressions in the adapter's module scope. Use RestArgs
//! when the reference requires overloads, early result-discard behavior, or a
//! different validation order. Keep size limits and C-string truncation explicit.
//!
//! Independent module-level plugin functions expose a callable without a dummy class:
//! ```
//! use tjs_bind::{function, Array, NativeCallable};
//! #[function]
//! fn repeat(#[tjs(coerce)] n: i32, #[tjs(default = 1)] value: i64) -> Array<Vec<i64>> {
//!     Array(vec![value; n.clamp(0, 100) as usize])
//! }
//! fn main() {
//!     let _callable: NativeCallable = repeat::CALL;
//! }
//! // Export callable with krkr_engine::plugins::Exports or native_plugin!.
//! ```
//! `Utf16`, `Array` and `Dictionary` express owned results; containers retain
//! their normal script receiver. `flow` composes native steps on the same VM.
//! Captured state derives Trace (structs/enums); opaque Rust fields may use
//! `#[trace(skip = "reason this field has no TJS handles")]`. A reason documents
//! the author's GC audit; it does not prove that arbitrary foreign types are safe
//! to skip. Never skip Value/ObjId or a container retaining managed handles.
extern crate self as tjs_bind;

pub use tjs_core::native::argument;
pub use tjs_core::{
    FromTjs, Heap, Inspection, IntoTjs, NativeCallable, NativeClass, NativeContinuation, NativeCx,
    NativeError, NativeMethod, NativeProperty, NativeResult, NativeStep, NativeStorage,
    NativeThunk, NativeTryContinuation, ObjId, RestArgs, Trace, Value, WaitMode, WaitRequest,
};
pub use tjs_macros::{Trace, class, function};
mod convert;
pub use convert::{Array, Coerce, Dictionary, Utf16, coerce_argument};
pub mod flow;

pub mod array;
mod container_io;
mod containers;
pub mod date;
pub mod dictionary;
pub mod math;
pub mod random;
pub mod regexp;
pub mod structured;

pub fn install_builtins(heap: &mut Heap) -> NativeResult<()> {
    tjs_core::exception::install(heap)?;
    array::install(heap)?;
    dictionary::install(heap)?;
    regexp::install(heap)?;
    date::install(heap)?;
    let math = math::install(heap)?;
    let random = random::install(heap)?;
    heap.nest_class(math, "RandomGenerator", random)?;
    Ok(())
}
pub fn new_heap() -> Heap {
    let mut heap = Heap::new();
    install_builtins(&mut heap).expect("valid built-in class metadata");
    heap
}
