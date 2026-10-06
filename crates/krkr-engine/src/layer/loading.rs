use super::*;
use crate::{
    io,
    operations::{Operations, Request},
};
use krkr_protocol::{graphics::Command, window::Response};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, WaitMode};

use super::images::Staged;
enum Phase {
    Probe(io::Delivery),
    Prepare(crate::window::Delivery, Box<krkr_image::Prepared>),
    Decode(io::Delivery),
    Upload(crate::window::Delivery, krkr_image::Tags),
    Cache(crate::window::Delivery, krkr_image::Tags, Staged),
}
struct Loading {
    shared: Shared,
    id: Option<LayerId>,
    window: WindowId,
    staged: Option<Staged>,
    phase: Option<Phase>,
    size: Size,
    province_only: bool,
    completion: Option<Box<dyn Loaded>>,
    cache_key: Option<krkr_protocol::image_cache::Key>,
    cache_generation: u64,
    cache_bytes: usize,
    trace_name: Option<Vec<u16>>,
    cache_hit: bool,
    phase_timer: krkr_protocol::diagnostics::Timer,
}
/// Standalone images use the same probe/reserve/decode/upload pipeline. The
/// consumer takes the lease only after upload succeeds, without mutating Layer.
pub(super) trait Loaded: Trace {
    fn loaded(self: Box<Self>, cx: &mut NativeCx<'_>, image: Staged) -> NativeResult<NativeStep>;
}
impl Trace for Loading {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(completion) = &self.completion {
            completion.trace(visit);
        }
        let world = self.shared.borrow();
        if let Some(record) = self.id.and_then(|id| world.records.get(id)) {
            record.owner.trace(visit);
            record.action_owner.trace(visit);
        }
        world.windows.borrow().owner(self.window).trace(visit);
    }
}
impl Loading {
    fn operations(&self) -> crate::operations::Shared {
        self.shared.borrow().windows.borrow().operations.clone()
    }
    fn graphics(
        mut self,
        command: Command,
        phase: impl FnOnce(crate::window::Delivery) -> Phase,
    ) -> NativeResult<NativeStep> {
        let (host, window) = {
            let world = self.shared.borrow();
            if let Some(id) = self.id {
                world.record(id)?;
            }
            (world.host()?, self.window)
        };
        self.phase_timer = krkr_protocol::diagnostics::Timer::start();
        let ticket = host
            .request(window, krkr_protocol::window::Command::Graphics(command))
            .map_err(NativeError::Detail)?;
        let delivery = crate::window::Delivery::default();
        let operations = self.operations();
        let mut this = self;
        this.phase = Some(phase(delivery.clone()));
        Operations::wait(
            &operations,
            Request::Window(ticket, delivery),
            WaitMode::Internal,
            Box::new(this),
        )
    }
    fn decode(mut self, prepared: krkr_image::Prepared) -> NativeResult<NativeStep> {
        self.phase_timer = krkr_protocol::diagnostics::Timer::start();
        let operations = self.operations();
        let delivery = io::Delivery::default();
        self.phase = Some(Phase::Decode(delivery.clone()));
        Operations::wait(
            &operations,
            Request::Read(Box::new(io::Work::ImageDecodeCompact(prepared)), delivery),
            WaitMode::Internal,
            Box::new(self),
        )
    }
}
impl NativeContinuation for Loading {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let phase = self.phase.take().expect("image load phase");
        self.phase_timer.report(|| {
            let stage = match &phase {
                Phase::Probe(_) => "probe",
                Phase::Prepare(..) => "reserve",
                Phase::Decode(_) => "decode",
                Phase::Upload(..) if self.cache_hit => "cached-assign",
                Phase::Upload(..) => "upload",
                Phase::Cache(..) => "retain-cache",
            };
            format!(
                "stage=image-wait phase={stage} name={}",
                String::from_utf16_lossy(self.trace_name.as_deref().unwrap_or_default())
            )
        });
        match phase {
            Phase::Probe(delivery) => {
                let io::Data::ImagePrepared(prepared) = delivery
                    .borrow_mut()
                    .take()
                    .expect("image probe completion")
                else {
                    return Err(NativeError::Message("unexpected image probe response"));
                };
                self.size = prepared.size;
                self.province_only = prepared.province_only;
                self.cache_bytes = if self.completion.is_some() {
                    prepared.size.rgba_bytes().unwrap_or(usize::MAX) / 4
                } else {
                    prepared.compressed_upload_bytes().unwrap_or_else(|| {
                        prepared
                            .upload_size()
                            .rgba_bytes()
                            .unwrap_or(usize::MAX)
                            .saturating_add(if prepared.has_province() {
                                prepared.size.width as usize * prepared.size.height as usize
                            } else {
                                0
                            })
                    })
                };
                if self.id.is_none() && self.cache_bytes > self.operations().borrow().images.limit()
                {
                    // Skip preloads that cannot fit even in their native format.
                    // Charging compressed assets as RGBA rejects useful hints
                    // and prematurely stops the batch. The host's actual charge
                    // below still bounds retention if it needs an RGBA fallback.
                    return Ok(NativeStep::Return(Value::Int(self.cache_bytes as i64)));
                }
                let (image, source) = {
                    let mut world = self.shared.borrow_mut();
                    let source = self
                        .id
                        .map(|id| world.record(id).map(|r| r.image.clone()))
                        .transpose()?
                        .flatten();
                    let image = ImageRef {
                        id: world.images.insert(()),
                        lifetime: Arc::default(),
                    };
                    (image, source)
                };
                if prepared.is_compressed_upload() {
                    let (texture, tags) = prepared
                        .into_compressed_with_tags()
                        .map_err(|e| NativeError::Detail(e.to_string()))?;
                    self.staged = Some(Staged {
                        shared: self.shared.clone(),
                        image: Some(image.clone()),
                        has_main: true,
                    });
                    // The IO probe already owns the GPU blocks. Sending them
                    // directly avoids both the RGBA reservation and decode job.
                    // The host reports its actual allocation for cache retention.
                    let command = Command::LoadCompressed {
                        image,
                        texture: Arc::new(texture),
                        logical_size: self.size,
                    };
                    return self.graphics(command, |d| Phase::Upload(d, tags));
                }
                let command = if self.completion.is_some() {
                    Command::CreateProvince {
                        image: image.clone(),
                        size: prepared.size,
                        operation: krkr_protocol::graphics::ProvinceOperation::Reserve,
                    }
                } else {
                    Command::BeginUpload {
                        image: image.clone(),
                        size: prepared.upload_size(),
                        main: !prepared.province_only,
                        province: prepared.has_province(),
                        staging_bytes: prepared
                            .decode_staging_bytes()
                            .map_err(|e| NativeError::Detail(e.to_string()))?,
                        source: if self.id.is_some() {
                            Some(
                                source
                                    .ok_or(NativeError::Message("layer has no drawable image"))?,
                            )
                        } else {
                            None
                        },
                    }
                };
                self.staged = Some(Staged {
                    shared: self.shared.clone(),
                    image: Some(image),
                    has_main: true,
                });
                self.graphics(command, |delivery| Phase::Prepare(delivery, prepared))
            }
            Phase::Prepare(delivery, prepared) => {
                if !matches!(delivery.borrow_mut().take(), Some(Response::Done)) {
                    return Err(NativeError::Message(
                        "unexpected image reservation response",
                    ));
                }
                self.decode(*prepared)
            }
            Phase::Decode(delivery) => {
                let io::Data::Image(decoded) = delivery
                    .borrow_mut()
                    .take()
                    .expect("image decode completion")
                else {
                    return Err(NativeError::Message("unexpected image decode response"));
                };
                let image = self
                    .staged
                    .as_ref()
                    .and_then(|s| s.image.clone())
                    .expect("reserved image");
                let command = if decoded.pixels.size != self.size {
                    Command::UploadScaled {
                        image,
                        pixels: Arc::new(decoded.pixels),
                        logical_size: self.size,
                    }
                } else {
                    Command::Upload {
                        image,
                        pixels: Arc::new(decoded.pixels),
                    }
                };
                self.graphics(command, |delivery| Phase::Upload(delivery, decoded.tags))
            }
            Phase::Upload(delivery, tags) => {
                match delivery.borrow_mut().take() {
                    Some(Response::Done) => {}
                    Some(Response::ImageStorage(bytes)) => self.cache_bytes = bytes,
                    _ => return Err(NativeError::Message("unexpected image upload response")),
                }
                if self.id.is_none() {
                    // A preload owns no Layer; retain this uploaded image
                    // directly, without an extra assignment/graphics request.
                    self.operations().borrow().images.insert(
                        self.cache_key.take().expect("preload cache key"),
                        krkr_protocol::image_cache::Entry {
                            image: self
                                .staged
                                .as_ref()
                                .unwrap()
                                .image
                                .as_ref()
                                .unwrap()
                                .clone(),
                            size: self.size,
                            tags: Arc::new(tags),
                            bytes: self.cache_bytes,
                        },
                        self.cache_generation,
                    );
                    return Ok(NativeStep::Return(Value::Int(self.cache_bytes as i64)));
                }
                if self.cache_key.is_some()
                    && self.cache_bytes <= self.operations().borrow().images.limit()
                {
                    let alias = Staged::reserve(&self.shared);
                    let command = Command::Assign {
                        image: alias.image.as_ref().unwrap().clone(),
                        source: self
                            .staged
                            .as_ref()
                            .unwrap()
                            .image
                            .as_ref()
                            .unwrap()
                            .clone(),
                    };
                    return self.graphics(command, |delivery| Phase::Cache(delivery, tags, alias));
                }
                self.finish(cx, tags)
            }
            Phase::Cache(delivery, tags, alias) => {
                if !matches!(delivery.borrow_mut().take(), Some(Response::Done)) {
                    return Err(NativeError::Message("unexpected image cache response"));
                }
                self.operations().borrow().images.insert(
                    self.cache_key.take().unwrap(),
                    krkr_protocol::image_cache::Entry {
                        image: alias.image.as_ref().unwrap().clone(),
                        size: self.size,
                        tags: Arc::new(tags.clone()),
                        bytes: self.cache_bytes,
                    },
                    self.cache_generation,
                );
                self.finish(cx, tags)
            }
        }
    }
}
impl Loading {
    fn finish(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        tags: krkr_image::Tags,
    ) -> NativeResult<NativeStep> {
        let mut staged = self.staged.take().expect("staged image");
        if let Some(completion) = self.completion.take() {
            return completion.loaded(cx, staged);
        }
        let id = self.id.expect("image load destination");
        let geometry = if self.province_only {
            None
        } else {
            Some(
                self.shared
                    .borrow()
                    .record(id)?
                    .geometry
                    .with_image(self.size),
            )
        };
        staged.commit(id, geometry)?;
        let result = if self.province_only {
            Value::Void
        } else if tags.is_empty() {
            null()
        } else {
            let dictionary = cx.heap_mut().alloc_dictionary();
            for (name, text) in tags {
                let key = cx.heap_mut().intern_str(&name);
                let value = Value::Str(
                    cx.heap_mut()
                        .alloc_string(text.encode_utf16().collect::<Vec<_>>()),
                );
                cx.heap_mut().set_member(dictionary, key, value)?;
            }
            object(dictionary)
        };
        Ok(NativeStep::Return(result))
    }
}
pub(super) fn start(
    shared: &Shared,
    id: LayerId,
    request: krkr_image::Request,
) -> NativeResult<NativeStep> {
    start_with(shared, id, request, None)
}
pub(super) fn start_with(
    shared: &Shared,
    id: LayerId,
    request: krkr_image::Request,
    completion: Option<Box<dyn Loaded>>,
) -> NativeResult<NativeStep> {
    let window = shared.borrow().record(id)?.window;
    start_inner(shared, Some(id), window, request, completion)
}
pub(super) fn cache_key(request: &krkr_image::Request) -> krkr_protocol::image_cache::Key {
    krkr_protocol::image_cache::Key {
        names: [
            Some(request.main.name.clone()),
            request.mask.as_ref().map(|p| p.name.clone()),
            request.province.as_ref().map(|p| p.name.clone()),
            request.scale.as_ref().map(|p| p.name.clone()),
        ],
        // Both legacy values mean no color key, including the preload default.
        color_key: if request.key == 0x02ffffff {
            0x1fffffff
        } else {
            request.key
        },
        rule_size: None,
    }
}
pub(super) fn preload(
    shared: &Shared,
    window: WindowId,
    request: krkr_image::Request,
) -> NativeResult<NativeStep> {
    start_inner(shared, None, window, request, None)
}
fn start_inner(
    shared: &Shared,
    id: Option<LayerId>,
    window: WindowId,
    request: krkr_image::Request,
    completion: Option<Box<dyn Loaded>>,
) -> NativeResult<NativeStep> {
    let trace_name = krkr_protocol::diagnostics::enabled().then(|| request.main.name.clone());
    let operations = shared.borrow().windows.borrow().operations.clone();
    let delivery = io::Delivery::default();
    let cache = operations.borrow().images.clone();
    let is_rule = completion.is_some();
    let cache_key = (request.province_size.is_none() || is_rule).then(|| {
        let mut key = cache_key(&request);
        if is_rule {
            key.rule_size = request.province_size.map(|s| (s.width, s.height));
        }
        key
    });
    if let Some(hit) = cache_key.as_ref().and_then(|key| cache.get(key)) {
        if id.is_none() {
            return Ok(NativeStep::Return(Value::Int(hit.bytes as i64)));
        }
        let staged = Staged::reserve(shared);
        let command = Command::Assign {
            image: staged.image.as_ref().unwrap().clone(),
            source: hit.image,
        };
        let task = Loading {
            shared: shared.clone(),
            id,
            window,
            staged: Some(staged),
            phase: None,
            size: hit.size,
            province_only: is_rule,
            completion,
            cache_key: None,
            cache_generation: cache.generation(),
            cache_bytes: 0,
            trace_name,
            cache_hit: true,
            phase_timer: krkr_protocol::diagnostics::Timer::start(),
        };
        return task.graphics(command, |delivery| {
            Phase::Upload(delivery, (*hit.tags).clone())
        });
    }
    Operations::wait(
        &operations,
        Request::Read(Box::new(io::Work::ImageProbe(request)), delivery.clone()),
        WaitMode::Internal,
        Box::new(Loading {
            shared: shared.clone(),
            id,
            window,
            staged: None,
            phase: Some(Phase::Probe(delivery)),
            size: Size {
                width: 0,
                height: 0,
            },
            province_only: false,
            completion,
            cache_key,
            cache_generation: cache.generation(),
            cache_bytes: 0,
            trace_name,
            cache_hit: false,
            phase_timer: krkr_protocol::diagnostics::Timer::start(),
        }),
    )
}
