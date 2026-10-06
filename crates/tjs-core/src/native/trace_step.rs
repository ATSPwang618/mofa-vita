use super::{NativeStep, Trace};
use crate::Value;

// An owned step may be held by a binding combinator before the VM adopts it.
// Match exhaustively so adding a VM operation requires updating its GC edges.
impl Trace for NativeStep {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        match self {
            Self::Return(value) | Self::Throw(value) => value.trace(visit),
            Self::Continue(next)
            | Self::Inspect {
                continuation: next, ..
            }
            | Self::Wait {
                continuation: next, ..
            } => next.trace(visit),
            Self::Evaluate { context, .. } => context.trace(visit), // CompileRequest owns text/bytes, not heap IDs.
            Self::Try { task, continuation } => {
                task.trace(visit);
                continuation.trace(visit);
            }
            Self::Invalidate {
                object,
                continuation,
            } => {
                object.trace(visit);
                continuation.trace(visit);
            }
            Self::TryInvalidate {
                object,
                continuation,
            } => {
                object.trace(visit);
                continuation.trace(visit);
            }
            Self::Get {
                object,
                key,
                continuation,
            }
            | Self::GetOptional {
                object,
                key,
                continuation,
            }
            | Self::GetRequired {
                object,
                key,
                continuation,
            }
            | Self::GetProperty {
                object,
                key,
                continuation,
                ..
            } => {
                object.trace(visit);
                key.trace(visit);
                continuation.trace(visit);
            }
            Self::GetOr {
                object,
                key,
                fallback,
                continuation,
                ..
            }
            | Self::GetRequiredOr {
                object,
                key,
                fallback,
                continuation,
            } => {
                object.trace(visit);
                key.trace(visit);
                fallback.trace(visit);
                continuation.trace(visit);
            }
            Self::Set {
                object,
                key,
                value,
                continuation,
            }
            | Self::SetExisting {
                object,
                key,
                value,
                continuation,
            }
            | Self::SetProperty {
                object,
                key,
                value,
                continuation,
                ..
            }
            | Self::CopyMember {
                object,
                key,
                value,
                continuation,
                ..
            } => {
                object.trace(visit);
                key.trace(visit);
                value.trace(visit);
                continuation.trace(visit);
            }
            Self::CallMember {
                object,
                key,
                arguments,
                continuation,
            }
            | Self::CallMemberOr {
                object,
                key,
                arguments,
                continuation,
                ..
            } => {
                object.trace(visit);
                key.trace(visit);
                arguments.trace(visit);
                continuation.trace(visit);
            }
            Self::Call {
                function,
                arguments,
                continuation,
            }
            | Self::CallDiscard {
                function,
                arguments,
                continuation,
            }
            | Self::Construct {
                class: function,
                arguments,
                continuation,
            } => {
                function.trace(visit);
                arguments.trace(visit);
                continuation.trace(visit);
            }
        }
    }
}
