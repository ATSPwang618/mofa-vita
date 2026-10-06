//! The original Layer/Bitmap bridges transfer the entire main plane; they do
//! not use draw face, holdAlpha, clip, image offset, or the visible subtree.
use super::{images::Staged, tasks::Change, *};
use crate::operations::{Operations, Request};
use krkr_protocol::{graphics::Command, window::Response};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, RestArgs, WaitMode};

struct ReadMain {
    shared: Shared,
    id: LayerId,
    destination: crate::bitmap::Replacement,
    delivery: crate::window::Delivery,
}
impl Trace for ReadMain {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.destination.trace(visit);
        if let Some(record) = self.shared.borrow().records.get(self.id) {
            record.owner.trace(visit);
            record.action_owner.trace(visit);
        }
    }
}
impl NativeContinuation for ReadMain {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(Response::Image(pixels)) = self.delivery.borrow_mut().take() else {
            return Err(NativeError::Message("unexpected bitmap readback response"));
        };
        self.destination.commit(pixels)?;
        Ok(NativeStep::Return(Value::Void))
    }
}
impl bindings::State {
    pub(super) fn main_to_bitmap(
        &self,
        cx: &mut NativeCx<'_>,
        destination: Value,
    ) -> NativeResult<NativeStep> {
        self.lease()?;
        if matches!(destination, Value::Obj(ObjRef { object: None, .. })) {
            return Ok(NativeStep::Return(Value::Void));
        }
        let destination = crate::bitmap::replacement(cx.heap_mut(), destination)?;
        let image = self.image()?;
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
            .request(
                window,
                krkr_protocol::window::Command::Graphics(Command::ReadImage { image }),
            )
            .map_err(NativeError::Detail)?;
        let delivery = crate::window::Delivery::default();
        Operations::wait(
            &operations,
            Request::Window(ticket, delivery.clone()),
            WaitMode::Internal,
            Box::new(ReadMain {
                shared: lease.shared.clone(),
                id: lease.id,
                destination,
                delivery,
            }),
        )
    }
    pub(super) fn bitmap_to_main(
        &self,
        cx: &mut NativeCx<'_>,
        source: Value,
    ) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        if matches!(source, Value::Obj(ObjRef { object: None, .. })) {
            return Ok(NativeStep::Return(Value::Void));
        }
        let pixels = crate::bitmap::snapshot(cx.heap_mut(), source)?;
        let (source, geometry) =
            self.read(|r| (r.image.clone(), r.geometry.with_image(pixels.size)))?;
        let staged = Staged::reserve(&lease.shared);
        let command = Command::AssignBitmap {
            image: staged.image.as_ref().unwrap().clone(),
            source,
            pixels,
        };
        self.replace_image(staged, geometry, command, None)
    }
    pub(super) fn independ_image(
        &self,
        cx: &NativeCx<'_>,
        args: RestArgs<'_>,
        province: bool,
    ) -> NativeResult<NativeStep> {
        let copy = args
            .first()
            .filter(|v| !matches!(v, Value::Void))
            .map(|v| v.truthy(cx.heap()))
            .transpose()?
            .unwrap_or(true);
        let image = self.read(|r| r.image.clone().filter(|_| province || r.has_main))?;
        let Some(image) = image else {
            return Ok(NativeStep::Return(Value::Void));
        };
        self.command(
            Command::Independ {
                image,
                province,
                copy,
            },
            Change::None,
        )
    }
}

/// Generic managed main-plane transfer. No plugin or script buffer addresses.
pub trait PixelContinuation: Trace {
    fn pixels(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<krkr_protocol::pixels::Pixels>,
    ) -> NativeResult<NativeStep>;
}
struct ReadPixels {
    all_planes: bool,
    owner: Value,
    delivery: crate::window::Delivery,
    continuation: Box<dyn PixelContinuation>,
}
impl Trace for ReadPixels {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.continuation.trace(visit);
    }
}
impl NativeContinuation for ReadPixels {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(Response::Image(pixels)) = self.delivery.borrow_mut().take() else {
            return Err(NativeError::Message("unexpected pixel readback response"));
        };
        if self.all_planes {
            return request_pixels(
                cx,
                self.owner,
                Box::new(JoinPlanes {
                    main: pixels,
                    next: self.continuation,
                }),
                false,
                true,
                None,
            );
        }
        self.continuation.pixels(cx, Arc::new(pixels))
    }
}
struct JoinPlanes {
    main: krkr_protocol::pixels::Pixels,
    next: Box<dyn PixelContinuation>,
}
impl Trace for JoinPlanes {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
    }
}
impl PixelContinuation for JoinPlanes {
    fn pixels(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<krkr_protocol::pixels::Pixels>,
    ) -> NativeResult<NativeStep> {
        let pixels = Arc::try_unwrap(pixels)
            .map_err(|_| NativeError::Message("unexpected shared province readback"))?;
        if pixels.size != self.main.size {
            return Err(NativeError::Message("image changed during plane readback"));
        }
        self.main.province = pixels.province;
        self.next.pixels(cx, Arc::new(self.main))
    }
}
pub(crate) fn pixel_budget(
    cx: &mut NativeCx<'_>,
    target: Value,
) -> NativeResult<krkr_protocol::budget::Budget> {
    if let Some(budget) = crate::bitmap::pixel_budget(cx.heap_mut(), target)? {
        return Ok(budget);
    }
    cx.heap_mut()
        .with_native_state::<bindings::State, _>(object_id(target)?, |s| {
            let lease = s.lease()?;
            let world = lease.shared.borrow();
            Ok(world.host()?.staging_budget())
        })?
}
pub(crate) fn read_pixels(
    cx: &mut NativeCx<'_>,
    source: Value,
    continuation: Box<dyn PixelContinuation>,
) -> NativeResult<NativeStep> {
    read_planes(cx, source, continuation, false)
}
pub(crate) fn read_planes(
    cx: &mut NativeCx<'_>,
    source: Value,
    continuation: Box<dyn PixelContinuation>,
    all_planes: bool,
) -> NativeResult<NativeStep> {
    if crate::bitmap::pixel_budget(cx.heap_mut(), source)?.is_some() {
        let pixels = crate::bitmap::snapshot(cx.heap_mut(), source)?;
        return continuation.pixels(cx, pixels);
    }
    request_pixels(cx, source, continuation, all_planes, false, None)
}
pub(crate) fn read_region(
    cx: &mut NativeCx<'_>,
    source: Value,
    rectangle: Rect,
    continuation: Box<dyn PixelContinuation>,
) -> NativeResult<NativeStep> {
    let size = crate::extensions::layer_image_size(cx, source)?;
    if size.rect().intersection(rectangle) != Some(rectangle) {
        return Err(NativeError::Message("read region must be inside the image"));
    }
    request_pixels(cx, source, continuation, false, false, Some(rectangle))
}
fn request_pixels(
    cx: &mut NativeCx<'_>,
    source: Value,
    continuation: Box<dyn PixelContinuation>,
    all_planes: bool,
    province: bool,
    rectangle: Option<Rect>,
) -> NativeResult<NativeStep> {
    let (host, window, operations, image) = cx
        .heap_mut()
        .with_native_state::<bindings::State, _>(object_id(source)?, |s| -> NativeResult<_> {
            let image = s.image()?;
            let lease = s.lease()?;
            let world = lease.shared.borrow();
            Ok((
                world.host()?,
                world.record(lease.id)?.window,
                world.windows.borrow().operations.clone(),
                image,
            ))
        })??;
    let ticket = host
        .request(
            window,
            krkr_protocol::window::Command::Graphics(if let Some(rectangle) = rectangle {
                Command::ReadRegion { image, rectangle }
            } else if province {
                Command::ReadProvince { image }
            } else {
                Command::ReadImage { image }
            }),
        )
        .map_err(NativeError::Detail)?;
    let delivery = crate::window::Delivery::default();
    Operations::wait(
        &operations,
        Request::Window(ticket, delivery.clone()),
        WaitMode::Internal,
        Box::new(ReadPixels {
            all_planes,
            owner: source,
            delivery,
            continuation,
        }),
    )
}
struct Written;
impl Trace for Written {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Written {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(Value::Int(1)))
    }
}
pub(crate) fn write_pixels(
    cx: &mut NativeCx<'_>,
    target: Value,
    mut pixels: krkr_protocol::pixels::Pixels,
) -> NativeResult<NativeStep> {
    if pixels.size.rgba_bytes() != pixels.main.as_ref().map(|m| m.as_slice().len()) {
        return Err(NativeError::Message("pixel image size mismatch"));
    }
    pixels.province = None;
    if crate::bitmap::pixel_budget(cx.heap_mut(), target)?.is_some() {
        crate::bitmap::replace_pixels(cx.heap_mut(), target, pixels)?;
        return Ok(NativeStep::Return(Value::Int(1)));
    }
    let size = pixels.size;
    write_shared_pixels(cx, target, Arc::new(pixels), size)
}
pub(crate) fn write_shared_pixels(
    cx: &mut NativeCx<'_>,
    target: Value,
    pixels: Arc<krkr_protocol::pixels::Pixels>,
    display_size: krkr_protocol::graphics::Size,
) -> NativeResult<NativeStep> {
    if pixels.size.rgba_bytes() != pixels.main.as_ref().map(|m| m.as_slice().len())
        || pixels.province.is_some()
    {
        return Err(NativeError::Message("invalid shared main image"));
    }
    cx.heap_mut()
        .with_native_state::<bindings::State, _>(object_id(target)?, |s| {
            let lease = s.lease()?;
            let (source, mut geometry) =
                s.read(|r| (r.image.clone(), r.geometry.with_image(pixels.size)))?;
            geometry.set_size(display_size);
            let staged = Staged::reserve(&lease.shared);
            let command = Command::AssignBitmap {
                image: staged.image.as_ref().unwrap().clone(),
                source,
                pixels,
            };
            s.replace_image_then(staged, geometry, command, None, Some(Box::new(Written)))
        })?
}
