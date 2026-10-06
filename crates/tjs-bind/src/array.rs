mod join;
mod search;
mod sort;
use crate::{NativeCx, NativeError, NativeResult, NativeStep, RestArgs, Value};
use tjs_core::value;

#[crate::class(name = "Array", storage = "array")]
/// TJS Array：连续元素、长度属性及基础修改方法。
mod implementation {
    use super::*;

    #[derive(Default, crate::Trace)]
    pub struct State;

    impl State {
        #[tjs::method(resumable = true)]
        fn load(
            &mut self,
            cx: &mut NativeCx<'_>,
            name: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            crate::container_io::load(cx, name, args)
        }
        #[tjs::method(resumable = true)]
        fn save(
            &self,
            cx: &mut NativeCx<'_>,
            name: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            crate::container_io::save(cx, name, args)
        }
        #[tjs::method(name = "loadStruct", resumable = true)]
        fn load_struct(
            &mut self,
            cx: &mut NativeCx<'_>,
            name: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            crate::container_io::load_structure(cx, name, args, false)
        }
        #[tjs::method(name = "saveStruct", resumable = true)]
        fn save_struct(
            &self,
            cx: &mut NativeCx<'_>,
            name: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            crate::container_io::save_structure(cx, name, args)
        }
        #[tjs::method]
        fn pack(&self, cx: &mut NativeCx<'_>, template: Value) -> NativeResult<Value> {
            if !cx.result_needed() {
                return Ok(Value::Void);
            }
            if !matches!(template, Value::Str(_)) {
                return Err(NativeError::Type("a pack/unpack template string"));
            }
            let this = cx.this();
            tjs_core::octet::pack_array(cx.heap_mut(), this, template)
        }
        #[tjs::method(resumable = true)]
        fn join(
            &self,
            cx: &mut NativeCx<'_>,
            delimiter: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            join::start(cx, delimiter, args)
        }
        #[tjs::method(resumable = true)]
        fn sort(&mut self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            sort::start(cx, args)
        }
        #[tjs::method]
        fn erase(&mut self, cx: &mut NativeCx<'_>, index: Value) -> NativeResult<()> {
            let this = cx.this();
            let index = offset(cx, index)?;
            cx.heap_mut().array_remove(this, index)?;
            Ok(())
        }
        #[tjs::method]
        fn insert(
            &mut self,
            cx: &mut NativeCx<'_>,
            index: Value,
            value: Value,
        ) -> NativeResult<()> {
            let this = cx.this();
            let index = offset(cx, index)?;
            Ok(cx.heap_mut().array_insert(this, index, &[value])?)
        }
        #[tjs::method(resumable = true)]
        fn find(
            &self,
            cx: &mut NativeCx<'_>,
            value: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            search::find(cx, value, args)
        }
        #[tjs::method(resumable = true)]
        fn remove(
            &mut self,
            cx: &mut NativeCx<'_>,
            value: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            search::remove(cx, value, args)
        }
        #[tjs::method(resumable = true)]
        fn assign(&mut self, cx: &mut NativeCx<'_>, source: Value) -> NativeResult<NativeStep> {
            crate::containers::assign(cx, source, true, false)
        }
        #[tjs::method(name = "assignStruct", resumable = true)]
        fn assign_struct(
            &mut self,
            cx: &mut NativeCx<'_>,
            source: Value,
        ) -> NativeResult<NativeStep> {
            crate::containers::assign(cx, source, true, true)
        }

        /// Split on a delimiter character set or a RegExp, replacing this array.
        #[tjs::method(resumable = true)]
        fn split(
            &mut self,
            cx: &mut NativeCx<'_>,
            pattern: Value,
            target: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            crate::regexp::split::array(cx, pattern, target, args)
        }
        #[tjs::constructor]
        fn new() -> Self {
            Self
        }

        /// 当前元素数量。
        #[tjs::getter(name = "count")]
        #[tjs::getter(name = "length")]
        fn count(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            // The length lives in heap storage. Taking native State here would
            // mark this read-only access as a write and requeue the array for GC.
            Ok(cx.heap().array(cx.this())?.len() as i64)
        }
        #[tjs::setter(name = "count")]
        #[tjs::setter(name = "length")]
        fn set_count(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            let value = value::to_integer(cx.heap(), value)?;
            // TJS narrows to tjs_uint before resize, including values whose
            // low 32 bits describe a small, valid array length.
            let length = value as u32;
            let this = cx.this();
            Ok(cx.heap_mut().array_resize(this, length as usize)?)
        }
        /// 追加所有实参，返回新的长度。
        #[tjs::method]
        fn push(&mut self, cx: &mut NativeCx<'_>, values: RestArgs<'_>) -> NativeResult<i64> {
            let this = cx.this();
            let length = cx.heap().array(this)?.len();
            cx.heap_mut().array_insert(this, length, values)?;
            Ok((length + values.len()) as i64)
        }
        #[tjs::method]
        fn add(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<i64> {
            let this = cx.this();
            let index = cx.heap().array(this)?.len();
            cx.heap_mut().array_push(this, value)?;
            Ok(index as i64)
        }
        #[tjs::method]
        fn pop(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let this = cx.this();
            let length = cx.heap().array(this)?.len();
            if length == 0 {
                return Ok(Value::Void);
            }
            Ok(cx.heap_mut().array_remove(this, length - 1)?)
        }
        #[tjs::method]
        fn shift(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let this = cx.this();
            if cx.heap().array(this)?.is_empty() {
                return Ok(Value::Void);
            }
            Ok(cx.heap_mut().array_remove(this, 0)?)
        }
        #[tjs::method]
        fn unshift(&mut self, cx: &mut NativeCx<'_>, values: RestArgs<'_>) -> NativeResult<i64> {
            let this = cx.this();
            cx.heap_mut().array_insert(this, 0, values)?;
            Ok(cx.heap().array(this)?.len() as i64)
        }
        #[tjs::method]
        fn clear(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<()> {
            let this = cx.this();
            Ok(cx.heap_mut().array_resize(this, 0)?)
        }
        #[tjs::method]
        fn reverse(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<()> {
            let this = cx.this();
            Ok(cx.heap_mut().array_reverse(this)?)
        }
    }
}
pub use implementation::{CLASS, install};

fn offset(cx: &NativeCx<'_>, index: Value) -> NativeResult<usize> {
    let index = value::to_integer(cx.heap(), index)? as i32;
    let index = if index < 0 {
        cx.heap().array(cx.this())?.len() as i64 + i64::from(index)
    } else {
        i64::from(index)
    };
    usize::try_from(index).map_err(|_| tjs_core::HeapError::ArrayIndex.into())
}
