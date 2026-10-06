use super::coordinates;
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, Value};
pub(super) fn native(cx: &mut NativeCx<'_>, input: Value, fields: &[&str]) -> Option<Vec<f64>> {
    let Value::Obj(reference) = input else {
        return None;
    };
    let id = reference.object?;
    if fields == ["x", "y"] {
        cx.heap_mut()
            .with_native_state::<point::State, _>(id, |s| s.values.to_vec())
            .ok()
    } else if fields == ["x", "y", "width", "height"] {
        cx.heap_mut()
            .with_native_state::<rect::State, _>(id, |s| s.values.to_vec())
            .ok()
    } else {
        None
    }
}
pub fn rectangle(cx: &mut NativeCx<'_>, rect: [f64; 4]) -> NativeResult<Value> {
    let class = cx
        .heap()
        .registered_class("GdiPlus.RectF")
        .ok_or(NativeError::Message("RectF class is not installed"))?;
    Ok(Value::Obj(
        cx.heap_mut()
            .alloc_native(class, rect::State { values: rect })?
            .into(),
    ))
}
fn point(cx: &mut NativeCx<'_>, values: [f64; 2]) -> NativeResult<Value> {
    let class = cx
        .heap()
        .registered_class("GdiPlus.PointF")
        .ok_or(NativeError::Message("PointF class is not installed"))?;
    Ok(Value::Obj(
        cx.heap_mut()
            .alloc_native(class, point::State { values })?
            .into(),
    ))
}
#[tjs_bind::class(name = "GdiPlus.PointF")]
pub(super) mod point {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        pub(super) values: [f64; 2],
    }
    impl State {
        #[tjs::constructor]
        fn new(x: f64, y: f64) -> Self {
            Self { values: [x, y] }
        }
        #[tjs::getter(name = "x")]
        fn get_x(&self) -> f64 {
            self.values[0]
        }
        #[tjs::setter(name = "x")]
        fn set_x(&mut self, v: f64) {
            self.values[0] = v;
        }
        #[tjs::getter(name = "y")]
        fn get_y(&self) -> f64 {
            self.values[1]
        }
        #[tjs::setter(name = "y")]
        fn set_y(&mut self, v: f64) {
            self.values[1] = v;
        }
        #[tjs::method(name = "Equals", resumable = true)]
        fn equals(cx: &mut NativeCx<'_>, other: Value) -> NativeResult<NativeStep> {
            coordinates::one(cx, other, &["x", "y"], cx.this(), |owner, cx, p| {
                let same = cx
                    .heap_mut()
                    .with_native_state::<State, _>(owner, |s| s.values == [p[0][0], p[0][1]])?;
                Ok(NativeStep::Return(Value::Int(i64::from(same))))
            })
        }
    }
}
#[tjs_bind::class(name = "GdiPlus.RectF")]
pub(super) mod rect {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        pub(super) values: [f64; 4],
    }
    impl State {
        #[tjs::constructor]
        fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
            Self {
                values: [x, y, width, height],
            }
        }
        #[tjs::getter(name = "x")]
        fn get_x(&self) -> f64 {
            self.values[0]
        }
        #[tjs::setter(name = "x")]
        fn set_x(&mut self, v: f64) {
            self.values[0] = v;
        }
        #[tjs::getter(name = "y")]
        fn get_y(&self) -> f64 {
            self.values[1]
        }
        #[tjs::setter(name = "y")]
        fn set_y(&mut self, v: f64) {
            self.values[1] = v;
        }
        #[tjs::getter(name = "width")]
        fn get_width(&self) -> f64 {
            self.values[2]
        }
        #[tjs::setter(name = "width")]
        fn set_width(&mut self, v: f64) {
            self.values[2] = v;
        }
        #[tjs::getter(name = "height")]
        fn get_height(&self) -> f64 {
            self.values[3]
        }
        #[tjs::setter(name = "height")]
        fn set_height(&mut self, v: f64) {
            self.values[3] = v;
        }
        #[tjs::getter(name = "left")]
        fn left(&self) -> f64 {
            self.values[0]
        }
        #[tjs::getter(name = "top")]
        fn top(&self) -> f64 {
            self.values[1]
        }
        #[tjs::getter(name = "right")]
        fn right(&self) -> f64 {
            self.values[0] + self.values[2]
        }
        #[tjs::getter(name = "bottom")]
        fn bottom(&self) -> f64 {
            self.values[1] + self.values[3]
        }
        #[tjs::getter]
        fn location(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            point(cx, [self.values[0], self.values[1]])
        }
        #[tjs::getter]
        fn bounds(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let [x, y, w, h] = self.values;
            rectangle(cx, [x, y, x + w, y + h])
        }
        #[tjs::method(name = "Clone")]
        fn duplicate(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            rectangle(cx, self.values)
        }
        #[tjs::method(name = "Inflate")]
        fn inflate(&mut self, w: f64, h: f64) {
            self.values[0] -= w;
            self.values[1] -= h;
            self.values[2] += 2. * w;
            self.values[3] += 2. * h;
        }
        #[tjs::method(name = "Offset")]
        fn offset(&mut self, x: f64, y: f64) {
            self.values[0] += x;
            self.values[1] += y;
        }
        #[tjs::method(name = "IsEmptyArea")]
        fn empty(&self) -> bool {
            self.values[2] <= 0. || self.values[3] <= 0.
        }
        #[tjs::method(name = "InflatePoint", resumable = true)]
        fn inflate_point(cx: &mut NativeCx<'_>, other: Value) -> NativeResult<NativeStep> {
            coordinates::one(cx, other, &["x", "y"], cx.this(), |owner, cx, p| {
                cx.heap_mut()
                    .with_native_state::<State, _>(owner, |s| s.inflate(p[0][0], p[0][1]))?;
                Ok(NativeStep::Return(Value::Void))
            })
        }
        #[tjs::method(name = "Equals", resumable = true)]
        fn equals(cx: &mut NativeCx<'_>, other: Value) -> NativeResult<NativeStep> {
            compare(cx, other, false)
        }
        #[tjs::method(name = "IntersectsWith", resumable = true)]
        fn intersects(cx: &mut NativeCx<'_>, other: Value) -> NativeResult<NativeStep> {
            compare(cx, other, true)
        }
        #[tjs::method(name = "Union", resumable = true)]
        fn union(
            cx: &mut NativeCx<'_>,
            output: Value,
            a: Value,
            b: Value,
        ) -> NativeResult<NativeStep> {
            // ncbind's RectFConvertor copies each argument into its own dst.
            // Union writes that temporary, with no script-visible copy-back.
            coordinates::one(
                cx,
                output,
                &["x", "y", "width", "height"],
                (a, b),
                |(a, b), cx, _| {
                    coordinates::one(cx, a, &["x", "y", "width", "height"], b, |b, cx, _| {
                        coordinates::one(cx, b, &["x", "y", "width", "height"], (), |(), _, _| {
                            Ok(NativeStep::Return(Value::Int(1)))
                        })
                    })
                },
            )
        }
    }
    fn compare(cx: &mut NativeCx<'_>, other: Value, intersect: bool) -> NativeResult<NativeStep> {
        coordinates::one(
            cx,
            other,
            &["x", "y", "width", "height"],
            (cx.this(), intersect),
            |(owner, intersect), cx, p| {
                let b = &p[0];
                let same = cx.heap_mut().with_native_state::<State, _>(owner, |s| {
                    let [x, y, w, h] = s.values;
                    if intersect {
                        !(b[0] > x + w || b[0] + b[2] < x || b[1] > y + h || b[1] + b[3] < y)
                    } else {
                        s.values == [b[0], b[1], b[2], b[3]]
                    }
                })?;
                Ok(NativeStep::Return(Value::Int(i64::from(same))))
            },
        )
    }
}
