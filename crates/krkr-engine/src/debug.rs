//! Per-engine logging, including resumable script handlers and host file output.
use std::{cell::Cell, collections::VecDeque, rc::Rc};
use tjs_bind::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, RestArgs,
    Trace, Value,
};
use tjs_core::{ObjRef, ObjectKind, value};

pub trait LogOutput {
    /// Disabled output skips formatting/history unless a script handler needs it.
    fn enabled(&self) -> bool {
        !cfg!(target_os = "vita")
    }
    fn timestamp(&mut self) -> String;
    fn console(&mut self, line: &[u16]);
    /// Console-only hosts do not resolve paths or enter file-log mode.
    fn file_output(&mut self) -> Option<&mut dyn FileOutput> {
        None
    }
}

pub trait FileOutput {
    fn normalize_directory(&mut self, directory: &[u16]) -> NativeResult<Vec<u16>>;
    fn write_file(&mut self, directory: &[u16], text: &[u16], clear: bool) -> NativeResult<()>;
}

struct Handler {
    function: Value,
    active: Rc<Cell<bool>>,
}

#[tjs_bind::class(name = "Debug", static_class = true)]
mod implementation {
    use super::*;
    pub struct State {
        pub output: Option<Box<dyn LogOutput>>,
        pub directory: Vec<u16>,
        pub lines: VecDeque<Vec<u16>>,
        pub important: Vec<u16>,
        pub handlers: Vec<Handler>,
        pub delivering: Rc<Cell<bool>>,
        pub to_file: bool,
        pub on_error: bool,
        pub clear_on_error: bool,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                output: None,
                directory: Vec::new(),
                lines: VecDeque::new(),
                important: Vec::new(),
                handlers: Vec::new(),
                delivering: Rc::new(Cell::new(false)),
                to_file: false,
                on_error: true,
                clear_on_error: false,
            }
        }
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            for handler in &self.handlers {
                if handler.active.get() {
                    visit(handler.function);
                }
            }
        }
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method(class_only = true, resumable = true)]
        fn message(
            cx: &mut NativeCx<'_>,
            first: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            log(cx, first, args, false, None)
        }
        #[tjs::method(class_only = true, resumable = true)]
        fn notice(
            cx: &mut NativeCx<'_>,
            first: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            log(cx, first, args, true, None)
        }
        #[tjs::method(name = "getLastLog", class_only = true)]
        fn get_last_log(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<Value> {
            let count = args
                .first()
                .map(|&v| value::to_integer(cx.heap(), v))
                .transpose()?
                .unwrap_or(2148) as u32;
            let text = with_log(cx, |state| Ok(state.last(count as usize)))?;
            Ok(Value::Str(cx.heap_mut().alloc_string(text)))
        }
        #[tjs::method(name = "startLogToFile", class_only = true)]
        fn start_log_to_file(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<()> {
            let clear = args
                .first()
                .map(|v| v.truthy(cx.heap()))
                .transpose()?
                .unwrap_or(false);
            with_log(cx, |state| state.start_file(clear))
        }
        #[tjs::method(name = "logAsError", class_only = true)]
        fn log_as_error(cx: &mut NativeCx<'_>) -> NativeResult<()> {
            with_log(cx, |state| {
                if state.on_error {
                    state.start_file(state.clear_on_error)
                } else {
                    Ok(())
                }
            })
        }
        #[tjs::method(name = "addLoggingHandler", class_only = true)]
        fn add_handler(cx: &mut NativeCx<'_>, handler: Value) -> NativeResult<()> {
            let Value::Obj(reference) = handler else {
                return Err(NativeError::Type("an object"));
            };
            with_log(cx, |state| {
                if !state
                    .handlers
                    .iter()
                    .any(|h| h.active.get() && same(h.function, reference))
                {
                    state.handlers.push(Handler {
                        function: handler,
                        active: Rc::new(Cell::new(reference.object.is_some())),
                    });
                }
                Ok(())
            })
        }
        #[tjs::method(name = "removeLoggingHandler", class_only = true)]
        fn remove_handler(cx: &mut NativeCx<'_>, handler: Value) -> NativeResult<()> {
            let Value::Obj(reference) = handler else {
                return Err(NativeError::Type("an object"));
            };
            with_log(cx, |state| {
                for handler in &state.handlers {
                    if same(handler.function, reference) {
                        handler.active.set(false);
                    }
                }
                if !state.delivering.get() {
                    state.handlers.retain(|h| h.active.get());
                }
                Ok(())
            })
        }
        #[tjs::getter(name = "logLocation", class_only = true)]
        fn location(&self, cx: &mut NativeCx<'_>) -> Value {
            Value::Str(cx.heap_mut().alloc_string(self.directory.clone()))
        }
        #[tjs::setter(name = "logLocation", class_only = true)]
        fn set_location(&mut self, cx: &mut NativeCx<'_>, location: Value) -> NativeResult<()> {
            let text = units(cx, location)?;
            if let Some(output) = self
                .output
                .as_mut()
                .expect("installed log output")
                .file_output()
            {
                self.directory = output.normalize_directory(&text)?;
            }
            Ok(())
        }
        #[tjs::getter(name = "logToFileOnError", class_only = true)]
        fn on_error(&self) -> bool {
            self.on_error
        }
        #[tjs::setter(name = "logToFileOnError", class_only = true)]
        fn set_on_error(&mut self, cx: &mut NativeCx<'_>, flag: Value) -> NativeResult<()> {
            self.on_error = flag.truthy(cx.heap())?;
            Ok(())
        }
        #[tjs::getter(name = "clearLogFileOnError", class_only = true)]
        fn clear_on_error(&self) -> bool {
            self.clear_on_error
        }
        #[tjs::setter(name = "clearLogFileOnError", class_only = true)]
        fn set_clear_on_error(&mut self, cx: &mut NativeCx<'_>, flag: Value) -> NativeResult<()> {
            self.clear_on_error = flag.truthy(cx.heap())?;
            Ok(())
        }
    }
    impl State {
        pub fn last(&self, count: usize) -> Vec<u16> {
            self.lines
                .iter()
                .skip(self.lines.len().saturating_sub(count))
                .flat_map(|line| line.iter().copied().chain([13, 10]))
                .collect()
        }
        pub fn start_file(&mut self, clear: bool) -> NativeResult<()> {
            if !self.output.as_ref().is_some_and(|output| output.enabled()) {
                return Ok(());
            }
            if self.to_file {
                return Ok(());
            }
            if self.output.as_mut().unwrap().file_output().is_none() {
                return Ok(());
            }
            let mut text = self.important.clone();
            text.extend(self.last(100));
            self.output
                .as_mut()
                .expect("installed log output")
                .file_output()
                .expect("file-capable log output")
                .write_file(&self.directory, &text, clear)?;
            self.to_file = true;
            Ok(())
        }
        pub fn write_line(&mut self, text: &[u16]) -> NativeResult<()> {
            if self.to_file && self.output.as_ref().is_some_and(|output| output.enabled()) {
                let text = text.iter().copied().chain([13, 10]).collect::<Vec<_>>();
                self.output
                    .as_mut()
                    .expect("installed log output")
                    .file_output()
                    .expect("file-capable log output")
                    .write_file(&self.directory, &text, false)?;
            }
            Ok(())
        }
    }
}

fn same(value: Value, other: ObjRef) -> bool {
    matches!(value, Value::Obj(reference) if reference == other)
}
fn units(cx: &mut NativeCx<'_>, input: Value) -> NativeResult<Vec<u16>> {
    let Value::Str(id) = value::to_string(cx.heap_mut(), input)? else {
        unreachable!()
    };
    Ok(tjs_core::string::c_string(cx.heap().string(id)?).to_vec())
}
fn with_log<T>(
    cx: &mut NativeCx<'_>,
    f: impl FnOnce(&mut implementation::State) -> NativeResult<T>,
) -> NativeResult<T> {
    let class = cx
        .heap()
        .registered_class("Debug")
        .expect("installed Debug");
    cx.heap_mut()
        .with_native_state::<implementation::State, _>(class, f)?
}

struct Delivery {
    line: Value,
    index: usize,
    delivering: Rc<Cell<bool>>,
    current: Option<Rc<Cell<bool>>>,
    completion: Option<Box<dyn NativeContinuation>>,
}
impl Trace for Delivery {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(self.line);
        if let Some(completion) = &self.completion {
            completion.trace(visit);
        }
    }
}
impl Drop for Delivery {
    fn drop(&mut self) {
        // An exception/cancellation drops the owned continuation. Remove the
        // failing handler and release recursion suppression on every exit path.
        if let Some(current) = &self.current {
            current.set(false);
        }
        self.delivering.set(false);
    }
}
impl Delivery {
    fn next(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        self.current = None;
        loop {
            let next = with_log(cx, |state| {
                let next = state
                    .handlers
                    .iter()
                    .enumerate()
                    .skip(self.index)
                    .find(|(_, h)| h.active.get());
                Ok(next.map(|(index, h)| (index + 1, h.function, Rc::clone(&h.active))))
            })?;
            let Some((index, function, active)) = next else {
                let line = units(cx, self.line)?;
                with_log(cx, |state| {
                    state.handlers.retain(|h| h.active.get());
                    state.write_line(&line)
                })?;
                return Ok(self
                    .completion
                    .take()
                    .map_or(NativeStep::Return(Value::Void), NativeStep::Continue));
            };
            self.index = index;
            let callable = match function {
                Value::Obj(reference) => reference
                    .object
                    .and_then(|id| cx.heap().object(id).ok())
                    .is_some_and(|r| {
                        matches!(
                            r.kind(),
                            ObjectKind::Function
                                | ObjectKind::Class
                                | ObjectKind::NativeFunction
                                | ObjectKind::NativeClass
                        )
                    }),
                _ => false,
            };
            if !callable {
                active.set(false);
                continue;
            }
            self.current = Some(active);
            return Ok(NativeStep::Call {
                function,
                arguments: vec![self.line],
                continuation: self,
            });
        }
    }
}
impl NativeContinuation for Delivery {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.next(cx)
    }
}

fn log(
    cx: &mut NativeCx<'_>,
    first: Value,
    rest: &[Value],
    important: bool,
    completion: Option<Box<dyn NativeContinuation>>,
) -> NativeResult<NativeStep> {
    let observed = with_log(cx, |state| {
        Ok(state.output.as_ref().is_some_and(|output| output.enabled())
            || state.handlers.iter().any(|handler| handler.active.get()))
    })?;
    if !observed {
        return Ok(completion.map_or(NativeStep::Return(Value::Void), NativeStep::Continue));
    }
    let mut text = units(cx, first)?;
    for &value in rest {
        text.extend([44, 32]);
        text.extend(units(cx, value)?);
    }
    let (line, delivering, dispatch) = with_log(cx, |state| {
        let output = state.output.as_mut().expect("installed log output");
        let time = output.timestamp();
        let line = time
            .encode_utf16()
            .chain([32])
            .chain(text.iter().copied())
            .collect::<Vec<_>>();
        if output.enabled() {
            output.console(&line);
        }
        state.lines.push_back(line.clone());
        if important {
            state.important.extend(
                time.encode_utf16()
                    .chain(" ! ".encode_utf16())
                    .chain(text)
                    .chain([13, 10]),
            );
        }
        if state.lines.len() >= 2148 {
            state.lines.drain(..100);
        }
        let dispatch = !state.delivering.get() && state.handlers.iter().any(|h| h.active.get());
        if dispatch {
            state.delivering.set(true);
        } else {
            state.write_line(&line)?;
        }
        Ok((line, Rc::clone(&state.delivering), dispatch))
    })?;
    if !dispatch {
        return Ok(completion.map_or(NativeStep::Return(Value::Void), NativeStep::Continue));
    }
    let line = Value::Str(cx.heap_mut().alloc_string(line));
    Box::new(Delivery {
        line,
        index: 0,
        delivering,
        current: None,
        completion,
    })
    .next(cx)
}

/// Use the same delivery path as Debug.message, including script handlers.
/// Their own errors propagate, as in TVPAddLog called from native cleanup.
pub(crate) fn native_error(
    cx: &mut NativeCx<'_>,
    error: Value,
    completion: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    if cx.heap().registered_class("Debug").is_none() {
        return Ok(NativeStep::Continue(completion));
    }
    let message = if let Value::Obj(reference) = error {
        let key = cx.heap_mut().intern_str("message");
        reference
            .object
            .and_then(|id| cx.heap().member(id, key).ok().flatten())
            .unwrap_or(error)
    } else {
        error
    };
    let text = cx
        .heap()
        .display(message)
        .unwrap_or_else(|_| "native object finalization failed".into());
    let message = Value::Str(
        cx.heap_mut()
            .alloc_string(text.encode_utf16().collect::<Vec<_>>()),
    );
    log(cx, message, &[], false, Some(completion))
}

pub fn install(heap: &mut Heap, mut output: impl LogOutput + 'static) -> NativeResult<ObjId> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)?;
    let directory = output
        .file_output()
        .map(|output| output.normalize_directory(&[]))
        .transpose()?
        .unwrap_or_default();
    heap.with_native_state::<implementation::State, _>(class, |state| {
        state.output = Some(Box::new(output));
        state.directory = directory;
    })?;
    Ok(class)
}

pub fn on_error(heap: &mut Heap) -> NativeResult<()> {
    let class = heap.registered_class("Debug").expect("installed Debug");
    heap.with_native_state::<implementation::State, _>(class, |state| {
        if state.on_error {
            state.start_file(state.clear_on_error)
        } else {
            Ok(())
        }
    })?
}
