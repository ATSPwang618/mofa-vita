use super::{GetMode, MemberError, array_context, object};
use crate::{Heap, NativeCallable, NativeCx, ObjId, ObjectKind, Value};

pub(crate) fn read_value(
    heap: &mut Heap,
    receiver: Value,
    id: ObjId,
    value: Value,
    mode: GetMode,
) -> Result<Value, MemberError> {
    let Value::Obj(mut reference) = value else {
        return Ok(value);
    };
    let this = array_context(receiver, id);
    let record = if !mode.raw() {
        reference.object.map(|id| heap.object(id)).transpose()?
    } else {
        None
    };
    let kind = record.map(|o| o.kind());
    if !mode.raw() {
        if let Some(record) = record.filter(|o| o.kind() == ObjectKind::Property)
            && let Some(function) = script_accessor(heap, reference, record, this, false)?
        {
            return Err(MemberError::Invoke {
                function,
                argument: None,
            });
        }
        if kind == Some(ObjectKind::NativeProperty)
            && let Some(property) = heap.native_property(value)?
        {
            let get = property.get.ok_or(MemberError::AccessDenied)?;
            let this = property_context(value, this);
            return match get {
                NativeCallable::Leaf(get) | NativeCallable::EmptyFinalizer(get) => {
                    Ok(get(&mut NativeCx::new(heap, this, true), &[])?)
                }
                NativeCallable::Resumable(_) => Err(native_invocation(heap, value, this, None)?),
            };
        }
    }
    if reference.this.is_none()
        && heap.object(id)?.kind() != ObjectKind::NativeClass
        && kind
            .or_else(|| {
                reference
                    .object
                    .and_then(|id| heap.object(id).ok().map(|o| o.kind()))
            })
            .is_some_and(|kind| {
                matches!(
                    kind,
                    ObjectKind::NativeFunction | ObjectKind::NativeProperty
                )
            })
    {
        reference.this = Some(this);
        return Ok(Value::Obj(reference));
    }
    Ok(value)
}

pub(super) fn write_property(
    heap: &mut Heap,
    receiver: Value,
    id: ObjId,
    old: Value,
    value: Value,
) -> Result<bool, MemberError> {
    let Value::Obj(reference) = old else {
        return Ok(false);
    };
    let record = reference.object.map(|id| heap.object(id)).transpose()?;
    let kind = record.map(|o| o.kind());
    if let Some(record) = record.filter(|o| o.kind() == ObjectKind::Property)
        && let Some(function) =
            script_accessor(heap, reference, record, array_context(receiver, id), true)?
    {
        return Err(MemberError::Invoke {
            function,
            argument: Some(value),
        });
    }
    if kind == Some(ObjectKind::NativeProperty)
        && let Some(property) = heap.native_property(old)?
    {
        let set = property.set.ok_or(MemberError::AccessDenied)?;
        let this = property_context(old, array_context(receiver, id));
        match set {
            NativeCallable::Leaf(set) | NativeCallable::EmptyFinalizer(set) => {
                set(&mut NativeCx::new(heap, this, false), &[value])?;
            }
            NativeCallable::Resumable(_) => {
                return Err(native_invocation(heap, old, this, Some(value))?);
            }
        }
        return Ok(true);
    }
    Ok(false)
}

fn native_invocation(
    heap: &Heap,
    property: Value,
    this: ObjId,
    argument: Option<Value>,
) -> Result<MemberError, MemberError> {
    let object = heap
        .native_accessor(property, argument.is_some())?
        .expect("registered resumable accessor");
    Ok(MemberError::Invoke {
        function: Value::Obj(crate::ObjRef {
            object: Some(object),
            this: Some(this),
        }),
        argument,
    })
}

fn script_accessor(
    heap: &Heap,
    reference: crate::ObjRef,
    record: &crate::ObjRecord,
    context: ObjId,
    set: bool,
) -> Result<Option<Value>, MemberError> {
    // Default member access treats an invalid property as an ordinary value.
    if record.ensure_valid().is_err() {
        return Ok(None);
    }
    let Some(function) = record.script_function() else {
        return Ok(None);
    };
    let crate::FunctionKind::Property { getter, setter } = function.kind() else {
        return Ok(None);
    };
    let id = (if set { setter } else { getter }).ok_or(MemberError::AccessDenied)?;
    let Value::Obj(mut accessor) = heap.function_value(function.pool, id) else {
        unreachable!("property accessors loaded with their definition")
    };
    accessor.this = reference.this.or(Some(context));
    Ok(Some(Value::Obj(accessor)))
}

fn property_context(value: Value, fallback: ObjId) -> ObjId {
    match value {
        Value::Obj(reference) => reference.this.unwrap_or(fallback),
        _ => fallback,
    }
}

/// A null member name accesses the property object itself, not one of its members.
pub(crate) fn dereference(
    heap: &mut Heap,
    property: Value,
    context: ObjId,
    argument: Option<Value>,
) -> Result<Value, MemberError> {
    if argument.is_none() && matches!(property, Value::Void) {
        return Ok(Value::Void);
    }
    let (id, kind) = object(heap, property)?;
    if !matches!(kind, ObjectKind::Property | ObjectKind::NativeProperty) {
        return Err(MemberError::NotProperty);
    }
    let receiver = Value::Obj(crate::ObjRef {
        object: Some(id),
        this: Some(property_context(property, context)),
    });
    if let Some(value) = argument {
        write_property(heap, receiver, id, property, value)?;
        Ok(Value::Void)
    } else {
        read_value(heap, receiver, id, property, GetMode::Value)
    }
}
