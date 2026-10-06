use tjs_core::{
    Heap, NativeError, NativeIndex, NativeResult, ObjId, Value,
    value::{self, UpdateOp},
};
#[tjs_bind::class(name = "CBinaryAccessor")]
pub(super) mod bindings {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        #[trace(skip = "Owned byte buffer has no managed values")]
        pub bytes: Option<Vec<u8>>,
        pub position: i32,
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method]
        fn finalize(&mut self) {
            self.bytes = None;
        }
        #[tjs::getter]
        fn count(&self) -> NativeResult<i64> {
            Ok(self
                .bytes
                .as_ref()
                .ok_or(NativeError::Message("expired filter buffer"))?
                .len() as i64)
        }
        #[tjs::getter]
        fn ptr(&self) -> i64 {
            i64::from(self.position)
        }
        #[tjs::setter(name = "ptr")]
        fn set_ptr(&mut self, #[tjs(coerce)] value: i32) {
            self.position = value;
        }
        #[tjs::method(name = "xor")]
        fn xor(
            &mut self,
            #[tjs(coerce)] offset: i32,
            #[tjs(coerce)] length: i32,
            #[tjs(coerce)] value: i32,
        ) -> NativeResult<()> {
            let bytes = self
                .bytes
                .as_mut()
                .ok_or(NativeError::Message("expired filter buffer"))?;
            let start = usize::try_from(self.position.wrapping_add(offset))
                .map_err(|_| NativeError::Message("filter buffer range"))?;
            let len =
                usize::try_from(length).map_err(|_| NativeError::Message("filter buffer range"))?;
            let end = start
                .checked_add(len)
                .ok_or(NativeError::Message("filter buffer range"))?;
            for byte in bytes
                .get_mut(start..end)
                .ok_or(NativeError::Message("filter buffer range"))?
            {
                *byte ^= value as u8;
            }
            Ok(())
        }
        #[tjs::method]
        fn missing(&self, set: bool, _name: Value, _result: Value) -> NativeResult<bool> {
            if set {
                return Err(NativeError::Message("unsupported filter buffer member"));
            }
            Ok(true)
        }
    }
}
fn index(state: &bindings::State, index: i32) -> NativeResult<usize> {
    let at = usize::try_from(state.position.wrapping_add(index))
        .map_err(|_| NativeError::Message("filter buffer index"))?;
    if at
        >= state
            .bytes
            .as_ref()
            .ok_or(NativeError::Message("expired filter buffer"))?
            .len()
    {
        return Err(NativeError::Message("filter buffer index"));
    }
    Ok(at)
}
static INDEX: NativeIndex = NativeIndex {
    get: |cx, index_| {
        cx.with_state::<bindings::State, _>(|s, _| {
            let at = index(s, index_)?;
            Ok(Value::Int(i64::from(s.bytes.as_ref().unwrap()[at])))
        })
    },
    set: |cx, index_, value| {
        let val = value::to_integer(cx.heap(), value)? as u8;
        cx.with_state::<bindings::State, _>(|s, _| {
            let at = index(s, index_)?;
            s.bytes.as_mut().unwrap()[at] = val;
            Ok(())
        })
    },
    update: |cx, index_, op, rhs| {
        let rhs = if op.unary() {
            0
        } else {
            value::to_integer(cx.heap(), rhs)? as u8
        };
        cx.with_state::<bindings::State, _>(|s, _| {
            let at = index(s, index_)?;
            let lhs = s.bytes.as_ref().unwrap()[at];
            let result = match op {
                UpdateOp::Increment => lhs.wrapping_add(1),
                UpdateOp::Decrement => lhs.wrapping_sub(1),
                UpdateOp::Add => lhs.wrapping_add(rhs),
                UpdateOp::Subtract => lhs.wrapping_sub(rhs),
                UpdateOp::Multiply => lhs.wrapping_mul(rhs),
                UpdateOp::BitAnd => lhs & rhs,
                UpdateOp::BitOr => lhs | rhs,
                UpdateOp::BitXor => lhs ^ rhs,
                UpdateOp::LogicalOr => u8::from(lhs != 0 || rhs != 0),
                UpdateOp::LogicalAnd => u8::from(lhs != 0 && rhs != 0),
                UpdateOp::Remainder | UpdateOp::IntDivide | UpdateOp::Divide => {
                    if rhs == 0 {
                        return Err(NativeError::Message("filter byte division by zero"));
                    }
                    match op {
                        UpdateOp::Divide => (i32::from(lhs) / i32::from(rhs as i8)) as u8,
                        UpdateOp::Remainder => lhs % rhs,
                        _ => lhs / rhs,
                    }
                }
                UpdateOp::ShiftLeft | UpdateOp::ShiftRight | UpdateOp::ShiftRightUnsigned => {
                    if rhs >= 32 {
                        return Err(NativeError::Message("filter byte shift out of range"));
                    }
                    if op == UpdateOp::ShiftLeft {
                        (u32::from(lhs) << rhs) as u8
                    } else {
                        (u32::from(lhs) >> rhs) as u8
                    }
                }
            };
            s.bytes.as_mut().unwrap()[at] = result;
            Ok(Value::Void)
        })
    },
};
pub(crate) fn create(heap: &mut Heap, bytes: Vec<u8>) -> NativeResult<ObjId> {
    let class = heap.register_class_variant("krkr.xp3.BinaryAccessor", &bindings::CLASS)?;
    let object = heap.alloc_native(
        class,
        bindings::State {
            bytes: Some(bytes),
            position: 0,
        },
    )?;
    heap.initialize_native_state(object, INDEX)?;
    heap.set_call_missing(object)?;
    Ok(object)
}
pub(crate) fn take(heap: &mut Heap, object: ObjId) -> NativeResult<Vec<u8>> {
    heap.with_native_state::<bindings::State, _>(object, |s| s.bytes.take())?
        .ok_or(NativeError::Message("filter invalidated its byte buffer"))
}
