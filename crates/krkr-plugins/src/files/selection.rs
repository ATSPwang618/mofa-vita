//! fstat selects using an option object, then writes its name property only on
//! success. Getter/setter execution always stays on the original VM.
use super::{error, path};
use crate::exports::arg;
use krkr_engine::{
    assets::{local, name},
    protocol::window::{DirectoryDialog, WindowId},
    storages,
    system::files,
};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value, value,
};

struct Options {
    object: Value,
    missing: ObjId,
    owner: Value,
    owner_id: Option<WindowId>,
    dialog: DirectoryDialog,
    index: usize,
    validating_missing: bool,
}
impl Trace for Options {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.object.trace(visit);
        self.missing.trace(visit);
        self.owner.trace(visit);
    }
}
impl NativeContinuation for Options {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        let mut missing =
            matches!(result, Value::Obj(reference) if reference.object == Some(self.missing));
        if self.validating_missing {
            self.validating_missing = false;
            if let Value::Obj(reference) = result
                && let Some(object) = reference.object
            {
                missing |= !cx.heap().is_valid(object)?;
            }
            if !missing {
                // Original IsValid invokes a dynamic missing handler once;
                // PropGet then reads it again. Known slots use the raw validity
                // check below, so ordinary accessors execute only once.
                let key = Value::Str(cx.heap_mut().alloc_string(name::units(
                    ["window", "title", "name", "rootDir"][self.index - 1],
                )));
                return Ok(NativeStep::GetRequiredOr {
                    object: self.object,
                    key,
                    fallback: Value::Obj(self.missing.into()),
                    continuation: self,
                });
            }
        }
        if self.index > 0 && !missing {
            match self.index - 1 {
                0 => {
                    self.owner_id = files::directory_owner(cx, result)?;
                    self.dialog.application_owner = self.owner_id.is_none();
                    self.owner = result;
                }
                1 if !matches!(result, Value::Void) => {
                    self.dialog.title =
                        name::c_string(&value::to_string_units(cx.heap(), result)?).to_vec();
                }
                2 if !matches!(result, Value::Void) => {
                    let text = value::to_string_units(cx.heap(), result)?;
                    if !text.is_empty() {
                        self.dialog.initial = local::units(&path(cx, result)?).map_err(error)?;
                    }
                }
                3 => {
                    self.dialog.root = local::units(&path(cx, result)?).map_err(error)?;
                }
                _ => {}
            }
        }
        while self.index < 4 {
            let text = name::units(["window", "title", "name", "rootDir"][self.index]);
            let symbol = cx.heap_mut().intern(&text);
            let Value::Obj(reference) = self.object else {
                unreachable!()
            };
            let raw = cx
                .heap()
                .member(reference.object.expect("options object"), symbol)?;
            self.index += 1;
            if let Some(Value::Obj(reference)) = raw
                && let Some(object) = reference.object
                && !cx.heap().is_valid(object)?
            {
                continue;
            }
            self.validating_missing = raw.is_none();
            let key = Value::Str(cx.heap_mut().alloc_string(text));
            return Ok(NativeStep::GetRequiredOr {
                object: self.object,
                key,
                fallback: Value::Obj(self.missing.into()),
                continuation: self,
            });
        }
        files::select_directory(
            cx,
            self.owner,
            self.owner_id,
            self.dialog,
            Box::new(WriteName {
                object: self.object,
                written: false,
            }),
        )
    }
}
struct WriteName {
    object: Value,
    written: bool,
}
impl Trace for WriteName {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.object.trace(visit);
    }
}
impl NativeContinuation for WriteName {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if self.written {
            return Ok(NativeStep::Return(Value::Int(1)));
        }
        if matches!(result, Value::Void) {
            return Ok(NativeStep::Return(Value::Int(0)));
        }
        let text = value::to_string_units(cx.heap(), result)?;
        let selected = storages::service(cx)?
            .borrow()
            .full_path(&text)
            .map_err(error)?;
        let value = Value::Str(cx.heap_mut().alloc_string(selected));
        let key = Value::Str(cx.heap_mut().alloc_string(name::units("name")));
        self.written = true;
        Ok(NativeStep::Set {
            object: self.object,
            key,
            value,
            continuation: self,
        })
    }
}
pub(super) fn select(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let object = arg(args, 0)?;
    if !matches!(object, Value::Obj(reference) if reference.object.is_some()) {
        return Err(NativeError::Type("a directory selection options object"));
    }
    Box::new(Options {
        object,
        missing: cx.heap_mut().alloc_dictionary(),
        owner: Value::Void,
        owner_id: None,
        dialog: DirectoryDialog::default(),
        index: 0,
        validating_missing: false,
    })
    .resume(cx, Value::Void)
}
