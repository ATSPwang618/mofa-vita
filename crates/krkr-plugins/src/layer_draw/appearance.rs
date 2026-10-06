use super::{
    brush,
    path::Path,
    properties::{get, index},
    raster::{Brush, Draw, Drawing, Pen},
};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value, value};
pub(super) fn snapshot(cx: &mut NativeCx<'_>, input: Value) -> NativeResult<Vec<Draw>> {
    cx.heap_mut()
        .with_native_state::<bindings::State, _>(crate::exports::object(input)?, |s| {
            s.draws.clone()
        })
}
#[tjs_bind::class(name = "GdiPlus.Appearance")]
pub(super) mod bindings {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub(super) draws: Vec<Draw>,
    }
    impl Trace for State {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method]
        fn clear(&mut self) {
            self.draws.clear();
        }
        #[tjs::method(name = "addBrush", resumable = true)]
        fn add_brush(
            cx: &mut NativeCx<'_>,
            brush: Value,
            #[tjs(default = 0.0)] x: f64,
            #[tjs(default = 0.0)] y: f64,
        ) -> NativeResult<NativeStep> {
            brush::read(
                cx,
                brush,
                Box::new(Append {
                    owner: cx.this(),
                    offset: [x as f32, y as f32],
                    width: None,
                }),
            )
        }
        #[tjs::method(name = "addPen", resumable = true)]
        fn add_pen(
            cx: &mut NativeCx<'_>,
            brush: Value,
            width: Value,
            #[tjs(default = 0.0)] x: f64,
            #[tjs(default = 0.0)] y: f64,
        ) -> NativeResult<NativeStep> {
            brush::read(
                cx,
                brush,
                Box::new(Append {
                    owner: cx.this(),
                    offset: [x as f32, y as f32],
                    width: Some(width),
                }),
            )
        }
    }
}
#[derive(tjs_bind::Trace)]
struct Append {
    owner: ObjId,
    offset: [f32; 2],
    width: Option<Value>,
}
impl Append {
    fn finish(self, cx: &mut NativeCx<'_>, drawing: Drawing) -> NativeResult<NativeStep> {
        cx.heap_mut()
            .with_native_state::<bindings::State, _>(self.owner, |s| {
                s.draws.push(Draw {
                    offset: self.offset,
                    drawing,
                });
            })?;
        Ok(NativeStep::Return(Value::Void))
    }
}
impl brush::Reply for Append {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, brush: Brush) -> NativeResult<NativeStep> {
        let Some(options) = self.width else {
            return self.finish(cx, Drawing::Fill(brush));
        };
        let mut pen = Pen::new(brush);
        if !matches!(options, Value::Obj(_)) {
            pen.width = value::to_real(cx.heap(), options)? as f32;
            return self.finish(cx, Drawing::Stroke(pen));
        }
        let missing = cx.heap_mut().alloc_dictionary();
        PenRead {
            append: *self,
            pen,
            options,
            missing,
            phase: 0,
        }
        .advance(cx)
    }
}
struct PenRead {
    append: Append,
    pen: Pen,
    options: Value,
    missing: ObjId,
    phase: usize,
}
impl Trace for PenRead {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.append.trace(v);
        self.options.trace(v);
        self.missing.trace(v);
    }
}
impl PenRead {
    fn advance(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let names = [
            "width",
            "dashOffset",
            "dashStyle",
            "startCap",
            "endCap",
            "lineJoin",
            "miterLimit",
        ];
        let Some(name) = names.get(self.phase) else {
            return self.append.finish(cx, Drawing::Stroke(self.pen));
        };
        get(
            cx,
            self.options,
            name,
            Value::Obj(self.missing.into()),
            self,
            |mut s, cx, v| {
                let phase = s.phase;
                s.phase += 1;
                if matches!(v,Value::Obj(r) if r.object==Some(s.missing)) {
                    return s.advance(cx);
                }
                match phase {
                    0 => s.pen.width = value::to_real(cx.heap(), v)? as f32,
                    1 => s.pen.dash_offset = value::to_real(cx.heap(), v)? as f32,
                    2 => {
                        if matches!(v,Value::Obj(r) if r.object.is_some_and(|id| cx.heap().array(id).is_ok()))
                        {
                            return get(
                                cx,
                                v,
                                "count",
                                Value::Int(0),
                                (s, v),
                                |(s, array), cx, v| {
                                    let count = value::to_integer(cx.heap(), v)? as i32;
                                    if count > 1_000_000 {
                                        return Err(NativeError::Message(
                                            "dash pattern exceeds budget",
                                        ));
                                    }
                                    Dashes {
                                        read: s,
                                        array,
                                        count,
                                        index: 0,
                                    }
                                    .advance(cx)
                                },
                            );
                        }
                        s.pen.dashes = match value::to_integer(cx.heap(), v)? as i32 {
                            1 => vec![5., 3.],
                            2 => vec![1., 3.],
                            3 => vec![5., 3., 1., 3.],
                            4 => vec![5., 3., 1., 3., 1., 3.],
                            _ => Vec::new(),
                        };
                    }
                    3 | 4 => match v {
                        Value::Int(_) | Value::Void => {
                            s.pen.cap = match value::to_integer(cx.heap(), v)? as i32 {
                                1 => tiny_skia::LineCap::Square,
                                2 => tiny_skia::LineCap::Round,
                                _ => tiny_skia::LineCap::Butt,
                            }
                        }
                        Value::Obj(_) => {
                            return get(
                                cx,
                                v,
                                "width",
                                Value::Real(1.),
                                (s, v),
                                |(s, object), cx, v| {
                                    let width =
                                        value::to_real(cx.heap(), v)? * f64::from(s.pen.width);
                                    get(
                                        cx,
                                        object,
                                        "height",
                                        Value::Real(1.),
                                        ((s, object), width),
                                        |((s, object), width), cx, v| {
                                            let height = value::to_real(cx.heap(), v)?
                                                * f64::from(s.pen.width);
                                            get(
                                                cx,
                                                object,
                                                "filled",
                                                Value::Int(1),
                                                (s, (width, height)),
                                                |(mut s, (width, height)), cx, v| {
                                                    let mut path = Path::default();
                                                    path.lines(&[
                                                        [-width / 2., -height],
                                                        [0., 0.],
                                                        [width / 2., -height],
                                                    ]);
                                                    if value::to_integer(cx.heap(), v)? != 0 {
                                                        path.close_figure();
                                                    }
                                                    if s.phase == 4 {
                                                        s.pen.start_cap = Some(path);
                                                    } else {
                                                        s.pen.end_cap = Some(path);
                                                    }
                                                    s.advance(cx)
                                                },
                                            )
                                        },
                                    )
                                },
                            );
                        }
                        _ => {}
                    },
                    5 => {
                        s.pen.join = match value::to_integer(cx.heap(), v)? as i32 {
                            1 => tiny_skia::LineJoin::Bevel,
                            2 => tiny_skia::LineJoin::Round,
                            _ => tiny_skia::LineJoin::Miter,
                        }
                    }
                    6 => s.pen.miter = value::to_real(cx.heap(), v)? as f32,
                    _ => unreachable!(),
                }
                s.advance(cx)
            },
        )
    }
}
struct Dashes {
    read: PenRead,
    array: Value,
    count: i32,
    index: i32,
}
impl Trace for Dashes {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.read.trace(v);
        self.array.trace(v);
    }
}
impl Dashes {
    fn advance(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index >= self.count {
            return self.read.advance(cx);
        }
        Ok(index(
            self.array,
            i64::from(self.index),
            Value::Int(0),
            self,
            |mut s, cx, v| {
                s.read.pen.dashes.push(value::to_real(cx.heap(), v)? as f32);
                s.index += 1;
                s.advance(cx)
            },
        ))
    }
}
