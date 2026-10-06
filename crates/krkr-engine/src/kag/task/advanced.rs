use super::*;
use crate::kag::advanced::{Loop, Scope};

pub(super) enum AdvancedCommand {
    WhileInit { exp: Text, each: Text },
    WhileCondition,
    LoopEach { tag: Tag, continuing: bool },
    LoopResult { tag: Tag, continuing: bool },
    PushLocal { call: Option<(Text, Text)> },
}
impl Task {
    pub(super) fn advanced(
        &mut self,
        cx: &mut NativeCx<'_>,
        command: AdvancedCommand,
        value: Value,
    ) -> NativeResult<Flow> {
        match command {
            AdvancedCommand::WhileInit { exp, each } => {
                if exp.is_empty() {
                    return Err(NativeError::Message("while requires exp"));
                }
                cx.with_state::<State, _>(|s, _| {
                    let frame = s
                        .advanced
                        .as_mut()
                        .and_then(|a| a.loops.last_mut())
                        .ok_or(NativeError::This)?;
                    frame.exp = exp.clone();
                    frame.each = each;
                    Ok(())
                })?;
                self.commands
                    .push(Command::Advanced(AdvancedCommand::WhileCondition));
                Ok(Flow::Evaluate(exp))
            }
            AdvancedCommand::WhileCondition => {
                let condition = value.truthy(cx.heap())?;
                cx.with_state::<State, _>(|s, _| {
                    let last = s.parser.conditions.last_mut().ok_or(NativeError::This)?;
                    last.excluded = last.parent_excluded || !condition;
                    Ok(())
                })?;
                self.commands.push(Command::Next);
                Ok(Flow::Next)
            }
            AdvancedCommand::LoopEach { tag, continuing } => {
                let exp = cx.with_state::<State, _>(|s, _| {
                    Ok(s.advanced
                        .as_ref()
                        .ok_or(NativeError::This)?
                        .check_loop(&s.parser)?
                        .exp
                        .clone())
                })?;
                self.commands
                    .push(Command::Advanced(AdvancedCommand::LoopResult {
                        tag,
                        continuing,
                    }));
                Ok(Flow::Evaluate(exp))
            }
            AdvancedCommand::LoopResult { tag, continuing } => {
                let again = value.truthy(cx.heap())?;
                cx.with_state::<State, _>(|s, _| {
                    if continuing {
                        s.parser
                            .conditions
                            .last_mut()
                            .ok_or(NativeError::This)?
                            .excluded = !again;
                    } else {
                        loop_end(s, &tag, again)?;
                    }
                    Ok(())
                })?;
                self.commands.push(Command::Next);
                Ok(Flow::Next)
            }
            AdvancedCommand::PushLocal { call } => {
                let copy_parent = value.truthy(cx.heap())?;
                cx.with_state::<State, _>(|s, cx| {
                    let tag = s.tag()?;
                    s.advanced.as_mut().ok_or(NativeError::This)?.push_local(
                        cx.heap_mut(),
                        tag,
                        copy_parent,
                    )?;
                    if call.is_some() {
                        push_call(s, cx)?;
                    }
                    Ok(())
                })?;
                if let Some((storage, label)) = call {
                    self.commands.push(Command::Restart);
                    self.commands.push(Command::Go { storage, label });
                } else {
                    self.commands.push(Command::Next);
                }
                Ok(Flow::Next)
            }
        }
    }
    pub(super) fn advanced_tag(
        &mut self,
        cx: &mut NativeCx<'_>,
        tag: &Tag,
        condition: bool,
        excluded: bool,
    ) -> NativeResult<Option<Flow>> {
        if is(&tag.name, "while") {
            let init = if excluded {
                Text::new()
            } else {
                Self::field_text(cx, "init")?
            };
            let exp = if excluded {
                Text::new()
            } else {
                Self::field_text(cx, "exp")?
            };
            let each = if excluded {
                Text::new()
            } else {
                Self::field_text(cx, "each")?
            };
            cx.with_state::<State, _>(|s, _| {
                s.parser.finish_tag(tag);
                let frame = s.parser.call_frame().map_err(error)?;
                let advanced = s.advanced.as_mut().ok_or(NativeError::This)?;
                if advanced.loops.len() >= 256 {
                    return Err(NativeError::Message("while depth limit"));
                }
                advanced.loops.push(Loop {
                    frame,
                    exp: Text::new(),
                    each: Text::new(),
                    marker: false,
                });
                s.parser.begin_if(true).map_err(error)
            })?;
            if excluded {
                self.commands.push(Command::Next);
                return Ok(Some(Flow::Next));
            }
            self.commands
                .push(Command::Advanced(AdvancedCommand::WhileInit { exp, each }));
            return Ok(Some(if init.is_empty() {
                Flow::Next
            } else {
                Flow::Evaluate(init)
            }));
        }
        if is(&tag.name, "endwhile")
            || (condition && !excluded && (is(&tag.name, "break") || is(&tag.name, "continue")))
        {
            let each = cx.with_state::<State, _>(|s, _| {
                Ok(s.advanced
                    .as_ref()
                    .ok_or(NativeError::This)?
                    .check_loop(&s.parser)?
                    .each
                    .clone())
            })?;
            let continuing = !is(&tag.name, "endwhile");
            if excluded || continuing {
                cx.with_state::<State, _>(|s, _| {
                    loop_end(s, tag, continuing)?;
                    if is(&tag.name, "break") {
                        s.parser
                            .conditions
                            .last_mut()
                            .ok_or(NativeError::This)?
                            .excluded = true;
                    }
                    Ok(())
                })?;
            }
            if excluded || is(&tag.name, "break") {
                self.commands.push(Command::Next);
                return Ok(Some(Flow::Next));
            }
            self.commands
                .push(Command::Advanced(AdvancedCommand::LoopEach {
                    tag: tag.clone(),
                    continuing,
                }));
            return Ok(Some(if each.is_empty() {
                Flow::Next
            } else {
                Flow::Evaluate(each)
            }));
        }
        if !condition || excluded {
            return Ok(None);
        }
        if tag.name.first() == Some(&38) || is(&tag.name, "emb") {
            let exp = if is(&tag.name, "emb") {
                super::control::required_expression(cx)?
            } else {
                tag.name[1..].to_vec()
            };
            self.commands.push(Command::Embedded(tag.clone()));
            return Ok(Some(Flow::Evaluate(exp)));
        }
        if cx.with_state::<State, _>(|s, cx| {
            Ok(!matches!(
                get(cx.heap_mut(), s.macros_id()?, &tag.name)?,
                Value::Void
            ))
        })? {
            return Ok(None);
        }
        if is(&tag.name, "pushlocalvar") {
            let value =
                cx.with_state::<State, _>(|s, cx| get(cx.heap_mut(), s.tag()?, &units("copyvar")))?;
            cx.with_state::<State, _>(|s, _| {
                s.parser.finish_tag(tag);
                Ok(())
            })?;
            self.commands
                .push(Command::Advanced(AdvancedCommand::PushLocal { call: None }));
            return Ok(Some(if matches!(value, Value::Void) {
                Flow::Input(Value::Int(0))
            } else {
                Flow::Evaluate(text(cx, value)?)
            }));
        }
        let handled = cx.with_state::<State, _>(|s, cx| {
            let dictionary = s.tag()?;
            let advanced = s.advanced.as_mut().ok_or(NativeError::This)?;
            if is(&tag.name, "localvar") {
                let local = object(advanced.local(cx.heap(), false)?)?;
                copy(cx.heap_mut(), dictionary, local, false)?;
            } else if is(&tag.name, "poplocalvar") {
                advanced.pop_local(cx.heap_mut())?;
            } else if is(&tag.name, "pmacro") {
                let name = get(cx.heap_mut(), dictionary, &units("name"))?;
                if matches!(name, Value::Void) {
                    return Err(NativeError::Message("pmacro requires name"));
                }
                let name = text(cx, name)?;
                let dict = clone_dictionary(cx.heap_mut(), dictionary)?;
                for key in ["name", "tagname"] {
                    let key = cx.heap_mut().intern(&units(key));
                    cx.heap_mut().remove_member(dict, key)?;
                }
                put(
                    cx.heap_mut(),
                    advanced.pmacros.ok_or(NativeError::This)?,
                    &name,
                    Value::Obj(ObjRef::bound(dict)),
                )?;
            } else {
                return Ok(false);
            }
            s.parser.finish_tag(tag);
            Ok(true)
        })?;
        if handled {
            self.commands.push(Command::Next);
            Ok(Some(Flow::Next))
        } else {
            Ok(None)
        }
    }
}
fn loop_end(s: &mut State, tag: &Tag, again: bool) -> NativeResult<()> {
    let a = s.advanced.as_mut().ok_or(NativeError::This)?;
    let saved = a.check_loop(&s.parser)?.clone();
    if again {
        // Preserve current macro arguments; while restores only cursor/condition state.
        crate::kag::advanced::return_position(&mut s.parser, &saved.frame, false)?;
        s.parser.begin_if(true).map_err(error)?;
    } else {
        s.parser.conditions = saved.frame.conditions;
        a.loops.pop();
        s.parser.finish_tag(tag);
    }
    Ok(())
}
pub(super) fn push_call(s: &mut State, cx: &mut NativeCx<'_>) -> NativeResult<()> {
    s.parser.push_call().map_err(error)?;
    let frame = s.parser.calls.last().ok_or(NativeError::This)?.clone();
    let a = s.advanced.as_mut().ok_or(NativeError::This)?;
    a.loops.push(Loop {
        frame,
        exp: Text::new(),
        each: Text::new(),
        marker: true,
    });
    a.calls.push(Scope {
        loops: a.loops.len(),
        locals: cx.heap().array(a.locals.ok_or(NativeError::This)?)?.len(),
    });
    Ok(())
}
