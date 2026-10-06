//! ExtKAGParser's independent dialect, local scopes and loop state.
use super::*;
use krkr_kag::CallFrame;
use std::sync::LazyLock;
use tjs_core::{NativeCallable, NativeClass, NativeProperty};

#[derive(Clone, Default)]
pub(super) struct Advanced {
    pub locals: Option<ObjId>,
    pub pmacros: Option<ObjId>,
    pub numeric: bool,
    pub fuzzy: bool,
    pub return_error: Text,
    pub loops: Vec<Loop>,
    pub calls: Vec<Scope>,
}
#[derive(Clone)]
pub(super) struct Loop {
    pub frame: CallFrame,
    pub exp: Text,
    pub each: Text,
    pub marker: bool,
}
#[derive(Clone)]
pub(super) struct Scope {
    pub loops: usize,
    pub locals: usize,
}
impl Trace for Advanced {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for id in self.locals.into_iter().chain(self.pmacros) {
            visit(Value::Obj(id.into()));
        }
    }
}
impl Advanced {
    pub fn new(heap: &mut Heap) -> NativeResult<Self> {
        let mut result = Self {
            locals: Some(heap.alloc_array()),
            pmacros: Some(heap.alloc_dictionary()),
            ..Self::default()
        };
        result.clear(heap)?;
        Ok(result)
    }
    pub fn clear(&mut self, heap: &mut Heap) -> NativeResult<()> {
        self.loops.clear();
        self.calls.clear();
        let dictionary = Value::Obj(ObjRef::bound(heap.alloc_dictionary()));
        heap.array_replace(self.locals.ok_or(NativeError::This)?, vec![dictionary])?;
        Ok(())
    }
    pub fn local(&self, heap: &Heap, previous: bool) -> NativeResult<Value> {
        let array = heap.array(self.locals.ok_or(NativeError::This)?)?;
        let index = array
            .len()
            .checked_sub(if previous { 2 } else { 1 })
            .ok_or(NativeError::Message("local variable stack underflow"))?;
        Ok(array[index])
    }
    pub fn push_local(
        &mut self,
        heap: &mut Heap,
        values: ObjId,
        copy_parent: bool,
    ) -> NativeResult<()> {
        let array = self.locals.ok_or(NativeError::This)?;
        if heap.array(array)?.len() >= 256 {
            return Err(NativeError::Message("local variable depth limit"));
        }
        let dictionary = if copy_parent {
            clone_dictionary(heap, object(self.local(heap, false)?)?)?
        } else {
            heap.alloc_dictionary()
        };
        copy(heap, values, dictionary, false)?;
        heap.array_push(array, Value::Obj(ObjRef::bound(dictionary)))?;
        Ok(())
    }
    pub fn pop_local(&mut self, heap: &mut Heap) -> NativeResult<()> {
        let array = self.locals.ok_or(NativeError::This)?;
        let count = heap.array(array)?.len();
        if count == 0 || self.calls.last().is_some_and(|call| call.locals >= count) {
            return Err(NativeError::Message("local variable stack underflow"));
        }
        heap.array_resize(array, count - 1)?;
        Ok(())
    }
    pub fn break_control(&mut self, heap: &mut Heap) -> NativeResult<()> {
        let (loops, locals) = self.calls.last().map_or((0, 1), |s| (s.loops, s.locals));
        let array = self.locals.ok_or(NativeError::This)?;
        if loops > self.loops.len() || locals > heap.array(array)?.len() {
            return Err(NativeError::Message("malformed ExtKAG scope"));
        }
        self.loops.truncate(loops);
        heap.array_resize(array, locals)?;
        Ok(())
    }
    pub fn check_loop(&self, parser: &Parser) -> NativeResult<&Loop> {
        let last = self
            .loops
            .last()
            .ok_or(NativeError::Message("while stack underflow"))?;
        if last.marker || parser.conditions.len() != last.frame.conditions.len() + 1 {
            return Err(NativeError::Message("unbalanced while/if scope"));
        }
        Ok(last)
    }
}

pub(super) fn install(heap: &mut Heap) -> NativeResult<ObjId> {
    heap.register_class_variant("krkr.plugins.ExtKAGParser", &CLASS)
}
pub(super) fn return_position(
    parser: &mut Parser,
    frame: &CallFrame,
    fuzzy: bool,
) -> NativeResult<()> {
    let scenario = parser
        .scenario
        .as_ref()
        .ok_or(NativeError::Message("missing return scenario"))?;
    let base = if frame.label.is_empty() {
        0
    } else {
        *scenario
            .labels()
            .map_err(error)?
            .by_name
            .get(&frame.label)
            .ok_or(NativeError::Message("return label not found"))?
    };
    let expected = base
        .checked_add(frame.offset)
        .ok_or(NativeError::Message("return offset overflow"))?;
    let matches = |line| {
        scenario
            .line(line)
            .is_some_and(|text| text == frame.original_line)
    };
    let line = if expected == scenario.line_count() || matches(expected) {
        Some(expected)
    } else if fuzzy {
        (1..=10)
            .filter_map(|n| expected.checked_add(n))
            .find(|&line| matches(line))
            .or_else(|| {
                (1..=10)
                    .filter_map(|n| expected.checked_sub(n))
                    .find(|&line| matches(line))
            })
    } else {
        None
    }
    .ok_or(NativeError::Message("scenario changed at return position"))?;
    parser.position = frame.position.clone();
    parser.position.line = line;
    parser.conditions = frame.conditions.clone();
    parser.label = frame.label.clone();
    Ok(())
}
fn initialize(heap: &mut Heap, object: ObjId) -> NativeResult<()> {
    heap.initialize_native_default::<State>(object)?;
    heap.with_native_state::<State, _>(object, |s| s.advanced = Some(Advanced::default()))
}
fn property(
    name: &'static str,
    get: tjs_core::NativeThunk,
    set: Option<tjs_core::NativeThunk>,
) -> NativeProperty {
    NativeProperty {
        name,
        doc: "",
        hidden: false,
        class_only: false,
        get: Some(NativeCallable::Leaf(get)),
        set: set.map(NativeCallable::Leaf),
    }
}
static CLASS: LazyLock<NativeClass> = LazyLock::new(|| {
    let base = bindings::implementation::CLASS;
    let mut properties = base.properties.to_vec();
    properties.extend([
        property("enableNP", enable_np, Some(set_enable_np)),
        property("fuzzyReturn", fuzzy, Some(set_fuzzy)),
        property("returnErrorStorage", return_error, Some(set_return_error)),
        property("multiLineTagEnabled", multiline, Some(set_multiline)),
        property("numericMacroArgumentsEnabled", numeric, Some(set_numeric)),
        property("currentLocalVariables", local, None),
        property("lf", local, None),
        property("prelf", previous, None),
        property("pmacros", pmacros, None),
        property("ifLevel", if_level, None),
        property("whileStackDepth", loop_depth, None),
        property("localVariablesDepth", local_depth, None),
        property("localVariables", locals, None),
    ]);
    NativeClass {
        initialize,
        properties: Box::leak(properties.into_boxed_slice()),
        ..base
    }
});
macro_rules! flag {
    ($get:ident,$set:ident,$state:ident,$field:expr) => {
        fn $get(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
            cx.with_state::<State, _>(|$state, _| Ok(Value::Int(i64::from($field))))
        }
        fn $set(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
            let value = args
                .first()
                .copied()
                .ok_or(NativeError::Missing(0))?
                .truthy(cx.heap())?;
            cx.with_state::<State, _>(|$state, _| {
                $field = value;
                Ok(Value::Void)
            })
        }
    };
}
flag!(enable_np, set_enable_np, s, s.parser.enable_np);
flag!(multiline, set_multiline, s, s.parser.multiline_tags);
flag!(
    fuzzy,
    set_fuzzy,
    s,
    s.advanced.as_mut().ok_or(NativeError::This)?.fuzzy
);
flag!(
    numeric,
    set_numeric,
    s,
    s.advanced.as_mut().ok_or(NativeError::This)?.numeric
);
fn local(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    cx.with_state::<State, _>(|s, cx| {
        s.advanced
            .as_ref()
            .ok_or(NativeError::This)?
            .local(cx.heap(), false)
    })
}
fn previous(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    cx.with_state::<State, _>(|s, cx| {
        s.advanced
            .as_ref()
            .ok_or(NativeError::This)?
            .local(cx.heap(), true)
    })
}
fn locals(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    cx.with_state::<State, _>(|s, _| {
        Ok(Value::Obj(ObjRef::bound(
            s.advanced
                .as_ref()
                .and_then(|a| a.locals)
                .ok_or(NativeError::This)?,
        )))
    })
}
fn pmacros(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    cx.with_state::<State, _>(|s, _| {
        Ok(Value::Obj(ObjRef::bound(
            s.advanced
                .as_ref()
                .and_then(|a| a.pmacros)
                .ok_or(NativeError::This)?,
        )))
    })
}
fn if_level(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    cx.with_state::<State, _>(|s, _| Ok(Value::Int(s.parser.conditions.len() as i64)))
}
fn loop_depth(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    cx.with_state::<State, _>(|s, _| {
        Ok(Value::Int(
            s.advanced.as_ref().ok_or(NativeError::This)?.loops.len() as i64,
        ))
    })
}
fn local_depth(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    cx.with_state::<State, _>(|s, cx| {
        Ok(Value::Int(
            cx.heap()
                .array(
                    s.advanced
                        .as_ref()
                        .and_then(|a| a.locals)
                        .ok_or(NativeError::This)?,
                )?
                .len() as i64,
        ))
    })
}
fn return_error(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    cx.with_state::<State, _>(|s, cx| {
        Ok(string(
            cx.heap_mut(),
            s.advanced
                .as_ref()
                .ok_or(NativeError::This)?
                .return_error
                .clone(),
        ))
    })
}
fn set_return_error(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let value = text(cx, *args.first().ok_or(NativeError::Missing(0))?)?;
    cx.with_state::<State, _>(|s, _| {
        s.advanced.as_mut().ok_or(NativeError::This)?.return_error = value;
        Ok(Value::Void)
    })
}
