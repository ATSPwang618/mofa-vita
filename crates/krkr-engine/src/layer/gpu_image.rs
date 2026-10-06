//! Owned backend image leases, without script helper Layers or pixel readback.
use super::{bindings::State, images::Staged, *};
use crate::operations::{Operations, Request};
use krkr_protocol::{graphics::Command, mesh::Batch, window::Response};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, WaitMode};

#[derive(Clone)]
pub struct GpuImage {
    allocation: Rc<Staged>,
    pub size: Size,
}
impl GpuImage {
    pub fn image(&self) -> ImageRef {
        self.allocation
            .image
            .as_ref()
            .expect("owned GPU image")
            .clone()
    }
    pub(super) fn from_staged(staged: Staged, size: Size) -> Self {
        Self {
            allocation: Rc::new(staged),
            size,
        }
    }
    /// A retained snapshot must get a new target before it can be overwritten.
    pub fn is_unique(&self) -> bool {
        Rc::strong_count(&self.allocation) == 1
    }
}
impl Trace for GpuImage {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
pub trait ImageContinuation: Trace {
    fn image(self: Box<Self>, cx: &mut NativeCx<'_>, image: GpuImage) -> NativeResult<NativeStep>;
}
pub fn allocate(cx: &mut NativeCx<'_>, size: Size) -> NativeResult<GpuImage> {
    if size.width == 0 || size.height == 0 || size.rgba_bytes().is_none() {
        return Err(NativeError::Message("invalid GPU target dimensions"));
    }
    let class = cx
        .heap()
        .registered_class("Layer")
        .ok_or(NativeError::This)?;
    let shared = cx
        .heap_mut()
        .with_native_state::<State, _>(class, |s| s.service.clone())?
        .ok_or(NativeError::This)?;
    Ok(GpuImage::from_staged(Staged::reserve(&shared), size))
}
pub fn draw(
    cx: &mut NativeCx<'_>,
    target: GpuImage,
    batch: Batch,
    next: Box<dyn ImageContinuation>,
) -> NativeResult<NativeStep> {
    let command = Command::Meshes {
        image: target.image(),
        size: target.size,
        batch,
    };
    request(cx, target, command, next)
}
fn request(
    _cx: &mut NativeCx<'_>,
    target: GpuImage,
    command: Command,
    next: Box<dyn ImageContinuation>,
) -> NativeResult<NativeStep> {
    let (host, window, owner, operations) = {
        let world = target.allocation.shared.borrow();
        let windows = world.windows.borrow();
        let (window, owner) = windows
            .graphics_window()
            .ok_or(NativeError::Message("GPU drawing requires an open Window"))?;
        (world.host()?, window, owner, windows.operations.clone())
    };
    let ticket = host
        .request(window, krkr_protocol::window::Command::Graphics(command))
        .map_err(NativeError::Detail)?;
    let delivery = crate::window::Delivery::default();
    Operations::wait(
        &operations,
        Request::Window(ticket, delivery.clone()),
        WaitMode::Internal,
        Box::new(Completed {
            target,
            next,
            delivery,
            owner,
        }),
    )
}
struct Completed {
    target: GpuImage,
    next: Box<dyn ImageContinuation>,
    delivery: crate::window::Delivery,
    owner: ObjId,
}
impl Trace for Completed {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
        self.owner.trace(visit);
    }
}
impl NativeContinuation for Completed {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if !matches!(self.delivery.borrow_mut().take(), Some(Response::Done)) {
            return Err(NativeError::Message("unexpected GPU image response"));
        }
        self.next.image(cx, self.target)
    }
}
pub fn snapshot(
    cx: &mut NativeCx<'_>,
    layer: Value,
    next: Box<dyn ImageContinuation>,
) -> NativeResult<NativeStep> {
    let (source, size) = cx
        .heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            Ok::<_, NativeError>((s.image()?, s.read(|r| r.geometry.image_size)?))
        })??;
    let target = allocate(cx, size)?;
    let command = Command::SnapshotMain {
        image: target.image(),
        source,
    };
    request(cx, target, command, next)
}
pub fn patch(cx: &mut NativeCx<'_>, layer: Value, source: GpuImage) -> NativeResult<NativeStep> {
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            if s.read(|r| r.geometry.image_size)? != source.size {
                return Err(NativeError::Message(
                    "GPU image and Layer dimensions differ",
                ));
            }
            s.command(
                Command::Copy {
                    image: s.image()?,
                    source: source.image(),
                    rectangle: source.size.rect(),
                    x: 0,
                    y: 0,
                    clip: source.size.rect(),
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                },
                tasks::Change::None,
            )
        })?
}

/// Upload an owned small image without reading the destination Layer back.
pub fn upload(
    cx: &mut NativeCx<'_>,
    target: GpuImage,
    pixels: Arc<krkr_protocol::pixels::Pixels>,
    next: Box<dyn ImageContinuation>,
) -> NativeResult<NativeStep> {
    if pixels.size != target.size {
        return Err(NativeError::Message("GPU upload dimensions differ"));
    }
    let command = Command::CopyPixels {
        image: target.image(),
        pixels,
        split_alpha: false,
        size: target.size,
    };
    request(cx, target, command, next)
}

/// Copy a generated image into the main plane, clipping to image bounds.
pub fn copy_region(
    cx: &mut NativeCx<'_>,
    layer: Value,
    source: GpuImage,
    x: i32,
    y: i32,
) -> NativeResult<NativeStep> {
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            s.require_main()?;
            let clip = s.read(|r| r.geometry.image_size)?.rect();
            s.command(
                Command::Copy {
                    image: s.image()?,
                    source: source.image(),
                    rectangle: source.size.rect(),
                    x,
                    y,
                    clip,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                },
                tasks::Change::None,
            )
        })?
}
