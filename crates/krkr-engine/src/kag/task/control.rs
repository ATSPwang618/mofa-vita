use super::*;
impl Task {
    pub(super) fn process_tag(
        &mut self,
        cx: &mut NativeCx<'_>,
        tag: Tag,
        condition: bool,
    ) -> NativeResult<Flow> {
        let (special, excluded) =
            cx.with_state::<State, _>(|s, _| Ok((s.parser.process_special, s.parser.excluded())))?;
        let advanced = cx.with_state::<State, _>(|s, _| Ok(s.advanced.is_some()))?;
        if special && advanced {
            if ["elsif", "else", "endif", "endignore"]
                .iter()
                .any(|name| is(&tag.name, name))
                && cx.with_state::<State, _>(|s, _| Ok(s.parser.conditions.is_empty()))?
            {
                return Err(NativeError::Message("if stack underflow"));
            }
            if let Some(flow) = self.advanced_tag(cx, &tag, condition, excluded)? {
                return Ok(flow);
            }
        }
        if special && (is(&tag.name, "if") || is(&tag.name, "ignore")) {
            if excluded {
                cx.with_state::<State, _>(|s, _| {
                    s.parser.begin_if(false).map_err(error)?;
                    s.parser.finish_tag(&tag);
                    Ok(())
                })?;
                self.commands.push(Command::Next);
            } else {
                let exp = required_expression(cx)?;
                let invert = is(&tag.name, "ignore");
                self.commands.push(Command::IfValue { tag, invert });
                return Ok(Flow::Evaluate(exp));
            }
            return Ok(Flow::Next);
        }
        if special && is(&tag.name, "elsif") {
            if cx.with_state::<State, _>(|s, _| Ok(s.parser.needs_elsif()))? {
                let exp = required_expression(cx)?;
                self.commands.push(Command::ElsifValue(tag));
                return Ok(Flow::Evaluate(exp));
            }
            cx.with_state::<State, _>(|s, _| {
                s.parser.branch(false);
                s.parser.finish_tag(&tag);
                Ok(())
            })?;
            self.commands.push(Command::Next);
            return Ok(Flow::Next);
        }
        if special
            && (is(&tag.name, "else") || is(&tag.name, "endif") || is(&tag.name, "endignore"))
        {
            cx.with_state::<State, _>(|s, _| {
                if is(&tag.name, "else") {
                    s.parser.branch(true);
                } else {
                    s.parser.end_if();
                }
                s.parser.finish_tag(&tag);
                Ok(())
            })?;
            self.commands.push(Command::Next);
            return Ok(Flow::Next);
        }
        if excluded || !condition {
            cx.with_state::<State, _>(|s, _| {
                s.parser.finish_tag(&tag);
                Ok(())
            })?;
            self.commands.push(Command::Next);
            return Ok(Flow::Next);
        }
        let macro_value =
            cx.with_state::<State, _>(|s, cx| get(cx.heap_mut(), s.macros_id()?, &tag.name))?;
        if !matches!(macro_value, Value::Void) {
            let body = text(cx, macro_value)?;
            cx.with_state::<State, _>(|s, cx| {
                if s.advanced.as_ref().is_some_and(|a| a.numeric) {
                    for (i, name) in self.numeric_names.iter().enumerate() {
                        let val = string(cx.heap_mut(), name.clone());
                        put(cx.heap_mut(), s.tag()?, &units(&(i + 1).to_string()), val)?;
                    }
                }
                s.push_args(cx)?;
                s.parser.expand(&tag, &body, false).map_err(error)
            })?;
            self.commands.push(Command::Next);
            return Ok(Flow::Next);
        }
        if special && is(&tag.name, "emb") {
            let exp = required_expression(cx)?;
            self.commands.push(Command::Embedded(tag));
            return Ok(Flow::Evaluate(exp));
        }
        if special && (is(&tag.name, "jump") || is(&tag.name, "call") || is(&tag.name, "return")) {
            let kind = if is(&tag.name, "jump") {
                0
            } else if is(&tag.name, "call") {
                1
            } else {
                2
            };
            let callback = ["onJump", "onCall", "onReturn"][kind];
            let storage = Self::field_text(cx, "storage")?;
            let label = Self::field_text(cx, "target")?;
            self.commands.push(Command::Branch {
                tag,
                storage,
                label,
                kind: kind as u8,
            });
            if self.has_callback(cx, callback)? {
                let arg =
                    cx.with_state::<State, _>(|s, _| Ok(Value::Obj(ObjRef::bound(s.tag()?))))?;
                return Ok(Flow::Callback(callback, vec![arg]));
            }
            return Ok(Flow::Input(Value::Int(1)));
        }
        let handled = cx.with_state::<State, _>(|s, cx| {
            if special && s.extended && is(&tag.name, "pmacro") {
                let val = get(cx.heap_mut(), s.tag()?, &units("name"))?;
                let name = text(cx, val)?;
                let mut pairs = Vec::new();
                for name in cx
                    .heap()
                    .array(s.tag_order.ok_or(NativeError::This)?)?
                    .to_vec()
                {
                    let key = text(cx, name)?;
                    if !is(&key, "name") && !is(&key, "tagname") {
                        let value = get(cx.heap_mut(), s.tag()?, &key)?;
                        pairs.extend([name, value]);
                    }
                }
                let pairs = Value::Obj(ObjRef::bound(cx.heap_mut().alloc_array_from(&pairs)?));
                put(
                    cx.heap_mut(),
                    s.param_macros.ok_or(NativeError::This)?,
                    &name,
                    pairs,
                )?;
            } else if special && s.extended && is(&tag.name, "erasepmacro") {
                let val = get(cx.heap_mut(), s.tag()?, &units("name"))?;
                let name = text(cx, val)?;
                let key = cx.heap_mut().intern(&name);
                if cx
                    .heap_mut()
                    .remove_member(s.param_macros.ok_or(NativeError::This)?, key)?
                    .is_none()
                {
                    return Err(NativeError::Message("unknown parameter macro"));
                }
            } else if special && is(&tag.name, "macro") {
                let val = get(cx.heap_mut(), s.tag()?, &units("name"))?;
                s.parser.record(text(cx, val)?).map_err(error)?;
                if s.advanced.is_some() {
                    let members = cx.heap().members(s.tag()?)?.collect::<Vec<_>>();
                    for (name, val) in members {
                        let name = cx.heap().symbol(name)?.to_vec();
                        if is(&name, "tagname") || is(&name, "name") {
                            continue;
                        }
                        let val = String::from_utf16_lossy(&text(cx, val)?)
                            .replace('\'', "\\'")
                            .replace('"', "\\\"");
                        let name = String::from_utf16_lossy(&name);
                        let prefix = if name.starts_with(|c: char| c.is_ascii_digit()) {
                            format!(
                                "[eval exp=\"mp['{name}']='{val}'\" cond=\"mp['{name}']===void\"]"
                            )
                        } else {
                            format!("[eval exp=mp.{name}='{val}' cond=mp.{name}===void]")
                        };
                        s.parser.append_recording(&units(&prefix)).map_err(error)?;
                    }
                }
            } else if special && is(&tag.name, "endmacro") {
                return Err(NativeError::Message("endmacro outside macro"));
            } else if special && is(&tag.name, "macropop") {
                s.parser.pop_macro().map_err(error)?;
            } else if special && is(&tag.name, "erasemacro") {
                let val = get(cx.heap_mut(), s.tag()?, &units("name"))?;
                let name = text(cx, val)?;
                let key = cx.heap_mut().intern(&name);
                if cx.heap_mut().remove_member(s.macros_id()?, key)?.is_none() {
                    return Err(NativeError::Message("unknown macro"));
                }
            } else {
                s.parser.finish_tag(&tag);
                return Ok(false);
            }
            s.parser.finish_tag(&tag);
            Ok(true)
        })?;
        if handled {
            self.commands.push(Command::Next);
        } else {
            let value = cx.with_state::<State, _>(|s, cx| s.return_tag(cx.heap_mut()))?;
            return Ok(Flow::Return(value));
        }
        Ok(Flow::Next)
    }
}
pub(super) fn required_expression(cx: &mut NativeCx<'_>) -> NativeResult<Text> {
    let text = Task::field_text(cx, "exp")?;
    if text.is_empty() {
        Err(NativeError::Message("KAG expression is missing"))
    } else {
        Ok(text)
    }
}
