use super::*;
use krkr_protocol::transition::{Direction, Stay};
const COMMON: &[&str] = &["selfupdate", "callback", "time"];

pub(in crate::layer) fn start(
    shared: Shared,
    destination: LayerId,
    source: LayerId,
    name: &str,
    with_children: bool,
    options: Value,
) -> NativeResult<NativeStep> {
    let provider = shared.borrow().transition_providers.get(name).cloned();
    let effect = match name {
        "crossfade" => Effect::CrossFade,
        "universal" => Effect::Universal { vague: 64 },
        "scroll" => Effect::Scroll {
            from: Direction::Left,
            stay: Stay::Neither,
        },
        _ if provider.is_some() => Effect::Custom,
        _ => return Err(NativeError::Message("unknown transition handler")),
    };
    let task = Begin {
        shared,
        destination,
        source,
        effect,
        provider,
        custom: None,
        with_children,
        options,
        index: 0,
        values: Vec::with_capacity(5),
    };
    task.validate()?;
    Ok(NativeStep::Continue(Box::new(task)))
}
struct Begin {
    shared: Shared,
    destination: LayerId,
    source: LayerId,
    effect: Effect,
    provider: Option<Arc<provider::Provider>>,
    custom: Option<krkr_protocol::transition::custom::Frame>,
    with_children: bool,
    options: Value,
    index: usize,
    values: Vec<Value>,
}
impl Trace for Begin {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        let world = self.shared.borrow();
        for id in [self.destination, self.source] {
            if let Some(r) = world.records.get(id) {
                r.owner.trace(visit);
            }
        }
        self.options.trace(visit);
        self.values.trace(visit);
    }
}
impl Begin {
    fn create_custom(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        rule: Option<Arc<krkr_protocol::pixels::Pixels>>,
    ) -> NativeResult<NativeStep> {
        let mut task = self;
        let (size, _) = task.validate()?;
        let host = task.shared.borrow().host()?;
        let p = task.provider.as_ref().expect("custom provider");
        let instance =
            (p.definition.create)(cx, size, &task.values[3..], rule, host.staging_budget())?;
        if instance.kernel() != p.definition.kernel {
            return Err(NativeError::Message(
                "transition factory returned a different kernel",
            ));
        }
        let Value::Int(duration) = task.values[2] else {
            unreachable!()
        };
        task.custom = Some(krkr_protocol::transition::custom::Frame {
            instance,
            elapsed: 0,
            duration: duration as u64,
            lifetime: p.lifetime.clone(),
        });
        task.install(None)
    }
    fn validate(&self) -> NativeResult<(Size, DrawFace)> {
        let world = self.shared.borrow();
        let a = world.record(self.destination)?;
        let b = world.record(self.source)?;
        if a.transition.is_some() {
            return Err(NativeError::Message(
                "current transition must be stopped first",
            ));
        }
        if a.shutdown || b.shutdown {
            return Err(NativeError::Message("transition layer is shutting down"));
        }
        if a.window != b.window {
            return Err(NativeError::Message(
                "transition layers belong to different tree owners",
            ));
        }
        if b.transition
            .and_then(|id| world.transitions.get(id))
            .is_some_and(|t| t.source == self.destination)
        {
            return Err(NativeError::Message(
                "transition sources cannot refer to each other",
            ));
        }
        let size = if self.with_children {
            a.geometry.size()
        } else {
            a.image()?;
            b.image()?;
            a.geometry.image_size
        };
        if size
            != if self.with_children {
                b.geometry.size()
            } else {
                b.geometry.image_size
            }
        {
            return Err(NativeError::Message(
                "transition images must have equal dimensions",
            ));
        }
        Ok((size, a.blend.face()))
    }
    fn keys(&self) -> impl Iterator<Item = &&'static str> {
        COMMON.iter().chain(
            match self.effect {
                Effect::CrossFade => [].as_slice(),
                Effect::Universal { .. } => ["vague", "rule"].as_slice(),
                Effect::Scroll { .. } => ["from", "stay"].as_slice(),
                Effect::Custom => {
                    self.provider
                        .as_ref()
                        .expect("custom provider")
                        .definition
                        .options
                }
            }
            .iter(),
        )
    }
    fn install(self, mut rule: Option<images::Staged>) -> NativeResult<NativeStep> {
        let (size, face) = self.validate()?;
        let mut world = self.shared.borrow_mut();
        let now = world.windows.borrow().now();
        let callback = self.values[1];
        // These conversions have already succeeded before resource loading.
        let Value::Int(duration) = self.values[2] else {
            unreachable!("converted duration");
        };
        let Value::Int(self_update) = self.values[0] else {
            unreachable!("converted selfupdate");
        };
        let id = world.transitions.insert(Active {
            destination: self.destination,
            source: self.source,
            callback,
            with_children: self.with_children,
            frame: Frame {
                effect: self.effect,
                face,
                size,
                phase: 0,
            },
            duration: duration as u64,
            start: (!matches!(callback, Value::Void)).then_some(0),
            next: now,
            self_update: self_update != 0,
            refresh: true,
            queued: false,
            complete: false,
            rule: rule.as_mut().and_then(|r| r.image.take()),
            custom: self.custom,
        });
        let r = world.record_mut(self.destination)?;
        r.transition = Some(id);
        let window = r.window;
        world.dirty.insert(window);
        drop(world);
        Ok(NativeStep::Return(Value::Void))
    }
}
impl NativeContinuation for Begin {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        mut value: Value,
    ) -> NativeResult<NativeStep> {
        if self.index != 0 {
            if let Some(p) = &self.provider {
                if self.index == 3 {
                    if matches!(value, Value::Void) {
                        return Err(NativeError::Message("transition requires the time option"));
                    }
                    value = Value::Int(
                        (tjs_core::value::to_integer(cx.heap(), value)? as u64).max(2) as i64,
                    );
                } else if self.index > 3 {
                    value = (p.definition.convert)(
                        cx,
                        self.validate()?.0,
                        self.index - 4,
                        &self.values[3..],
                        value,
                    )?;
                }
            }
            self.values.push(value);
        }
        let key = self.keys().nth(self.index).copied();
        if let Some(key) = key {
            if matches!(
                self.options,
                Value::Void | Value::Obj(ObjRef { object: None, .. })
            ) {
                return Err(NativeError::Message("transition requires the time option"));
            }
            let key = Value::Str(
                cx.heap_mut()
                    .alloc_string(key.encode_utf16().collect::<Vec<_>>()),
            );
            self.index += 1;
            return Ok(NativeStep::GetOptional {
                object: self.options,
                key,
                continuation: self,
            });
        }
        if matches!(self.values[2], Value::Void) {
            return Err(NativeError::Message("transition requires the time option"));
        }
        self.values[0] = Value::Int(i64::from(self.values[0].truthy(cx.heap())?));
        if self.provider.is_none() {
            self.values[2] =
                Value::Int(tjs_core::value::to_integer(cx.heap(), self.values[2])?.max(2));
        }
        if !matches!(self.values[1], Value::Void | Value::Obj(_)) {
            return Err(NativeError::Type("a transition clock callback object"));
        }
        match self.effect {
            Effect::Custom => {
                self.validate()?;
                let host = self.shared.borrow().host()?;
                let p = self.provider.as_ref().expect("custom provider");
                if !host.supports_transition(p.definition.kernel) {
                    return Err(NativeError::Detail(format!(
                        "render host does not support transition kernel {}",
                        p.definition.kernel
                    )));
                }
                if let Some(index) = p.definition.rule_option {
                    let rule = self.values[3 + index];
                    if matches!(rule, Value::Obj(_)) {
                        return crate::extensions::layer_read_pixels(cx, rule, self);
                    }
                    if matches!(rule, Value::Void) {
                        return Err(NativeError::Message("transition requires the rule option"));
                    }
                    let Value::Str(name) = tjs_core::value::to_string(cx.heap_mut(), rule)? else {
                        unreachable!()
                    };
                    let options = crate::storages::image::Options {
                        name: tjs_core::string::c_string(cx.heap().string(name)?).to_vec(),
                        key: 0x02ffffff,
                        size: None,
                        grayscale: false,
                        budget: host.staging_budget(),
                    };
                    return crate::storages::image::request(
                        cx,
                        options,
                        self,
                        |task, _, request| {
                            RuleLoad::start(task, crate::io::Work::ImageProbe(request))
                        },
                    );
                }
                self.create_custom(cx, None)
            }
            Effect::Universal { .. } => {
                let vague = if matches!(self.values[3], Value::Void) {
                    64
                } else {
                    bindings::integer(cx, self.values[3])?
                };
                if !(0..=i32::MAX / 255).contains(&vague) {
                    return Err(NativeError::Message(
                        "transition vague exceeds integer kernel range",
                    ));
                }
                self.effect = Effect::Universal {
                    vague: vague as u32,
                };
                if matches!(self.values[4], Value::Void) {
                    return Err(NativeError::Message(
                        "universal transition requires the rule option",
                    ));
                }
                let Value::Str(name) = tjs_core::value::to_string(cx.heap_mut(), self.values[4])?
                else {
                    unreachable!()
                };
                let name = tjs_core::string::c_string(cx.heap().string(name)?).to_vec();
                let (size, _) = self.validate()?;
                let budget = self.shared.borrow().host()?.staging_budget();
                let options = crate::storages::image::Options {
                    name,
                    key: 0x02ffffff,
                    size: Some(size),
                    grayscale: true,
                    budget,
                };
                crate::storages::image::request(cx, options, self, |task, _, request| {
                    task.validate()?;
                    let shared = task.shared.clone();
                    loading::start_with(&shared, task.destination, request, Some(task))
                })
            }
            Effect::Scroll { .. } => {
                let number = |v| {
                    if matches!(v, Value::Void) {
                        Ok(0)
                    } else {
                        bindings::integer(cx, v)
                    }
                };
                self.effect = Effect::Scroll {
                    from: Direction::from_legacy(number(self.values[3])?)
                        .ok_or(NativeError::Message("invalid scroll direction"))?,
                    stay: Stay::from_legacy(number(self.values[4])?)
                        .ok_or(NativeError::Message("invalid scroll stay mode"))?,
                };
                self.install(None)
            }
            _ => self.install(None),
        }
    }
}
impl crate::extensions::PixelContinuation for Begin {
    fn pixels(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<krkr_protocol::pixels::Pixels>,
    ) -> NativeResult<NativeStep> {
        self.create_custom(cx, Some(pixels))
    }
}
struct RuleLoad {
    task: Box<Begin>,
    delivery: crate::io::Delivery,
}
impl Trace for RuleLoad {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.task.trace(visit);
    }
}
impl RuleLoad {
    fn start(task: Box<Begin>, work: crate::io::Work) -> NativeResult<NativeStep> {
        task.validate()?;
        let operations = task.shared.borrow().windows.borrow().operations.clone();
        let delivery = crate::io::Delivery::default();
        crate::operations::Operations::wait(
            &operations,
            crate::operations::Request::Read(Box::new(work), delivery.clone()),
            tjs_core::WaitMode::Internal,
            Box::new(Self { task, delivery }),
        )
    }
}
impl NativeContinuation for RuleLoad {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let result = self
            .delivery
            .borrow_mut()
            .take()
            .expect("rule image completion");
        match result {
            crate::io::Data::ImagePrepared(prepared) => {
                Self::start(self.task, crate::io::Work::ImageDecode(*prepared))
            }
            crate::io::Data::Image(image) => {
                self.task.create_custom(cx, Some(Arc::new(image.pixels)))
            }
            _ => Err(NativeError::Message("unexpected transition rule response")),
        }
    }
}
impl loading::Loaded for Begin {
    fn loaded(
        self: Box<Self>,
        _: &mut NativeCx<'_>,
        image: images::Staged,
    ) -> NativeResult<NativeStep> {
        self.install(Some(image))
    }
}
