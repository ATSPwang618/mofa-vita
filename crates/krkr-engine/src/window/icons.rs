//! windowEx icon ownership and IO continuations; native icons live in the host.
use super::{Shared, bindings::State};
use crate::{
    io,
    operations::{Operations, Request},
};
use krkr_protocol::window::{Command, IconCommand, IconImage, Response, WindowId};
use std::sync::Arc;
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Trace, Value, WaitMode,
};

#[derive(Clone, Copy)]
enum Target {
    Window(WindowId, bool),
    Application,
}
struct Selection {
    shared: Shared,
    target: Target,
}
impl Trace for Selection {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Target::Window(id, _) = self.target
            && let Some(owner) = self.shared.borrow().owner(id)
        {
            owner.trace(visit);
        }
    }
}
impl Selection {
    fn load(self, cx: &mut NativeCx<'_>, value: Option<Value>) -> NativeResult<NativeStep> {
        // Exact variant test: integers, objects, octets and void all clear the
        // selection. In particular, do not invoke object-to-string callbacks.
        let name = match value {
            Some(Value::Str(id)) => tjs_core::string::c_string(cx.heap().string(id)?).to_vec(),
            _ => Vec::new(),
        };
        if name.is_empty() {
            return self.apply(None);
        }
        crate::storages::managed::plans(cx, vec![(name, true)], self, |selection, _, mut plans| {
            selection.open_plan(plans.pop().flatten().expect("required icon plan"))
        })
    }
    fn open_plan(self, plan: crate::storages::ReadPlan) -> NativeResult<NativeStep> {
        if krkr_assets::name::split_archive(&plan.name).1.is_some() {
            return Err(NativeError::Message("cannot get in archive icon"));
        }
        let (operations, budget) = {
            let world = self.shared.borrow();
            let host = world
                .host
                .as_ref()
                .ok_or(NativeError::Message("icons require a window host"))?;
            (world.operations.clone(), host.staging_budget())
        };
        let delivery = io::Delivery::default();
        Operations::wait(
            &operations,
            Request::Read(Box::new(io::Work::Icon(plan, budget)), delivery.clone()),
            WaitMode::Internal,
            Box::new(Loaded {
                selection: self,
                delivery,
            }),
        )
    }
    fn apply(self, image: Option<Arc<IconImage>>) -> NativeResult<NativeStep> {
        let (host, operations) = {
            let world = self.shared.borrow();
            if let Target::Window(id, _) = self.target {
                world.record(id)?;
            }
            (
                world
                    .host
                    .clone()
                    .ok_or(NativeError::Message("icons require a window host"))?,
                world.operations.clone(),
            )
        };
        let (id, command) = match self.target {
            Target::Window(id, with_application) => {
                // Like windowEx, a successful load becomes the selected icon
                // before it is applied. A host error remains an explicit error.
                self.shared.borrow_mut().record_mut(id)?.icon = image.clone();
                (
                    id,
                    IconCommand::Window {
                        image,
                        with_application,
                    },
                )
            }
            Target::Application => (WindowId::default(), IconCommand::Application(image)),
        };
        let ticket = host
            .request(id, Command::Icon(command))
            .map_err(NativeError::Detail)?;
        let delivery = super::Delivery::default();
        Operations::wait(
            &operations,
            Request::Window(ticket, delivery.clone()),
            WaitMode::Internal,
            Box::new(Applied {
                selection: self,
                delivery,
            }),
        )
    }
}
fn window(cx: &mut NativeCx<'_>, with_application: bool) -> NativeResult<Selection> {
    let owner = cx.this();
    cx.heap_mut()
        .with_native_state::<State, _>(owner, |state| {
            let lease = state.lease()?;
            Ok(Selection {
                shared: lease.shared.clone(),
                target: Target::Window(lease.id, with_application),
            })
        })?
}
pub fn set_window_icon(
    cx: &mut NativeCx<'_>,
    value: Option<Value>,
    with_application: bool,
) -> NativeResult<NativeStep> {
    let selection = window(cx, with_application)?;
    let Target::Window(id, _) = selection.target else {
        unreachable!()
    };
    // windowEx destroys externalIcon BEFORE resolving/loading the new file.
    // The displayed native icon retains its own lease until another application
    // succeeds; a later reset after failure uses the default, never the old one.
    selection.shared.borrow_mut().record_mut(id)?.icon = None;
    selection.load(cx, value)
}
pub fn reset_window_icon(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    let selection = window(cx, false)?;
    let Target::Window(id, _) = selection.target else {
        unreachable!()
    };
    let image = selection.shared.borrow().record(id)?.icon.clone();
    selection.apply(image)
}
pub fn set_application_icon(
    cx: &mut NativeCx<'_>,
    value: Option<Value>,
) -> NativeResult<NativeStep> {
    let class = cx
        .heap()
        .registered_class("Window")
        .ok_or(NativeError::Message("Window is not installed"))?;
    let shared = cx
        .heap_mut()
        .with_native_state::<State, _>(class, |state| state.service.clone())?
        .ok_or(NativeError::Message("window service unavailable"))?;
    // The application setter resolves/decodes first. Failure preserves its
    // current native icon; no per-window lifetime owns this shared setting.
    Selection {
        shared,
        target: Target::Application,
    }
    .load(cx, value)
}
struct Loaded {
    selection: Selection,
    delivery: io::Delivery,
}
impl Trace for Loaded {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.selection.trace(visit);
    }
}
impl NativeContinuation for Loaded {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(io::Data::Icon(image)) = self.delivery.borrow_mut().take() else {
            return Err(NativeError::Message("unexpected icon decoder response"));
        };
        self.selection.apply(Some(Arc::new(image)))
    }
}
struct Applied {
    selection: Selection,
    delivery: super::Delivery,
}
impl Trace for Applied {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.selection.trace(visit);
    }
}
impl NativeContinuation for Applied {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if !matches!(self.delivery.borrow_mut().take(), Some(Response::Done)) {
            return Err(NativeError::Message("unexpected icon host response"));
        }
        Ok(NativeStep::Return(Value::Void))
    }
}
