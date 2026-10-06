use super::*;
use crate::events::ActionOwner;
use tjs_core::{RestArgs, value};
pub(super) fn register_decoder(
    heap: &mut Heap,
    kind: krkr_audio::Codec,
) -> NativeResult<krkr_audio::Registration> {
    let class = heap
        .registered_class("WaveSoundBuffer")
        .ok_or(NativeError::This)?;
    let service = heap
        .with_native_state::<implementation::State, _>(class, |s| s.service.clone())?
        .ok_or(NativeError::This)?;
    service
        .borrow()
        .backend
        .register_decoder(kind)
        .map_err(NativeError::Detail)
}

pub(super) fn service(cx: &mut NativeCx<'_>) -> NativeResult<Shared> {
    let class = cx
        .heap()
        .registered_class("WaveSoundBuffer")
        .expect("installed sound");
    cx.heap_mut()
        .with_native_state::<implementation::State, _>(class, |s| s.service.clone())?
        .ok_or(NativeError::This)
}
#[tjs_bind::class(name = "WaveSoundBuffer")]
pub(super) mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub service: Option<Shared>,
        lease: Option<Lease>,
        status_action: ActionOwner,
        fade_action: ActionOwner,
        label_action: ActionOwner,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            if let Some(l) = &self.lease
                && let Ok(r) = l.shared.borrow().record(l.id)
            {
                r.flags.trace(visit);
                r.labels.trace(visit);
                r.filters.trace(visit);
                r.active_filters.trace(visit);
            }
            self.status_action.trace(visit);
            self.fade_action.trace(visit);
            self.label_action.trace(visit);
        }
    }
    impl State {
        pub(crate) fn lease(&self) -> NativeResult<&Lease> {
            self.lease.as_ref().ok_or(NativeError::This)
        }
        #[tjs::constructor]
        fn create(cx: &mut NativeCx<'_>, owner: Value) -> NativeResult<Self> {
            let status_action = ActionOwner::new(cx, owner, None, "onStatusChanged")?;
            let fade_action = ActionOwner::new(cx, owner, None, "onFadeCompleted")?;
            let label_action = ActionOwner::new(cx, owner, None, "onLabel")?;
            let shared = service(cx)?;
            let mut world = shared.borrow_mut();
            // KAG games pre-create large pools of unloaded buffers. The audio
            // service limits real decoder/voice leases when open() is called;
            // an empty script object has no PCM, decoder or mixer allocation.
            let source = world
                .events
                .borrow_mut()
                .insert(cx.this(), Kind::Sound, 4)?;
            let id = world.records.insert(Record {
                filters: cx.heap_mut().alloc_array(),
                active_filters: Vec::new(),
                flags: None,
                labels: None,
                owner: cx.this(),
                source,
                handle: None,
                status: "unload",
                volume: 100000,
                volume2: 100000,
                pan: 0,
                frequency: 0,
                looping: false,
                paused: false,
                use_vis_buffer: false,
                fade: None,
                events: VecDeque::new(),
            });
            drop(world);
            Ok(Self {
                service: None,
                lease: Some(Lease { shared, id }),
                status_action,
                fade_action,
                label_action,
            })
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::getter(name = "filters")]
        fn filters(&self) -> NativeResult<Value> {
            let lease = self.lease()?;
            let array = lease.shared.borrow().record(lease.id)?.filters;
            // The reference returns tTJSVariant(array, array). Numeric access
            // must retain that context inside script methods/accessors too.
            Ok(Value::Obj(tjs_core::ObjRef::bound(array)))
        }
        #[tjs::getter(name = "useVisBuffer")]
        fn use_vis_buffer(&self) -> NativeResult<bool> {
            let l = self.lease()?;
            Ok(l.shared.borrow().record(l.id)?.use_vis_buffer)
        }
        #[tjs::setter(name = "useVisBuffer")]
        fn set_use_vis_buffer(&self, enabled: i64) -> NativeResult<()> {
            let enabled = enabled as i32 != 0;
            let l = self.lease()?;
            let mut world = l.shared.borrow_mut();
            let r = world.record_mut(l.id)?;
            if let Some(h) = &r.handle {
                h.visualization(enabled).map_err(NativeError::Detail)?;
            }
            r.use_vis_buffer = enabled;
            Ok(())
        }
        #[tjs::method(name = "getVisBuffer")]
        fn get_vis_buffer(
            &self,
            cx: &mut NativeCx<'_>,
            buffer: i64,
            count: i64,
            channels: i64,
            args: RestArgs<'_>,
        ) -> NativeResult<i64> {
            let count = count as i32;
            let channels = channels as i32;
            let ahead = args
                .first()
                .map(|v| value::to_integer(cx.heap(), *v))
                .transpose()?
                .unwrap_or(0) as i32;
            let l = self.lease()?;
            let world = l.shared.borrow();
            let r = world.record(l.id)?;
            if !r.use_vis_buffer || count <= 0 || channels <= 0 {
                return Ok(0);
            }
            let Some(handle) = &r.handle else {
                return Ok(0);
            };
            let storage = world
                .sample_buffers
                .get(&buffer)
                .and_then(std::rc::Weak::upgrade)
                .ok_or(NativeError::Message(
                    "getVisBuffer requires a live managed sample buffer",
                ))?;
            let mut storage = storage.borrow_mut();
            let size = (count as usize)
                .checked_mul(channels as usize)
                .ok_or(NativeError::Message("sample buffer size overflow"))?;
            let dest = storage
                .samples
                .get_mut(..size)
                .ok_or(NativeError::Message("sample buffer is too small"))?;
            Ok(handle.read_visualization(dest, channels as usize, ahead) as i64)
        }
        #[tjs::invalidate(resumable = true)]
        fn invalidate(&mut self) -> NativeResult<NativeStep> {
            let labels = self
                .lease
                .as_ref()
                .and_then(|l| l.shared.borrow_mut().record_mut(l.id).ok()?.labels.take());
            self.lease.take();
            Ok(if let Some(labels) = labels {
                NativeStep::Invalidate {
                    object: Value::Obj(labels.into()),
                    continuation: Box::new(Done),
                }
            } else {
                NativeStep::Return(Value::Void)
            })
        }
        #[tjs::method(name = "open", resumable = true)]
        fn open(&self, cx: &mut NativeCx<'_>, storage: Value) -> NativeResult<NativeStep> {
            let Value::Str(id) = value::to_string(cx.heap_mut(), storage)? else {
                unreachable!()
            };
            let path = tjs_core::string::c_string(cx.heap().string(id)?).to_vec();
            task::start(self.lease()?, cx, task::Phase::OpenStop(path))
        }
        #[tjs::method(name = "play", resumable = true)]
        fn play(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            task::start(self.lease()?, cx, task::Phase::Play)
        }
        #[tjs::method(name = "stop", resumable = true)]
        fn stop(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            task::start(self.lease()?, cx, task::Phase::Stop)
        }
        #[tjs::method(name = "fade", resumable = true)]
        fn fade(
            &self,
            cx: &mut NativeCx<'_>,
            to: Value,
            time: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let to = value::to_integer(cx.heap(), to)? as i32;
            let time = value::to_integer(cx.heap(), time)? as i32;
            let delay = args
                .first()
                .filter(|v| !matches!(v, Value::Void))
                .map(|v| value::to_integer(cx.heap(), *v))
                .transpose()?
                .unwrap_or(0) as i32;
            if time <= 0 || delay < 0 {
                return Err(NativeError::Message("invalid fade duration"));
            }
            task::start(self.lease()?, cx, task::Phase::FadeStop(to, time, delay))
        }
        #[tjs::method(name = "stopFade", resumable = true)]
        fn stop_fade(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            task::start(self.lease()?, cx, task::Phase::StopFade)
        }
        #[tjs::method(name = "onStatusChanged", resumable = true)]
        fn on_status(&self, cx: &mut NativeCx<'_>, status: Value) -> NativeResult<NativeStep> {
            self.lease()?;
            self.status_action.invoke_with(cx, &[("status", status)])
        }
        #[tjs::method(name = "onFadeCompleted", resumable = true)]
        fn on_fade(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.lease()?;
            self.fade_action.invoke(cx)
        }
        #[tjs::method(name = "onLabel", resumable = true)]
        fn on_label(&self, cx: &mut NativeCx<'_>, name: Value) -> NativeResult<NativeStep> {
            self.lease()?;
            self.label_action.invoke_with(cx, &[("name", name)])
        }
        #[tjs::getter(name = "status")]
        fn status(&self) -> NativeResult<String> {
            let l = self.lease()?;
            Ok(l.shared.borrow().record(l.id)?.status.into())
        }
        #[tjs::getter(name = "flags")]
        fn flags(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let l = self.lease()?;
            let mut w = l.shared.borrow_mut();
            let r = w.record_mut(l.id)?;
            let object = if let Some(object) = r.flags {
                object
            } else {
                let object =
                    super::super::flags::object(cx.heap_mut(), l.shared.clone(), l.id, r.owner)?;
                r.flags = Some(object);
                object
            };
            Ok(Value::Obj(object.into()))
        }
        #[tjs::getter(name = "labels")]
        fn labels(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let l = self.lease()?;
            let mut w = l.shared.borrow_mut();
            let r = w.record_mut(l.id)?;
            if let Some(object) = r.labels {
                return Ok(Value::Obj(object.into()));
            }
            let object = cx.heap_mut().alloc_dictionary();
            if let Some(h) = &r.handle {
                for label in h.labels() {
                    let item = cx.heap_mut().alloc_dictionary();
                    for (name, value) in [
                        ("name", text(cx.heap_mut(), &label.name)),
                        ("samplePosition", Value::Int(label.position as i64)),
                        (
                            "position",
                            Value::Int(
                                (label.position.saturating_mul(1000) / h.format().rate as u64)
                                    as i64,
                            ),
                        ),
                    ] {
                        let key = cx.heap_mut().intern_str(name);
                        cx.heap_mut().set_member(item, key, value)?;
                    }
                    if !label.name.is_empty() {
                        let key = cx.heap_mut().intern_str(&label.name);
                        cx.heap_mut()
                            .set_member(object, key, Value::Obj(item.into()))?;
                    }
                }
            }
            r.labels = Some(object);
            Ok(Value::Obj(object.into()))
        }

        #[tjs::getter(name = "volume")]
        fn get_volume(&self) -> NativeResult<i64> {
            let l = self.lease()?;
            Ok(l.shared.borrow().record(l.id)?.volume.into())
        }
        #[tjs::setter(name = "volume")]
        fn set_volume(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = value::to_integer(cx.heap(), v)? as i32;
            let v = v.clamp(0, 100000);
            let l = self.lease()?;
            let mut w = l.shared.borrow_mut();
            w.record_mut(l.id)?.volume = v;
            w.update_gain(l.id);
            Ok(())
        }

        #[tjs::getter(name = "volume2")]
        fn get_volume2(&self) -> NativeResult<i64> {
            let l = self.lease()?;
            Ok(l.shared.borrow().record(l.id)?.volume2.into())
        }
        #[tjs::setter(name = "volume2")]
        fn set_volume2(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = value::to_integer(cx.heap(), v)? as i32;
            let v = v.clamp(0, 100000);
            let l = self.lease()?;
            let mut w = l.shared.borrow_mut();
            w.record_mut(l.id)?.volume2 = v;
            w.update_gain(l.id);
            Ok(())
        }

        #[tjs::getter(name = "pan")]
        fn get_pan(&self) -> NativeResult<i64> {
            let l = self.lease()?;
            Ok(l.shared.borrow().record(l.id)?.pan.into())
        }
        #[tjs::setter(name = "pan")]
        fn set_pan(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = value::to_integer(cx.heap(), v)? as i32;
            let v = v.clamp(-100000, 100000);
            let l = self.lease()?;
            let mut w = l.shared.borrow_mut();
            w.record_mut(l.id)?.pan = v;
            w.update_gain(l.id);
            Ok(())
        }

        #[tjs::getter(name = "frequency")]
        fn get_frequency(&self) -> NativeResult<i64> {
            let l = self.lease()?;
            Ok(l.shared.borrow().record(l.id)?.frequency.into())
        }
        #[tjs::setter(name = "frequency")]
        fn set_frequency(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = value::to_integer(cx.heap(), v)? as i32;
            if v <= 0 {
                return Err(NativeError::Message("audio frequency must be positive"));
            }
            let l = self.lease()?;
            let mut w = l.shared.borrow_mut();
            w.record_mut(l.id)?.frequency = v;
            if let Some(h) = &w.record(l.id)?.handle {
                h.frequency(v);
            }
            Ok(())
        }

        #[tjs::getter(name = "paused")]
        fn get_paused(&self) -> NativeResult<bool> {
            let l = self.lease()?;
            Ok(l.shared.borrow().record(l.id)?.paused)
        }
        #[tjs::setter(name = "paused")]
        fn set_paused(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = (value::to_integer(cx.heap(), v)? as i32) != 0;
            let l = self.lease()?;
            let mut w = l.shared.borrow_mut();
            let r = w.record_mut(l.id)?;
            r.paused = v;
            if let Some(h) = &r.handle {
                h.pause(v);
            }
            Ok(())
        }

        #[tjs::getter(name = "looping")]
        fn get_looping(&self) -> NativeResult<bool> {
            let l = self.lease()?;
            Ok(l.shared.borrow().record(l.id)?.looping)
        }
        #[tjs::setter(name = "looping")]
        fn set_looping(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = (value::to_integer(cx.heap(), v)? as i32) != 0;
            let l = self.lease()?;
            let mut w = l.shared.borrow_mut();
            let r = w.record_mut(l.id)?;
            r.looping = v;
            if let Some(h) = &r.handle {
                h.looping(v);
            }
            Ok(())
        }

        #[tjs::getter(name = "bits")]
        fn get_bits(&self) -> NativeResult<i64> {
            let l = self.lease()?;
            let w = l.shared.borrow();
            let f = w
                .record(l.id)?
                .handle
                .as_ref()
                .map(|h| h.format())
                .unwrap_or_default();
            Ok(f.bits as i64)
        }

        #[tjs::getter(name = "channels")]
        fn get_channels(&self) -> NativeResult<i64> {
            let l = self.lease()?;
            let w = l.shared.borrow();
            let f = w
                .record(l.id)?
                .handle
                .as_ref()
                .map(|h| h.format())
                .unwrap_or_default();
            Ok(f.channels as i64)
        }

        #[tjs::getter(name = "totalTime")]
        fn get_totaltime(&self) -> NativeResult<i64> {
            let l = self.lease()?;
            let w = l.shared.borrow();
            let f = w
                .record(l.id)?
                .handle
                .as_ref()
                .map(|h| h.format())
                .unwrap_or_default();
            Ok((f.frames.saturating_mul(1000) / f.rate.max(1) as u64) as i64)
        }

        #[tjs::getter(name = "position")]
        fn get_position(&self) -> NativeResult<i64> {
            let l = self.lease()?;
            let w = l.shared.borrow();
            let Some(h) = &w.record(l.id)?.handle else {
                return Ok(0);
            };
            let p = h.position().played;
            Ok((p.saturating_mul(1000) / h.format().rate.max(1) as u64) as i64)
        }
        #[tjs::setter(name = "position", resumable = true)]
        fn set_position(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<NativeStep> {
            let mut p = value::to_integer(cx.heap(), v)? as u64;
            let l = self.lease()?;
            let rate = l
                .shared
                .borrow()
                .record(l.id)?
                .handle
                .as_ref()
                .map_or(0, |h| h.format().rate);
            p = p.wrapping_mul(rate as u64) / 1000;
            task::start(l, cx, task::Phase::Seek(p))
        }

        #[tjs::getter(name = "samplePosition")]
        fn get_sampleposition(&self) -> NativeResult<i64> {
            let l = self.lease()?;
            let w = l.shared.borrow();
            let Some(h) = &w.record(l.id)?.handle else {
                return Ok(0);
            };
            let p = h.position().played;
            Ok(p as i64)
        }
        #[tjs::setter(name = "samplePosition", resumable = true)]
        fn set_sampleposition(
            &mut self,
            cx: &mut NativeCx<'_>,
            v: Value,
        ) -> NativeResult<NativeStep> {
            let p = value::to_integer(cx.heap(), v)? as u64;
            let l = self.lease()?;
            task::start(l, cx, task::Phase::Seek(p))
        }

        #[tjs::getter(name = "globalVolume", class_only = true)]
        fn global_volume(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(service(cx)?.borrow().global_volume.into())
        }
        #[tjs::setter(name = "globalVolume", class_only = true)]
        fn set_global_volume(cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = (value::to_integer(cx.heap(), v)? as i32).clamp(0, 100000);
            let shared = service(cx)?;
            let mut w = shared.borrow_mut();
            w.global_volume = v;
            for id in w.records.keys() {
                w.update_gain(id);
            }
            Ok(())
        }
    }
}
pub(super) fn install(heap: &mut Heap, shared: Shared) -> NativeResult<()> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)?;
    heap.with_native_state::<implementation::State, _>(class, |s| s.service = Some(shared))?;
    filter::install(heap, class)?;
    Ok(())
}
pub(super) fn link(heap: &mut Heap, value: Value) -> NativeResult<(Shared, SoundId, ObjId)> {
    let Value::Obj(reference) = value else {
        return Err(NativeError::Type("a WaveSoundBuffer"));
    };
    let owner = reference.object.ok_or(NativeError::This)?;
    heap.with_native_state::<implementation::State, _>(owner, |s| {
        s.lease().map(|l| (l.shared.clone(), l.id, owner))
    })?
}
