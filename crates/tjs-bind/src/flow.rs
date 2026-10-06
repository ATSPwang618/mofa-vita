//! Composable native work on the existing VM. Captures are explicit Trace
//! states; callbacks are function pointers, so closures cannot hide GC roots.
//! No executor, VM reentry, or new wait/cancellation semantics are introduced.
use crate::{
    NativeContinuation, NativeCx, NativeResult, NativeStep, NativeTryContinuation, Trace, Value,
};
use std::collections::VecDeque;
use tjs_core::MemberFlags;

type Resume<S> = fn(S, &mut NativeCx<'_>, Value) -> NativeResult<NativeStep>;
struct Callback<S> {
    state: S,
    resume: Resume<S>,
}
impl<S: Trace> Trace for Callback<S> {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.state.trace(v);
    }
}
impl<S: Trace + 'static> NativeContinuation for Callback<S> {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        (self.resume)(self.state, cx, value)
    }
}
pub fn callback<S: Trace + 'static>(state: S, resume: Resume<S>) -> Box<dyn NativeContinuation> {
    Box::new(Callback { state, resume })
}
/// Return the nested operation's value without a plugin-specific marker type.
pub fn identity() -> Box<dyn NativeContinuation> {
    callback((), |(), _, value| Ok(NativeStep::Return(value)))
}
/// Complete with an explicitly retained value, ignoring the nested result.
pub fn complete(value: Value) -> Box<dyn NativeContinuation> {
    callback(value, |value, _, _| Ok(NativeStep::Return(value)))
}
#[derive(crate::Trace)]
struct Delivery {
    value: Value,
    next: Box<dyn NativeContinuation>,
}
/// Deliver a fallback on the next VM budget unit, with both captures rooted.
pub fn deliver(value: Value, next: Box<dyn NativeContinuation>) -> NativeStep {
    NativeStep::Continue(callback(Delivery { value, next }, |s, cx, _| {
        s.next.resume(cx, s.value)
    }))
}

type Poll<S> = fn(&mut S, &mut NativeCx<'_>) -> NativeResult<Option<NativeStep>>;
struct Work<S> {
    state: S,
    poll: Poll<S>,
}
impl<S: Trace> Trace for Work<S> {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.state.trace(v);
    }
}
impl<S: Trace + 'static> NativeContinuation for Work<S> {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        match (self.poll)(&mut self.state, cx)? {
            Some(step) => Ok(step),
            None => Ok(NativeStep::Continue(self)),
        }
    }
}
/// None yields one ordinary VM budget unit; Some transfers to the next step.
/// The same allocation is reused for every slice until this work completes.
pub fn work<S: Trace + 'static>(state: S, poll: Poll<S>) -> NativeStep {
    NativeStep::Continue(Box::new(Work { state, poll }))
}

#[derive(crate::Trace)]
struct Start(NativeStep);
impl NativeContinuation for Start {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(self.0)
    }
}
#[derive(crate::Trace)]
struct Success(Box<dyn NativeContinuation>);
impl NativeTryContinuation for Success {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        match result {
            Ok(value) => self.0.resume(cx, value),
            Err(error) => Ok(NativeStep::Throw(error)),
        }
    }
}
/// Continue only after the whole nested operation succeeds. Script exceptions
/// retain their value; cancellation drops both operations and never calls next.
pub fn then(step: NativeStep, next: Box<dyn NativeContinuation>) -> NativeStep {
    NativeStep::Try {
        task: Box::new(Start(step)),
        continuation: Box::new(Success(next)),
    }
}
pub fn returning(step: NativeStep, value: Value) -> NativeStep {
    then(step, complete(value))
}

type Check<S> = fn(&S, &mut NativeCx<'_>) -> NativeResult<()>;
type Next<S> = fn(S, &mut NativeCx<'_>) -> NativeResult<NativeStep>;
struct Properties<S> {
    state: S,
    target: Value,
    values: VecDeque<(&'static str, Value)>,
    flags: MemberFlags,
    check: Check<S>,
    next: Next<S>,
}
impl<S: Trace> Trace for Properties<S> {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.state.trace(v);
        self.target.trace(v);
        for (_, value) in &self.values {
            value.trace(v);
        }
    }
}
impl<S: Trace + 'static> NativeContinuation for Properties<S> {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        (self.check)(&self.state, cx)?;
        if let Some((name, value)) = self.values.pop_front() {
            let key = Value::Str(
                cx.heap_mut()
                    .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
            );
            return Ok(NativeStep::SetProperty {
                object: self.target,
                key,
                value,
                flags: self.flags,
                continuation: self,
            });
        }
        (self.next)(self.state, cx)
    }
}
/// Ordered writes through real setters/missing handlers. The guard runs before
/// every write and after the last setter, permitting generation/lifetime checks.
pub fn set_properties<S: Trace + 'static>(
    state: S,
    target: Value,
    values: impl IntoIterator<Item = (&'static str, Value)>,
    flags: MemberFlags,
    check: Check<S>,
    next: Next<S>,
) -> NativeStep {
    NativeStep::Continue(Box::new(Properties {
        state,
        target,
        values: values.into_iter().collect(),
        flags,
        check,
        next,
    }))
}
