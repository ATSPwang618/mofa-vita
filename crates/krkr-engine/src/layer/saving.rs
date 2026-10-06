use super::*;
use crate::{
    io,
    operations::{Operations, Request},
};
use krkr_image::save::Format;
use krkr_protocol::{graphics::Command, window::Response};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, WaitMode};

enum Phase {
    Read(
        crate::window::Delivery,
        krkr_assets::WritePlan,
        krkr_image::Tags,
    ),
    Write(io::Delivery),
}
struct Saving {
    shared: Shared,
    id: LayerId,
    storage: crate::storages::Shared,
    format: Format,
    phase: Option<Phase>,
}
impl Trace for Saving {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(record) = self.shared.borrow().records.get(self.id) {
            record.owner.trace(visit);
            record.action_owner.trace(visit);
        }
    }
}
impl NativeContinuation for Saving {
    fn resume(mut self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        match self.phase.take().expect("image save phase") {
            Phase::Read(delivery, target, tags) => {
                let Some(Response::Image(pixels)) = delivery.borrow_mut().take() else {
                    return Err(NativeError::Message("unexpected image readback response"));
                };
                let budget = self.shared.borrow().host()?.staging_budget();
                let operations = self.shared.borrow().windows.borrow().operations.clone();
                let delivery = io::Delivery::default();
                let request = krkr_image::save::Request {
                    target,
                    format: self.format,
                    pixels,
                    tags,
                    budget,
                };
                self.phase = Some(Phase::Write(delivery.clone()));
                Operations::wait(
                    &operations,
                    Request::Read(Box::new(io::Work::ImageSave(request)), delivery),
                    WaitMode::Internal,
                    self,
                )
            }
            Phase::Write(delivery) => {
                let Some(io::Data::FileWritten(path)) = delivery.borrow_mut().take() else {
                    return Err(NativeError::Message("unexpected image save response"));
                };
                self.storage.borrow_mut().invalidate_file(&path);
                Ok(NativeStep::Return(Value::Void))
            }
        }
    }
}
impl bindings::State {
    pub(super) fn save(
        &self,
        cx: &mut NativeCx<'_>,
        name: Value,
        mode: Value,
    ) -> NativeResult<NativeStep> {
        self.require_main()?;
        let units = |cx: &mut NativeCx<'_>, value| -> NativeResult<Vec<u16>> {
            let Value::Str(id) = tjs_core::value::to_string(cx.heap_mut(), value)? else {
                unreachable!()
            };
            Ok(tjs_core::string::c_string(cx.heap().string(id)?).to_vec())
        };
        let name = units(cx, name)?;
        let mode = if matches!(mode, Value::Void) {
            "bmp".into()
        } else {
            String::from_utf16_lossy(&units(cx, mode)?)
        };
        let format = Format::parse(&mode).map_err(|e| NativeError::Detail(e.to_string()))?;
        let storage = crate::storages::service(cx)?;
        let target = storage
            .borrow()
            .write_plan(&name)
            .map_err(|e| NativeError::Detail(e.to_string()))?;
        let lease = self.lease()?;
        let (host, window, operations, tags) = {
            let world = lease.shared.borrow();
            let record = world.record(lease.id)?;
            let mode = record.blend.name();
            let mut tags = vec![("mode".into(), mode.into())];
            let g = record.geometry;
            if g.image_left > 0 {
                tags.push(("offs_x".into(), g.image_left.to_string()));
            }
            if g.image_top > 0 {
                tags.push(("offs_y".into(), g.image_top.to_string()));
            }
            if g.image_left > 0 || g.image_top > 0 {
                tags.push(("offs_unit".into(), "pixel".into()));
            }
            (
                world.host()?,
                record.window,
                world.windows.borrow().operations.clone(),
                tags,
            )
        };
        let ticket = host
            .request(
                window,
                krkr_protocol::window::Command::Graphics(Command::ReadImage {
                    image: self.image()?,
                }),
            )
            .map_err(NativeError::Detail)?;
        let delivery = crate::window::Delivery::default();
        Operations::wait(
            &operations,
            Request::Window(ticket, delivery.clone()),
            WaitMode::Internal,
            Box::new(Saving {
                shared: lease.shared.clone(),
                id: lease.id,
                storage,
                format,
                phase: Some(Phase::Read(delivery, target, tags)),
            }),
        )
    }
}
