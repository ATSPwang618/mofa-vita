use super::*;
use crate::events::ActionOwner;
use tjs_core::{RestArgs, value};
pub(super) fn service(cx: &mut NativeCx<'_>) -> NativeResult<Shared> {
    let class = cx
        .heap()
        .registered_class("VideoOverlay")
        .ok_or(NativeError::This)?;
    cx.heap_mut()
        .with_native_state::<implementation::State, _>(class, |s| s.service.clone())?
        .ok_or(NativeError::This)
}
fn integer(cx: &NativeCx<'_>, v: Value) -> NativeResult<i64> {
    Ok(value::to_integer(cx.heap(), v)?)
}
fn null() -> Value {
    Value::Obj(tjs_core::ObjRef {
        object: None,
        this: None,
    })
}
#[tjs_bind::class(name = "VideoOverlay")]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub service: Option<Shared>,
        lease: Option<Lease>,
        status_action: ActionOwner,
        period_action: ActionOwner,
        frame_action: ActionOwner,
        command_action: ActionOwner,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            if let Some(l) = &self.lease
                && let Ok(r) = l.shared.borrow().record(l.id)
            {
                for value in r.target_values {
                    value.trace(visit);
                }
            }
            self.status_action.trace(visit);
            self.period_action.trace(visit);
            self.frame_action.trace(visit);
            self.command_action.trace(visit);
        }
    }
    impl State {
        fn lease(&self) -> NativeResult<&Lease> {
            self.lease.as_ref().ok_or(NativeError::This)
        }
        fn read<T>(&self, f: impl FnOnce(&Record) -> T) -> NativeResult<T> {
            let l = self.lease()?;
            Ok(f(l.shared.borrow().record(l.id)?))
        }
        fn write(&self, f: impl FnOnce(&mut Record)) -> NativeResult<()> {
            let l = self.lease()?;
            f(l.shared.borrow_mut().record_mut(l.id)?);
            Ok(())
        }
        fn task(&self, cx: &mut NativeCx<'_>, phase: task::Phase) -> NativeResult<NativeStep> {
            task::start(self.lease()?, cx, phase)
        }
        fn geometry(&self, f: impl FnOnce(&mut Rect)) -> NativeResult<()> {
            self.write(|r| {
                if r.mode != 1 {
                    f(&mut r.bounds);
                    r.geometry_dirty = true;
                }
            })
        }
        fn move_targets(&self, left: Option<i32>, top: Option<i32>) -> NativeResult<bool> {
            let l = self.lease()?;
            let w = l.shared.borrow();
            let r = w.record(l.id)?;
            if r.mode != 1 {
                return Ok(false);
            }
            w.layers
                .borrow_mut()
                .video_targets_position(r.targets, left, top)?;
            Ok(true)
        }
        fn target(&self, cx: &mut NativeCx<'_>, index: usize, value: Value) -> NativeResult<()> {
            let id = match value {
                Value::Void | Value::Obj(tjs_core::ObjRef { object: None, .. }) => None,
                _ => Some(crate::layer::video::layer(cx.heap_mut(), value)?),
            };
            let l = self.lease()?;
            let mut w = l.shared.borrow_mut();
            let window = w.record(l.id)?.window;
            if let Some(id) = id {
                w.layers.borrow().video_layer(id, window)?;
            }
            let r = w.record_mut(l.id)?;
            r.targets[index] = id;
            r.target_values[index] = if id.is_some() { value } else { null() };
            r.geometry_dirty = true;
            Ok(())
        }
        #[tjs::constructor]
        fn create(cx: &mut NativeCx<'_>, owner: Value) -> NativeResult<Self> {
            let Value::Obj(owner_ref) = owner else {
                return Err(NativeError::Type("a Window"));
            };
            let window = crate::window::bindings::id(
                cx.heap_mut(),
                owner_ref.object.ok_or(NativeError::This)?,
            )?;
            let status_action = ActionOwner::new(cx, owner, None, "onStatusChanged")?;
            let period_action = ActionOwner::new(cx, owner, None, "onPeriod")?;
            let frame_action = ActionOwner::new(cx, owner, None, "onFrameUpdate")?;
            let command_action = ActionOwner::new(cx, owner, None, "onCallbackCommand")?;
            let shared = service(cx)?;
            let mut w = shared.borrow_mut();
            if w.records.len() >= 8 {
                return Err(NativeError::Message("movie capacity reached"));
            }
            if !w.windows.borrow().is_live(window) {
                return Err(NativeError::Message("movie window is closing"));
            }
            let source = w.events.borrow_mut().insert(cx.this(), Kind::Video, 8)?;
            let id = w.records.insert(Record {
                owner: cx.this(),
                window,
                source,
                handle: None,
                images: None,
                tickets: VecDeque::new(),
                rendered_frame: None,
                status: "unload",
                mode: 0,
                bounds: Rect {
                    left: 0,
                    top: 0,
                    width: 320,
                    height: 240,
                },
                visible: false,
                targets: [None; 2],
                target_values: [null(); 2],
                looping: false,
                segment: (-1, -1),
                period: -1,
                period_past: false,
                preparing: false,
                busy: false,
                geometry_dirty: false,
                volume: 100000,
                balance: 0,
                rate: 1.0,
                audio_stream: -1,
                events: VecDeque::new(),
                generation: 0,
            });
            drop(w);
            Ok(Self {
                service: None,
                lease: Some(Lease { shared, id }),
                status_action,
                period_action,
                frame_action,
                command_action,
            })
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.lease.take();
        }
        #[tjs::method(resumable = true)]
        fn open(&self, cx: &mut NativeCx<'_>, storage: Value) -> NativeResult<NativeStep> {
            let Value::Str(id) = value::to_string(cx.heap_mut(), storage)? else {
                unreachable!()
            };
            let path = tjs_core::string::c_string(cx.heap().string(id)?).to_vec();
            self.task(cx, task::Phase::Open(path))
        }
        #[tjs::method(resumable = true)]
        fn play(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.task(cx, task::Phase::Play)
        }
        #[tjs::method(resumable = true)]
        fn pause(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.task(cx, task::Phase::Pause)
        }
        #[tjs::method(resumable = true)]
        fn stop(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.task(cx, task::Phase::Stop)
        }
        #[tjs::method(resumable = true)]
        fn close(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.task(cx, task::Phase::Close)
        }
        #[tjs::method(resumable = true)]
        fn rewind(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.task(cx, task::Phase::Seek(0.0, Box::new(task::Phase::Done)))
        }
        #[tjs::method(resumable = true)]
        fn prepare(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            if self.read(|r| r.mode == 1 && r.handle.is_some())? {
                self.task(cx, task::Phase::Prepare)
            } else {
                Ok(NativeStep::Return(Value::Void))
            }
        }
        #[tjs::method(name = "setPos")]
        fn set_pos(&self, #[tjs(coerce)] left: i64, #[tjs(coerce)] top: i64) -> NativeResult<()> {
            if self.move_targets(Some(left as i32), Some(top as i32))? {
                return Ok(());
            }
            self.geometry(|r| {
                r.left = left as i32;
                r.top = top as i32;
            })
        }
        #[tjs::method(name = "setSize")]
        fn set_size(
            &self,
            #[tjs(coerce)] width: i64,
            #[tjs(coerce)] height: i64,
        ) -> NativeResult<()> {
            self.geometry(|r| {
                r.width = width.clamp(0, 16384) as u32;
                r.height = height.clamp(0, 16384) as u32;
            })
        }
        #[tjs::method(name = "setBounds")]
        fn set_bounds(
            &self,
            #[tjs(coerce)] left: i64,
            #[tjs(coerce)] top: i64,
            #[tjs(coerce)] width: i64,
            #[tjs(coerce)] height: i64,
        ) -> NativeResult<()> {
            self.geometry(|r| {
                r.left = left as i32;
                r.top = top as i32;
                r.width = width.clamp(0, 16384) as u32;
                r.height = height.clamp(0, 16384) as u32;
            })
        }
        #[tjs::method(name = "setSegmentLoop")]
        fn set_segment_loop(&self, start: i64, end: i64) -> NativeResult<()> {
            self.write(|r| r.segment = (start, end))
        }
        #[tjs::method(name = "cancelSegmentLoop")]
        fn cancel_segment_loop(&self) -> NativeResult<()> {
            self.write(|r| r.segment = (-1, -1))
        }
        #[tjs::method(name = "setPeriodEvent")]
        fn set_period_event(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<()> {
            let frame = args
                .first()
                .filter(|v| !matches!(v, Value::Void))
                .map(|v| integer(cx, *v))
                .transpose()?
                .unwrap_or(-1);
            self.set_period(frame)
        }
        #[tjs::method(name = "cancelPeriodEvent")]
        fn cancel_period(&self) -> NativeResult<()> {
            self.set_period(-1)
        }
        #[tjs::method(name = "selectAudioStream", resumable = true)]
        fn select_audio_stream(
            &self,
            cx: &mut NativeCx<'_>,
            stream: i64,
        ) -> NativeResult<NativeStep> {
            self.task(cx, task::Phase::Audio(stream))
        }
        #[tjs::method(name = "disableAudioStream", resumable = true)]
        fn disable_audio_stream(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.task(cx, task::Phase::Audio(-1))
        }
        #[tjs::method(name = "onStatusChanged", resumable = true)]
        fn on_status(&self, cx: &mut NativeCx<'_>, status: Value) -> NativeResult<NativeStep> {
            self.status_action.invoke_with(cx, &[("status", status)])
        }
        #[tjs::method(name = "onPeriod", resumable = true)]
        fn on_period(&self, cx: &mut NativeCx<'_>, reason: Value) -> NativeResult<NativeStep> {
            self.period_action.invoke_with(cx, &[("reason", reason)])
        }
        #[tjs::method(name = "onFrameUpdate", resumable = true)]
        fn on_frame(&self, cx: &mut NativeCx<'_>, frame: Value) -> NativeResult<NativeStep> {
            self.frame_action.invoke_with(cx, &[("frame", frame)])
        }
        #[tjs::method(name = "onCallbackCommand", resumable = true)]
        fn on_command(
            &self,
            cx: &mut NativeCx<'_>,
            command: Value,
            argument: Value,
        ) -> NativeResult<NativeStep> {
            self.command_action
                .invoke_with(cx, &[("command", command), ("argument", argument)])
        }
        #[tjs::getter(name = "status")]
        fn status(&self) -> NativeResult<String> {
            self.read(|r| r.status.to_owned())
        }
        #[tjs::getter(name = "mode")]
        fn mode(&self) -> NativeResult<i64> {
            self.read(|r| r.mode)
        }
        #[tjs::setter(name = "mode")]
        fn set_mode(&self, mode: i64) -> NativeResult<()> {
            if !(0..=3).contains(&mode) {
                return Err(NativeError::Message("invalid movie mode"));
            }
            self.write(|r| {
                if r.handle.is_none() {
                    r.mode = mode;
                }
            })
        }
        #[tjs::getter(name = "visible")]
        fn visible(&self) -> NativeResult<bool> {
            self.read(|r| r.visible)
        }
        #[tjs::setter(name = "visible")]
        fn set_visible(&self, visible: bool) -> NativeResult<()> {
            let l = self.lease()?;
            let mut w = l.shared.borrow_mut();
            let layers = w.layers.clone();
            let r = w.record_mut(l.id)?;
            r.visible = visible;
            if r.mode == 1 {
                layers
                    .borrow_mut()
                    .video_targets_visible(r.targets, visible)?;
            } else if let Some(images) = &r.images {
                layers
                    .borrow_mut()
                    .video_visible(r.window, images, visible, r.bounds);
            }
            r.geometry_dirty = true;
            Ok(())
        }
        #[tjs::getter(name = "loop")]
        fn looping(&self) -> NativeResult<bool> {
            self.read(|r| r.looping)
        }
        #[tjs::setter(name = "loop")]
        fn set_looping(&self, value: bool) -> NativeResult<()> {
            self.write(|r| r.looping = value)
        }
        #[tjs::getter(name = "left")]
        fn left(&self) -> NativeResult<i64> {
            self.read(|r| r.bounds.left as i64)
        }
        #[tjs::setter(name = "left")]
        fn set_left(&self, #[tjs(coerce)] value: i64) -> NativeResult<()> {
            if self.move_targets(Some(value as i32), None)? {
                return Ok(());
            }
            self.geometry(|r| r.left = value as i32)
        }
        #[tjs::getter(name = "top")]
        fn top(&self) -> NativeResult<i64> {
            self.read(|r| r.bounds.top as i64)
        }
        #[tjs::setter(name = "top")]
        fn set_top(&self, #[tjs(coerce)] value: i64) -> NativeResult<()> {
            if self.move_targets(None, Some(value as i32))? {
                return Ok(());
            }
            self.geometry(|r| r.top = value as i32)
        }
        #[tjs::getter(name = "width")]
        fn width(&self) -> NativeResult<i64> {
            self.read(|r| r.bounds.width as i64)
        }
        #[tjs::setter(name = "width")]
        fn set_width(&self, #[tjs(coerce)] value: i64) -> NativeResult<()> {
            self.geometry(|r| r.width = value.clamp(0, 16384) as u32)
        }
        #[tjs::getter(name = "height")]
        fn height(&self) -> NativeResult<i64> {
            self.read(|r| r.bounds.height as i64)
        }
        #[tjs::setter(name = "height")]
        fn set_height(&self, #[tjs(coerce)] value: i64) -> NativeResult<()> {
            self.geometry(|r| r.height = value.clamp(0, 16384) as u32)
        }
        #[tjs::getter(name = "originalWidth")]
        fn original_width(&self) -> NativeResult<i64> {
            self.read(|r| r.handle.as_ref().map_or(0, |h| h.info().size.width as i64))
        }
        #[tjs::getter(name = "originalHeight")]
        fn original_height(&self) -> NativeResult<i64> {
            self.read(|r| r.handle.as_ref().map_or(0, |h| h.info().size.height as i64))
        }
        #[tjs::getter(name = "fps")]
        fn fps(&self) -> NativeResult<f64> {
            self.read(|r| r.handle.as_ref().map_or(0.0, |h| h.info().fps))
        }
        #[tjs::getter(name = "numberOfFrame")]
        fn frames(&self) -> NativeResult<i64> {
            self.read(|r| r.handle.as_ref().map_or(0, |h| h.info().frames as i64))
        }
        #[tjs::getter(name = "totalTime")]
        fn total_time(&self) -> NativeResult<i64> {
            self.read(|r| {
                r.handle
                    .as_ref()
                    .map_or(0, |h| (h.info().duration * 1000.0) as i64)
            })
        }
        #[tjs::getter(name = "position")]
        fn position(&self) -> NativeResult<i64> {
            self.read(|r| {
                r.handle
                    .as_ref()
                    .map_or(0, |h| (h.position() * 1000.0) as i64)
            })
        }
        #[tjs::setter(name = "position", resumable = true)]
        fn set_position(&self, cx: &mut NativeCx<'_>, position: i64) -> NativeResult<NativeStep> {
            self.task(
                cx,
                task::Phase::Seek(position.max(0) as f64 / 1000.0, Box::new(task::Phase::Done)),
            )
        }
        #[tjs::getter(name = "frame")]
        fn frame(&self) -> NativeResult<i64> {
            self.read(|r| {
                r.handle
                    .as_ref()
                    .map_or(0, |h| (h.position() * h.info().fps).floor() as i64)
            })
        }
        #[tjs::setter(name = "frame", resumable = true)]
        fn set_frame(&self, cx: &mut NativeCx<'_>, frame: i64) -> NativeResult<NativeStep> {
            let fps = self.fps()?.max(1.0);
            self.task(
                cx,
                task::Phase::Seek(frame.max(0) as f64 / fps, Box::new(task::Phase::Done)),
            )
        }
        #[tjs::getter(name = "playRate")]
        fn rate(&self) -> NativeResult<f64> {
            self.read(|r| r.rate)
        }
        #[tjs::setter(name = "playRate")]
        fn set_rate(&self, rate: f64) -> NativeResult<()> {
            if !rate.is_finite() || !(0.1..=8.0).contains(&rate) {
                return Err(NativeError::Message(
                    "video playRate must be between 0.1 and 8",
                ));
            }
            let h = self.read(|r| r.handle.clone())?;
            if let Some(h) = h {
                h.rate(rate).map_err(NativeError::Detail)?;
            }
            self.write(|r| r.rate = rate)
        }
        #[tjs::getter(name = "layer1")]
        fn layer1(&self) -> NativeResult<Value> {
            self.read(|r| r.target_values[0])
        }
        #[tjs::setter(name = "layer1")]
        fn set_layer1(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.target(cx, 0, value)
        }
        #[tjs::getter(name = "layer2")]
        fn layer2(&self) -> NativeResult<Value> {
            self.read(|r| r.target_values[1])
        }
        #[tjs::setter(name = "layer2")]
        fn set_layer2(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.target(cx, 1, value)
        }
        #[tjs::getter(name = "segmentLoopStartFrame")]
        fn segment_start(&self) -> NativeResult<i64> {
            self.read(|r| r.segment.0)
        }
        #[tjs::getter(name = "segmentLoopEndFrame")]
        fn segment_end(&self) -> NativeResult<i64> {
            self.read(|r| r.segment.1)
        }
        #[tjs::getter(name = "periodEventFrame")]
        fn period(&self) -> NativeResult<i64> {
            self.read(|r| r.period)
        }
        #[tjs::setter(name = "periodEventFrame")]
        fn set_period(&self, frame: i64) -> NativeResult<()> {
            let current = self.frame()?;
            self.write(|r| {
                r.period = frame;
                r.period_past = frame <= current;
            })
        }
        #[tjs::getter(name = "numberOfAudioStream")]
        fn audio_count(&self) -> NativeResult<i64> {
            self.read(|r| {
                r.handle
                    .as_ref()
                    .map_or(0, |h| h.info().audio_streams as i64)
            })
        }
        #[tjs::getter(name = "numberOfVideoStream")]
        fn video_count(&self) -> NativeResult<i64> {
            self.read(|r| {
                r.handle
                    .as_ref()
                    .map_or(0, |h| h.info().video_streams as i64)
            })
        }
        #[tjs::getter(name = "enabledAudioStream")]
        fn audio_stream(&self) -> NativeResult<i64> {
            self.read(|r| r.audio_stream)
        }
        #[tjs::setter(name = "enabledAudioStream", resumable = true)]
        fn set_audio_stream(&self, cx: &mut NativeCx<'_>, index: i64) -> NativeResult<NativeStep> {
            self.task(cx, task::Phase::Audio(index))
        }
        #[tjs::getter(name = "enabledVideoStream")]
        fn video_stream(&self) -> NativeResult<i64> {
            self.read(|r| {
                r.handle
                    .as_ref()
                    .map_or(-1, |h| h.info().video_stream as i64)
            })
        }
        #[tjs::setter(name = "enabledVideoStream", resumable = true)]
        fn set_video_stream(&self, cx: &mut NativeCx<'_>, index: i64) -> NativeResult<NativeStep> {
            self.task(cx, task::Phase::Video(index))
        }
        #[tjs::getter(name = "audioVolume")]
        fn volume(&self) -> NativeResult<i64> {
            self.read(|r| r.volume as i64)
        }
        #[tjs::setter(name = "audioVolume")]
        fn set_volume(&self, volume: i64) -> NativeResult<()> {
            self.write(|r| {
                r.volume = volume.clamp(0, 100000) as i32;
                if let Some(h) = &r.handle {
                    h.gain(r.volume, r.balance);
                }
            })
        }
        #[tjs::getter(name = "audioBalance")]
        fn balance(&self) -> NativeResult<i64> {
            self.read(|r| r.balance as i64)
        }
        #[tjs::setter(name = "audioBalance")]
        fn set_balance(&self, balance: i64) -> NativeResult<()> {
            self.write(|r| {
                r.balance = balance.clamp(-100000, 100000) as i32;
                if let Some(h) = &r.handle {
                    h.gain(r.volume, r.balance);
                }
            })
        }
    }
}
pub(super) fn install(heap: &mut Heap, shared: Shared) -> NativeResult<()> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)?;
    heap.with_native_state::<implementation::State, _>(class, |s| s.service = Some(shared))?;
    Ok(())
}
