use super::*;

pub(super) fn start(
    shared: Shared,
    window: WindowId,
    input: Input,
    point: (i64, i64, u32),
    completion: Box<dyn NativeContinuation>,
) -> NativeStep {
    NativeStep::Continue(Box::new(Mouse {
        shared,
        window,
        input,
        x: point.0,
        y: point.1,
        shift: point.2,
        phase: Phase::Start,
        querying: false,
        presentation_lock: None,
        target: None,
        entered: None,
        changed: false,
        release: 0,
        completion,
    }))
}

#[derive(Clone, Copy)]
enum Phase {
    Start,
    MoveHit,
    Left,
    Enter,
    Entered,
    Rechecked,
    SecondLeft,
    SecondEntered,
    MoveDelivery,
    Moved,
    Down,
    DownDone,
    Up,
    UpDone,
    Click,
    DoubleClick,
    Done,
}
struct Mouse {
    shared: Shared,
    window: WindowId,
    input: Input,
    x: i64,
    y: i64,
    shift: u32,
    phase: Phase,
    querying: bool,
    presentation_lock: Option<Rc<()>>,
    target: Option<LayerId>,
    entered: Option<LayerId>,
    changed: bool,
    release: u64,
    completion: Box<dyn NativeContinuation>,
}
impl Trace for Mouse {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.completion.trace(visit);
        let world = self.shared.borrow();
        world.trace_input(self.window, visit);
        for id in [self.target, self.entered].into_iter().flatten() {
            if let Some(record) = world.records.get(id) {
                record.owner.trace(visit);
            }
        }
    }
}
impl Mouse {
    fn done(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        self.completion.resume(cx, Value::Void)
    }
    fn query(mut self: Box<Self>, phase: Phase, capture: bool) -> NativeResult<NativeStep> {
        self.phase = phase;
        let captured = if capture {
            self.shared
                .borrow()
                .input
                .get(&self.window)
                .and_then(|s| s.capture)
        } else {
            None
        };
        if let Some(id) = captured {
            self.target = Some(id);
            self.querying = false;
            return Ok(NativeStep::Continue(self));
        }
        self.querying = true;
        let shared = self.shared.clone();
        Ok(hit::start(
            &shared,
            self.window,
            self.x,
            self.y,
            None,
            false,
            Some(self),
        ))
    }
    fn send(
        mut self: Box<Self>,
        id: Option<LayerId>,
        name: &'static str,
        args: Vec<Value>,
        next: Phase,
    ) -> NativeResult<NativeStep> {
        self.phase = next;
        let shared = self.shared.clone();
        Ok(super::send(&shared, id, name, args, self))
    }
    fn local(&self, id: LayerId) -> NativeResult<Vec<Value>> {
        let (_, x, y) = self.shared.borrow().coordinates(id)?;
        Ok(vec![Value::Int(self.x - x), Value::Int(self.y - y)])
    }
    fn begin_move(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        {
            let mut world = self.shared.borrow_mut();
            let Some(state) = world.input.get_mut(&self.window) else {
                drop(world);
                return self.done(cx);
            };
            if matches!(self.input, Input::MouseLeave) && state.capture.is_some() {
                drop(world);
                return self.done(cx);
            }
            self.changed = state.position != Some((self.x, self.y));
            state.outside = matches!(self.input, Input::MouseLeave);
            state.position = Some((self.x, self.y));
        }
        self.query(Phase::MoveHit, true)
    }
    fn finish_move(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let changed = {
            let mut world = self.shared.borrow_mut();
            self.target = self.target.filter(|id| world.records.contains_key(*id));
            if let Some(state) = world.input.get_mut(&self.window) {
                let changed = state.hover != self.target;
                state.hover = self.target;
                changed
            } else {
                false
            }
        };
        self.phase = Phase::MoveDelivery;
        self.presentation_lock = None;
        if changed {
            presentation::hint(&self.shared, self.window, self.target)?;
        }
        let unknown_style = self
            .shared
            .borrow()
            .windows
            .borrow()
            .input_style(self.window)?
            .0
            .is_none();
        if changed || unknown_style {
            let shared = self.shared.clone();
            return presentation::sync(&shared, self.window, cx, Value::Void, self);
        }
        Ok(NativeStep::Continue(self))
    }
    fn deliver_move(self: Box<Self>) -> NativeResult<NativeStep> {
        let args = if self.changed {
            self.target.map(|id| self.local(id)).transpose()?
        } else {
            None
        };
        if let Some(mut args) = args {
            args.push(Value::Int(self.shift.into()));
            let target = self.target;
            self.send(target, "onMouseMove", args, Phase::Moved)
        } else {
            let mut this = self;
            this.phase = Phase::Moved;
            Ok(NativeStep::Continue(this))
        }
    }
}
impl NativeContinuation for Mouse {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        if !self.shared.borrow().windows.borrow().is_live(self.window) {
            return self.done(cx);
        }
        if self.querying {
            self.querying = false;
            self.target = if matches!(
                value,
                Value::Obj(ObjRef {
                    object: Some(_),
                    ..
                })
            ) {
                bindings::layer_id(cx.heap_mut(), value).ok()
            } else {
                None
            };
        }
        if self
            .target
            .is_some_and(|id| self.shared.borrow().coordinates(id).is_err())
        {
            self.target = None;
        }
        match self.phase {
            Phase::Start => match self.input {
                Input::MouseUp { .. } => self.query(Phase::Up, true),
                Input::Click { .. } => self.query(Phase::Click, false),
                Input::DoubleClick { .. } => self.query(Phase::DoubleClick, false),
                _ => self.begin_move(cx),
            },
            Phase::MoveHit => {
                let old = self
                    .shared
                    .borrow()
                    .input
                    .get(&self.window)
                    .and_then(|s| s.hover);
                if old != self.target {
                    self.send(old, "onMouseLeave", vec![], Phase::Left)
                } else {
                    self.finish_move(cx)
                }
            }
            Phase::Left => self.query(Phase::Enter, true),
            Phase::Enter => {
                let lock = Rc::new(());
                if let Some(state) = self.shared.borrow_mut().input.get_mut(&self.window) {
                    state.presentation_lock = Rc::downgrade(&lock);
                }
                self.presentation_lock = Some(lock);
                self.entered = self.target;
                let target = self.target;
                self.send(target, "onMouseEnter", vec![], Phase::Entered)
            }
            Phase::Entered => self.query(Phase::Rechecked, true),
            Phase::Rechecked => {
                if self.entered != self.target {
                    let old = self.entered;
                    self.send(old, "onMouseLeave", vec![], Phase::SecondLeft)
                } else {
                    self.finish_move(cx)
                }
            }
            Phase::SecondLeft => {
                let target = self.target;
                self.send(target, "onMouseEnter", vec![], Phase::SecondEntered)
            }
            Phase::SecondEntered => self.finish_move(cx),
            Phase::MoveDelivery => self.deliver_move(),
            Phase::Moved => {
                if matches!(self.input, Input::MouseDown { .. }) {
                    self.query(Phase::Down, true)
                } else {
                    self.done(cx)
                }
            }
            Phase::Down | Phase::Up => {
                let Some(target) = self.target else {
                    if matches!(self.phase, Phase::Down) {
                        self.shared.borrow_mut().release_capture(self.window);
                    }
                    return self.done(cx);
                };
                let mut args = self.local(target)?;
                let button = match self.input {
                    Input::MouseDown { button, .. } | Input::MouseUp { button, .. } => button,
                    _ => unreachable!(),
                };
                args.extend([Value::Int(button.into()), Value::Int(self.shift.into())]);
                if matches!(self.phase, Phase::Down) {
                    self.release = self.shared.borrow().input[&self.window].release;
                    self.send(Some(target), "onMouseDown", args, Phase::DownDone)
                } else {
                    self.send(Some(target), "onMouseUp", args, Phase::UpDone)
                }
            }
            Phase::DownDone => {
                self.shared.borrow().windows.borrow_mut().set_hint(
                    self.window,
                    None,
                    Arc::from([]),
                )?;
                if let Some(state) = self.shared.borrow_mut().input.get_mut(&self.window)
                    && state.release == self.release
                {
                    state.capture = self.target;
                }
                self.done(cx)
            }
            Phase::UpDone => {
                if self.shift & (8 | 16 | 32 | 256 | 512) == 0 {
                    self.shared.borrow_mut().release_capture(self.window);
                    self.begin_move(cx)
                } else {
                    self.done(cx)
                }
            }
            Phase::Click | Phase::DoubleClick => {
                let captured = self
                    .shared
                    .borrow()
                    .input
                    .get(&self.window)
                    .and_then(|s| s.capture);
                if let Some(target) = self.target
                    && (!matches!(self.phase, Phase::Click) || captured == Some(target))
                {
                    let name = if matches!(self.phase, Phase::Click) {
                        "onClick"
                    } else {
                        "onDoubleClick"
                    };
                    let args = self.local(target)?;
                    self.send(Some(target), name, args, Phase::Done)
                } else {
                    self.done(cx)
                }
            }
            Phase::Done => self.done(cx),
        }
    }
}
