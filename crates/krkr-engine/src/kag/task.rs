use super::*;
use krkr_kag::{Argument, Attribute, Tag, Token, is};
use tjs_core::{NativeContinuation, NativeStep, WaitMode};

mod advanced;
mod attributes;
use advanced::AdvancedCommand;
mod control;
mod loading;

enum Flow {
    Storage {
        name: Text,
        operations: Option<crate::operations::Shared>,
    },
    Next,
    Input(Value),
    Return(Value),
    Evaluate(Text),
    Callback(&'static str, Vec<Value>),
    Get {
        object: Value,
        key: Value,
    },
}

pub(super) struct Task {
    owner: ObjId,
    commands: Vec<Command>,
    buffered: Option<Value>,
    parameters: Vec<ParameterFrame>,
    skip_parameter_fetch: bool,
    skip_parameter_lookup: bool,
    parameter_expansions: usize,
    numeric_names: Vec<Text>,
}
struct ParameterFrame {
    array: Value,
    count: usize,
    index: usize,
}
enum Command {
    Advanced(AdvancedCommand),
    Next,
    Restart,
    Token(Token),
    Done,
    Log(Text),
    AdvanceLine,
    Attributes {
        tag: Tag,
        index: usize,
        condition: bool,
    },
    ParameterLookup {
        tag: Tag,
        index: usize,
        condition: bool,
    },
    ParameterCount {
        tag: Tag,
        index: usize,
        condition: bool,
        array: Value,
    },
    ParameterName {
        tag: Tag,
        index: usize,
        condition: bool,
    },
    ParameterValue {
        tag: Tag,
        index: usize,
        condition: bool,
        name: Value,
    },
    AttributeValue {
        tag: Tag,
        index: usize,
        condition: bool,
        name: Text,
    },
    ConditionValue {
        tag: Tag,
        index: usize,
    },
    Process {
        tag: Tag,
        condition: bool,
    },
    IfValue {
        tag: Tag,
        invert: bool,
    },
    ElsifValue(Tag),
    Embedded(Tag),
    Branch {
        tag: Tag,
        storage: Text,
        label: Text,
        kind: u8,
    },
    Go {
        storage: Text,
        label: Text,
    },
    Goto(Text),
    Load(Text),
    LoadValue(Text),
    ReadValue {
        name: Text,
        delivery: crate::io::Delivery,
    },
    Loaded(Text),
    Returned {
        frame: krkr_kag::CallFrame,
        explicit: bool,
    },
    Restore(Box<save::Restored>),
}
impl Trace for Task {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(Value::Obj(self.owner.into()));
        self.buffered.trace(visit);
        for frame in &self.parameters {
            frame.array.trace(visit);
        }
        for c in &self.commands {
            if let Command::Restore(s) = c {
                s.trace(visit);
            }
            match c {
                Command::ParameterCount { array, .. } => array.trace(visit),
                Command::ParameterValue { name, .. } => name.trace(visit),
                _ => {}
            }
        }
    }
}
impl NativeContinuation for Task {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        let value = self.buffered.take().unwrap_or(value);
        self.drive(cx, value)
    }
}
impl Task {
    fn new(owner: ObjId, commands: Vec<Command>) -> Box<Self> {
        Box::new(Self {
            owner,
            commands,
            buffered: None,
            parameters: Vec::new(),
            skip_parameter_fetch: false,
            skip_parameter_lookup: false,
            parameter_expansions: 0,
            numeric_names: Vec::new(),
        })
    }
    pub fn next(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let token = Self::read_token(cx, false)?;
        match token {
            Token::End => Ok(NativeStep::Return(Value::Void)),
            Token::Character(_) | Token::Newline(_) | Token::Interrupt => {
                // Ordinary text does not allocate a task/command stack per code unit.
                let value = cx.with_state::<State, _>(|s, cx| s.simple(cx, token))?;
                Ok(NativeStep::Return(value))
            }
            token => Self::new(cx.this(), vec![Command::Token(token)]).drive(cx, Value::Void),
        }
    }
    fn read_token(cx: &mut NativeCx<'_>, continuing: bool) -> NativeResult<Token> {
        cx.with_state::<State, _>(|s, cx| {
            cx.heap_mut().clear_members(s.tag()?)?;
            // KAGParser's loop checks Interrupted before EOF, including after an
            // owner callback vetoes the final tag. Entry/parse_start checks EOF first.
            if continuing && std::mem::take(&mut s.parser.interrupted) {
                return Ok(Token::Interrupt);
            }
            s.parser.next_token().map_err(error)
        })
    }
    pub fn load(cx: &mut NativeCx<'_>, name: Text) -> NativeResult<NativeStep> {
        Self::new(cx.this(), vec![Command::Done, Command::Load(name)]).drive(cx, Value::Void)
    }
    pub fn go(cx: &mut NativeCx<'_>, label: Text, call: bool) -> NativeResult<NativeStep> {
        if call {
            cx.with_state::<State, _>(|s, cx| {
                if let Some(a) = s.advanced.as_mut() {
                    let dict = cx.heap_mut().alloc_dictionary();
                    a.push_local(cx.heap_mut(), dict, false)?;
                    advanced::push_call(s, cx)
                } else {
                    s.parser.push_call().map_err(error)
                }
            })?;
        }
        Self::new(cx.this(), vec![Command::Done, Command::Goto(label)]).drive(cx, Value::Void)
    }
    pub fn restore(cx: &mut NativeCx<'_>, state: save::Restored) -> NativeResult<NativeStep> {
        let name = state.parser.storage.clone();
        save::prepare(cx, &state)?;
        Self::new(
            cx.this(),
            vec![
                Command::Done,
                Command::Restore(Box::new(state)),
                Command::Load(name),
            ],
        )
        .drive(cx, Value::Void)
    }
    fn callback(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        name: &str,
        arguments: Vec<Value>,
    ) -> NativeResult<NativeStep> {
        let key = string(cx.heap_mut(), units(name));
        Ok(NativeStep::CallMember {
            object: Value::Obj(ObjRef::bound(self.owner)),
            key,
            arguments,
            continuation: self,
        })
    }
    fn has_callback(&self, cx: &mut NativeCx<'_>, name: &str) -> NativeResult<bool> {
        Ok(matches!(
            get(cx.heap_mut(), self.owner, &units(name))?,
            Value::Obj(ObjRef {
                object: Some(_),
                ..
            })
        ))
    }
    fn evaluate(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        expression: Text,
    ) -> NativeResult<NativeStep> {
        let scripts = cx
            .heap()
            .registered_class("Scripts")
            .ok_or(NativeError::Message("KAG evaluation requires Scripts"))?;
        let (name, line) = cx.with_state::<State, _>(|s, _| {
            Ok((short(&s.parser.storage), s.parser.position.line))
        })?;
        let arguments = vec![
            string(cx.heap_mut(), expression),
            string(cx.heap_mut(), name),
            Value::Int(line as i64),
            Value::Obj(ObjRef::bound(self.owner)),
        ];
        let key = string(cx.heap_mut(), units("eval"));
        Ok(NativeStep::CallMember {
            object: Value::Obj(ObjRef::bound(scripts)),
            key,
            arguments,
            continuation: self,
        })
    }
    fn field(cx: &mut NativeCx<'_>, name: &str) -> NativeResult<Value> {
        cx.with_state::<State, _>(|s, cx| get(cx.heap_mut(), s.tag()?, &units(name)))
    }
    fn field_text(cx: &mut NativeCx<'_>, name: &str) -> NativeResult<Text> {
        let value = Self::field(cx, name)?;
        text(cx, value)
    }
    fn log(
        &mut self,
        cx: &mut NativeCx<'_>,
        level: i64,
        message: impl FnOnce(&Parser) -> Text,
    ) -> NativeResult<()> {
        let line = cx.with_state::<State, _>(|s, _| {
            Ok((s.debug_level >= level).then(|| message(&s.parser)))
        })?;
        if let Some(line) = line {
            self.commands.push(Command::Log(line));
        }
        Ok(())
    }
    fn drive(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        mut value: Value,
    ) -> NativeResult<NativeStep> {
        for _ in 0..64 {
            let Some(command) = self.commands.pop() else {
                return Ok(NativeStep::Return(Value::Void));
            };
            let mut flow = Flow::Next;
            match command {
                Command::Advanced(command) => flow = self.advanced(cx, command, value)?,
                Command::Done => return Ok(NativeStep::Return(Value::Void)),
                Command::Log(line) => {
                    if let Some(class) = cx.heap().registered_class("Debug") {
                        let key = string(cx.heap_mut(), units("message"));
                        let line = string(cx.heap_mut(), line);
                        return Ok(NativeStep::CallMember {
                            object: Value::Obj(ObjRef::bound(class)),
                            key,
                            arguments: vec![line],
                            continuation: self,
                        });
                    }
                }
                Command::AdvanceLine => cx.with_state::<State, _>(|s, _| {
                    s.parser.advance_line();
                    Ok(())
                })?,
                Command::Next | Command::Restart => {
                    let token = Self::read_token(cx, matches!(command, Command::Next))?;
                    self.commands.push(Command::Token(token));
                }
                Command::Token(token) => match token {
                    Token::End => return Ok(NativeStep::Return(Value::Void)),
                    Token::Character(_) | Token::Newline(_) | Token::Interrupt => {
                        return Ok(NativeStep::Return(
                            cx.with_state::<State, _>(|s, cx| s.simple(cx, token))?,
                        ));
                    }
                    Token::Tag(tag) => {
                        self.numeric_names.clear();
                        self.parameters.clear();
                        self.parameter_expansions = 0;
                        cx.with_state::<State, _>(|s, cx| s.set_tag(cx, tag.name.clone()))?;
                        self.commands.push(Command::Attributes {
                            tag,
                            index: 0,
                            condition: true,
                        });
                    }
                    Token::Skip => self.commands.push(Command::Next),
                    Token::Macro { name, body } => {
                        self.commands.push(Command::Next);
                        self.log(cx, 2, |_| {
                            [units("macro : "), name.clone(), units(" : "), body.clone()].concat()
                        })?;
                        cx.with_state::<State, _>(|s, cx| {
                            let value = string(cx.heap_mut(), body);
                            put(cx.heap_mut(), s.macros_id()?, &name, value)
                        })?;
                    }
                    Token::Label { name, page } => {
                        cx.with_state::<State, _>(|s, cx| {
                            if let Some(a) = s.advanced.as_ref() {
                                if !s.parser.conditions.is_empty() {
                                    return Err(NativeError::Message(
                                        "label inside conditional scope",
                                    ));
                                }
                                let expected = a.calls.last().map_or(1, |c| c.locals);
                                if cx.heap().array(a.locals.ok_or(NativeError::This)?)?.len()
                                    != expected
                                {
                                    return Err(NativeError::Message(
                                        "label inside local variable scope",
                                    ));
                                }
                            }
                            Ok(())
                        })?;
                        self.commands.extend([Command::Next, Command::AdvanceLine]);
                        self.log(cx, 2, |p| {
                            [short(&p.storage), units(" : "), p.line().to_vec()].concat()
                        })?;
                        if self.has_callback(cx, "onLabel")? {
                            let args = vec![
                                string(cx.heap_mut(), name),
                                page.map_or(Value::Void, |v| string(cx.heap_mut(), v)),
                            ];
                            return self.callback(cx, "onLabel", args);
                        }
                    }
                    Token::Script { text: source, line } => {
                        self.commands.extend([Command::Next, Command::AdvanceLine]);
                        if self.has_callback(cx, "onScript")? {
                            let name =
                                cx.with_state::<State, _>(|s, _| Ok(short(&s.parser.storage)))?;
                            let args = vec![
                                string(cx.heap_mut(), source),
                                string(cx.heap_mut(), name),
                                Value::Int(line as i64),
                            ];
                            return self.callback(cx, "onScript", args);
                        }
                    }
                },
                Command::Attributes {
                    tag,
                    index,
                    condition,
                } => flow = self.attributes(cx, tag, index, condition)?,
                Command::ParameterLookup {
                    mut tag,
                    index,
                    condition,
                } => {
                    if matches!(
                        value,
                        Value::Obj(ObjRef {
                            object: Some(_),
                            ..
                        })
                    ) {
                        self.parameter_expansions += 1;
                        if self.parameter_expansions > 16384 || self.parameters.len() >= 128 {
                            return Err(NativeError::Message("parameter macro expansion limit"));
                        }
                        tag.attributes.remove(index);
                        self.commands.push(Command::ParameterCount {
                            tag,
                            index,
                            condition,
                            array: value,
                        });
                        flow = Flow::Get {
                            object: value,
                            key: string(cx.heap_mut(), units("count")),
                        };
                    } else {
                        self.skip_parameter_fetch = true;
                        self.skip_parameter_lookup = true;
                        self.commands.push(Command::Attributes {
                            tag,
                            index,
                            condition,
                        });
                    }
                }
                Command::ParameterCount {
                    tag,
                    index,
                    condition,
                    array,
                } => {
                    let count = value::to_integer(cx.heap(), value)?.max(0) as usize;
                    if count > 32768 {
                        return Err(NativeError::Message("parameter macro array limit"));
                    }
                    self.parameters.push(ParameterFrame {
                        array,
                        count,
                        index: 0,
                    });
                    self.commands.push(Command::Attributes {
                        tag,
                        index,
                        condition,
                    });
                }
                Command::ParameterName {
                    tag,
                    index,
                    condition,
                } => {
                    let frame = self.parameters.last().unwrap();
                    flow = Flow::Get {
                        object: frame.array,
                        key: Value::Int(frame.index as i64 + 1),
                    };
                    self.commands.push(Command::ParameterValue {
                        tag,
                        index,
                        condition,
                        name: value,
                    });
                }
                Command::ParameterValue {
                    mut tag,
                    index,
                    condition,
                    name,
                } => {
                    self.parameters.last_mut().unwrap().index += 2;
                    let name = text(cx, name)?;
                    let raw = text(cx, value)?;
                    let argument = match raw.first() {
                        Some(38) => Argument::Expression(raw[1..].to_vec()),
                        Some(37) => Argument::Macro(raw[1..].to_vec()),
                        _ => Argument::Literal(raw),
                    };
                    tag.attributes.insert(
                        index,
                        Attribute::Named {
                            name,
                            value: argument,
                        },
                    );
                    self.skip_parameter_fetch = true;
                    self.commands.push(Command::Attributes {
                        tag,
                        index,
                        condition,
                    });
                }
                Command::AttributeValue {
                    tag,
                    index,
                    condition,
                    name,
                } => {
                    let value = if matches!(value, Value::Void) {
                        value
                    } else {
                        value::to_string(cx.heap_mut(), value)?
                    };
                    if let Some(expression) =
                        self.attribute(cx, tag, index, condition, name, value)?
                    {
                        return self.evaluate(cx, expression);
                    }
                }
                Command::ConditionValue { tag, index } => self.commands.push(Command::Attributes {
                    tag,
                    index,
                    condition: value.truthy(cx.heap())?,
                }),
                Command::Process { tag, condition } => {
                    flow = self.process_tag(cx, tag, condition)?
                }
                Command::IfValue { tag, invert } => {
                    let cond = value.truthy(cx.heap())? ^ invert;
                    cx.with_state::<State, _>(|s, _| {
                        s.parser.begin_if(cond).map_err(error)?;
                        s.parser.finish_tag(&tag);
                        Ok(())
                    })?;
                    self.commands.push(Command::Next);
                }
                Command::ElsifValue(tag) => {
                    let cond = value.truthy(cx.heap())?;
                    cx.with_state::<State, _>(|s, _| {
                        s.parser.branch(cond);
                        s.parser.finish_tag(&tag);
                        Ok(())
                    })?;
                    self.commands.push(Command::Next);
                }
                Command::Embedded(tag) => {
                    let source = text(cx, value)?;
                    cx.with_state::<State, _>(|s, _| {
                        s.parser.expand(&tag, &source, true).map_err(error)
                    })?;
                    self.commands.push(Command::Next);
                }
                Command::Branch {
                    tag,
                    storage,
                    label,
                    kind,
                } => {
                    if !value.truthy(cx.heap())? {
                        self.commands.push(Command::Next);
                        cx.with_state::<State, _>(|s, _| {
                            s.parser.finish_tag(&tag);
                            Ok(())
                        })?;
                        continue;
                    }
                    self.commands.push(Command::Restart);
                    if kind == 2 {
                        let frame = cx.with_state::<State, _>(|s, _| {
                            let frame = s
                                .parser
                                .calls
                                .last()
                                .cloned()
                                .ok_or(NativeError::Message("KAG return without call"))?;
                            s.parser.macro_base = frame.macro_depth;
                            s.parser.macro_depth = frame.macro_depth;
                            Ok(frame)
                        })?;
                        let explicit = !storage.is_empty() || !label.is_empty();
                        let saved = frame.storage.clone();
                        self.commands.push(Command::Returned { frame, explicit });
                        if explicit {
                            self.commands.push(Command::Go { storage, label });
                        } else {
                            self.commands.push(Command::Load(saved));
                        }
                    } else {
                        if kind == 1 {
                            let advanced =
                                cx.with_state::<State, _>(|s, _| Ok(s.advanced.is_some()))?;
                            if advanced {
                                let copy = cx.with_state::<State, _>(|s, cx| {
                                    s.parser.finish_tag(&tag);
                                    get(cx.heap_mut(), s.tag()?, &units("copyvar"))
                                })?;
                                // PushLocal supplies its own next command.
                                self.commands.pop();
                                self.commands
                                    .push(Command::Advanced(AdvancedCommand::PushLocal {
                                        call: Some((storage, label)),
                                    }));
                                flow = if matches!(copy, Value::Void) {
                                    Flow::Input(Value::Int(0))
                                } else {
                                    Flow::Evaluate(text(cx, copy)?)
                                };
                                // Dispatch the expression before loading the callee.
                            } else {
                                cx.with_state::<State, _>(|s, _| {
                                    s.parser.finish_tag(&tag);
                                    s.parser.push_call().map_err(error)
                                })?;
                                self.commands.push(Command::Go { storage, label });
                            }
                        } else {
                            self.commands.push(Command::Go { storage, label });
                        }
                    }
                }
                Command::Go { storage, label } => {
                    if storage.is_empty()
                        && label.is_empty()
                        && cx.with_state::<State, _>(|s, _| Ok(s.advanced.is_some()))?
                    {
                        return Err(NativeError::Message("jump/call requires storage or target"));
                    }
                    self.commands.push(Command::Goto(label));
                    if !storage.is_empty() {
                        self.commands.push(Command::Load(storage));
                    }
                }
                Command::Goto(label) => {
                    if !label.is_empty() {
                        cx.with_state::<State, _>(|s, cx| {
                            if let Some(a) = s.advanced.as_mut() {
                                a.break_control(cx.heap_mut())?;
                            }
                            Ok(())
                        })?;
                    }
                    cx.with_state::<State, _>(|s, _| s.parser.goto(&label).map_err(error))?;
                    if !label.is_empty() {
                        self.log(cx, 1, |p| {
                            [short(&p.storage), units(" : jumped to : "), label].concat()
                        })?;
                    }
                }
                Command::Load(name) => {
                    let existing = cx.with_state::<State, _>(|s, cx| {
                        if let Some(a) = s.advanced.as_mut() {
                            a.break_control(cx.heap_mut())?;
                        }
                        s.parser.break_control();
                        if s.parser.storage == name {
                            Ok(s.parser.scenario.clone())
                        } else {
                            s.parser.clear_buffer();
                            Ok(None)
                        }
                    })?;
                    if let Some(scenario) = existing {
                        cx.with_state::<State, _>(|s, _| {
                            s.parser.load(name.clone(), scenario);
                            Ok(())
                        })?;
                        self.commands.push(Command::Loaded(name));
                    } else {
                        self.commands.push(Command::LoadValue(name.clone()));
                        if self.has_callback(cx, "onScenarioLoad")? {
                            let arg = string(cx.heap_mut(), name);
                            return self.callback(cx, "onScenarioLoad", vec![arg]);
                        }
                        value = Value::Void;
                        continue;
                    }
                }
                Command::LoadValue(name) => flow = self.load_value(cx, name, value)?,
                Command::ReadValue { name, delivery } => {
                    let data = delivery
                        .borrow_mut()
                        .take()
                        .ok_or(NativeError::Message("scenario IO completed without data"))?;
                    let crate::io::Data::Text(source) = data else {
                        return Err(NativeError::Message("scenario is not text"));
                    };
                    self.loaded_source(cx, name, source)?;
                }
                Command::Loaded(name) => {
                    self.log(cx, 1, |_| {
                        [units("Scenario loaded : "), name.clone()].concat()
                    })?;
                    if self.has_callback(cx, "onScenarioLoaded")? {
                        let arg = string(cx.heap_mut(), name);
                        return self.callback(cx, "onScenarioLoaded", vec![arg]);
                    }
                }
                Command::Returned { frame, explicit } => {
                    let fallback = cx.with_state::<State, _>(|s, cx| {
                        if !explicit {
                            if let Some(a) = s.advanced.as_ref() {
                                if let Err(failure) = crate::kag::advanced::return_position(
                                    &mut s.parser,
                                    &frame,
                                    a.fuzzy,
                                ) {
                                    if !a.return_error.is_empty() {
                                        return Ok(Some(a.return_error.clone()));
                                    }
                                    return Err(failure);
                                }
                            } else {
                                s.parser.return_position(&frame).map_err(error)?;
                            }
                        }
                        s.parser.macro_base = frame.macro_base;
                        s.parser.macro_depth = frame.macro_depth;
                        s.parser.calls.pop();
                        if let Some(a) = s.advanced.as_mut() {
                            a.calls.pop();
                            a.loops.pop();
                            a.pop_local(cx.heap_mut())?;
                            if explicit {
                                a.break_control(cx.heap_mut())?;
                                s.parser.break_control();
                            }
                        }
                        Ok(None)
                    })?;
                    if let Some(storage) = fallback {
                        self.commands.push(Command::Load(storage));
                        continue;
                    }
                    if cx.with_state::<State, _>(|s, _| Ok(s.advanced.is_some()))?
                        && self.has_callback(cx, "onAfterReturn")?
                    {
                        return self.callback(cx, "onAfterReturn", vec![]);
                    }
                    self.log(cx, 1, |p| {
                        [
                            short(&p.storage),
                            units(" : returned to : "),
                            p.label.clone(),
                            units(&format!(" line {}", p.position.line)),
                        ]
                        .concat()
                    })?;
                    if self.has_callback(cx, "onAfterReturn")? {
                        return self.callback(cx, "onAfterReturn", Vec::new());
                    }
                }
                Command::Restore(state) => save::apply(cx, *state)?,
            }
            match flow {
                Flow::Storage { name, operations } => {
                    return loading::begin(cx, self, name, operations);
                }
                Flow::Next => {}
                Flow::Input(input) => value = input,
                Flow::Return(value) => return Ok(NativeStep::Return(value)),
                Flow::Evaluate(expression) => return self.evaluate(cx, expression),
                Flow::Callback(name, args) => return self.callback(cx, name, args),
                Flow::Get { object, key } => {
                    return Ok(NativeStep::GetOr {
                        object,
                        key,
                        raw: false,
                        fallback: Value::Void,
                        continuation: self,
                    });
                }
            }
        }
        self.buffered = Some(value);
        Ok(NativeStep::Continue(self))
    }
}
fn short(name: &[u16]) -> Text {
    krkr_assets::name::split_name(name).1.to_vec()
}
