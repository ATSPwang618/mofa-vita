//! Event delivery and owned operation completions at scheduler boundaries.
use super::*;

impl<C: Clock + 'static> Engine<C> {
    pub fn poll(&mut self, budget: RunBudget, controls: NonZeroUsize) -> EngineEvent {
        if let Some(code) = self.terminated {
            return EngineEvent::Terminated(code);
        }
        let exit = self.take_exit();
        if let Some(code) = exit {
            return self.stop(code);
        }
        self.completion_budget_exhausted = false;
        for index in 0..controls.get() {
            let completion = self.operations.borrow_mut().completion();
            let Some((wait, result)) = completion else {
                break;
            };
            self.completion_budget_exhausted = index + 1 == controls.get();
            if let Some(wait) = wait {
                self.driver.complete(wait, result);
            }
        }
        self.timers.borrow_mut().advance(controls.get());
        let now = self.driver.now();
        if !self.presentation_deferred || now >= self.next_media_poll {
            self.sounds.borrow_mut().advance();
            self.videos.borrow_mut().advance();
            self.next_media_poll = now.saturating_add(Duration::from_millis(1));
        }
        // Input and timer deadlines are never gated by the media scan interval.
        self.windows.borrow_mut().pump(controls.get(), &self.system);

        if self.can_dispatch() {
            let pending = self.events.borrow_mut().peek_round();
            if let Some(event) = pending {
                let context = match event.kind {
                    Kind::Video => {
                        if !self.driver.can_enter_event() {
                            return EngineEvent::Yielded;
                        }
                        let callback = self
                            .videos
                            .borrow_mut()
                            .callback(event.source, &mut self.driver.runtime_mut().heap);
                        let Some(callback) = callback else {
                            self.events.borrow_mut().pop(event.priority);
                            return EngineEvent::Yielded;
                        };
                        self.driver.enqueue_task(self.global, callback)
                    }
                    Kind::Timer | Kind::Trigger => self.driver.enqueue_callback(
                        self.global,
                        Callback::Member {
                            object: Value::Obj(ObjRef::bound(event.owner)),
                            key: if matches!(event.kind, Kind::Timer) {
                                self.timer_name
                            } else {
                                self.fire_name
                            },
                        },
                        Vec::new(),
                    ),
                    Kind::Sound => {
                        if !self.driver.can_enter_event() {
                            return EngineEvent::Yielded;
                        }
                        let callback = self
                            .sounds
                            .borrow_mut()
                            .callback(event.source, &mut self.driver.runtime_mut().heap);
                        let Some(callback) = callback else {
                            self.events.borrow_mut().pop(event.priority);
                            return EngineEvent::Yielded;
                        };
                        self.driver.enqueue_task(self.global, callback)
                    }
                    Kind::Window => {
                        // Reserve a scheduler slot before removing payloads.
                        if !self.driver.can_enter_event() {
                            return EngineEvent::Yielded;
                        }
                        let callback = self
                            .windows
                            .borrow_mut()
                            .callback(event.source, &mut self.driver.runtime_mut().heap);
                        let Some(callback) = callback else {
                            self.events.borrow_mut().pop(event.priority);
                            return EngineEvent::Yielded;
                        };
                        self.driver.enqueue_task(self.global, callback)
                    }
                };
                if let Ok(context) = context {
                    self.events.borrow_mut().pop(event.priority);
                    self.callbacks.insert(
                        context,
                        CallbackState {
                            kind: CallbackKind::Posted(event),
                            finished: false,
                            modal_wait: false,
                        },
                    );
                }
            } else if !self.dispatch_system() {
                self.events.borrow_mut().finish_round();
                if self.events.borrow().peek().is_some() {
                    // Start the next round on the next poll before reporting
                    // an older EventWait to a host that would otherwise sleep.
                    return EngineEvent::Yielded;
                }
            }
        }
        let event = self.driver.poll(budget, controls);
        // A work slice or internal IO wait is still inside the same script
        // update. Publishing here exposes clear/draw and constructor steps,
        // and makes each GPU command compete with a vsynced presentation.
        self.presentation_deferred = matches!(event, SchedulerEvent::Yielded(_))
            || matches!(&event, SchedulerEvent::Waiting { request, .. }
                if request.mode == tjs_core::WaitMode::Internal);
        if !self.presentation_deferred {
            self.layers.borrow_mut().advance_transitions();
            self.layers.borrow_mut().publish();
        }
        if matches!(event, SchedulerEvent::Finalized)
            && !self.windows.borrow().closing(&self.driver.runtime().heap)
        {
            self.exiting_context = None;
        }
        // An asynchronous quit must not cut a script/native call in half just
        // because its work budget ended. Keep its context through unwinding
        // and System.exceptionHandler; an explicit EventWait permits exit.
        if self.system.borrow().exit.is_some() && self.exiting_context.is_none() {
            self.exiting_context = match &event {
                SchedulerEvent::Yielded(context)
                | SchedulerEvent::Waiting { context, .. }
                | SchedulerEvent::Completed { context, .. } => Some(*context),
                SchedulerEvent::Idle | SchedulerEvent::Finalized => None,
            };
        }
        let exit = self.take_exit();
        if let Some(code) = exit {
            return self.stop(code);
        }
        match event {
            SchedulerEvent::Idle => {
                if self.can_dispatch() && self.events.borrow().peek().is_some() {
                    EngineEvent::Yielded
                } else {
                    EngineEvent::Idle
                }
            }
            SchedulerEvent::Finalized | SchedulerEvent::Yielded(_) => EngineEvent::Yielded,
            SchedulerEvent::Waiting {
                context,
                wait,
                request,
            } => {
                if self.exiting_context == Some(context)
                    && request.mode == tjs_core::WaitMode::Event
                    && !self.windows.borrow().closing(&self.driver.runtime().heap)
                {
                    self.exiting_context = None;
                    if let Some(code) = self.take_exit() {
                        return self.stop(code);
                    }
                }
                let operation = self.operations.borrow_mut().bind(request.token, wait);
                if operation.is_some()
                    && let Some(state) = self.callbacks.get_mut(&context)
                {
                    // A modal call explicitly admits nested UI events even
                    // inside an exclusive callback. Other waits keep its gate.
                    state.modal_wait = matches!(&operation, Some(Request::Modal(..)));
                }
                match operation {
                    Some(Request::Delay(deadline)) => {
                        self.driver.arm(wait, deadline);
                    }
                    Some(Request::Read(read, delivery)) => {
                        let submitted =
                            self.operations
                                .borrow_mut()
                                .read(request.token, read, delivery);
                        if let Err(error) = submitted {
                            self.driver.complete(wait, Err(error));
                        }
                    }
                    Some(Request::Exit(code)) => return self.stop(code),
                    Some(Request::Window(ticket, delivery) | Request::Modal(ticket, delivery)) => {
                        self.operations
                            .borrow_mut()
                            .window(request.token, ticket, delivery);
                    }
                    Some(Request::Compact(level)) => {
                        if level >= 10 {
                            self.operations.borrow().images.clear();
                        }
                        let result = if level >= 10 {
                            crate::storages::clear_cache(&mut self.driver.runtime_mut().heap)
                        } else {
                            Ok(())
                        };
                        if level >= 5 {
                            self.collect([]);
                        }
                        self.driver.complete(wait, result.map(|()| Value::Void));
                    }
                    None => {}
                }
                EngineEvent::Waiting {
                    context,
                    wait,
                    request,
                }
            }
            SchedulerEvent::Completed { context, result } => {
                if !matches!(&result, RuntimeExit::Finished(_)) {
                    // Fatal VM exits can also drop an in-progress plugin hook.
                    // Clean up before entering the game's exception handler.
                    crate::plugins::cancelled(&mut self.driver.runtime_mut().heap);
                }
                let Some(result) = self.handle_exception(context, result) else {
                    return EngineEvent::Yielded;
                };
                if self.exiting_context == Some(context) {
                    self.exiting_context = None;
                }
                if matches!(result, RuntimeExit::Finished(_))
                    && let Some(code) = self.take_exit()
                {
                    return self.stop(code);
                }
                if matches!(self.startup, Startup::Running(id) if id == context) {
                    if matches!(result, RuntimeExit::Finished(_)) {
                        self.startup = Startup::Completed;
                        if self.system.borrow().exit_on_no_window_startup
                            && self.window_count() == 0
                        {
                            self.system.borrow_mut().exit = Some(0);
                        }
                    } else {
                        self.startup = Startup::Cancelled;
                    }
                }
                if let Some(state) = self.callbacks.get_mut(&context) {
                    state.finished = true;
                    match state.kind {
                        CallbackKind::Posted(event) => match event.kind {
                            Kind::Timer => EngineEvent::Timer {
                                context,
                                owner: event.owner,
                                result,
                            },
                            Kind::Trigger => EngineEvent::AsyncTrigger {
                                context,
                                owner: event.owner,
                                result,
                            },
                            Kind::Sound => EngineEvent::Sound {
                                context,
                                owner: event.owner,
                                result,
                            },
                            Kind::Video => EngineEvent::Video {
                                context,
                                owner: event.owner,
                                result,
                            },
                            Kind::Window => EngineEvent::Window {
                                context,
                                owner: event.owner,
                                result,
                            },
                        },
                        CallbackKind::Continuous(function) => {
                            self.system.borrow_mut().finished(function, false);
                            EngineEvent::System {
                                context,
                                event: SystemEvent::Continuous,
                                result,
                            }
                        }
                        CallbackKind::Activation(active) => EngineEvent::System {
                            context,
                            event: if active {
                                SystemEvent::Activate
                            } else {
                                SystemEvent::Deactivate
                            },
                            result,
                        },
                    }
                } else {
                    EngineEvent::Completed { context, result }
                }
            }
        }
    }
    fn stop(&mut self, code: i32) -> EngineEvent {
        self.reset();
        self.terminated = Some(code);
        EngineEvent::Terminated(code)
    }
    fn dispatch_system(&mut self) -> bool {
        let active = self.system.borrow_mut().activations.pop_front();
        let (callback, arguments, kind) = if let Some(active) = active {
            let class = self.system.borrow().class.expect("System class");
            let name = if active { "onActivate" } else { "onDeactivate" };
            let heap = &mut self.driver.runtime_mut().heap;
            let key = heap.intern_str(name);
            // Original activation events ignore a missing or null handler.
            if !matches!(heap.member(class, key), Ok(Some(Value::Obj(reference))) if reference.object.is_some())
            {
                return true;
            }
            (
                Callback::Member {
                    object: Value::Obj(ObjRef::bound(class)),
                    key: Value::Str(heap.alloc_string(name.encode_utf16().collect::<Vec<_>>())),
                },
                Vec::new(),
                CallbackKind::Activation(active),
            )
        } else {
            let Some((function, tick)) = self.system.borrow_mut().next_continuous() else {
                return false;
            };
            (
                Callback::Function(function),
                vec![Value::Int(tick)],
                CallbackKind::Continuous(function),
            )
        };
        if let Ok(context) = self
            .driver
            .enqueue_callback(self.global, callback, arguments)
        {
            if matches!(kind, CallbackKind::Continuous(_)) {
                self.system.borrow_mut().continuous_busy = true;
            }
            self.callbacks.insert(
                context,
                CallbackState {
                    kind,
                    finished: false,
                    modal_wait: false,
                },
            );
        }
        true
    }
}
