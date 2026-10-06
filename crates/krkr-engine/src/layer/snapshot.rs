//! Composed subtree readback without adding helper nodes to the script tree.
use super::{
    bindings::{State, layer_id},
    images::Staged,
    *,
};
use crate::operations::{Operations, Request};
use krkr_protocol::{graphics::Command, window::Response};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, WaitMode};

enum Destination {
    Pixels(Box<dyn PixelContinuation>),
    Image(Box<dyn super::gpu_image::ImageContinuation>),
}
impl Trace for Destination {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        match self {
            Self::Pixels(next) => next.trace(visit),
            Self::Image(next) => next.trace(visit),
        }
    }
}
pub fn capture(
    cx: &mut NativeCx<'_>,
    source: Value,
    previous: Option<super::gpu_image::GpuImage>,
    next: Box<dyn super::gpu_image::ImageContinuation>,
) -> NativeResult<NativeStep> {
    start(cx, source, Destination::Image(next), previous)
}
pub(crate) fn read(
    cx: &mut NativeCx<'_>,
    source: Value,
    next: Box<dyn PixelContinuation>,
) -> NativeResult<NativeStep> {
    start(cx, source, Destination::Pixels(next), None)
}
fn start(
    cx: &mut NativeCx<'_>,
    source: Value,
    next: Destination,
    previous: Option<super::gpu_image::GpuImage>,
) -> NativeResult<NativeStep> {
    let (shared, id) = cx
        .heap_mut()
        .with_native_state::<State, _>(object_id(source)?, |s| {
            s.lease().map(|lease| (lease.shared.clone(), lease.id))
        })??;
    let source_id = layer_id(cx.heap_mut(), source)?;
    debug_assert_eq!(id, source_id);
    let pending = shared.borrow().nodes(Some(id)).into();
    let staged = Staged::reserve(&shared);
    Ok(NativeStep::Continue(Box::new(Read {
        staged,
        previous,
        source: id,
        pending,
        next,
        delivery: Default::default(),
        phase: 0,
        scene: None,
        size: Size {
            width: 0,
            height: 0,
        },
    })))
}
struct Read {
    staged: Staged,
    previous: Option<super::gpu_image::GpuImage>,
    source: LayerId,
    pending: std::collections::VecDeque<LayerId>,
    next: Destination,
    delivery: crate::window::Delivery,
    phase: u8,
    scene: Option<Scene>,
    size: Size,
}
impl Trace for Read {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
        let world = self.staged.shared.borrow();
        for id in std::iter::once(self.source).chain(self.pending.iter().copied()) {
            if let Some(record) = world.records.get(id) {
                record.owner.trace(visit);
                record.action_owner.trace(visit);
            }
        }
        world.trace_transitions(visit);
    }
}
impl NativeContinuation for Read {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if self.phase == 0 {
            while let Some(id) = self.pending.pop_front() {
                let call = {
                    let mut world = self.staged.shared.borrow_mut();
                    let key = world.names["onPaint"];
                    world
                        .records
                        .get_mut(id)
                        .filter(|r| r.ready && !r.shutdown && r.call_on_paint)
                        .map(|r| {
                            r.call_on_paint = false;
                            (object(r.owner), key)
                        })
                };
                if let Some((object, key)) = call {
                    return Ok(NativeStep::CallMember {
                        object,
                        key,
                        arguments: Vec::new(),
                        continuation: self,
                    });
                }
            }
            let world = self.staged.shared.borrow();
            let source = world.record(self.source)?;
            source.image()?;
            self.size = source.geometry.size();
            self.previous = self.previous.take().filter(|image| image.size == self.size);
            self.scene = Some(world.subtree_scene(self.source)?);
        } else {
            let response = self.delivery.borrow_mut().take();
            if self.phase == 2 {
                let Some(Response::Image(pixels)) = response else {
                    return Err(NativeError::Message("unexpected subtree readback response"));
                };
                let Destination::Pixels(next) = self.next else {
                    unreachable!()
                };
                return next.pixels(cx, Arc::new(pixels));
            }
            if !matches!(response, Some(Response::Done)) {
                return Err(NativeError::Message(
                    "unexpected subtree composition response",
                ));
            }
        }
        if self.phase == 1 && matches!(self.next, Destination::Image(_)) {
            let Destination::Image(next) = self.next else {
                unreachable!()
            };
            let image = match self.previous {
                Some(image) => image,
                None => super::gpu_image::GpuImage::from_staged(self.staged, self.size),
            };
            return next.image(cx, image);
        }
        let image = self.previous.as_ref().map_or_else(
            || {
                self.staged
                    .image
                    .as_ref()
                    .expect("staged subtree image")
                    .clone()
            },
            |image| image.image(),
        );
        let command = match self.phase {
            // Both destinations start with the final composed image. Creating
            // a blank image then piled-copying into it adds an allocation, a
            // full-image transfer and another host round trip before readback.
            0 => Command::ComposeScene {
                image,
                size: self.size,
                scene: self.scene.take().expect("prepared subtree scene"),
            },
            _ => Command::ReadImage { image },
        };
        self.phase += 1;
        let (host, window, operations) = {
            let world = self.staged.shared.borrow();
            (
                world.host()?,
                world.record(self.source)?.window,
                world.windows.borrow().operations.clone(),
            )
        };
        let ticket = host
            .request(window, krkr_protocol::window::Command::Graphics(command))
            .map_err(NativeError::Detail)?;
        Operations::wait(
            &operations,
            Request::Window(ticket, self.delivery.clone()),
            WaitMode::Internal,
            self,
        )
    }
}
