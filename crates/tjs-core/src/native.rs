//! Native leaves and owned continuations. Native code never reenters the VM.
//! The binding crate supplies adapters; core owns only their execution contract.
use std::any::Any;
mod trace_containers;
mod trace_step;

use crate::{Heap, HeapError, ObjId, StrId, Value};

pub type NativeResult<T> = Result<T, NativeError>;
pub type NativeThunk = fn(&mut NativeCx<'_>, &[Value]) -> NativeResult<Value>;

#[derive(Clone, Copy)]
pub enum NativeCallable {
    Leaf(NativeThunk),
    /// An empty finalize body. GC can reclaim it without scheduling a callback;
    /// explicit calls still use the thunk's ordinary receiver validation.
    EmptyFinalizer(NativeThunk),
    Resumable(fn(&mut NativeCx<'_>, &[Value]) -> NativeResult<NativeStep>),
}

/// Owned work may outlive a native borrow. The VM executes callbacks in ordinary
/// frames, with the same budgets, exception handling and GC roots as script calls.
pub trait NativeContinuation: Trace {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, result: Value) -> NativeResult<NativeStep>;
}
/// A native catch boundary. Explicit throws retain their original value;
/// runtime failures carry core Exception data without invoking a script's
/// replacement Exception constructor, like a native C++ exception handler.
pub trait NativeTryContinuation: Trace {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitMode {
    Internal,
    Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitRequest {
    pub mode: WaitMode,
    /// Opaque operation identity owned by the host. The scheduler adds a
    /// generational completion ID so a late result cannot resume a reused task.
    pub token: u64,
}

#[derive(Clone, Copy, Debug)]
pub enum Inspection {
    StackTrace { limit: i32 },
    Dump,
}

/// TJS property-dispatch flags. Hosts translate public numeric constants here;
/// accessors, missing handlers and class lookup remain owned by the VM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemberFlags {
    pub ensure: bool,
    pub must_exist: bool,
    pub ignore_property: bool,
    pub hidden: bool,
    pub class_only: bool,
}

pub enum NativeStep {
    GetProperty {
        object: Value,
        key: Value,
        flags: MemberFlags,
        continuation: Box<dyn NativeContinuation>,
    },
    SetProperty {
        object: Value,
        key: Value,
        value: Value,
        flags: MemberFlags,
        continuation: Box<dyn NativeContinuation>,
    },
    /// Continue owned native work under the next VM budget unit. This does not
    /// open an event wait or execute an unrelated script context.
    Continue(Box<dyn NativeContinuation>),
    Inspect {
        kind: Inspection,
        continuation: Box<dyn NativeContinuation>,
    },
    Evaluate {
        request: crate::CompileRequest,
        /// None selects the VM global, not the caller's this.
        context: Option<ObjId>,
    },
    Wait {
        request: WaitRequest,
        continuation: Box<dyn NativeContinuation>,
    },
    Return(Value),
    /// Re-raise a caught script value through the ordinary exception machinery.
    Throw(Value),
    /// Execute owned work with a native catch boundary, including its callbacks.
    Try {
        task: Box<dyn NativeContinuation>,
        continuation: Box<dyn NativeTryContinuation>,
    },
    /// Invalidate through ordinary script finalization, including suspension.
    Invalidate {
        object: Value,
        continuation: Box<dyn NativeContinuation>,
    },
    /// Invalidate one object, resuming this continuation on success or error.
    /// Cancellation still drops the work; it is never converted into success.
    TryInvalidate {
        object: Value,
        continuation: Box<dyn NativeTryContinuation>,
    },
    /// Read through VM member dispatch, including script getters and class bases.
    Get {
        object: Value,
        key: Value,
        continuation: Box<dyn NativeContinuation>,
    },
    /// As Get, but a missing member returns void. Getter errors still propagate.
    GetOptional {
        object: Value,
        key: Value,
        continuation: Box<dyn NativeContinuation>,
    },
    /// Read through ordinary dispatch, optionally bypassing property getters.
    /// A failed structural dispatch returns the supplied fallback; an existing
    /// void remains void. Class lookup and missing handlers still run, and
    /// exceptions from accessor/handler bodies propagate through the same VM.
    GetOr {
        object: Value,
        key: Value,
        raw: bool,
        fallback: Value,
        continuation: Box<dyn NativeContinuation>,
    },
    /// Require an existing member even on Dictionary. Accessors and missing
    /// handlers still run; their exceptions are not converted to void.
    GetRequired {
        object: Value,
        key: Value,
        continuation: Box<dyn NativeContinuation>,
    },
    /// MUSTEXIST read with a distinct failed-dispatch fallback. Accessors still
    /// execute; a successfully read void is not treated as a missing member.
    GetRequiredOr {
        object: Value,
        key: Value,
        fallback: Value,
        continuation: Box<dyn NativeContinuation>,
    },
    /// Assign through ordinary member dispatch, including script setters.
    Set {
        object: Value,
        key: Value,
        value: Value,
        continuation: Box<dyn NativeContinuation>,
    },
    /// Assign without MEMBERENSURE. Ignore structural dispatch failures, but
    /// preserve errors raised while a setter or missing handler executes.
    SetExisting {
        object: Value,
        key: Value,
        value: Value,
        continuation: Box<dyn NativeContinuation>,
    },
    /// Copy a raw member slot. Bypasses property setters, but retains missing
    /// handlers and the member's static flag, through the same VM dispatch.
    /// Like the reference container callbacks, ignore an invalid-target status;
    /// exceptions thrown by missing handlers still propagate.
    CopyMember {
        object: Value,
        key: Value,
        value: Value,
        class_only: bool,
        continuation: Box<dyn NativeContinuation>,
    },
    /// Construct a script or native class with the same semantics as new.
    Construct {
        class: Value,
        arguments: Vec<Value>,
        continuation: Box<dyn NativeContinuation>,
    },
    /// Invoke a named member using FuncCall semantics, preserving the receiver.
    CallMember {
        object: Value,
        key: Value,
        arguments: Vec<Value>,
        continuation: Box<dyn NativeContinuation>,
    },
    /// Native FuncCall whose caller ignores failure statuses. Missing members,
    /// invalid receivers and non-callable slots return void; exceptions from
    /// getters, missing handlers and invoked functions still propagate.
    CallMemberOr {
        object: Value,
        key: Value,
        arguments: Vec<Value>,
        result_needed: bool,
        continuation: Box<dyn NativeContinuation>,
    },
    Call {
        function: Value,
        arguments: Vec<Value>,
        continuation: Box<dyn NativeContinuation>,
    },
    /// Invoke without a result slot, then resume with void. Some native APIs
    /// observably skip work when their caller does not request a result.
    CallDiscard {
        function: Value,
        arguments: Vec<Value>,
        continuation: Box<dyn NativeContinuation>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    #[error(transparent)]
    Heap(#[from] HeapError),
    #[error(transparent)]
    Arithmetic(#[from] crate::value::ArithmeticError),
    #[error("native method requires a compatible this object")]
    This,
    #[error("native argument {0} is missing")]
    Missing(usize),
    #[error("native argument requires {0}")]
    Type(&'static str),
    #[error("{0}")]
    Message(&'static str),
    #[error("{0}")]
    Detail(String),
}

/// Report managed edges only. Tracing never executes script or mutates the heap.
pub trait Trace {
    fn trace(&self, visit: &mut dyn FnMut(Value));
}

impl Trace for Value {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(*self);
    }
}
impl Trace for ObjId {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(Value::Obj((*self).into()));
    }
}
impl Trace for StrId {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(Value::Str(*self));
    }
}
impl<T: Trace> Trace for Vec<T> {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for value in self {
            value.trace(visit);
        }
    }
}
impl<T: Trace> Trace for Option<T> {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(value) = self {
            value.trace(visit);
        }
    }
}
impl<T: Trace + ?Sized> Trace for Box<T> {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        (**self).trace(visit);
    }
}
macro_rules! no_edges {
    ($($ty:ty),*) => { $(impl Trace for $ty {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    })* };
}
no_edges!(
    (),
    bool,
    i8,
    i16,
    i32,
    i64,
    isize,
    u8,
    u16,
    u32,
    u64,
    usize,
    f32,
    f64,
    String
);

pub(crate) trait NativeState: Any {
    fn trace(&self, visit: &mut dyn FnMut(Value));
    fn any(&self) -> &dyn Any;
    fn any_mut(&mut self) -> &mut dyn Any;
}
impl<T: Trace + Any> NativeState for T {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        Trace::trace(self, visit);
    }
    fn any(&self) -> &dyn Any {
        self
    }
    fn any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeStorage {
    Object,
    Array,
    Dictionary,
}

pub struct NativeMethod {
    pub hidden: bool,
    pub name: &'static str,
    pub doc: &'static str,
    /// Static TJS members are visible on the class, not copied to instances.
    pub class_only: bool,
    pub call: NativeCallable,
}
#[derive(Clone, Copy)]
pub struct NativeProperty {
    pub hidden: bool,
    pub class_only: bool,
    pub name: &'static str,
    pub doc: &'static str,
    pub get: Option<NativeCallable>,
    pub set: Option<NativeCallable>,
}
#[derive(Clone, Copy)]
pub struct NativeClass {
    pub name: &'static str,
    pub doc: &'static str,
    pub storage: NativeStorage,
    pub constructor: NativeCallable,
    pub constructor_class_only: bool,
    /// Allocate unconstructed Rust state before the script constructor runs.
    pub initialize: fn(&mut Heap, ObjId) -> NativeResult<()>,
    /// Runs after script finalize, before Rust states and members are released.
    /// This intrinsic hook is never exposed as a replaceable script member.
    pub invalidate: Option<(fn() -> std::any::TypeId, NativeCallable)>,
    /// Intrinsic literals bypass the public constructor but still have state.
    pub literal: Option<fn(&mut Heap, ObjId) -> ObjId>,
    pub methods: &'static [NativeMethod],
    pub properties: &'static [NativeProperty],
}

pub struct NativeCx<'a> {
    heap: &'a mut Heap,
    this: ObjId,
    result_needed: bool,
    function: Option<ObjId>,
}

/// Attach as native state when an object owns indexed storage. The VM keeps
/// normal validation, budgets and exception propagation around these leaves.
#[derive(Clone, Copy)]
pub struct NativeIndex {
    pub get: fn(&mut NativeCx<'_>, i32) -> NativeResult<Value>,
    pub set: fn(&mut NativeCx<'_>, i32, Value) -> NativeResult<()>,
    pub update: fn(&mut NativeCx<'_>, i32, crate::value::UpdateOp, Value) -> NativeResult<Value>,
}
impl Trace for NativeIndex {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl<'a> NativeCx<'a> {
    pub(crate) fn new(heap: &'a mut Heap, this: ObjId, result_needed: bool) -> Self {
        Self {
            heap,
            this,
            result_needed,
            function: None,
        }
    }
    pub(crate) fn with_function(mut self, function: ObjId) -> Self {
        self.function = Some(function);
        self
    }
    /// The invoked native function, for reading its traced capture state.
    /// Available at callable entry, not on continuation resumes or direct
    /// property/invalidation hooks. Copy needed captures into the continuation.
    pub fn function(&self) -> Option<ObjId> {
        self.function
    }
    /// Whether the caller supplied a result slot. Discarding a result never
    /// skips invocation or argument evaluation; each API decides which work
    /// depends on the result, after its required argument/receiver checks.
    pub fn result_needed(&self) -> bool {
        self.result_needed
    }
    /// Invoke an already-bound native leaf without converting its native status
    /// to a script exception. Script/resumable functions return None and must be
    /// called through NativeStep. The short child context cannot reenter a VM.
    pub fn try_call_leaf(
        &mut self,
        function: Value,
        args: &[Value],
    ) -> NativeResult<Option<Value>> {
        let Value::Obj(crate::ObjRef {
            object: Some(object),
            this,
        }) = function
        else {
            return Ok(None);
        };
        self.heap.ensure_valid(object)?;
        let call = self.heap.native_callable(object, false)?;
        let Some(NativeCallable::Leaf(call) | NativeCallable::EmptyFinalizer(call)) = call else {
            return Ok(None);
        };
        let this = this.ok_or(NativeError::This)?;
        self.heap.ensure_valid(this)?;
        call(
            &mut NativeCx::new(self.heap, this, true).with_function(object),
            args,
        )
        .map(Some)
    }
    pub fn this(&self) -> ObjId {
        self.this
    }
    pub fn heap(&self) -> &Heap {
        self.heap
    }
    /// Allocation is allowed; collect and VM reentry are forbidden within a leaf.
    pub fn heap_mut(&mut self) -> &mut Heap {
        self.heap
    }
    /// Complete a raw copy without allocating a VM continuation when possible.
    /// False means a missing handler needs CopyMember; no slot was changed.
    pub fn try_copy_member(
        &mut self,
        target: ObjId,
        key: Value,
        value: Value,
        class_only: bool,
    ) -> NativeResult<bool> {
        use crate::member::MemberError;
        match crate::member::set_flags(
            self.heap,
            Value::Obj(crate::ObjRef::bound(target)),
            key,
            value,
            false,
            true,
            (false, class_only),
        ) {
            Ok(()) | Err(MemberError::Heap(HeapError::InvalidObject)) => Ok(true),
            Err(MemberError::MissingHook { .. }) => Ok(false),
            Err(MemberError::Heap(error)) => Err(error.into()),
            Err(error) => Err(NativeError::Detail(error.to_string())),
        }
    }
    /// Install constructor output into the already allocated native facet.
    pub fn construct<T: Trace + 'static>(&mut self, state: T) -> NativeResult<Value> {
        self.heap.replace_native_state(self.this, state)?;
        Ok(Value::Obj(crate::ObjRef::bound(self.this)))
    }
    pub fn with_state<T: Trace + 'static, R>(
        &mut self,
        call: impl FnOnce(&mut T, &mut Self) -> NativeResult<R>,
    ) -> NativeResult<R> {
        // Move the box, not its payload. End the record borrow before giving the
        // method heap access, then restore even when the method returns an error.
        let (index, mut state) = self.heap.take_native_state::<T>(self.this)?;
        let result = match state.any_mut().downcast_mut::<T>() {
            Some(value) => call(value, self),
            None => Err(NativeError::This),
        };
        self.heap.restore_native_state(self.this, index, state);
        result
    }
}

pub trait FromTjs: Sized {
    fn from_tjs(value: Value, heap: &Heap) -> NativeResult<Self>;
}
pub trait IntoTjs {
    fn into_tjs(self, heap: &mut Heap) -> NativeResult<Value>;
}
impl FromTjs for Value {
    fn from_tjs(value: Value, _: &Heap) -> NativeResult<Self> {
        Ok(value)
    }
}
impl IntoTjs for Value {
    fn into_tjs(self, _: &mut Heap) -> NativeResult<Value> {
        Ok(self)
    }
}
impl FromTjs for bool {
    fn from_tjs(value: Value, heap: &Heap) -> NativeResult<Self> {
        Ok(value.truthy(heap)?)
    }
}
impl FromTjs for f64 {
    fn from_tjs(value: Value, heap: &Heap) -> NativeResult<Self> {
        Ok(crate::value::to_real(heap, value)?)
    }
}
impl FromTjs for i64 {
    fn from_tjs(value: Value, _: &Heap) -> NativeResult<Self> {
        value
            .as_integer()
            .ok_or(NativeError::Type("an integer in this subset"))
    }
}
impl IntoTjs for i64 {
    fn into_tjs(self, _: &mut Heap) -> NativeResult<Value> {
        Ok(Value::Int(self))
    }
}
impl IntoTjs for f64 {
    fn into_tjs(self, _: &mut Heap) -> NativeResult<Value> {
        Ok(Value::Real(self))
    }
}
impl IntoTjs for () {
    fn into_tjs(self, _: &mut Heap) -> NativeResult<Value> {
        Ok(Value::Void)
    }
}
impl IntoTjs for bool {
    fn into_tjs(self, _: &mut Heap) -> NativeResult<Value> {
        Ok(Value::Int(i64::from(self)))
    }
}
impl IntoTjs for String {
    fn into_tjs(self, heap: &mut Heap) -> NativeResult<Value> {
        Ok(Value::Str(
            heap.alloc_string(self.encode_utf16().collect::<Vec<_>>()),
        ))
    }
}

pub fn argument<T: FromTjs>(args: &[Value], index: usize, heap: &Heap) -> NativeResult<T> {
    T::from_tjs(
        *args.get(index).ok_or(NativeError::Missing(index + 1))?,
        heap,
    )
}

/// Extra arguments without a heap allocation; valid only for this leaf call.
pub type RestArgs<'a> = &'a [Value];
