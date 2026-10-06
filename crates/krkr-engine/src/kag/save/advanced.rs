use super::*;
use crate::kag::advanced::{Advanced, Loop, Scope};

/// assignStruct copies container trees and turns recursive edges into null.
fn tree(
    cx: &mut NativeCx<'_>,
    value: Value,
    path: &mut Vec<ObjId>,
    budget: &mut usize,
) -> NativeResult<Value> {
    if *budget == 0 || path.len() > 256 {
        return Err(NativeError::Message("ExtKAG saved structure limit"));
    }
    *budget -= 1;
    let Value::Obj(reference) = value else {
        return Ok(value);
    };
    let Some(id) = reference.object else {
        return Ok(value);
    };
    let kind = cx.heap().object(id)?.kind();
    if !matches!(
        kind,
        tjs_core::ObjectKind::Array | tjs_core::ObjectKind::Dictionary
    ) {
        return Ok(value);
    }
    if path.contains(&id) {
        return Ok(Value::Obj(ObjRef::default()));
    }
    path.push(id);
    let result = if kind == tjs_core::ObjectKind::Array {
        // This traversal invokes no script callbacks, so allocating the copy
        // cannot change the source array. Borrow only one value at a time.
        let len = cx.heap().array(id)?.len();
        let mut result = Vec::with_capacity(len);
        for index in 0..len {
            let item = cx.heap().array(id)?[index];
            result.push(tree(cx, item, path, budget)?);
        }
        array(cx, result)?
    } else {
        let target = cx.heap_mut().alloc_dictionary();
        let members = cx.heap().members(id)?.collect::<Vec<_>>();
        for (key, val) in members {
            let val = tree(cx, val, path, budget)?;
            cx.heap_mut().set_member(target, key, val)?;
        }
        Value::Obj(ObjRef::bound(target))
    };
    path.pop();
    Ok(result)
}
fn deep(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Value> {
    tree(cx, value, &mut Vec::new(), &mut 1_000_000)
}
pub(super) fn store(cx: &mut NativeCx<'_>, id: ObjId, state: &State) -> NativeResult<()> {
    let a = state.advanced.as_ref().ok_or(NativeError::This)?;
    for (name, object) in [("LocalVariables", a.locals), ("paramMacros", a.pmacros)] {
        let value = deep(
            cx,
            Value::Obj(ObjRef::bound(object.ok_or(NativeError::This)?)),
        )?;
        put(cx.heap_mut(), id, &units(name), value)?;
    }
    for name in ["macros", "macroArgs"] {
        let value = read(cx, id, name)?;
        let value = deep(cx, value)?;
        put(cx.heap_mut(), id, &units(name), value)?;
    }
    set_int(cx, id, "IgnoreCR", usize::from(state.parser.ignore_cr))?;
    set_int(cx, id, "EnableNP", usize::from(state.parser.enable_np))?;
    let calls = read_array(cx, id, "callStack")?;
    for (index, call) in calls.iter().enumerate() {
        let call = object(*call)?;
        let scope = a
            .calls
            .get(index)
            .ok_or(NativeError::Message("missing call scope"))?;
        set_int(cx, call, "WhileStackDepth", scope.loops)?;
        set_int(cx, call, "LocalVariablesCount", scope.locals)?;
        compress(
            cx,
            call,
            &a.loops[if index == 0 {
                0
            } else {
                a.calls[index - 1].loops
            }..scope.loops.saturating_sub(1)],
        )?;
    }
    compress(cx, id, &a.loops[a.calls.last().map_or(0, |s| s.loops)..])?;
    let mut loops = Vec::new();
    let mut previous_exp = Text::new();
    let mut previous_each = Text::new();
    for saved in &a.loops {
        let item = object(write_frame(cx, &saved.frame)?)?;
        set_text(cx, item, "WhileLevelExp", previous_exp)?;
        set_text(cx, item, "WhileLevelEach", previous_each)?;
        previous_exp = saved.exp.clone();
        previous_each = saved.each.clone();
        // Reference while frames only carry depth/exclusion, not the if stacks.
        for name in [
            "ExcludeLevelStack",
            "IfLevelExecutedStack",
            "macroArgStackBase",
            "macroArgStackDepth",
        ] {
            let key = cx.heap_mut().intern(&units(name));
            cx.heap_mut().remove_member(item, key)?;
        }
        loops.push(Value::Obj(ObjRef::bound(item)));
    }
    set_text(cx, id, "WhileLevelExp", previous_exp)?;
    set_text(cx, id, "WhileLevelEach", previous_each)?;
    let loops = array(cx, loops)?;
    put(cx.heap_mut(), id, &units("whileStack"), loops)
}
fn compress(cx: &mut NativeCx<'_>, id: ObjId, loops: &[Loop]) -> NativeResult<()> {
    let excluded = read_text(cx, id, "ExcludeLevelStack")?;
    let executed = read_text(cx, id, "IfLevelExecutedStack")?;
    let keep: Vec<_> = (0..executed.len())
        .filter(|&index| {
            !loops
                .iter()
                .any(|l| !l.marker && l.frame.conditions.len() == index)
        })
        .collect();
    set_text(
        cx,
        id,
        "ExcludeLevelStack",
        keep.iter()
            .flat_map(|&i| excluded[i * 8..i * 8 + 8].iter().copied())
            .collect(),
    )?;
    set_text(
        cx,
        id,
        "IfLevelExecutedStack",
        keep.iter().map(|&i| executed[i]).collect(),
    )
}
fn expand(cx: &mut NativeCx<'_>, id: ObjId, loops: &[(usize, i64)]) -> NativeResult<()> {
    let depth = read_int(cx, id, "IfLevel")?;
    let stack = read_text(cx, id, "ExcludeLevelStack")?;
    let executed = read_text(cx, id, "IfLevelExecutedStack")?;
    if depth > 256 || stack.len() != executed.len() * 8 || executed.len() + loops.len() != depth {
        return Err(NativeError::Message("malformed ExtKAG conditional stack"));
    }
    let mut seen = std::collections::HashSet::new();
    if loops
        .iter()
        .any(|(at, _)| *at >= depth || !seen.insert(*at))
    {
        return Err(NativeError::Message("invalid loop condition depth"));
    }
    let mut full = Text::new();
    let mut flags = Text::new();
    let mut cursor = 0;
    for i in 0..depth {
        if let Some((_, exclude)) = loops.iter().find(|(at, _)| *at == i) {
            full.extend(units(&format!("{:08x}", *exclude as i32 as u32)));
            flags.push(49);
        } else {
            full.extend_from_slice(&stack[cursor * 8..cursor * 8 + 8]);
            flags.push(executed[cursor]);
            cursor += 1;
        }
    }
    set_text(cx, id, "ExcludeLevelStack", full)?;
    set_text(cx, id, "IfLevelExecutedStack", flags)
}
pub(super) fn restore(cx: &mut NativeCx<'_>, id: ObjId) -> NativeResult<(ObjId, Advanced)> {
    let copy = deep(cx, Value::Obj(ObjRef::bound(id)))?;
    let id = object(copy)?;
    let locals = object(read(cx, id, "LocalVariables")?)?;
    let pmacros = object(read(cx, id, "paramMacros")?)?;
    if cx.heap().array(locals)?.len() > 256 {
        return Err(NativeError::Message("local variable depth limit"));
    }
    let mut a = Advanced {
        locals: Some(locals),
        pmacros: Some(pmacros),
        ..Advanced::default()
    };
    let calls = read_array(cx, id, "callStack")?;
    let loops = read_array(cx, id, "whileStack")?;
    if calls.len() > 256 || loops.len() > 256 {
        return Err(NativeError::Message("ExtKAG stack limit"));
    }
    for call in &calls {
        let call = object(*call)?;
        let scope = Scope {
            loops: read_int(cx, call, "WhileStackDepth")?,
            locals: read_int(cx, call, "LocalVariablesCount")?,
        };
        if scope.loops == 0
            || scope.loops > loops.len()
            || scope.locals == 0
            || scope.locals > cx.heap().array(locals)?.len()
            || a.calls.last().is_some_and(|previous| {
                previous.loops >= scope.loops || previous.locals >= scope.locals
            })
        {
            return Err(NativeError::Message("malformed saved scope"));
        }
        a.calls.push(scope);
    }
    let loop_info =
        |cx: &mut NativeCx<'_>, start: usize, end: usize| -> NativeResult<Vec<(usize, i64)>> {
            (start..end)
                .filter(|&i| !a.calls.iter().any(|c| c.loops == i + 1))
                .map(|i| {
                    let item = object(loops[i])?;
                    let depth = read_int(cx, item, "IfLevel")?;
                    let val = read(cx, item, "ExcludeLevel")?;
                    Ok((depth, value::to_integer(cx.heap(), val)?))
                })
                .collect()
        };
    for (i, call) in calls.iter().enumerate() {
        let start = if i == 0 { 0 } else { a.calls[i - 1].loops };
        let info = loop_info(cx, start, a.calls[i].loops - 1)?;
        expand(cx, object(*call)?, &info)?;
    }
    let info = loop_info(cx, a.calls.last().map_or(0, |s| s.loops), loops.len())?;
    expand(cx, id, &info)?;
    for (i, item) in loops.iter().enumerate() {
        let item = object(*item)?;
        let parent = a
            .calls
            .iter()
            .enumerate()
            .find(|(_, c)| c.loops > i)
            .map_or(id, |(index, _)| object(calls[index]).unwrap());
        let depth = read_int(cx, item, "IfLevel")?;
        let mut conditions = read_conditions(cx, parent)?;
        if depth > conditions.len() {
            return Err(NativeError::Message("invalid saved loop depth"));
        }
        conditions.truncate(depth);
        let exclusion = read(cx, item, "ExcludeLevel")?;
        if let Some(last) = conditions.last_mut() {
            last.excluded = value::to_integer(cx.heap(), exclusion)? != -1;
        }
        write_conditions(cx, item, &conditions)?;
        set_int(cx, item, "macroArgStackBase", 0)?;
        set_int(cx, item, "macroArgStackDepth", 0)?;
        let next = if i + 1 < loops.len() {
            object(loops[i + 1])?
        } else {
            id
        };
        a.loops.push(Loop {
            frame: read_frame(cx, item)?,
            exp: read_text(cx, next, "WhileLevelExp")?,
            each: read_text(cx, next, "WhileLevelEach")?,
            marker: a.calls.iter().any(|c| c.loops == i + 1),
        });
    }
    Ok((id, a))
}
pub(super) fn apply(
    cx: &mut NativeCx<'_>,
    state: &mut State,
    saved: &Advanced,
) -> NativeResult<()> {
    let target = state.advanced.as_mut().ok_or(NativeError::This)?;
    let values = cx
        .heap()
        .array(saved.locals.ok_or(NativeError::This)?)?
        .to_vec();
    cx.heap_mut()
        .array_replace(target.locals.ok_or(NativeError::This)?, values)?;
    copy(
        cx.heap_mut(),
        saved.pmacros.ok_or(NativeError::This)?,
        target.pmacros.ok_or(NativeError::This)?,
        true,
    )?;
    target.loops = saved.loops.clone();
    target.calls = saved.calls.clone();
    Ok(())
}
