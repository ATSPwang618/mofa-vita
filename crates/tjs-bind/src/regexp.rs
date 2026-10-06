//! TJS RegExp. Native state owns the compiled Rust regex; only managed result
//! values and class identity participate in tracing.
mod engine;
mod replace;
pub(crate) mod split;

use std::rc::Rc;

use crate::{Heap, NativeCx, NativeError, NativeResult, NativeStep, ObjId, RestArgs, Trace, Value};
use engine::{Engine, array, byte_offset, string, substring, text};

#[crate::class(name = "RegExp")]
/// Regular expressions with captures, search state and literal string replacement.
mod implementation {
    use super::*;

    pub struct State {
        pub(super) engine: Option<Rc<Engine>>,
        pub(super) start: u32,
        index: u32,
        last_index: u32,
        matches: Value,
        input: Value,
        last_match: Value,
        last_paren: Value,
        left: Value,
        right: Value,
        last: Value,
    }

    impl Default for State {
        fn default() -> Self {
            Self {
                engine: None,
                start: 0,
                index: 0,
                last_index: 0,
                matches: Value::Void,
                input: Value::Void,
                last_match: Value::Void,
                last_paren: Value::Void,
                left: Value::Void,
                right: Value::Void,
                last: Value::Void,
            }
        }
    }

    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            for value in [
                self.matches,
                self.input,
                self.last_match,
                self.last_paren,
                self.left,
                self.right,
                self.last,
            ] {
                visit(value);
            }
        }
    }

    impl State {
        #[tjs::method]
        fn finalize(&self) {}

        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<Self> {
            let mut state = Self::default();
            if let Some(&pattern) = args.first() {
                state.compile(cx, pattern, &args[1..])?;
            }
            Ok(state)
        }

        #[tjs::method]
        fn compile(
            &mut self,
            cx: &mut NativeCx<'_>,
            pattern: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<()> {
            let pattern = tjs_core::value::to_string_units(cx.heap(), pattern)?;
            let flags = tjs_core::value::to_string_units(
                cx.heap(),
                args.first().copied().unwrap_or(Value::Void),
            )?;
            self.engine = None;
            self.engine = Some(Rc::new(Engine::compile(&pattern, &flags)?));
            Ok(())
        }

        #[tjs::method]
        fn _compile(
            &mut self,
            cx: &mut NativeCx<'_>,
            encoded: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<()> {
            if !args.is_empty() {
                return Err(NativeError::Message(
                    "RegExp._compile requires exactly one argument",
                ));
            }
            let encoded = tjs_core::value::to_string_units(cx.heap(), encoded)?;
            let body = encoded
                .strip_prefix(&[47, 47])
                .ok_or(NativeError::Message("invalid encoded RegExp literal"))?;
            let separator = body
                .iter()
                .take_while(|&&unit| unit != 0)
                .position(|&unit| unit == 47)
                .ok_or(NativeError::Message("invalid encoded RegExp literal"))?;
            let (flags, tail) = body.split_at(separator);
            let pattern = &tail[1..];
            self.engine = None;
            self.engine = Some(Rc::new(Engine::compile(pattern, flags)?));
            Ok(())
        }

        fn engine(&self) -> NativeResult<&Engine> {
            self.engine
                .as_deref()
                .ok_or(NativeError::Message("RegExp has not been compiled"))
        }

        /// Return capture strings without changing search state.
        #[tjs::method(name = "match")]
        fn match_text(&self, cx: &mut NativeCx<'_>, target: Value) -> NativeResult<Value> {
            if !cx.result_needed() {
                return Ok(Value::Void);
            }
            let target = text(cx.heap_mut(), target)?;
            let offset = byte_offset(&target, self.start)?;
            let suffix = &target[offset..];
            let captures = if suffix.is_empty() {
                None
            } else {
                self.engine()?.captures(suffix)?
            };
            array(
                cx.heap_mut(),
                captures
                    .iter()
                    .flatten()
                    .map(|range| range.as_ref().map_or("", |range| &suffix[range.clone()])),
            )
        }

        fn search(&mut self, cx: &mut NativeCx<'_>, target: Value) -> NativeResult<bool> {
            let target = text(cx.heap_mut(), target)?;
            let offset = byte_offset(&target, self.start)?;
            let suffix = &target[offset..];
            let captures = if suffix.is_empty() {
                None
            } else {
                self.engine()?.captures(suffix)?
            };
            let heap = cx.heap_mut();
            self.matches = array(
                heap,
                captures
                    .iter()
                    .flatten()
                    .map(|range| range.as_ref().map_or("", |range| &suffix[range.clone()])),
            )?;
            self.input = string(heap, &target);
            if let Some(captures) = &captures {
                let whole = captures[0].as_ref().expect("matched expression");
                let end = captures
                    .iter()
                    .flatten()
                    .map(|range| range.end)
                    .max()
                    .unwrap_or(whole.end);
                // tjsRegExp.cpp adds OnigRegion's UTF-16 BYTE offset here;
                // LastIndex and capture slicing below instead divide by two.
                self.index = self.start.wrapping_add(
                    (suffix[..whole.start].encode_utf16().count() as u32).wrapping_mul(2),
                );
                self.last_index = self.start + suffix[..end].encode_utf16().count() as u32;
                self.last_match = substring(heap, &suffix[whole.clone()]);
                self.last_paren = substring(
                    heap,
                    captures
                        .last()
                        .and_then(Option::as_ref)
                        .map_or("", |range| &suffix[range.clone()]),
                );
                self.left = substring(heap, &target[..offset + whole.start]);
                self.right = string(heap, suffix[end..].split('\0').next().unwrap_or_default());
                if self.engine()?.global {
                    self.start = self.last_index;
                }
            } else {
                self.index = self.start;
                self.last_index = self.start;
                self.last_match = string(heap, "");
                self.last_paren = self.last_match;
                self.left = substring(heap, &target[..offset]);
                // The reference preserves rightContext after a failed search.
            }
            let this = cx.this();
            let class = cx
                .heap()
                .registered_class("RegExp")
                .ok_or(NativeError::This)?;
            cx.heap_mut()
                .with_native_state::<State, _>(class, |state| {
                    state.last = Value::Obj(tjs_core::ObjRef::bound(this))
                })?;
            Ok(captures.is_some())
        }

        #[tjs::method]
        fn test(&mut self, cx: &mut NativeCx<'_>, target: Value) -> NativeResult<bool> {
            self.search(cx, target)
        }

        #[tjs::method]
        fn exec(&mut self, cx: &mut NativeCx<'_>, target: Value) -> NativeResult<Value> {
            self.search(cx, target)?;
            Ok(self.matches)
        }

        #[tjs::method(resumable = true)]
        fn replace(
            &self,
            cx: &mut NativeCx<'_>,
            target: Value,
            replacement: Value,
        ) -> NativeResult<NativeStep> {
            let target = text(cx.heap_mut(), target)?;
            let replacement = replace::Substitution::new(cx, replacement)?;
            self.engine()?;
            replace::Replacement::start(
                cx,
                self.engine.as_ref().unwrap().clone(),
                self.start,
                target,
                replacement,
            )
        }

        #[tjs::method(resumable = true)]
        fn split(
            &self,
            cx: &mut NativeCx<'_>,
            target: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let target = text(cx.heap_mut(), target)?;
            let purge = args
                .get(1)
                .copied()
                .unwrap_or(Value::Void)
                .truthy(cx.heap())?;
            self.engine()?;
            let output = cx.heap_mut().alloc_array();
            split::regex(
                cx,
                self.engine.as_ref().unwrap().clone(),
                target,
                output,
                purge,
            )
        }

        #[tjs::getter]
        fn start(&self) -> i64 {
            i64::from(self.start)
        }
        #[tjs::setter(name = "start")]
        fn set_start(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.start = tjs_core::value::to_integer(cx.heap(), value)? as u32;
            Ok(())
        }
        #[tjs::getter]
        fn index(&self) -> i64 {
            i64::from(self.index)
        }
        #[tjs::getter(name = "lastIndex")]
        fn last_index(&self) -> i64 {
            i64::from(self.last_index)
        }
        #[tjs::getter]
        fn matches(&self) -> Value {
            self.matches
        }
        #[tjs::getter]
        fn input(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            Ok(tjs_core::value::to_string(cx.heap_mut(), self.input)?)
        }
        #[tjs::getter(name = "lastMatch")]
        fn last_match(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            Ok(tjs_core::value::to_string(cx.heap_mut(), self.last_match)?)
        }
        #[tjs::getter(name = "lastParen")]
        fn last_paren(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            Ok(tjs_core::value::to_string(cx.heap_mut(), self.last_paren)?)
        }
        #[tjs::getter(name = "leftContext")]
        fn left(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            Ok(tjs_core::value::to_string(cx.heap_mut(), self.left)?)
        }
        #[tjs::getter(name = "rightContext")]
        fn right(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            Ok(tjs_core::value::to_string(cx.heap_mut(), self.right)?)
        }
        #[tjs::getter(class_only = true)]
        fn last(&self) -> Value {
            self.last
        }
    }
}
pub use implementation::CLASS;

pub fn install(heap: &mut Heap) -> NativeResult<ObjId> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)?;
    Ok(class)
}
fn snapshot(heap: &mut Heap, object: ObjId) -> NativeResult<(Rc<Engine>, u32)> {
    heap.with_native_state::<implementation::State, _>(object, |state| {
        let engine = state
            .engine
            .clone()
            .ok_or(NativeError::Message("RegExp has not been compiled"))?;
        Ok((engine, state.start))
    })?
}
