//! Script member semantics above the heap's raw storage operations.
//! Script accessors return a call request; the VM owns suspension and class
//! proxy lookup. Neither belongs in raw heap storage.

#[cfg(test)]
#[path = "../tests/internal/member_assignment.rs"]
mod assignment_tests;
mod primitives;
mod properties;
use crate::{Heap, HeapError, NativeError, ObjId, ObjectKind, StrId, SymbolId, Value};
pub(crate) use properties::dereference;
pub(crate) use properties::read_value;
use properties::write_property;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GetMode {
    Value,
    Raw,
    Required,
    Flags(crate::MemberFlags),
}
impl GetMode {
    pub(super) fn raw(self) -> bool {
        matches!(
            self,
            Self::Raw
                | Self::Flags(crate::MemberFlags {
                    ignore_property: true,
                    ..
                })
        )
    }
    fn required(self) -> bool {
        matches!(
            self,
            Self::Required
                | Self::Flags(crate::MemberFlags {
                    must_exist: true,
                    ..
                })
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MemberError {
    #[error("script property access requires a VM call")]
    Invoke {
        function: Value,
        argument: Option<Value>,
    },
    #[error("property is read-only or write-only")]
    AccessDenied,
    #[error("value is not a property object")]
    NotProperty,
    #[error("string or octet index is out of range")]
    Range,
    #[error(transparent)]
    Conversion(#[from] crate::value::ArithmeticError),
    #[error("member access requires an object, string or octet")]
    NotObject,
    #[error("member access on null")]
    NullObject,
    #[error("string or octet property names require a string or number")]
    Name,
    #[error("object member does not exist")]
    Missing,
    #[error("missing member requires a VM callback")]
    MissingHook {
        object: ObjId,
        name: Value,
        value: Option<Value>,
    },
    #[error(transparent)]
    Heap(#[from] HeapError),
    #[error(transparent)]
    Native(#[from] NativeError),
}

fn object(heap: &Heap, receiver: Value) -> Result<(ObjId, ObjectKind), MemberError> {
    let Value::Obj(reference) = receiver else {
        return Err(MemberError::NotObject);
    };
    let id = reference.object.ok_or(MemberError::NullObject)?;
    let record = heap.object(id)?;
    record.ensure_valid()?;
    let kind = record.kind();
    Ok((id, kind))
}

enum Name {
    Default,
    String(StrId),
    Integer(i64),
    Converted(Vec<u16>),
}

fn default_key(heap: &Heap, value: Value) -> Result<bool, HeapError> {
    Ok(match value {
        Value::Void => true,
        Value::Str(id) => heap.string(id)?.is_empty(),
        _ => false,
    })
}

impl Name {
    fn new(heap: &Heap, value: Value, numeric_index: bool) -> Result<Self, MemberError> {
        match value {
            // AsString returns a null pointer for void and empty strings. Prop*
            // uses it to address the object itself; it is not a named "" slot.
            Value::Void => Ok(Self::Default),
            Value::Str(id) if heap.string(id)?.is_empty() => Ok(Self::Default),
            Value::Str(id) => Ok(Self::String(id)),
            // TJS's indirect get/set pass integers through the signed 32-bit
            // Prop*ByNum API. delete uses AsString and preserves all 64 bits.
            Value::Int(value) => Ok(Self::Integer(if numeric_index {
                i64::from(value as i32)
            } else {
                value
            })),
            _ => Ok(Self::Converted(crate::value::to_string_units(heap, value)?)),
        }
    }

    fn find(&self, heap: &Heap) -> Result<Option<SymbolId>, HeapError> {
        Ok(match *self {
            Self::Default => None,
            Self::Converted(ref units) => heap.find_symbol(crate::string::c_string(units)),
            Self::String(id) => heap.find_string_symbol(id)?,
            Self::Integer(value) => integer_name(value, |units| heap.find_symbol(units)),
        })
    }

    fn intern(&self, heap: &mut Heap) -> Result<SymbolId, MemberError> {
        match *self {
            Self::Default => Err(MemberError::NotProperty),
            Self::Converted(ref units) => Ok(heap.intern(crate::string::c_string(units))),
            Self::String(id) => Ok(heap.intern_string(id)?),
            Self::Integer(value) => Ok(integer_name(value, |units| heap.intern(units))),
        }
    }

    fn array_key(&self, heap: &Heap) -> Result<Option<i32>, HeapError> {
        Ok(match *self {
            Self::Default => None,
            Self::Converted(ref units) => array_index(crate::string::c_string(units)),
            Self::Integer(index) => Some(index as i32),
            Self::String(id) => {
                let units = heap.string(id)?;
                let units = &units[..units
                    .iter()
                    .position(|&unit| unit == 0)
                    .unwrap_or(units.len())];
                array_index(units)
            }
        })
    }
}

// tjsArray's IsNumber accepts signed decimal names, ASCII whitespace and dots;
// TJS_atoi then reads only the initial integer part ("1.9" addresses slot 1).
fn array_index(units: &[u16]) -> Option<i32> {
    let space = |unit: &u16| matches!(*unit, 9..=13 | 32);
    let units = &units[units.iter().take_while(|u| space(u)).count()..];
    let end = units
        .iter()
        .rposition(|u| !space(u))
        .map_or(0, |index| index + 1);
    let units = &units[..end];
    let (negative, units) = match units.first() {
        Some(45) => (true, &units[1..]),
        Some(43) => (false, &units[1..]),
        _ => (false, units),
    };
    let units = &units[units.iter().take_while(|u| space(u)).count()..];
    if !units.first().is_some_and(|u| (48..=57).contains(u))
        || !units.iter().all(|u| (48..=57).contains(u) || *u == 46)
    {
        return None;
    }
    let mut value = 0_i32;
    for &digit in units.iter().take_while(|&&u| (48..=57).contains(&u)) {
        value = value.wrapping_mul(10).wrapping_add(i32::from(digit - 48));
    }
    Some(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

fn array_context(receiver: Value, object: ObjId) -> ObjId {
    let Value::Obj(reference) = receiver else {
        unreachable!("object checked above")
    };
    reference.this.unwrap_or(object)
}

fn array_offset(length: usize, index: i32) -> Option<usize> {
    let index = if index < 0 {
        length as i64 + i64::from(index)
    } else {
        i64::from(index)
    };
    usize::try_from(index).ok()
}

fn array_get(
    heap: &Heap,
    context: ObjId,
    name: &Name,
    required: bool,
) -> Result<Option<Value>, MemberError> {
    match name.array_key(heap)? {
        Some(index) => {
            let values = heap.array(context)?;
            let value =
                array_offset(values.len(), index).and_then(|index| values.get(index).copied());
            if required && value.is_none() {
                return Err(MemberError::Missing);
            }
            Ok(Some(value.unwrap_or(Value::Void)))
        }
        _ => Ok(None),
    }
}

fn array_set(
    heap: &mut Heap,
    context: ObjId,
    name: &Name,
    value: Value,
    existing: bool,
    receiver: Value,
    raw: bool,
) -> Result<bool, MemberError> {
    match name.array_key(heap)? {
        Some(index) => {
            let length = heap.array(context)?.len();
            let index = array_offset(length, index).ok_or(MemberError::Missing)?;
            if index >= length {
                if existing {
                    return Err(MemberError::Missing);
                }
                heap.array_resize(context, index + 1)?;
            }
            if !raw {
                let old = heap.array(context)?[index];
                if write_property(heap, receiver, context, old, value)? {
                    return Ok(true);
                }
            }
            heap.array_set(context, index, value)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn integer_name<T>(value: i64, use_name: impl FnOnce(&[u16]) -> T) -> T {
    let mut decimal = itoa::Buffer::new();
    let text = decimal.format(value);
    let mut units = [0_u16; 20]; // i64::MIN needs 19 decimal digits plus its sign.
    for (unit, byte) in units.iter_mut().zip(text.bytes()) {
        *unit = u16::from(byte);
    }
    use_name(&units[..text.len()])
}

/// Declaration initialization bypasses property handlers.
pub(crate) fn define(
    heap: &mut Heap,
    receiver: Value,
    key: Value,
    value: Value,
) -> Result<(), MemberError> {
    let (id, _) = object(heap, receiver)?;
    let name = Name::new(heap, key, true)?.intern(heap)?;
    heap.set_member(id, name, value)?;
    Ok(())
}

pub fn get(heap: &mut Heap, receiver: Value, key: Value) -> Result<Value, MemberError> {
    get_mode(heap, receiver, key, GetMode::Value)
}

pub(crate) fn get_mode(
    heap: &mut Heap,
    receiver: Value,
    key: Value,
    mode: GetMode,
) -> Result<Value, MemberError> {
    if matches!(receiver, Value::Str(_) | Value::Octet(_)) {
        return primitives::get(heap, receiver, key);
    }
    // The reference VM propagates void before converting the key.
    let (id, kind) = object(heap, receiver)?;
    let name = Name::new(heap, key, true)?;
    if let Name::Integer(index) = name
        && let Some(dispatch) = heap.native_index(id)?
    {
        return (dispatch.get)(&mut crate::NativeCx::new(heap, id, true), index as i32)
            .map_err(MemberError::from);
    }
    if matches!(name, Name::Default) {
        return dereference(heap, receiver, array_context(receiver, id), None);
    }
    if kind == ObjectKind::Array {
        if let Some(value) = array_get(heap, array_context(receiver, id), &name, mode.required())? {
            return read_value(heap, receiver, id, value, mode);
        }
    }
    if let Some(name) = name.find(heap)? {
        if let Some(value) = heap.lookup_member(id, name)? {
            return read_value(heap, receiver, id, value, mode);
        }
    }
    missing_hook(heap, id, &name, None)?;
    if matches!(
        mode,
        GetMode::Flags(crate::MemberFlags { ensure: true, .. })
    ) {
        let name = name.intern(heap)?;
        heap.set_member(id, name, Value::Void)?;
        return Ok(Value::Void);
    }
    if kind == ObjectKind::Dictionary && !mode.required() {
        Ok(Value::Void)
    } else {
        Err(MemberError::Missing)
    }
}

pub fn set(heap: &mut Heap, receiver: Value, key: Value, value: Value) -> Result<(), MemberError> {
    set_mode(heap, receiver, key, value, false, false)
}

pub(crate) fn property_ensure(
    heap: &Heap,
    receiver: Value,
    key: Value,
    flags: crate::MemberFlags,
) -> Result<bool, MemberError> {
    let (_, kind) = object(heap, receiver)?;
    // Array numeric PropSet grows unless MUSTEXIST is supplied; named members
    // (including numeric names on plain objects) require MEMBERENSURE instead.
    Ok(
        if kind == ObjectKind::Array && Name::new(heap, key, true)?.array_key(heap)?.is_some() {
            !flags.must_exist
        } else {
            flags.ensure
        },
    )
}

pub(crate) fn set_mode(
    heap: &mut Heap,
    receiver: Value,
    key: Value,
    value: Value,
    existing: bool,
    raw: bool,
) -> Result<(), MemberError> {
    set_flags(heap, receiver, key, value, existing, raw, (false, false))
}

pub(crate) fn set_flags(
    heap: &mut Heap,
    receiver: Value,
    key: Value,
    value: Value,
    existing: bool,
    raw: bool,
    (hidden, class_only): (bool, bool),
) -> Result<(), MemberError> {
    if matches!(receiver, Value::Str(_) | Value::Octet(_)) {
        return primitives::set(heap, key);
    }
    let (id, kind) = object(heap, receiver)?;
    let name = Name::new(heap, key, true)?;
    if let Name::Integer(index) = name
        && let Some(dispatch) = heap.native_index(id)?
    {
        return (dispatch.set)(
            &mut crate::NativeCx::new(heap, id, false),
            index as i32,
            value,
        )
        .map_err(MemberError::from);
    }
    if matches!(name, Name::Default) {
        return dereference(heap, receiver, array_context(receiver, id), Some(value)).map(|_| ());
    }
    if kind == ObjectKind::Array
        && array_set(
            heap,
            array_context(receiver, id),
            &name,
            value,
            existing,
            receiver,
            raw,
        )?
    {
        return Ok(());
    }
    if heap.calls_missing(id)?
        && name
            .find(heap)?
            .map(|name| heap.lookup_member(id, name))
            .transpose()?
            .flatten()
            .is_none()
    {
        missing_hook(heap, id, &name, Some(value))?;
    }
    let name = if existing {
        name.find(heap)?.ok_or(MemberError::Missing)?
    } else {
        name.intern(heap)?
    };
    let old = heap.lookup_member(id, name)?;
    if existing && old.is_none() {
        return Err(MemberError::Missing);
    }
    if !raw {
        if let Some(old) = old {
            // Only object values can invoke a setter or fail while resolving
            // one. Update flags before that observable call/error; ordinary
            // value writes apply them once in set_member_flags below.
            if matches!(old, Value::Obj(_)) {
                heap.set_member_attributes(id, name, hidden, class_only)?;
            }
            if write_property(heap, receiver, id, old, value)? {
                return Ok(());
            }
        }
    }
    heap.set_member_flags(id, name, value, hidden, class_only)?;
    Ok(())
}

pub(crate) fn update_value(
    heap: &mut Heap,
    receiver: Value,
    key: Value,
) -> Result<Value, MemberError> {
    let (id, kind) = object(heap, receiver)?;
    let name = Name::new(heap, key, true)?;
    if let Name::Integer(index) = name
        && let Some(dispatch) = heap.native_index(id)?
    {
        return (dispatch.get)(&mut crate::NativeCx::new(heap, id, true), index as i32)
            .map_err(MemberError::from);
    }
    if matches!(name, Name::Default) {
        return if matches!(kind, ObjectKind::Property | ObjectKind::NativeProperty) {
            Ok(receiver)
        } else {
            Err(MemberError::NotProperty)
        };
    }
    if kind == ObjectKind::Array {
        if let Some(index) = name.array_key(heap)? {
            let context = array_context(receiver, id);
            let length = heap.array(context)?.len();
            let index = array_offset(length, index).ok_or(MemberError::Missing)?;
            if index >= length {
                heap.array_resize(context, index + 1)?;
            }
            return read_value(
                heap,
                receiver,
                id,
                heap.array(context)?[index],
                GetMode::Raw,
            );
        }
    }
    if let Some(symbol) = name.find(heap)? {
        if let Some(value) = heap.lookup_member(id, symbol)? {
            return read_value(heap, receiver, id, value, GetMode::Raw);
        }
    }
    missing_hook(heap, id, &name, None)?;
    if kind != ObjectKind::Dictionary {
        return Err(MemberError::Missing);
    }
    // Dictionary::Operation creates the missing slot before attempting the
    // conversion/arithmetic, so a failed operation leaves an explicit void.
    let symbol = name.intern(heap)?;
    heap.set_member(id, symbol, Value::Void)?;
    Ok(Value::Void)
}

pub(crate) fn commit_update(
    heap: &mut Heap,
    receiver: Value,
    key: Value,
    value: Value,
) -> Result<(), MemberError> {
    let (id, kind) = object(heap, receiver)?;
    let name = Name::new(heap, key, true)?;
    if kind == ObjectKind::Array {
        if let Some(index) = name.array_key(heap)? {
            let context = array_context(receiver, id);
            let index =
                array_offset(heap.array(context)?.len(), index).ok_or(MemberError::Missing)?;
            heap.array_set(context, index, value)?;
            return Ok(());
        }
    }
    let name = name.find(heap)?.ok_or(MemberError::Missing)?;
    heap.update_member_value(id, name, value)?;
    Ok(())
}

pub fn delete(heap: &mut Heap, receiver: Value, key: Value) -> Result<bool, MemberError> {
    // Receiver conversion and key conversion throw; an invalid object's
    // DeleteMember status is a false result, not an exception.
    let Value::Obj(reference) = receiver else {
        return Err(MemberError::NotObject);
    };
    let id = reference.object.ok_or(MemberError::NullObject)?;
    let name = Name::new(heap, key, false)?;
    let record = heap.object(id)?;
    if !heap.is_valid(id)? {
        return Ok(false);
    }
    let kind = record.kind();
    if kind == ObjectKind::Array {
        if let Some(index) = name.array_key(heap)? {
            let context = array_context(receiver, id);
            let length = heap.array(context)?.len();
            let Some(index) = array_offset(length, index).filter(|&index| index < length) else {
                return Ok(false);
            };
            heap.array_remove(context, index)?;
            return Ok(true);
        }
    }
    let Some(name) = name.find(heap)? else {
        return Ok(false);
    };
    Ok(heap.delete_member(id, name)?)
}

/// Only a missing named member on a valid object continues through class bases.
pub(crate) fn delete_lookup(
    heap: &mut Heap,
    receiver: Value,
    key: Value,
) -> Result<Value, MemberError> {
    let deleted = delete(heap, receiver, key)?;
    if !deleted && !default_key(heap, key)? {
        let Value::Obj(reference) = receiver else {
            unreachable!("delete validated receiver")
        };
        if heap.is_valid(reference.object.expect("delete validated object"))? {
            return Err(MemberError::Missing);
        }
    }
    Ok(Value::Int(i64::from(deleted)))
}

/// FuncCall uses full string conversion for computed names and requires the
/// member to exist, even on Dictionary. It does not perform ordinary PropGet.
pub(crate) fn callable(heap: &mut Heap, receiver: Value, key: Value) -> Result<Value, MemberError> {
    let (id, kind) = object(heap, receiver)?;
    // CallFunctionIndirect uses ttstr::c_str(), which turns the null string
    // pointer into an empty *named* member, unlike PropGet/PropSet/DeleteMember.
    let name = match Name::new(heap, key, false)? {
        Name::Default => Name::Converted(Vec::new()),
        name => name,
    };
    if kind == ObjectKind::Array {
        if let Some(value) = array_get(heap, array_context(receiver, id), &name, false)? {
            return read_callable_value(heap, receiver, id, value);
        }
    }
    let found = name
        .find(heap)?
        .map(|name| heap.lookup_member(id, name))
        .transpose()?
        .flatten();
    if found.is_none() {
        missing_hook(heap, id, &name, None)?;
    }
    let value = found.ok_or(MemberError::Missing)?;
    read_callable_value(heap, receiver, id, value)
}

/// FuncCall's property fallback uses AsObjectClosure on the successful getter
/// result. A primitive returned by a getter is a conversion error, whereas a
/// primitive stored directly in the slot produces an invalid-call status.
pub(crate) fn read_callable_value(
    heap: &mut Heap,
    receiver: Value,
    owner: ObjId,
    value: Value,
) -> Result<Value, MemberError> {
    let property = match value {
        Value::Obj(reference) => reference.object.is_some_and(|id| {
            heap.object(id).is_ok_and(|o| {
                o.ensure_valid().is_ok()
                    && matches!(o.kind(), ObjectKind::Property | ObjectKind::NativeProperty)
            })
        }),
        _ => false,
    };
    let result = read_value(heap, receiver, owner, value, GetMode::Value)?;
    if property && !matches!(result, Value::Obj(_)) {
        return Err(NativeError::Type("an object returned by the property getter").into());
    }
    Ok(result)
}

/// Presence does not invoke an accessor or confuse a stored void with absence.
/// Array indices follow the same negative-index rules as ordinary member access.
pub(crate) fn contains(heap: &Heap, receiver: Value, key: Value) -> Result<Value, MemberError> {
    let (id, kind) = object(heap, receiver)?;
    let name = Name::new(heap, key, true)?;
    if kind == ObjectKind::Array {
        if let Some(index) = name.array_key(heap)? {
            let values = heap.array(array_context(receiver, id))?;
            return if array_offset(values.len(), index).is_some_and(|i| i < values.len()) {
                Ok(Value::Int(1))
            } else {
                Err(MemberError::Missing)
            };
        }
    }
    if let Some(name) = name.find(heap)? {
        if heap.lookup_member(id, name)?.is_some() {
            return Ok(Value::Int(1));
        }
    }
    Err(MemberError::Missing)
}

fn missing_hook(
    heap: &mut Heap,
    object: ObjId,
    name: &Name,
    value: Option<Value>,
) -> Result<(), MemberError> {
    if heap.calls_missing(object)? {
        let units = match *name {
            Name::Default => return Ok(()),
            Name::Converted(ref units) => crate::string::c_string(units).to_vec(),
            Name::String(id) => crate::string::c_string(heap.string(id)?).to_vec(),
            Name::Integer(n) => integer_name(n, |units| units.to_vec()),
        };
        let name = Value::Str(heap.alloc_string(units));
        return Err(MemberError::MissingHook {
            object,
            name,
            value,
        });
    }
    Ok(())
}
