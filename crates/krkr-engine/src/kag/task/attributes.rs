use super::*;
impl Task {
    pub(super) fn attribute(
        &mut self,
        cx: &mut NativeCx<'_>,
        tag: Tag,
        index: usize,
        condition: bool,
        name: Text,
        value: Value,
    ) -> NativeResult<Option<Text>> {
        if is(&name, "cond") {
            let exp = text(cx, value)?;
            self.commands.push(Command::ConditionValue { tag, index });
            Ok(Some(exp))
        } else {
            if cx
                .with_state::<State, _>(|s, _| Ok(s.advanced.as_ref().is_some_and(|a| a.numeric)))?
            {
                self.numeric_names.push(name.clone());
            }
            cx.with_state::<State, _>(|s, cx| s.put_tag(cx.heap_mut(), &name, value))?;
            self.commands.push(Command::Attributes {
                tag,
                index,
                condition,
            });
            Ok(None)
        }
    }

    pub(super) fn attributes(
        &mut self,
        cx: &mut NativeCx<'_>,
        tag: Tag,
        index: usize,
        condition: bool,
    ) -> NativeResult<Flow> {
        if !std::mem::take(&mut self.skip_parameter_fetch) {
            while self
                .parameters
                .last()
                .is_some_and(|frame| frame.index >= frame.count)
            {
                self.parameters.pop();
            }
            if let Some(frame) = self.parameters.last() {
                let request = Flow::Get {
                    object: frame.array,
                    key: Value::Int(frame.index as i64),
                };
                self.commands.push(Command::ParameterName {
                    tag,
                    index,
                    condition,
                });
                return Ok(request);
            }
        }
        let Some(attribute) = tag.attributes.get(index).cloned() else {
            self.commands.push(Command::Process { tag, condition });
            return Ok(Flow::Next);
        };
        if let Attribute::Named {
            name,
            value: Argument::Omitted,
        } = &attribute
        {
            let expanded = cx.with_state::<State, _>(|s, cx| {
                let Some(advanced) = s.advanced.as_ref() else {
                    return Ok(false);
                };
                let val = get(
                    cx.heap_mut(),
                    advanced.pmacros.ok_or(NativeError::This)?,
                    name,
                )?;
                if matches!(val, Value::Void) {
                    return Ok(false);
                };
                let dict = object(val)?;
                let members = cx.heap().members(dict)?.collect::<Vec<_>>();
                for (key, val) in members {
                    cx.heap_mut().set_member(s.tag()?, key, val)?;
                    if advanced.numeric {
                        self.numeric_names.push(cx.heap().symbol(key)?.to_vec());
                    }
                }
                Ok(true)
            })?;
            if expanded {
                self.commands.push(Command::Attributes {
                    tag,
                    index: index + 1,
                    condition,
                });
                return Ok(Flow::Next);
            }
        }
        let evaluate = cx.with_state::<State, _>(|s, _| {
            Ok(!s.parser.excluded() || (s.parser.process_special && is(&tag.name, "elsif")))
        })?;
        let skip_lookup = std::mem::take(&mut self.skip_parameter_lookup);
        if evaluate
            && !skip_lookup
            && let Attribute::Named { name, .. } = &attribute
        {
            let macros = cx.with_state::<State, _>(|s, _| Ok(s.param_macros))?;
            if let Some(macros) = macros {
                let key = string(cx.heap_mut(), name.clone());
                self.commands.push(Command::ParameterLookup {
                    tag,
                    index,
                    condition,
                });
                return Ok(Flow::Get {
                    object: Value::Obj(ObjRef::bound(macros)),
                    key,
                });
            }
        }
        match attribute {
            Attribute::Spread => {
                cx.with_state::<State, _>(|s, cx| {
                    if s.advanced.is_some() {
                        if !evaluate {
                            return Ok(());
                        }
                        if s.parser.macro_depth == 0 {
                            return Err(NativeError::Message("macro arguments outside macro"));
                        }
                    }
                    if let Some(args) = s.params_id() {
                        if s.extended {
                            let order = s.arg_order[s.parser.macro_depth - 1];
                            for name in cx.heap().array(order)?.to_vec() {
                                let name = text(cx, name)?;
                                if !is(&name, "tagname") {
                                    let value = get(cx.heap_mut(), args, &name)?;
                                    s.put_tag(cx.heap_mut(), &name, value)?;
                                }
                            }
                        } else {
                            copy(cx.heap_mut(), args, s.tag()?, s.advanced.is_none())?;
                            let val = string(cx.heap_mut(), tag.name.clone());
                            put(cx.heap_mut(), s.tag()?, &units("tagname"), val)?;
                        }
                    }
                    Ok(())
                })?;
                self.commands.push(Command::Attributes {
                    tag,
                    index: index + 1,
                    condition,
                });
            }
            Attribute::Named {
                name,
                value: Argument::Expression(expression),
            } if evaluate => {
                self.commands.push(Command::AttributeValue {
                    tag,
                    index: index + 1,
                    condition,
                    name,
                });
                return Ok(Flow::Evaluate(expression));
            }
            Attribute::Named {
                name,
                value: argument,
            } => {
                let value = match argument {
                    Argument::Omitted => string(cx.heap_mut(), units("true")),
                    Argument::Literal(ref raw)
                        if evaluate
                            && raw.first() == Some(&36)
                            && cx.with_state::<State, _>(|s, _| Ok(s.advanced.is_some()))? =>
                    {
                        let raw = &raw[1..];
                        cx.with_state::<State, _>(|s, cx| {
                            let args = object(
                                s.advanced
                                    .as_ref()
                                    .ok_or(NativeError::This)?
                                    .local(cx.heap(), false)?,
                            )?;
                            let split = raw.iter().position(|&u| u == 124);
                            let val = get(cx.heap_mut(), args, &raw[..split.unwrap_or(raw.len())])?;
                            if matches!(val, Value::Void) {
                                Ok(split.map_or(val, |at| {
                                    string(cx.heap_mut(), raw[at + 1..].to_vec())
                                }))
                            } else {
                                value::to_string(cx.heap_mut(), val).map_err(NativeError::from)
                            }
                        })?
                    }
                    Argument::Macro(raw) if evaluate => cx.with_state::<State, _>(|s, cx| {
                        if s.advanced.is_some() && s.parser.macro_depth == 0 {
                            return Err(NativeError::Message("macro arguments outside macro"));
                        }
                        if let Some(args) = s.params_id() {
                            let split = raw.iter().position(|&u| u == 124);
                            let key = &raw[..split.unwrap_or(raw.len())];
                            let value = get(cx.heap_mut(), args, key)?;
                            Ok(if matches!(value, Value::Void) {
                                split.map_or(value, |at| {
                                    string(cx.heap_mut(), raw[at + 1..].to_vec())
                                })
                            } else {
                                value
                            })
                        } else {
                            Ok(string(cx.heap_mut(), raw))
                        }
                    })?,
                    Argument::Literal(raw) | Argument::Expression(raw) | Argument::Macro(raw) => {
                        string(cx.heap_mut(), raw)
                    }
                };
                if evaluate {
                    if let Some(expression) =
                        self.attribute(cx, tag, index + 1, condition, name, value)?
                    {
                        return Ok(Flow::Evaluate(expression));
                    }
                } else {
                    cx.with_state::<State, _>(|s, cx| s.put_tag(cx.heap_mut(), &name, value))?;
                    self.commands.push(Command::Attributes {
                        tag,
                        index: index + 1,
                        condition,
                    });
                }
            }
        }
        Ok(Flow::Next)
    }
}
