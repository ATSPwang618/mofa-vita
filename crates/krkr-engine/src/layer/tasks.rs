use super::*;
use crate::operations::{Operations, Request};
use krkr_protocol::{graphics::Command, window::Response};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, WaitMode};

pub(super) enum Change {
    None,
    Geometry(Geometry),
    Pixel {
        mask: u32,
        shift: u32,
    },
    MaskPixel {
        image: ImageRef,
        province: bool,
        revision: u64,
        x: i32,
        y: i32,
    },
}
struct Updated {
    shared: Shared,
    id: LayerId,
    delivery: crate::window::Delivery,
    change: Change,
    created: Option<Lease>,
    modified: bool,
}
impl Trace for Updated {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(record) = self.shared.borrow().records.get(self.id) {
            record.owner.trace(visit);
            record.action_owner.trace(visit);
        }
    }
}
impl NativeContinuation for Updated {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let response = self
            .delivery
            .borrow_mut()
            .take()
            .expect("graphics completion");
        let result = match (response, &self.change) {
            (Response::Pixel(pixel), Change::Pixel { mask, shift }) => {
                Value::Int(((pixel >> shift) & mask) as i64)
            }
            (
                Response::HitPlane(plane),
                Change::MaskPixel {
                    image,
                    province,
                    revision,
                    x,
                    y,
                },
            ) => {
                let value = Value::Int(plane.sample(i64::from(*x), i64::from(*y)).into());
                if image.lifetime.revision(*province) == *revision {
                    let mut world = self.shared.borrow_mut();
                    let limits = world.host()?.limits();
                    world
                        .hit_cache
                        .insert(image, *province, *revision, Arc::new(plane), limits);
                }
                value
            }
            (Response::Done, Change::Geometry(geometry)) => {
                let mut world = self.shared.borrow_mut();
                world.set_geometry(self.id, *geometry)?;
                Value::Void
            }
            (Response::Done, Change::None) => Value::Void,
            _ => return Err(NativeError::Message("unexpected graphics response")),
        };
        if self.modified {
            let mut world = self.shared.borrow_mut();
            let record = world.record_mut(self.id)?;
            record.image_modified = true;
            let window = record.window;
            world.changed(window);
        }
        if let Some(lease) = self.created.take() {
            {
                let mut world = self.shared.borrow_mut();
                let record = world.record_mut(self.id)?;
                record.ready = true;
                let window = record.window;
                world.changed(window);
            }
            return Ok(NativeStep::Return(cx.construct(
                super::bindings::State {
                    service: None,
                    lease: Some(lease),
                    pending_name: Vec::new(),
                },
            )?));
        }
        Ok(NativeStep::Return(result))
    }
}
pub(super) fn request(
    shared: &Shared,
    id: LayerId,
    command: Command,
    change: Change,
    created: Option<Lease>,
) -> NativeResult<NativeStep> {
    let modified = !matches!(
        command,
        Command::Pixel { .. }
            | Command::ReadImage { .. }
            | Command::ReadRegion { .. }
            | Command::ReadProvince { .. }
            | Command::ReadHitPlane { .. }
            | Command::Independ { .. }
    );
    let (host, window, operations) = {
        let world = shared.borrow();
        (
            world.host()?,
            world.record(id)?.window,
            world.windows.borrow().operations.clone(),
        )
    };
    if created.is_none()
        && matches!(change, Change::None)
        && host
            .try_draw(window, &command)
            .map_err(NativeError::Detail)?
    {
        let mut world = shared.borrow_mut();
        let record = world.record_mut(id)?;
        record.image_modified = true;
        let window = record.window;
        world.changed(window);
        return Ok(NativeStep::Return(Value::Void));
    }
    let ticket = host
        .request(window, krkr_protocol::window::Command::Graphics(command))
        .map_err(NativeError::Detail)?;
    let delivery = crate::window::Delivery::default();
    Operations::wait(
        &operations,
        Request::Window(ticket, delivery.clone()),
        WaitMode::Internal,
        Box::new(Updated {
            shared: shared.clone(),
            id,
            delivery,
            change,
            created,
            modified,
        }),
    )
}
