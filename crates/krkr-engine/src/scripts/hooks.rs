//! Explicit script-load callbacks. Unmatched loads keep the direct eval path;
//! matching callbacks run in the same VM after successful script execution.
use super::*;
use krkr_assets::name;
use std::collections::HashMap;
use tjs_core::{NativeContinuation, NativeTryContinuation, Trace};

#[derive(Default)]
pub(super) struct Registry {
    files: HashMap<Vec<u16>, Vec<Hook>>,
}
struct Hook {
    selector: Vec<u16>,
    handler: Value,
}
impl Registry {
    pub fn register(&mut self, selector: &[u16], handler: Value) -> NativeResult<()> {
        let selector = name::fold(selector);
        let (_, file) = name::split_name(&selector);
        if file.is_empty() {
            return Err(NativeError::Message(
                "afterLoad requires a script file name",
            ));
        }
        self.files
            .entry(file.to_vec())
            .or_default()
            .push(Hook { selector, handler });
        Ok(())
    }
    fn matching(&self, storage: &str) -> Vec<Value> {
        if self.files.is_empty() {
            return Vec::new();
        }
        let storage = name::fold(&storage.encode_utf16().collect::<Vec<_>>());
        let (_, file) = name::split_name(&storage);
        self.files
            .get(file)
            .into_iter()
            .flatten()
            .filter(|hook| hook.selector == file || hook.selector == storage)
            .map(|hook| hook.handler)
            .collect()
    }
}
impl Trace for Registry {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for hook in self.files.values().flatten() {
            hook.handler.trace(visit);
        }
    }
}

pub(super) fn evaluate(
    cx: &mut NativeCx<'_>,
    request: CompileRequest,
    context: Option<ObjId>,
) -> NativeResult<NativeStep> {
    // compileStorage does not execute code and must not install patches.
    let callbacks = if request.output.is_none() {
        let class = cx
            .heap()
            .registered_class("Scripts")
            .expect("installed Scripts");
        cx.heap_mut()
            .with_native_state::<implementation::State, _>(class, |state| {
                state.after_load.matching(&request.name)
            })?
    } else {
        Vec::new()
    };
    if callbacks.is_empty() {
        return Ok(NativeStep::Evaluate { request, context });
    }
    let name = Value::Str(
        cx.heap_mut()
            .alloc_string(request.name.encode_utf16().collect::<Vec<_>>()),
    );
    Ok(NativeStep::Try {
        task: Box::new(Evaluate { request, context }),
        continuation: Box::new(AfterLoad {
            callbacks: callbacks.into_iter(),
            name,
            result: Value::Void,
        }),
    })
}

struct Evaluate {
    request: CompileRequest,
    context: Option<ObjId>,
}
impl Trace for Evaluate {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.context.trace(visit);
    }
}
impl NativeContinuation for Evaluate {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        // Preserve the original expression/result demand, including discarded
        // evalStorage results; the callback boundary must not change parsing.
        Ok(NativeStep::Evaluate {
            request: self.request,
            context: self.context,
        })
    }
}

struct AfterLoad {
    callbacks: std::vec::IntoIter<Value>,
    name: Value,
    result: Value,
}
impl Trace for AfterLoad {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for callback in self.callbacks.as_slice() {
            callback.trace(visit);
        }
        self.name.trace(visit);
        self.result.trace(visit);
    }
}
impl AfterLoad {
    fn next(mut self: Box<Self>) -> NativeStep {
        match self.callbacks.next() {
            Some(function) => NativeStep::CallDiscard {
                function,
                arguments: vec![self.name],
                continuation: self,
            },
            None => NativeStep::Return(self.result),
        }
    }
}
impl NativeTryContinuation for AfterLoad {
    fn resume(
        mut self: Box<Self>,
        _: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        Ok(match result {
            Ok(result) => {
                self.result = result;
                self.next()
            }
            Err(error) => NativeStep::Throw(error),
        })
    }
}
impl NativeContinuation for AfterLoad {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(self.next())
    }
}
