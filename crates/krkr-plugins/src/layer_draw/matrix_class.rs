use super::{coordinates, matrix::Matrix};
use tjs_bind::RestArgs;
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, Trace, Value, value};
pub(super) fn native(cx: &mut NativeCx<'_>, input: Value) -> Option<Matrix> {
    let Value::Obj(reference) = input else {
        return None;
    };
    cx.heap_mut()
        .with_native_state::<bindings::State, _>(reference.object?, |s| s.matrix)
        .ok()
}

fn order(cx: &NativeCx<'_>, args: &[Value]) -> NativeResult<i32> {
    Ok(args
        .first()
        .map(|&v| value::to_integer(cx.heap(), v).map(|n| n as i32))
        .transpose()
        .map(|v| v.unwrap_or(0))?)
}
#[tjs_bind::class(name = "GdiPlus.Matrix")]
pub(super) mod bindings {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub(super) matrix: Matrix,
    }
    impl Trace for State {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl State {
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<Self> {
            if args.is_empty() {
                return Ok(Self::default());
            }
            if args.len() != 6 {
                return Err(NativeError::Message("Matrix expects zero or six arguments"));
            }
            let mut elements = [0.; 6];
            for (target, &arg) in elements.iter_mut().zip(args) {
                *target = value::to_real(cx.heap(), arg)? as f32;
            }
            Ok(Self {
                matrix: Matrix::new(elements),
            })
        }
        #[tjs::method(name = "OffsetX")]
        fn x(&self) -> f64 {
            self.matrix.offset[0]
        }
        #[tjs::method(name = "OffsetY")]
        fn y(&self) -> f64 {
            self.matrix.offset[1]
        }
        #[tjs::method(name = "GetLastStatus")]
        fn status(&self) -> i64 {
            0
        }
        #[tjs::method(name = "SetElements")]
        fn set(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> i64 {
            self.matrix = Matrix::new([a, b, c, d, e, f].map(|v| v as f32));
            0
        }
        #[tjs::method(name = "Invert")]
        fn invert(&mut self) -> i64 {
            if let Some(inverse) = self.matrix.inverse() {
                self.matrix = inverse;
                0
            } else {
                2
            }
        }
        #[tjs::method(name = "IsInvertible")]
        fn invertible(&self) -> bool {
            self.matrix.inverse().is_some()
        }
        #[tjs::method(name = "IsIdentity")]
        fn identity(&self) -> bool {
            self.matrix.equals(&Matrix::default())
        }
        #[tjs::method(name = "Reset")]
        fn reset(&mut self) {
            self.matrix = Matrix::default();
        }
        #[tjs::method(name = "Rotate")]
        fn rotate(
            &mut self,
            cx: &mut NativeCx<'_>,
            angle: f64,
            args: RestArgs<'_>,
        ) -> NativeResult<()> {
            self.matrix.rotate(angle, order(cx, args)?);
            Ok(())
        }
        #[tjs::method(name = "Scale")]
        fn scale(
            &mut self,
            cx: &mut NativeCx<'_>,
            x: f64,
            y: f64,
            args: RestArgs<'_>,
        ) -> NativeResult<()> {
            self.matrix.scale(x, y, order(cx, args)?);
            Ok(())
        }
        #[tjs::method(name = "Translate")]
        fn translate(
            &mut self,
            cx: &mut NativeCx<'_>,
            x: f64,
            y: f64,
            args: RestArgs<'_>,
        ) -> NativeResult<()> {
            self.matrix.translate(x, y, order(cx, args)?);
            Ok(())
        }
        #[tjs::method(name = "Shear")]
        fn shear(
            &mut self,
            cx: &mut NativeCx<'_>,
            x: f64,
            y: f64,
            args: RestArgs<'_>,
        ) -> NativeResult<()> {
            self.matrix.shear(x, y, order(cx, args)?);
            Ok(())
        }
        #[tjs::method(name = "Equals", resumable = true)]
        fn equals(cx: &mut NativeCx<'_>, other: Value) -> NativeResult<NativeStep> {
            convert(cx, other, Action::Equals)
        }
        #[tjs::method(name = "Multiply", resumable = true)]
        fn multiply(
            cx: &mut NativeCx<'_>,
            other: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            convert(cx, other, Action::Multiply(order(cx, args)?))
        }
        #[tjs::method(name = "RotateAt", resumable = true)]
        fn at(
            cx: &mut NativeCx<'_>,
            angle: f64,
            center: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let order = order(cx, args)?;
            coordinates::one(
                cx,
                center,
                &["x", "y"],
                (cx.this(), (angle, order)),
                |(owner, (angle, order)), cx, points| {
                    cx.heap_mut().with_native_state::<State, _>(owner, |s| {
                        s.matrix
                            .rotate_at(angle, [points[0][0], points[0][1]], order)
                    })?;
                    Ok(NativeStep::Return(Value::Void))
                },
            )
        }
    }
}
#[derive(tjs_bind::Trace)]
enum Action {
    Equals,
    Multiply(i32),
}
fn finish(
    cx: &mut NativeCx<'_>,
    owner: tjs_core::ObjId,
    action: Action,
    other: Matrix,
) -> NativeResult<NativeStep> {
    let value = cx
        .heap_mut()
        .with_native_state::<bindings::State, _>(owner, |s| match action {
            Action::Equals => Value::Int(i64::from(s.matrix.equals(&other))),
            Action::Multiply(order) => {
                s.matrix.multiply(&other, order);
                Value::Int(0)
            }
        })?;
    Ok(NativeStep::Return(value))
}
fn convert(cx: &mut NativeCx<'_>, input: Value, action: Action) -> NativeResult<NativeStep> {
    if let Value::Obj(reference) = input
        && let Some(id) = reference.object
    {
        if let Ok(matrix) = cx
            .heap_mut()
            .with_native_state::<bindings::State, _>(id, |s| s.matrix)
        {
            return finish(cx, cx.this(), action, matrix);
        }
        return coordinates::one(
            cx,
            input,
            &["m11", "m12", "m21", "m22", "dx", "dy"],
            (cx.this(), action),
            |(owner, action), cx, values| {
                let v = &values[0];
                finish(
                    cx,
                    owner,
                    action,
                    Matrix::new([v[0], v[1], v[2], v[3], v[4], v[5]].map(|v| v as f32)),
                )
            },
        );
    }
    Ok(NativeStep::Return(Value::Int(match action {
        Action::Multiply(_) => 2,
        Action::Equals => 0,
    })))
}
