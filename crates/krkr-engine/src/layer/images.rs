use super::*;
use crate::operations::{Operations, Request};
use krkr_protocol::{graphics::Command, window::Response};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, WaitMode};

pub(super) struct Staged {
    pub shared: Shared,
    pub image: Option<ImageRef>,
    pub has_main: bool,
}
impl Staged {
    pub fn reserve(shared: &Shared) -> Self {
        let image = ImageRef {
            id: shared.borrow_mut().images.insert(()),
            lifetime: Arc::default(),
        };
        Self {
            shared: shared.clone(),
            image: Some(image),
            has_main: true,
        }
    }
    pub fn commit(&mut self, id: LayerId, geometry: Option<Geometry>) -> NativeResult<()> {
        let mut world = self.shared.borrow_mut();
        let record = world.record_mut(id)?;
        record.has_main = self.has_main;
        record.image_modified = true;
        let old = record
            .image
            .replace(self.image.take().expect("uncommitted image"));
        let window = record.window;
        if let Some(geometry) = geometry {
            world.set_geometry(id, geometry)?;
        }
        if let Some(old) = old {
            world.hit_cache.remove(old.id);
            world.images.remove(old.id);
        }
        world.changed(window);
        Ok(())
    }
}
impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(image) = &self.image {
            self.shared.borrow_mut().images.remove(image.id);
        }
    }
}
impl Geometry {
    pub(super) fn with_image(mut self, size: Size) -> Self {
        if (size.width as i32) < self.width {
            self.image_left = 0;
        }
        if (size.height as i32) < self.height {
            self.image_top = 0;
        }
        self.image_size = size;
        self.width = self.width.min(size.width as i32);
        self.height = self.height.min(size.height as i32);
        self.image_left = self
            .image_left
            .max(self.width.saturating_sub(size.width as i32));
        self.image_top = self
            .image_top
            .max(self.height.saturating_sub(size.height as i32));
        self.clip = size.rect();
        self
    }
}
struct Changed {
    after: Option<Box<dyn NativeContinuation>>,
    staged: Staged,
    id: LayerId,
    geometry: Geometry,
    blend: Option<Blend>,
    delivery: crate::window::Delivery,
}
impl Trace for Changed {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(after) = &self.after {
            after.trace(visit);
        }
        if let Some(record) = self.staged.shared.borrow().records.get(self.id) {
            record.owner.trace(visit);
            record.action_owner.trace(visit);
        }
    }
}
impl NativeContinuation for Changed {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if !matches!(self.delivery.borrow_mut().take(), Some(Response::Done)) {
            return Err(NativeError::Message("unexpected image assignment response"));
        }
        self.staged.commit(self.id, Some(self.geometry))?;
        if let Some(blend) = self.blend {
            let mut world = self.staged.shared.borrow_mut();
            let record = world.record_mut(self.id)?;
            record.blend = blend;
            record.neutral = blend.neutral();
        }
        if let Some(after) = self.after.take() {
            return after.resume(cx, Value::Void);
        }
        Ok(NativeStep::Return(Value::Void))
    }
}
impl bindings::State {
    pub(super) fn change_blend(&self, blend: Blend) -> NativeResult<NativeStep> {
        let (old, has_main, mut geometry) = self.read(|r| (r.blend, r.has_main, r.geometry))?;
        if old == blend {
            return Ok(NativeStep::Return(Value::Void));
        }
        if has_main {
            self.update(|r| {
                r.blend = blend;
                r.neutral = blend.neutral();
                Ok(())
            })?;
            return Ok(NativeStep::Return(Value::Void));
        }
        geometry.image_left = 0;
        geometry.image_top = 0;
        geometry = geometry.with_image(geometry.size());
        let staged = Staged::reserve(&self.lease()?.shared);
        let command = Command::EnableImage {
            image: staged.image.as_ref().unwrap().clone(),
            source: self.read(|r| r.image.clone())?,
            size: geometry.size(),
            color: blend.neutral(),
        };
        self.replace_image(staged, geometry, command, Some(blend))
    }
    pub(super) fn has_image(&self, enabled: bool) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        if !enabled {
            let mut world = lease.shared.borrow_mut();
            let record = world.record_mut(lease.id)?;
            let old = record.image.take();
            record.has_main = false;
            record.image_modified = true;
            let window = record.window;
            if let Some(old) = old {
                world.hit_cache.remove(old.id);
                world.images.remove(old.id);
            }
            world.changed(window);
            return Ok(NativeStep::Return(Value::Void));
        }
        let (has_image, mut g, color) = self.read(|r| (r.has_main, r.geometry, r.neutral()))?;
        if has_image {
            self.update(|r| {
                r.geometry.clip = r.geometry.image_size.rect();
                r.image_modified = true;
                Ok(())
            })?;
            return Ok(NativeStep::Return(Value::Void));
        }
        g.image_left = 0;
        g.image_top = 0;
        g = g.with_image(g.size());
        let staged = Staged::reserve(&lease.shared);
        let image = staged.image.as_ref().unwrap();
        let command = Command::EnableImage {
            image: image.clone(),
            source: self.read(|r| r.image.clone())?,
            size: g.size(),
            color,
        };
        self.replace_image(staged, g, command, None)
    }
    pub(super) fn assign(&self, cx: &mut NativeCx<'_>, source: Value) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        let source = if object_id(source)? == cx.this() {
            lease.id
        } else {
            bindings::layer_id(cx.heap_mut(), source)?
        };
        let (image, size, has_main) = {
            let world = lease.shared.borrow();
            let source = world.record(source)?;
            (
                source.image.clone(),
                source.geometry.image_size,
                source.has_main,
            )
        };
        let Some(source_image) = image else {
            return self.has_image(false);
        };
        if source == lease.id {
            return if has_main {
                self.has_image(true)
            } else {
                Ok(NativeStep::Return(Value::Void))
            };
        }
        let mut g = self.read(|r| r.geometry)?;
        if has_main {
            g = g.with_image(size);
        }
        let mut staged = Staged::reserve(&lease.shared);
        staged.has_main = has_main;
        let command = Command::Assign {
            image: staged.image.as_ref().unwrap().clone(),
            source: source_image,
        };
        self.replace_image(staged, g, command, None)
    }
    pub(super) fn create_province(
        &self,
        operation: krkr_protocol::graphics::ProvinceOperation,
    ) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        let geometry = self.read(|r| r.geometry)?;
        let mut staged = Staged::reserve(&lease.shared);
        staged.has_main = false;
        let command = Command::CreateProvince {
            image: staged.image.as_ref().unwrap().clone(),
            size: geometry.size(),
            operation,
        };
        self.replace_image(staged, geometry, command, None)
    }
    pub(super) fn replace_image(
        &self,
        staged: Staged,
        geometry: Geometry,
        command: Command,
        blend: Option<Blend>,
    ) -> NativeResult<NativeStep> {
        self.replace_image_then(staged, geometry, command, blend, None)
    }
    pub(super) fn replace_image_then(
        &self,
        staged: Staged,
        geometry: Geometry,
        command: Command,
        blend: Option<Blend>,
        after: Option<Box<dyn NativeContinuation>>,
    ) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        let (host, window, operations) = {
            let world = lease.shared.borrow();
            (
                world.host()?,
                world.record(lease.id)?.window,
                world.windows.borrow().operations.clone(),
            )
        };
        let ticket = host
            .request(window, krkr_protocol::window::Command::Graphics(command))
            .map_err(NativeError::Detail)?;
        let delivery = crate::window::Delivery::default();
        Operations::wait(
            &operations,
            Request::Window(ticket, delivery.clone()),
            WaitMode::Internal,
            Box::new(Changed {
                after,
                staged,
                id: lease.id,
                geometry,
                blend,
                delivery,
            }),
        )
    }
}
