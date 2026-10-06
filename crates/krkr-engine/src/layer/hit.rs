//! Ordered tree traversal with resumable pixel reads and script vetoes.
use super::*;
use crate::operations::{Operations, Request};
use krkr_protocol::{graphics::Command, hit::Plane, window::Response};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, WaitMode};

pub(super) struct Cache {
    planes: lru::LruCache<(ImageId, bool), (u64, Arc<Plane>)>,
    bytes: usize,
    pixel_reads: std::collections::VecDeque<((ImageId, bool), u64, u8)>,
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            planes: lru::LruCache::unbounded(),
            bytes: 0,
            pixel_reads: Default::default(),
        }
    }
}
impl Cache {
    pub(super) fn get(&mut self, image: &ImageRef, province: bool) -> Option<Arc<Plane>> {
        let key = (image.id, province);
        let (version, plane) = self.planes.get(&key)?;
        if *version == image.lifetime.revision(province) {
            return Some(plane.clone());
        }
        self.bytes -= self.planes.pop(&key).unwrap().1.bytes();
        None
    }
    pub(super) fn insert(
        &mut self,
        image: &ImageRef,
        province: bool,
        revision: u64,
        plane: Arc<Plane>,
        limits: krkr_protocol::window::Limits,
    ) {
        if let Some((_, old)) = self
            .planes
            .put((image.id, province), (revision, plane.clone()))
        {
            self.bytes -= old.bytes();
        }
        self.bytes += plane.bytes();
        while self.bytes > limits.hit_cache_bytes || self.planes.len() > limits.scene_nodes * 2 {
            self.bytes -= self.planes.pop_lru().unwrap().1.1.bytes();
        }
    }
    pub fn remove(&mut self, image: ImageId) {
        self.pixel_reads.retain(|entry| entry.0.0 != image);
        for province in [false, true] {
            if let Some((_, old)) = self.planes.pop(&(image, province)) {
                self.bytes -= old.bytes();
            }
        }
    }
    /// Learn repeated mask reads without re-learning every animation frame.
    /// Only tiny masks retain this preference after a write; larger images
    /// still require repeated reads of their current revision.
    pub(super) fn repeated_pixel(&mut self, image: &ImageRef, province: bool, pixels: u64) -> bool {
        let key = (image.id, province);
        let revision = image.lifetime.revision(province);
        let previous = self
            .pixel_reads
            .iter()
            .position(|entry| entry.0 == key)
            .and_then(|index| self.pixel_reads.remove(index));
        let count = previous
            .filter(|entry| entry.1 == revision || (pixels <= 4096 && entry.2 >= 4))
            .map_or(1, |entry| entry.2.saturating_add(1));
        if self.pixel_reads.len() == 16 {
            self.pixel_reads.pop_front();
        }
        self.pixel_reads.push_back((key, revision, count));
        count >= 4
    }
}
#[derive(Clone, Copy)]
struct Entry {
    id: LayerId,
    x: i64,
    y: i64,
    check: bool,
}
enum Stage {
    Walk,
    Read {
        image: ImageRef,
        province: bool,
        revision: u64,
        delivery: crate::window::Delivery,
    },
    Callback,
}
struct Search {
    shared: Shared,
    stack: Vec<Entry>,
    current: Option<Entry>,
    exclude: Option<LayerId>,
    disabled: bool,
    stage: Stage,
    completion: Option<Box<dyn NativeContinuation>>,
}
impl Trace for Search {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        let world = self.shared.borrow();
        for entry in self.stack.iter().chain(self.current.iter()) {
            if let Some(record) = world.records.get(entry.id) {
                record.owner.trace(visit);
            }
        }
        if let Some(completion) = &self.completion {
            completion.trace(visit);
        }
    }
}
impl Layers {
    pub(super) fn node_enabled(&self, id: LayerId) -> bool {
        if self.disabled_by_mode(id) {
            return false;
        }
        let mut node = Some(id);
        while let Some(id) = node {
            let Some(record) = self.records.get(id) else {
                return false;
            };
            if !record.enabled {
                return false;
            }
            node = record.parent;
        }
        true
    }
    pub(super) fn coordinates(&self, id: LayerId) -> NativeResult<(WindowId, i64, i64)> {
        let window = self.record(id)?.window;
        let (mut x, mut y, mut node) = (0, 0, id);
        loop {
            let record = self.record(node)?;
            let Some(parent) = record.parent else {
                if self.input_primary(window) != Some(node) {
                    return Err(NativeError::Message("layer is detached from its primary"));
                }
                return Ok((window, x, y));
            };
            x += i64::from(record.geometry.left);
            y += i64::from(record.geometry.top);
            node = parent;
        }
    }
}
pub(super) fn start(
    shared: &Shared,
    window: WindowId,
    x: i64,
    y: i64,
    exclude: Option<LayerId>,
    disabled: bool,
    completion: Option<Box<dyn NativeContinuation>>,
) -> NativeStep {
    let stack = shared
        .borrow()
        .input_primary(window)
        .map(|id| {
            vec![Entry {
                id,
                x,
                y,
                check: false,
            }]
        })
        .unwrap_or_default();
    NativeStep::Continue(Box::new(Search {
        shared: shared.clone(),
        stack,
        current: None,
        exclude,
        disabled,
        stage: Stage::Walk,
        completion,
    }))
}
impl Search {
    fn finish(mut self, cx: &mut NativeCx<'_>, result: Value) -> NativeResult<NativeStep> {
        match self.completion.take() {
            Some(next) => next.resume(cx, result),
            None => Ok(NativeStep::Return(result)),
        }
    }
    fn callback(mut self: Box<Self>) -> NativeResult<NativeStep> {
        let entry = self.current.unwrap();
        let (target, key) = {
            let mut world = self.shared.borrow_mut();
            let key = world.names["onHitTest"];
            let record = world.record_mut(entry.id)?;
            record.hit_work = true;
            (object(record.owner), key)
        };
        self.stage = Stage::Callback;
        Ok(NativeStep::CallMember {
            object: target,
            key,
            arguments: vec![Value::Int(entry.x), Value::Int(entry.y), Value::Int(1)],
            continuation: self,
        })
    }
    fn sample(&self, plane: &Plane, province: bool) -> NativeResult<bool> {
        let entry = self.current.unwrap();
        let world = self.shared.borrow();
        let record = world.record(entry.id)?;
        let g = record.geometry;
        let (x, y) = (
            entry.x - i64::from(g.image_left),
            entry.y - i64::from(g.image_top),
        );
        if x < 0 || y < 0 || x >= plane.size.width.into() || y >= plane.size.height.into() {
            return Ok(false);
        }
        let value = plane.sample(x, y);
        Ok(if province {
            value != 0
        } else {
            i32::from(value) >= record.hit_threshold
        })
    }
}
impl NativeContinuation for Search {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        match std::mem::replace(&mut self.stage, Stage::Walk) {
            Stage::Callback => {
                let result = {
                    let world = self.shared.borrow();
                    let id = self.current.unwrap().id;
                    world.records.get(id).filter(|r| r.hit_work).map(|r| {
                        if self.disabled || world.node_enabled(id) {
                            object(r.owner)
                        } else {
                            null()
                        }
                    })
                };
                if let Some(result) = result {
                    return self.finish(cx, result);
                }
            }
            Stage::Read {
                image,
                province,
                revision,
                delivery,
            } => {
                let Some(Response::HitPlane(plane)) = delivery.borrow_mut().take() else {
                    return Err(NativeError::Message("unexpected hit plane response"));
                };
                let plane = Arc::new(plane);
                let hit = self.sample(&plane, province)?;
                // Later writes invalidate the cache, not this ordered read.
                // Retrying until the revision is stable can trap an input
                // callback for the entire duration of a playing video.
                if image.lifetime.revision(province) == revision {
                    let mut world = self.shared.borrow_mut();
                    let limits = world.host()?.limits();
                    world
                        .hit_cache
                        .insert(&image, province, revision, plane, limits);
                }
                if hit {
                    return self.callback();
                }
            }
            Stage::Walk => {}
        }
        let Some(mut entry) = self.stack.pop() else {
            return self.finish(cx, null());
        };
        let (image, province, direct) = {
            let world = self.shared.borrow();
            let Some(record) = world.records.get(entry.id) else {
                drop(world);
                return Ok(NativeStep::Continue(self));
            };
            let g = record.geometry;
            if !entry.check {
                entry.x -= i64::from(g.left);
                entry.y -= i64::from(g.top);
                if !record.visible
                    || !record.ready
                    || entry.x < 0
                    || entry.y < 0
                    || entry.x >= g.width.into()
                    || entry.y >= g.height.into()
                {
                    drop(world);
                    return Ok(NativeStep::Continue(self));
                }
                entry.check = true;
                self.stack.push(entry);
                self.stack.extend(record.children.iter().map(|&id| Entry {
                    id,
                    check: false,
                    ..entry
                }));
                drop(world);
                return Ok(NativeStep::Continue(self));
            }
            if self.exclude == Some(entry.id) {
                drop(world);
                return Ok(NativeStep::Continue(self));
            }
            let province = record.hit_type == 1;
            let direct = match record.hit_type {
                0 if !record.has_main => Some(record.hit_threshold <= 0),
                0 if record.hit_threshold <= 0 => Some(true),
                0 if record.hit_threshold > 255 => Some(false),
                0 | 1 if record.image.is_none() => Some(false),
                0 | 1 => None,
                _ => Some(true),
            };
            (record.image.clone(), province, direct)
        };
        self.current = Some(entry);
        if let Some(hit) = direct {
            return if hit {
                self.callback()
            } else {
                Ok(NativeStep::Continue(self))
            };
        }
        let image = image.unwrap();
        let cached = self.shared.borrow_mut().hit_cache.get(&image, province);
        if let Some(plane) = cached {
            return if self.sample(&plane, province)? {
                self.callback()
            } else {
                Ok(NativeStep::Continue(self))
            };
        }
        let (host, window, operations) = {
            let world = self.shared.borrow();
            (
                world.host()?,
                world.record(entry.id)?.window,
                world.windows.borrow().operations.clone(),
            )
        };
        let revision = image.lifetime.revision(province);
        let ticket = host
            .request(
                window,
                krkr_protocol::window::Command::Graphics(Command::ReadHitPlane {
                    image: image.clone(),
                    province,
                }),
            )
            .map_err(NativeError::Detail)?;
        let delivery = crate::window::Delivery::default();
        self.stage = Stage::Read {
            image,
            province,
            revision,
            delivery: delivery.clone(),
        };
        Operations::wait(
            &operations,
            Request::Window(ticket, delivery),
            WaitMode::Internal,
            self,
        )
    }
}
