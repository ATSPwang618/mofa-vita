//! Native rectangle values used by Font and drawing APIs.
use tjs_core::{
    Heap, NativeCx, NativeError, NativeResult, ObjId, ObjRef, RestArgs, Trace, Value, value,
};
#[tjs_bind::class(name = "Rect")]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub v: [i32; 4],
    }
    impl Trace for State {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl State {
        fn other(&self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<Option<[i32; 4]>> {
            let Value::Obj(o) = v else {
                return Err(NativeError::This);
            };
            let Some(id) = o.object else {
                return Ok(None);
            };
            if id == cx.this() {
                return Ok(Some(self.v));
            }
            cx.heap_mut()
                .with_native_state::<Self, _>(id, |s| Some(s.v))
        }
        fn empty(v: [i32; 4]) -> bool {
            v[0] >= v[2] || v[1] >= v[3]
        }
        #[tjs::constructor]
        fn create(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<Self> {
            let mut s = Self::default();
            if args.len() == 1 {
                s.v = s.other(cx, args[0])?.unwrap_or_default();
            } else if args.len() == 4 {
                for (out, &input) in s.v.iter_mut().zip(args) {
                    *out = value::to_integer(cx.heap(), input)? as i32;
                }
            }
            Ok(s)
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method(name = "isEmpty")]
        fn is_empty(&self) -> bool {
            Self::empty(self.v)
        }
        #[tjs::method]
        fn clear(&mut self) {
            self.v = [0; 4];
        }
        #[tjs::method]
        fn set(
            &mut self,
            cx: &mut NativeCx<'_>,
            left: Value,
            top: Value,
            right: Value,
            bottom: Value,
        ) -> NativeResult<()> {
            let mut out = [0; 4];
            for (out, v) in out.iter_mut().zip([left, top, right, bottom]) {
                *out = value::to_integer(cx.heap(), v)? as i32;
            }
            self.v = out;
            Ok(())
        }
        #[tjs::method(name = "setSize")]
        fn set_size(
            &mut self,
            cx: &mut NativeCx<'_>,
            width: Value,
            height: Value,
        ) -> NativeResult<()> {
            self.v[2] = self.v[0].wrapping_add(value::to_integer(cx.heap(), width)? as i32);
            self.v[3] = self.v[1].wrapping_add(value::to_integer(cx.heap(), height)? as i32);
            Ok(())
        }
        #[tjs::method(name = "addOffset")]
        fn add_offset(&mut self, cx: &mut NativeCx<'_>, x: Value, y: Value) -> NativeResult<()> {
            let x = value::to_integer(cx.heap(), x)? as i32;
            let y = value::to_integer(cx.heap(), y)? as i32;
            self.v = [
                self.v[0].wrapping_add(x),
                self.v[1].wrapping_add(y),
                self.v[2].wrapping_add(x),
                self.v[3].wrapping_add(y),
            ];
            Ok(())
        }
        #[tjs::method(name = "setOffset")]
        fn set_offset(&mut self, cx: &mut NativeCx<'_>, x: Value, y: Value) -> NativeResult<()> {
            let x = (value::to_integer(cx.heap(), x)? as i32).wrapping_sub(self.v[0]);
            let y = (value::to_integer(cx.heap(), y)? as i32).wrapping_sub(self.v[1]);
            self.add_offset(cx, Value::Int(x.into()), Value::Int(y.into()))
        }
        #[tjs::method]
        fn clip(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<Value> {
            let Some(v) = self.other(cx, v)? else {
                return Ok(Value::Void);
            };
            let r = [
                self.v[0].max(v[0]),
                self.v[1].max(v[1]),
                self.v[2].min(v[2]),
                self.v[3].min(v[3]),
            ];
            if Self::empty(r) {
                return Ok(Value::Int(0));
            }
            self.v = r;
            Ok(Value::Int(1))
        }
        #[tjs::method(name = "union")]
        fn union_rect(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<Value> {
            let Some(v) = self.other(cx, v)? else {
                return Ok(Value::Void);
            };
            let old = self.v;
            let result = [
                old[0].min(v[0]),
                old[1].min(v[1]),
                old[2].max(v[2]),
                old[3].max(v[3]),
            ];
            if Self::empty(result) {
                return Ok(Value::Int(0));
            }
            self.v = result;
            Ok(Value::Int(1))
        }
        #[tjs::method]
        fn intersects(&self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<Value> {
            let Some(v) = self.other(cx, v)? else {
                return Ok(Value::Void);
            };
            Ok(Value::Int(i64::from(
                !Self::empty(self.v)
                    && !Self::empty(v)
                    && self.v[0] < v[2]
                    && self.v[2] > v[0]
                    && self.v[1] < v[3]
                    && self.v[3] > v[1],
            )))
        }
        #[tjs::method]
        fn included(&self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<Value> {
            let Some(v) = self.other(cx, v)? else {
                return Ok(Value::Void);
            };
            Ok(Value::Int(i64::from(
                !Self::empty(self.v)
                    && !Self::empty(v)
                    && self.v[0] >= v[0]
                    && self.v[1] >= v[1]
                    && self.v[2] <= v[2]
                    && self.v[3] <= v[3],
            )))
        }
        #[tjs::method(name = "includedPos")]
        fn included_pos(&self, cx: &mut NativeCx<'_>, x: Value, y: Value) -> NativeResult<bool> {
            let x = value::to_integer(cx.heap(), x)? as i32;
            let y = value::to_integer(cx.heap(), y)? as i32;
            Ok(self.v[0] <= x && self.v[1] <= y && self.v[2] > x && self.v[3] > y)
        }
        #[tjs::method]
        fn equal(&self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<Value> {
            Ok(self
                .other(cx, v)?
                .map_or(Value::Void, |v| Value::Int(i64::from(v == self.v))))
        }
        #[tjs::getter(name = "left")]
        fn get_left(&self) -> i64 {
            self.v[0].into()
        }
        #[tjs::setter(name = "left")]
        fn set_left(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            self.v[0] = value::to_integer(cx.heap(), v)? as i32;
            Ok(())
        }
        #[tjs::getter(name = "top")]
        fn get_top(&self) -> i64 {
            self.v[1].into()
        }
        #[tjs::setter(name = "top")]
        fn set_top(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            self.v[1] = value::to_integer(cx.heap(), v)? as i32;
            Ok(())
        }
        #[tjs::getter(name = "right")]
        fn get_right(&self) -> i64 {
            self.v[2].into()
        }
        #[tjs::setter(name = "right")]
        fn set_right(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            self.v[2] = value::to_integer(cx.heap(), v)? as i32;
            Ok(())
        }
        #[tjs::getter(name = "bottom")]
        fn get_bottom(&self) -> i64 {
            self.v[3].into()
        }
        #[tjs::setter(name = "bottom")]
        fn set_bottom(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            self.v[3] = value::to_integer(cx.heap(), v)? as i32;
            Ok(())
        }
        #[tjs::getter(name = "width")]
        fn get_width(&self) -> i64 {
            self.v[2].wrapping_sub(self.v[0]).into()
        }
        #[tjs::setter(name = "width")]
        fn set_width(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            self.v[2] = self.v[0].wrapping_add(value::to_integer(cx.heap(), v)? as i32);
            Ok(())
        }
        #[tjs::getter(name = "height")]
        fn get_height(&self) -> i64 {
            self.v[3].wrapping_sub(self.v[1]).into()
        }
        #[tjs::setter(name = "height")]
        fn set_height(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            self.v[3] = self.v[1].wrapping_add(value::to_integer(cx.heap(), v)? as i32);
            Ok(())
        }
    }
}
pub(crate) fn install(heap: &mut Heap) -> NativeResult<ObjId> {
    implementation::install(heap)
}
pub(crate) fn object(heap: &mut Heap, r: krkr_protocol::graphics::Rect) -> NativeResult<Value> {
    let class = heap.registered_class("Rect").expect("installed Rect");
    Ok(Value::Obj(ObjRef::bound(heap.alloc_native(
        class,
        implementation::State {
            v: [
                r.left,
                r.top,
                r.left.wrapping_add(r.width as i32),
                r.top.wrapping_add(r.height as i32),
            ],
        },
    )?)))
}
