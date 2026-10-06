use super::{coordinates, path::Path};
use tjs_core::{NativeCx, NativeResult, NativeStep, Trace, Value};
pub(super) fn snapshot(cx: &mut NativeCx<'_>, input: Value) -> NativeResult<Path> {
    cx.heap_mut()
        .with_native_state::<bindings::State, _>(crate::exports::object(input)?, |s| s.path.clone())
}

#[tjs_bind::class(name = "GdiPlus.Path")]
pub(super) mod bindings {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub(super) path: Path,
    }
    impl Trace for State {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method(name = "startFigure")]
        fn start(&mut self) {
            self.path.start_figure();
        }
        #[tjs::method(name = "closeFigure")]
        fn close(&mut self) {
            self.path.close_figure();
        }
        #[tjs::method(name = "drawLine")]
        fn line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64) {
            self.path.line([x1, y1], [x2, y2]);
        }
        #[tjs::method(name = "drawBezier")]
        #[allow(clippy::too_many_arguments)] // Original script signature.
        fn bezier(
            &mut self,
            x1: f64,
            y1: f64,
            x2: f64,
            y2: f64,
            x3: f64,
            y3: f64,
            x4: f64,
            y4: f64,
        ) {
            self.path.bezier([[x1, y1], [x2, y2], [x3, y3], [x4, y4]]);
        }
        #[tjs::method(name = "drawRectangle")]
        fn rectangle(&mut self, x: f64, y: f64, w: f64, h: f64) {
            self.path.rectangle([x, y, w, h]);
        }
        #[tjs::method(name = "drawEllipse")]
        fn ellipse(&mut self, x: f64, y: f64, w: f64, h: f64) -> NativeResult<()> {
            self.path.ellipse([x, y, w, h])
        }
        #[tjs::method(name = "drawArc")]
        fn arc(
            &mut self,
            x: f64,
            y: f64,
            w: f64,
            h: f64,
            start: f64,
            sweep: f64,
        ) -> NativeResult<()> {
            self.path.arc([x, y, w, h], start, sweep)
        }
        #[tjs::method(name = "drawPie")]
        fn pie(
            &mut self,
            x: f64,
            y: f64,
            w: f64,
            h: f64,
            start: f64,
            sweep: f64,
        ) -> NativeResult<()> {
            self.path.pie([x, y, w, h], start, sweep)
        }
        #[tjs::method(name = "drawLines", resumable = true)]
        fn lines(cx: &mut NativeCx<'_>, points: Value) -> NativeResult<NativeStep> {
            read(cx, points, Action::Lines)
        }
        #[tjs::method(name = "drawPolygon", resumable = true)]
        fn polygon(cx: &mut NativeCx<'_>, points: Value) -> NativeResult<NativeStep> {
            read(cx, points, Action::Polygon)
        }
        #[tjs::method(name = "drawBeziers", resumable = true)]
        fn beziers(cx: &mut NativeCx<'_>, points: Value) -> NativeResult<NativeStep> {
            read(cx, points, Action::Beziers)
        }
        #[tjs::method(name = "drawRectangles", resumable = true)]
        fn rectangles(cx: &mut NativeCx<'_>, points: Value) -> NativeResult<NativeStep> {
            read(cx, points, Action::Rectangles)
        }
        #[tjs::method(name = "drawClosedCurve", resumable = true)]
        fn closed(cx: &mut NativeCx<'_>, points: Value) -> NativeResult<NativeStep> {
            read(cx, points, Action::Closed(0.5))
        }
        #[tjs::method(name = "drawClosedCurve2", resumable = true)]
        fn closed2(cx: &mut NativeCx<'_>, points: Value, tension: f64) -> NativeResult<NativeStep> {
            read(cx, points, Action::Closed(tension))
        }
        #[tjs::method(name = "drawCurve", resumable = true)]
        fn curve(cx: &mut NativeCx<'_>, points: Value) -> NativeResult<NativeStep> {
            read(cx, points, Action::Curve(0, -1, 0.5))
        }
        #[tjs::method(name = "drawCurve2", resumable = true)]
        fn curve2(cx: &mut NativeCx<'_>, points: Value, tension: f64) -> NativeResult<NativeStep> {
            read(cx, points, Action::Curve(0, -1, tension))
        }
        #[tjs::method(name = "drawCurve3", resumable = true)]
        fn curve3(
            cx: &mut NativeCx<'_>,
            points: Value,
            #[tjs(coerce)] offset: i32,
            #[tjs(coerce)] count: i32,
            tension: f64,
        ) -> NativeResult<NativeStep> {
            read(cx, points, Action::Curve(offset, count, tension))
        }
    }
}
#[derive(tjs_bind::Trace)]
enum Action {
    Lines,
    Polygon,
    Beziers,
    Rectangles,
    Closed(f64),
    Curve(i32, i32, f64),
}
fn read(cx: &mut NativeCx<'_>, input: Value, action: Action) -> NativeResult<NativeStep> {
    let fields: &'static [&'static str] = if matches!(action, Action::Rectangles) {
        &["x", "y", "width", "height"]
    } else {
        &["x", "y"]
    };
    coordinates::read(
        cx,
        input,
        fields,
        (cx.this(), action),
        |(owner, action), cx, coordinates| {
            cx.heap_mut()
                .with_native_state::<bindings::State, _>(owner, |state| {
                    if matches!(action, Action::Rectangles) {
                        state.path.rectangles(
                            &coordinates
                                .iter()
                                .map(|v| [v[0], v[1], v[2], v[3]])
                                .collect::<Vec<_>>(),
                        );
                        return;
                    }
                    let points = coordinates.iter().map(|v| [v[0], v[1]]).collect::<Vec<_>>();
                    match action {
                        Action::Lines => state.path.lines(&points),
                        Action::Polygon => state.path.polygon(&points),
                        Action::Beziers => state.path.beziers(&points),
                        Action::Closed(tension) => state.path.closed_curve(&points, tension),
                        Action::Curve(offset, count, tension) => {
                            state.path.curve(&points, offset, count, tension)
                        }
                        Action::Rectangles => unreachable!(),
                    }
                })?;
            Ok(NativeStep::Return(Value::Void))
        },
    )
}
