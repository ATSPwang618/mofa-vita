//! Host filesystem metadata and asynchronous directory selection. No OS objects
//! or script callbacks enter the host boundary.
use super::{SystemHost, bindings};
use crate::operations::{Operations, Request};
use krkr_protocol::window::{Command, DirectoryDialog, Response, WindowId};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, RestArgs, Trace, Value,
    WaitMode, value,
};

struct Informed {
    delivery: crate::window::Delivery,
    indexed: bool,
}
impl Trace for Informed {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Informed {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(Response::Informed(index)) = self.delivery.borrow_mut().take() else {
            return Err(NativeError::Message("unexpected message dialog response"));
        };
        Ok(NativeStep::Return(if self.indexed {
            Value::Int(index.into())
        } else {
            Value::Void
        }))
    }
}
pub(super) fn inform(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    fn text(cx: &mut NativeCx<'_>, input: Value) -> NativeResult<String> {
        let Value::Str(id) = value::to_string(cx.heap_mut(), input)? else {
            unreachable!()
        };
        Ok(String::from_utf16_lossy(krkr_assets::name::c_string(
            cx.heap().string(id)?,
        )))
    }
    let message = *args
        .first()
        .ok_or(NativeError::Message("System.inform requires a message"))?;
    let message = text(cx, message)?;
    let caption = match args.get(1) {
        Some(&v) if !matches!(v, Value::Void) => text(cx, v)?,
        _ => "Information".into(),
    };
    let mut indexed = false;
    let buttons = match args.get(2) {
        None | Some(Value::Void) => vec!["OK".into()],
        Some(&Value::Obj(reference)) => {
            indexed = true;
            let items = cx
                .heap()
                .array(reference.object.ok_or(NativeError::Type("an Array"))?)?
                .to_vec();
            items
                .into_iter()
                .map(|v| text(cx, v))
                .collect::<NativeResult<Vec<_>>>()?
        }
        Some(&v) => {
            indexed = true;
            let count = value::to_integer(cx.heap(), v)? as i32;
            ["OK", "Cancel"]
                .into_iter()
                .take(count.clamp(0, 2) as usize)
                .map(str::to_owned)
                .collect()
        }
    };
    let service = bindings::service(cx)?;
    let (client, operations) = {
        let system = service.borrow();
        (
            system
                .input
                .clone()
                .ok_or(NativeError::Message("System.inform requires a window host"))?,
            system.operations.clone(),
        )
    };
    let ticket = client
        .request(
            WindowId::default(),
            Command::Inform {
                text: message,
                caption,
                buttons,
            },
        )
        .map_err(NativeError::Detail)?;
    let delivery = crate::window::Delivery::default();
    Operations::wait(
        &operations,
        Request::Window(ticket, delivery.clone()),
        WaitMode::Internal,
        Box::new(Informed { delivery, indexed }),
    )
}

fn host<T>(
    cx: &mut NativeCx<'_>,
    call: impl FnOnce(&mut dyn SystemHost) -> Result<T, String>,
) -> NativeResult<T> {
    let service = bindings::service(cx)?;
    let mut system = service.borrow_mut();
    call(
        system
            .config
            .host
            .as_deref_mut()
            .ok_or(NativeError::Message("filesystem host is unavailable"))?,
    )
    .map_err(NativeError::Detail)
}
pub fn attributes(cx: &mut NativeCx<'_>, path: &[u16]) -> NativeResult<u32> {
    host(cx, |host| host.file_attributes(path))
}
pub fn change_attributes(
    cx: &mut NativeCx<'_>,
    path: &[u16],
    mask: u32,
    set: bool,
) -> NativeResult<bool> {
    host(cx, |host| host.change_file_attributes(path, mask, set))
}
pub fn display_name(cx: &mut NativeCx<'_>, path: &[u16]) -> NativeResult<Vec<u16>> {
    host(cx, |host| host.file_display_name(path))
}

/// Capture a typed owner before subsequent option getters can change script state.
pub fn directory_owner(cx: &mut NativeCx<'_>, owner: Value) -> NativeResult<Option<WindowId>> {
    if matches!(owner, Value::Void) {
        return Ok(None);
    }
    let Value::Obj(reference) = owner else {
        return Err(NativeError::Type("a Window object"));
    };
    let object = reference
        .object
        .ok_or(NativeError::Type("a Window object"))?;
    match crate::window::bindings::id(cx.heap_mut(), object) {
        Ok(id) => Ok(Some(id)),
        Err(NativeError::This) => Ok(None),
        Err(error) => Err(error),
    }
}
struct Selected {
    delivery: crate::window::Delivery,
    owner: Value,
    next: Box<dyn NativeContinuation>,
}
impl Trace for Selected {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.next.trace(visit);
    }
}
impl NativeContinuation for Selected {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(Response::DirectorySelected(selected)) = self.delivery.borrow_mut().take() else {
            return Err(NativeError::Message(
                "unexpected directory selection response",
            ));
        };
        let value = selected.map_or(Value::Void, |path| {
            Value::Str(cx.heap_mut().alloc_string(path))
        });
        self.next.resume(cx, value)
    }
}
pub fn select_directory(
    cx: &mut NativeCx<'_>,
    owner: Value,
    id: Option<WindowId>,
    options: DirectoryDialog,
    next: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let service = bindings::service(cx)?;
    let (client, operations) = {
        let system = service.borrow();
        (
            system.input.clone().ok_or(NativeError::Message(
                "directory selection requires a window host",
            ))?,
            system.operations.clone(),
        )
    };
    let ticket = client
        .request(id.unwrap_or_default(), Command::SelectDirectory(options))
        .map_err(NativeError::Detail)?;
    let delivery = crate::window::Delivery::default();
    Operations::wait(
        &operations,
        Request::Window(ticket, delivery.clone()),
        WaitMode::Internal,
        Box::new(Selected {
            delivery,
            owner,
            next,
        }),
    )
}
