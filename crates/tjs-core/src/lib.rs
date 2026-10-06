//! The language machine: managed values/heap, source identity, instructions and VM.
//! Native leaves use an explicit contract; no parser, filesystem or renderer.

pub mod error;
pub mod heap;
pub mod ir;
pub mod member;
pub mod module;
pub mod native;
pub mod number;
pub mod source;
pub mod value;
mod verify;
pub mod vm;

pub use error::{Diagnostic, Phase, ScriptException, TraceFrame};
pub use heap::{
    CollectionPhase, CollectionStats, CollectionStep, Heap, HeapCounts, HeapError, ObjId,
    ObjRecord, ObjRef, ObjectKind, OctetId, RootId, StrId, SymbolId, WeakObjId,
};
pub use ir::{Instruction, Register, StoreMode};
pub use module::{
    ArgumentSource, CallArguments, CallSite, CallTarget, CatchHandler, CodeOrigin, Constant,
    ConstantValue, Function, FunctionId, FunctionKind, FunctionMember, Module, OriginEntry,
    WeakModule,
};
pub use native::{
    FromTjs, Inspection, IntoTjs, MemberFlags, NativeCallable, NativeClass, NativeContinuation,
    NativeCx, NativeError, NativeIndex, NativeMethod, NativeProperty, NativeResult, NativeStep,
    NativeStorage, NativeThunk, NativeTryContinuation, RestArgs, Trace, WaitMode, WaitRequest,
};
pub use source::{SourceFile, SourceId, SourceMap, Span, Utf16Offset};
pub use value::Value;
pub use vm::{
    Callback, CompileOutput, CompileRequest, RunBudget, ScriptSource, Vm, VmExit, VmLimits,
};
pub mod exception;
pub mod octet;
pub mod storage;
pub mod string;
