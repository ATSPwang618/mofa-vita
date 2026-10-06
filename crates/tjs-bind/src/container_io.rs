use crate::{NativeCx, NativeResult, NativeStep, RestArgs, Value};
use tjs_core::{
    ObjId, ObjRef, ObjectKind,
    storage::{Io, Request},
    string,
};

fn arguments(
    cx: &mut NativeCx<'_>,
    name: Value,
    args: RestArgs<'_>,
) -> NativeResult<(Vec<u16>, Vec<u16>)> {
    let name = string::units(cx.heap_mut(), name)?;
    let mode = string::units(cx.heap_mut(), args.first().copied().unwrap_or(Value::Void))?;
    Ok((name, mode))
}
pub(crate) fn load(
    cx: &mut NativeCx<'_>,
    name: Value,
    args: RestArgs<'_>,
) -> NativeResult<NativeStep> {
    let this = cx.this();
    let (name, mode) = arguments(cx, name, args)?;
    cx.storage_io(
        Request {
            name,
            mode,
            io: Io::ReadText,
        },
        crate::flow::callback(this, |this, cx, value| {
            let Value::Str(id) = value else {
                return Err(crate::NativeError::Type("storage text"));
            };
            load_lines(cx, this, id).map(NativeStep::Return)
        }),
    )
}
fn load_lines(cx: &mut NativeCx<'_>, this: ObjId, text: tjs_core::StrId) -> NativeResult<Value> {
    cx.heap_mut().array_resize(this, 0)?;
    let length = string::c_string(cx.heap().string(text)?).len();
    let mut start = 0;
    let mut pos = 0;
    while pos < length {
        let source = cx.heap().string(text)?;
        pos += source[pos..length]
            .iter()
            .position(|&u| u == 13 || u == 10)
            .unwrap_or(length - pos);
        if pos < length {
            let skip = usize::from(source[pos] == 13 && source.get(pos + 1) == Some(&10));
            let line = source[start..pos].to_vec();
            let value = Value::Str(cx.heap_mut().alloc_string(line));
            cx.heap_mut().array_push(this, value)?;
            pos += skip;
            start = pos + 1;
        }
        pos += 1;
    }
    if start < length {
        let line = cx.heap().string(text)?[start..length].to_vec();
        let value = Value::Str(cx.heap_mut().alloc_string(line));
        cx.heap_mut().array_push(this, value)?;
    }
    Ok(Value::Obj(ObjRef::bound(this)))
}
pub(crate) fn save(
    cx: &mut NativeCx<'_>,
    name: Value,
    args: RestArgs<'_>,
) -> NativeResult<NativeStep> {
    let this = cx.this();
    let (name, mode) = arguments(cx, name, args)?;
    let values = cx.heap().array(this)?;
    let mut text = Vec::new();
    for &value in values {
        if matches!(value, Value::Str(_) | Value::Int(_) | Value::Real(_)) {
            tjs_core::value::append_string_units(cx.heap(), value, &mut text)?;
        }
        text.extend("\r\n".encode_utf16());
    }
    cx.storage_io(
        Request {
            name,
            mode,
            io: Io::WriteText(text),
        },
        returned(this),
    )
}
pub(crate) fn load_structure(
    cx: &mut NativeCx<'_>,
    name: Value,
    args: RestArgs<'_>,
    dictionary: bool,
) -> NativeResult<NativeStep> {
    let mut this = cx.this();
    if dictionary && cx.heap().container_kind(this)? != ObjectKind::Dictionary {
        this = cx.heap_mut().alloc_dictionary();
    }
    if dictionary {
        cx.heap_mut().clear_members(this)?;
    }
    let (name, mode) = arguments(cx, name, args)?;
    if !dictionary {
        cx.heap_mut().array_resize(this, 0)?;
    }
    cx.storage_io(
        Request {
            name,
            mode,
            io: Io::ReadBinary,
        },
        crate::flow::callback(this, |this, cx, value| {
            let Value::Octet(id) = value else {
                return Err(crate::NativeError::Type("storage bytes"));
            };
            let bytes = cx.heap().octet(id)?.to_vec();
            crate::structured::decode(cx.heap_mut(), &bytes, this).map(NativeStep::Return)
        }),
    )
}
pub(crate) fn save_structure(
    cx: &mut NativeCx<'_>,
    name: Value,
    args: RestArgs<'_>,
) -> NativeResult<NativeStep> {
    let this = cx.this();
    let (name, mode) = arguments(cx, name, args)?;
    let io = if string::c_string(&mode).contains(&98) {
        let bytes = crate::structured::encode(cx.heap(), this)?;
        Io::WriteBinary(bytes)
    } else {
        let text = crate::structured::encode_text(cx.heap_mut(), this)?;
        Io::WriteText(text)
    };
    cx.storage_io(Request { name, mode, io }, returned(this))
}
fn returned(this: ObjId) -> Box<dyn tjs_core::NativeContinuation> {
    crate::flow::callback(this, |this, _, _| {
        Ok(NativeStep::Return(Value::Obj(ObjRef::bound(this))))
    })
}
