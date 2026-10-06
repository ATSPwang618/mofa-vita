//! Explicit binding conversions. Ordinary `i64` parameters remain strict;
//! `#[tjs(coerce)]` opts a declaration into the shared TJS value conversions.
use crate::{FromTjs, Heap, IntoTjs, NativeError, NativeResult, Value};

pub trait Coerce: Sized {
    fn coerce(value: Value, heap: &Heap) -> NativeResult<Self>;
}
pub fn coerce_argument<T: Coerce>(args: &[Value], index: usize, heap: &Heap) -> NativeResult<T> {
    T::coerce(
        *args.get(index).ok_or(NativeError::Missing(index + 1))?,
        heap,
    )
}
macro_rules! integer {
    ($($ty:ty),*) => {$(impl Coerce for $ty {
        fn coerce(value: Value, heap: &Heap) -> NativeResult<Self> {
            Ok(tjs_core::value::to_integer(heap, value)? as Self)
        }
    })*};
}
integer!(i32, u32, i64);
impl Coerce for f64 {
    fn coerce(value: Value, heap: &Heap) -> NativeResult<Self> {
        Ok(tjs_core::value::to_real(heap, value)?)
    }
}
impl Coerce for bool {
    fn coerce(value: Value, heap: &Heap) -> NativeResult<Self> {
        Ok(value.truthy(heap)?)
    }
}
impl Coerce for Value {
    fn coerce(value: Value, _: &Heap) -> NativeResult<Self> {
        Ok(value)
    }
}

/// UTF-16 text is distinct from a script Array<u16>. Conversion retains NULs;
/// native APIs that use C strings choose truncation explicitly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Utf16(pub Vec<u16>);
impl crate::Trace for Utf16 {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl FromTjs for Utf16 {
    fn from_tjs(value: Value, heap: &Heap) -> NativeResult<Self> {
        match value {
            Value::Str(id) => Ok(Self(heap.string(id)?.to_vec())),
            _ => Err(NativeError::Type("a String")),
        }
    }
}
impl Coerce for Utf16 {
    fn coerce(value: Value, heap: &Heap) -> NativeResult<Self> {
        Ok(Self(tjs_core::value::to_string_units(heap, value)?))
    }
}
impl Coerce for String {
    fn coerce(value: Value, heap: &Heap) -> NativeResult<Self> {
        let units = Utf16::coerce(value, heap)?;
        String::from_utf16(&units.0).map_err(|_| NativeError::Message("text is not valid Unicode"))
    }
}
impl IntoTjs for Utf16 {
    fn into_tjs(self, heap: &mut Heap) -> NativeResult<Value> {
        Ok(Value::Str(heap.alloc_string(self.0)))
    }
}

/// Explicit output containers; do not assign meaning to `Option` parameters
/// or change the established strict FromTjs rules in the language core.
pub struct Array<T>(pub T);
impl<I: IntoIterator> IntoTjs for Array<I>
where
    I::Item: IntoTjs,
{
    fn into_tjs(self, heap: &mut Heap) -> NativeResult<Value> {
        let array = heap.alloc_array();
        for item in self.0 {
            let value = item.into_tjs(heap)?;
            heap.array_push(array, value)?;
        }
        Ok(Value::Obj(tjs_core::ObjRef::bound(array)))
    }
}
pub struct Dictionary<T>(pub T);
impl<K: AsRef<str>, V: IntoTjs, I: IntoIterator<Item = (K, V)>> IntoTjs for Dictionary<I> {
    fn into_tjs(self, heap: &mut Heap) -> NativeResult<Value> {
        let object = heap.alloc_dictionary();
        for (key, value) in self.0 {
            let value = value.into_tjs(heap)?;
            let key = heap.intern(&key.as_ref().encode_utf16().collect::<Vec<_>>());
            heap.set_member(object, key, value)?;
        }
        Ok(Value::Obj(tjs_core::ObjRef::bound(object)))
    }
}
