use super::{
    bindings::State,
    images::Staged,
    tasks::{self, Change},
    *,
};
use crate::operations::{Operations, Request};
use krkr_protocol::{graphics::Command, pixels::Pixels, window::Response};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, WaitMode};

pub(super) struct Source {
    pub image: Option<ImageRef>,
    pub mode: Blend,
    upload: Option<(Staged, Arc<Pixels>)>,
}
impl Source {
    pub fn layer(image: Option<ImageRef>, mode: Blend) -> Self {
        Self {
            image,
            mode,
            upload: None,
        }
    }
    pub fn bitmap(shared: &Shared, pixels: Arc<Pixels>) -> Self {
        let staged = Staged::reserve(shared);
        Self {
            image: staged.image.clone(),
            mode: Blend::Alpha,
            upload: Some((staged, pixels)),
        }
    }
    pub fn image(&self) -> NativeResult<ImageRef> {
        self.image
            .clone()
            .ok_or(NativeError::Message("source has no drawable image"))
    }
    pub fn bitmap_size(&self) -> Option<Size> {
        self.upload.as_ref().map(|(_, pixels)| pixels.size)
    }
    pub fn command(
        self,
        state: &State,
        command: Command,
        change: Change,
    ) -> NativeResult<NativeStep> {
        let Some((staged, pixels)) = self.upload else {
            return state.command(command, change);
        };
        let lease = state.lease()?;
        let image = staged.image.as_ref().unwrap().clone();
        let create = Command::Create {
            image: image.id,
            lifetime: Arc::downgrade(&image.lifetime),
            size: pixels.size,
            color: 0,
        };
        Upload {
            staged,
            id: lease.id,
            commands: [command, Command::Upload { image, pixels }, create].into(),
            delivery: None,
            change: Some(change),
        }
        .next()
    }
}
struct Upload {
    staged: Staged,
    id: LayerId,
    commands: Vec<Command>,
    delivery: Option<crate::window::Delivery>,
    change: Option<Change>,
}
impl Trace for Upload {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(r) = self.staged.shared.borrow().records.get(self.id) {
            r.owner.trace(visit);
            r.action_owner.trace(visit);
        }
    }
}
impl Upload {
    fn next(mut self) -> NativeResult<NativeStep> {
        let command = self
            .commands
            .pop()
            .ok_or(NativeError::Message("missing source upload command"))?;
        if self.commands.is_empty() {
            // The queued draw owns its source lease until the host consumes it.
            return tasks::request(
                &self.staged.shared,
                self.id,
                command,
                self.change.take().unwrap(),
                None,
            );
        }
        let (host, window, operations) = {
            let world = self.staged.shared.borrow();
            (
                world.host()?,
                world.record(self.id)?.window,
                world.windows.borrow().operations.clone(),
            )
        };
        let ticket = host
            .request(window, krkr_protocol::window::Command::Graphics(command))
            .map_err(NativeError::Detail)?;
        let delivery = crate::window::Delivery::default();
        self.delivery = Some(delivery.clone());
        Operations::wait(
            &operations,
            Request::Window(ticket, delivery),
            WaitMode::Internal,
            Box::new(self),
        )
    }
}
impl NativeContinuation for Upload {
    fn resume(mut self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if !matches!(
            self.delivery.take().and_then(|d| d.borrow_mut().take()),
            Some(Response::Done)
        ) {
            return Err(NativeError::Message("unexpected source upload response"));
        }
        self.next()
    }
}
