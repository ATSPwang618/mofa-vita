//! Ordinary TJS dictionaries/arrays, including the original hexadecimal
//! condition stacks. No native pointer or live VM frame is serialized.
use super::*;
use krkr_kag::{CallFrame, Condition, Position};
use tjs_core::NativeStep;
mod advanced;

pub(super) struct Restored {
    pub parser: Parser,
    macros: Option<ObjId>,
    param_macros: Option<ObjId>,
    args: Vec<ObjId>,
    arg_order: Vec<ObjId>,
    advanced: Option<crate::kag::advanced::Advanced>,
}
impl Trace for Restored {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(macros) = self.macros {
            visit(Value::Obj(macros.into()));
        }
        if let Some(macros) = self.param_macros {
            visit(Value::Obj(macros.into()));
        }
        for &order in &self.arg_order {
            visit(Value::Obj(order.into()));
        }
        for &id in &self.args {
            visit(Value::Obj(id.into()));
        }
        self.advanced.trace(visit);
    }
}
pub(super) fn prepare(cx: &mut NativeCx<'_>, restored: &Restored) -> NativeResult<()> {
    cx.with_state::<State, _>(|state, cx| {
        state.parser.clear_buffer();
        if let Some(macros) = restored.macros {
            copy(cx.heap_mut(), macros, state.macros_id()?, true)?;
        }
        state.args = restored.args.clone();
        state.arg_order = restored.arg_order.clone();
        if let (Some(source), Some(target)) = (restored.param_macros, state.param_macros) {
            copy(cx.heap_mut(), source, target, true)?;
        }
        state.parser.calls = restored.parser.calls.clone();
        if let Some(a) = &restored.advanced {
            advanced::apply(cx, state, a)?;
        }
        // Load callbacks see the saved macro arguments, just as in Restore.
        state.parser.macro_base = restored.parser.macro_depth;
        state.parser.macro_depth = restored.parser.macro_depth;
        Ok(())
    })
}
fn set_text(cx: &mut NativeCx<'_>, id: ObjId, key: &str, text: Text) -> NativeResult<()> {
    let val = string(cx.heap_mut(), text);
    put(cx.heap_mut(), id, &units(key), val)
}
fn set_int(cx: &mut NativeCx<'_>, id: ObjId, key: &str, val: usize) -> NativeResult<()> {
    put(cx.heap_mut(), id, &units(key), Value::Int(val as i64))
}
fn read(cx: &mut NativeCx<'_>, id: ObjId, key: &str) -> NativeResult<Value> {
    get(cx.heap_mut(), id, &units(key))
}
fn read_text(cx: &mut NativeCx<'_>, id: ObjId, key: &str) -> NativeResult<Text> {
    let val = read(cx, id, key)?;
    text(cx, val)
}
fn read_int(cx: &mut NativeCx<'_>, id: ObjId, key: &str) -> NativeResult<usize> {
    let val = read(cx, id, key)?;
    usize::try_from(value::to_integer(cx.heap(), val)?)
        .map_err(|_| NativeError::Message("negative KAG saved position/depth"))
}
fn array(cx: &mut NativeCx<'_>, items: Vec<Value>) -> NativeResult<Value> {
    Ok(Value::Obj(ObjRef::bound(
        cx.heap_mut().alloc_array_from(&items)?,
    )))
}
fn read_array(cx: &mut NativeCx<'_>, id: ObjId, key: &str) -> NativeResult<Vec<Value>> {
    let val = read(cx, id, key)?;
    if matches!(val, Value::Void) {
        return Ok(Vec::new());
    }
    Ok(cx.heap().array(object(val)?)?.to_vec())
}
fn write_conditions(
    cx: &mut NativeCx<'_>,
    id: ObjId,
    conditions: &[Condition],
) -> NativeResult<()> {
    let mut previous = -1i32;
    let mut stack = String::new();
    let mut executed = Vec::new();
    for (index, condition) in conditions.iter().enumerate() {
        stack.push_str(&format!("{:08x}", previous as u32));
        executed.push(if condition.executed { 49 } else { 48 });
        if !condition.parent_excluded {
            previous = if condition.excluded {
                (index + 1) as i32
            } else {
                -1
            };
        }
    }
    put(
        cx.heap_mut(),
        id,
        &units("ExcludeLevel"),
        Value::Int(previous as i64),
    )?;
    set_int(cx, id, "IfLevel", conditions.len())?;
    set_text(cx, id, "ExcludeLevelStack", units(&stack))?;
    set_text(cx, id, "IfLevelExecutedStack", executed)
}
fn read_conditions(cx: &mut NativeCx<'_>, id: ObjId) -> NativeResult<Vec<Condition>> {
    let stack = read_text(cx, id, "ExcludeLevelStack")?;
    let executed = read_text(cx, id, "IfLevelExecutedStack")?;
    let depth = read_int(cx, id, "IfLevel")?;
    if depth > 256 || stack.len() != depth * 8 || executed.len() != depth {
        return Err(NativeError::Message("malformed KAG conditional stack"));
    }
    let mut levels = Vec::new();
    for chunk in stack.as_chunks::<8>().0 {
        let text = String::from_utf16_lossy(chunk);
        levels.push(
            u32::from_str_radix(&text, 16)
                .map_err(|_| NativeError::Message("malformed KAG conditional stack"))?
                as i32,
        );
    }
    let val = read(cx, id, "ExcludeLevel")?;
    levels.push(value::to_integer(cx.heap(), val)? as i32);
    Ok((0..depth)
        .map(|i| Condition {
            parent_excluded: levels[i] != -1,
            executed: executed[i] == 49,
            excluded: levels[i + 1] != -1,
        })
        .collect())
}
fn write_position(
    cx: &mut NativeCx<'_>,
    id: ObjId,
    position: &Position,
    pos_key: &str,
) -> NativeResult<()> {
    set_int(cx, id, pos_key, position.pos)?;
    set_int(
        cx,
        id,
        "lineBufferUsing",
        usize::from(position.buffer.is_some()),
    )?;
    set_text(
        cx,
        id,
        "lineBuffer",
        position.buffer.as_deref().cloned().unwrap_or_default(),
    )
}
fn read_position(cx: &mut NativeCx<'_>, id: ObjId, pos_key: &str) -> NativeResult<Position> {
    let pos = read_int(cx, id, pos_key)?;
    let buffer = read_text(cx, id, "lineBuffer")?;
    let using = read_int(cx, id, "lineBufferUsing")? != 0;
    if using && pos > buffer.len() {
        return Err(NativeError::Message(
            "KAG saved column exceeds expanded line",
        ));
    }
    Ok(Position {
        line: 0,
        pos,
        buffer: using.then(|| Arc::new(buffer)),
    })
}
fn write_frame(cx: &mut NativeCx<'_>, frame: &CallFrame) -> NativeResult<Value> {
    let id = cx.heap_mut().alloc_dictionary();
    for (key, value) in [
        ("storage", frame.storage.clone()),
        ("label", frame.label.clone()),
        ("orgLineStr", frame.original_line.clone()),
    ] {
        set_text(cx, id, key, value)?;
    }
    for (key, val) in [
        ("offset", frame.offset),
        ("macroArgStackBase", frame.macro_base),
        ("macroArgStackDepth", frame.macro_depth),
    ] {
        set_int(cx, id, key, val)?;
    }
    write_position(cx, id, &frame.position, "pos")?;
    write_conditions(cx, id, &frame.conditions)?;
    Ok(Value::Obj(ObjRef::bound(id)))
}
fn read_frame(cx: &mut NativeCx<'_>, id: ObjId) -> NativeResult<CallFrame> {
    Ok(CallFrame {
        storage: read_text(cx, id, "storage")?,
        label: read_text(cx, id, "label")?,
        offset: read_int(cx, id, "offset")?,
        original_line: read_text(cx, id, "orgLineStr")?,
        position: read_position(cx, id, "pos")?,
        conditions: read_conditions(cx, id)?,
        macro_base: read_int(cx, id, "macroArgStackBase")?,
        macro_depth: read_int(cx, id, "macroArgStackDepth")?,
    })
}
pub(super) fn store(cx: &mut NativeCx<'_>, state: &State) -> NativeResult<Value> {
    let id = cx.heap_mut().alloc_dictionary();
    let macros = clone_dictionary(cx.heap_mut(), state.macros_id()?)?;
    put(
        cx.heap_mut(),
        id,
        &units("macros"),
        Value::Obj(ObjRef::bound(macros)),
    )?;
    if let Some(macros) = state.param_macros {
        let macros = clone_dictionary(cx.heap_mut(), macros)?;
        put(
            cx.heap_mut(),
            id,
            &units("paramMacros"),
            Value::Obj(ObjRef::bound(macros)),
        )?;
    }
    let mut args = Vec::new();
    for (index, &arg) in state.args[..state.parser.macro_depth].iter().enumerate() {
        args.push(if state.extended {
            ordered_pairs(cx, arg, state.arg_order[index])?
        } else {
            Value::Obj(ObjRef::bound(clone_dictionary(cx.heap_mut(), arg)?))
        });
    }
    let args = array(cx, args)?;
    put(cx.heap_mut(), id, &units("macroArgs"), args)?;
    let calls = state
        .parser
        .calls
        .iter()
        .map(|f| write_frame(cx, f))
        .collect::<NativeResult<Vec<_>>>()?;
    let calls = array(cx, calls)?;
    put(cx.heap_mut(), id, &units("callStack"), calls)?;
    for (key, val) in [
        ("storageName", state.parser.storage.clone()),
        (
            "storageShortName",
            krkr_assets::name::split_name(&state.parser.storage)
                .1
                .to_vec(),
        ),
        ("curLabel", state.parser.label.clone()),
    ] {
        set_text(cx, id, key, val)?;
    }
    for (key, val) in [
        ("curLine", state.parser.position.line),
        ("macroArgStackBase", state.parser.macro_base),
        ("macroArgStackDepth", state.parser.macro_depth),
    ] {
        set_int(cx, id, key, val)?;
    }
    write_position(cx, id, &state.parser.position, "curPos")?;
    write_conditions(cx, id, &state.parser.conditions)?;
    if state.advanced.is_some() {
        advanced::store(cx, id, state)?;
    }
    Ok(Value::Obj(ObjRef::bound(id)))
}
pub(super) fn restore(cx: &mut NativeCx<'_>, id: ObjId) -> NativeResult<NativeStep> {
    let extended = cx.with_state::<State, _>(|s, _| Ok(s.extended))?;
    let (id, advanced) = if cx.with_state::<State, _>(|s, _| Ok(s.advanced.is_some()))? {
        let (id, a) = advanced::restore(cx, id)?;
        (id, Some(a))
    } else {
        (id, None)
    };
    let storage = read_text(cx, id, "storageName")?;
    let label = read_text(cx, id, "curLabel")?;
    // KAG deliberately stores void when saveMacros is disabled. In that case
    // Restore leaves the current macro dictionary (including load hooks) alone.
    let macros = match read(cx, id, "macros")? {
        Value::Void => None,
        value => Some(clone_dictionary(cx.heap_mut(), object(value)?)?),
    };
    let mut args = Vec::new();
    let param_macros = if extended {
        match read(cx, id, "paramMacros")? {
            Value::Void => None,
            value => Some(clone_dictionary(cx.heap_mut(), object(value)?)?),
        }
    } else {
        None
    };
    let mut arg_order = Vec::new();
    for value in read_array(cx, id, "macroArgs")? {
        if extended {
            let pairs = cx.heap().array(object(value)?)?.to_vec();
            let dict = cx.heap_mut().alloc_dictionary();
            let order = cx.heap_mut().alloc_array();
            for pair in pairs.chunks(2) {
                if pair.len() != 2 {
                    break;
                }
                let key = text(cx, pair[0])?;
                put(cx.heap_mut(), dict, &key, pair[1])?;
                cx.heap_mut().array_push(order, pair[0])?;
            }
            args.push(dict);
            arg_order.push(order);
        } else {
            args.push(clone_dictionary(cx.heap_mut(), object(value)?)?);
        }
    }
    let mut calls = Vec::new();
    for value in read_array(cx, id, "callStack")? {
        calls.push(read_frame(cx, object(value)?)?);
    }
    let depth = read_int(cx, id, "macroArgStackDepth")?;
    let base = read_int(cx, id, "macroArgStackBase")?;
    if depth != args.len()
        || base > depth
        || depth > 256
        || calls.len() > 256
        || calls
            .iter()
            .any(|f| f.macro_base > f.macro_depth || f.macro_depth > depth)
    {
        return Err(NativeError::Message("malformed KAG macro/call stack"));
    }
    let mut parser = Parser::default();
    parser.storage = storage;
    parser.label = label;
    parser.conditions = read_conditions(cx, id)?;
    parser.calls = calls;
    parser.macro_base = base;
    parser.macro_depth = depth;
    if advanced.is_some() {
        let val = read(cx, id, "IgnoreCR")?;
        parser.ignore_cr = val.truthy(cx.heap())?;
        let val = read(cx, id, "EnableNP")?;
        parser.enable_np = val.truthy(cx.heap())?;
    }
    task::Task::restore(
        cx,
        Restored {
            parser,
            macros,
            param_macros,
            args,
            arg_order,
            advanced,
        },
    )
}
pub(super) fn apply(cx: &mut NativeCx<'_>, restored: Restored) -> NativeResult<()> {
    cx.with_state::<State, _>(|state, cx| {
        // Original restore resumes at the saved label. assign is the exact
        // cursor copy operation; curLine/curPos in the store are informational.
        state.parser.goto(&restored.parser.label).map_err(error)?;
        state.parser.conditions = restored.parser.conditions;
        state.parser.calls = restored.parser.calls;
        state.parser.macro_base = restored.parser.macro_base;
        state.parser.macro_depth = restored.parser.macro_depth;
        if let Some(a) = &restored.advanced {
            advanced::apply(cx, state, a)?;
            state.parser.ignore_cr = restored.parser.ignore_cr;
            state.parser.enable_np = restored.parser.enable_np;
        }
        if let Some(macros) = restored.macros {
            copy(cx.heap_mut(), macros, state.macros_id()?, true)?;
        }
        state.args = restored.args;
        state.arg_order = restored.arg_order;
        if let (Some(source), Some(target)) = (restored.param_macros, state.param_macros) {
            copy(cx.heap_mut(), source, target, true)?;
        }
        Ok(())
    })
}
pub(super) fn assign(cx: &mut NativeCx<'_>, source: ObjId) -> NativeResult<()> {
    if source == cx.this() {
        return Ok(());
    }
    let (parser, macros, args, debug, extended, param_macros, arg_order, advanced) =
        cx.heap_mut().with_native_state::<State, _>(source, |s| {
            (
                s.parser.clone(),
                s.macros,
                s.args[..s.parser.macro_depth].to_vec(),
                s.debug_level,
                s.extended,
                s.param_macros,
                s.arg_order.clone(),
                s.advanced.clone(),
            )
        })?;
    if cx.with_state::<State, _>(|s, _| {
        Ok(s.extended != extended || s.advanced.is_some() != advanced.is_some())
    })? {
        return Err(NativeError::Message("incompatible KAGParser class"));
    }
    let macros = macros.ok_or(NativeError::This)?;
    let args = args
        .into_iter()
        .map(|id| clone_dictionary(cx.heap_mut(), id))
        .collect::<NativeResult<Vec<_>>>()?;
    let arg_order = arg_order
        .into_iter()
        .map(|id| {
            let values = cx.heap().array(id)?.to_vec();
            cx.heap_mut()
                .alloc_array_from(&values)
                .map_err(NativeError::from)
        })
        .collect::<NativeResult<Vec<_>>>()?;
    cx.with_state::<State, _>(|state, cx| {
        let special = state.parser.process_special;
        let interrupted = state.parser.interrupted;
        let multiline = state.parser.multiline_tags;
        state.parser = parser;
        state.parser.process_special = special;
        state.parser.interrupted = interrupted;
        state.parser.multiline_tags = multiline;
        state.debug_level = debug;
        state.args = args;
        state.arg_order = arg_order;
        if let Some(a) = &advanced {
            advanced::apply(cx, state, a)?;
            let target = state.advanced.as_mut().ok_or(NativeError::This)?;
            target.fuzzy = a.fuzzy;
            target.return_error = a.return_error.clone();
        }
        if let (Some(source), Some(target)) = (param_macros, state.param_macros) {
            copy(cx.heap_mut(), source, target, true)?;
        }
        copy(cx.heap_mut(), macros, state.macros_id()?, true)
    })
}
fn ordered_pairs(cx: &mut NativeCx<'_>, dict: ObjId, order: ObjId) -> NativeResult<Value> {
    let mut pairs = Vec::new();
    for name in cx.heap().array(order)?.to_vec() {
        let key = text(cx, name)?;
        let key = cx.heap_mut().intern(&key);
        if let Some(value) = cx.heap().member(dict, key)? {
            pairs.extend([name, value]);
        }
    }
    array(cx, pairs)
}
