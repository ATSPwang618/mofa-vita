use super::{
    coordinates,
    matrix::Matrix,
    properties::get,
    raster::{self, Brush, Texture},
};
use krkr_engine::{extensions, protocol::budget::Budget, storages};
use std::sync::Arc;
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, Trace, Value, value};

pub trait Reply: Trace {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, brush: Brush) -> NativeResult<NativeStep>;
}
#[derive(tjs_bind::Trace)]
struct Read {
    input: Value,
    next: Box<dyn Reply>,
    missing: tjs_core::ObjId,
}
fn number(cx: &NativeCx<'_>, v: Value) -> NativeResult<u32> {
    Ok(value::to_integer(cx.heap(), v)? as u32)
}
fn budget(cx: &mut NativeCx<'_>) -> NativeResult<Budget> {
    Ok(extensions::image_staging_budget(cx.heap_mut())?
        .unwrap_or_else(|| Budget::new(64 * 1024 * 1024)))
}
pub fn read(cx: &mut NativeCx<'_>, input: Value, next: Box<dyn Reply>) -> NativeResult<NativeStep> {
    if !matches!(input, Value::Obj(_)) {
        return next.resume(cx, Brush::Solid(number(cx, input)?));
    }
    let missing = cx.heap_mut().alloc_dictionary();
    get(
        cx,
        input,
        "type",
        Value::Int(0),
        Read {
            input,
            next,
            missing,
        },
        |s, cx, v| match number(cx, v)? as i32 {
            0 => get(cx, s.input, "color", Value::Int(-1), s, |s, cx, v| {
                s.next.resume(cx, Brush::Solid(number(cx, v)?))
            }),
            1 => get(cx, s.input, "hatchStyle", Value::Int(0), s, |s, cx, v| {
                let style = number(cx, v)? as i32;
                get(
                    cx,
                    s.input,
                    "foreColor",
                    Value::Int(-1),
                    (s, style),
                    |(s, style), cx, v| {
                        let foreground = number(cx, v)?;
                        get(
                            cx,
                            s.input,
                            "backColor",
                            Value::Int(0xff000000),
                            ((s, style), foreground),
                            |((s, style), foreground), cx, v| {
                                let background = number(cx, v)?;
                                let texture =
                                    raster::hatch(style, foreground, background, &budget(cx)?)
                                        .map_err(NativeError::Message)?;
                                s.next.resume(
                                    cx,
                                    Brush::Texture {
                                        image: Arc::new(texture),
                                        matrix: Matrix::default(),
                                        plain: false,
                                    },
                                )
                            },
                        )
                    },
                )
            }),
            2 => get(cx, s.input, "image", Value::Void, s, |s, cx, v| {
                let name = value::to_string_units(cx.heap(), v)?;
                storages::managed::plans(cx, vec![(name, false)], s, |s, cx, mut plans| {
                    let Some(plan) = plans.pop().flatten() else {
                        return s.next.resume(cx, Brush::Solid(0xff000000));
                    };
                    let request = krkr_image::Request::from_plans(
                        plan,
                        None,
                        None,
                        0x02ffffff,
                        None,
                        false,
                        budget(cx)?,
                    );
                    extensions::run_work(
                        cx,
                        move |stop| {
                            let mut pixels = request
                                .probe(stop)
                                .map_err(|e| NativeError::Detail(e.to_string()))?
                                .decode(stop)
                                .map_err(|e| NativeError::Detail(e.to_string()))?
                                .pixels;
                            Texture::from_rgba(
                                pixels
                                    .main
                                    .take()
                                    .ok_or(NativeError::Message("texture has no color plane"))?,
                                pixels.size,
                            )
                            .ok_or(NativeError::Message(
                                "texture dimensions do not match pixels",
                            ))
                        },
                        Box::new(TextureReady(s)),
                    )
                })
            }),
            3 => get(cx, s.input, "points", Value::Void, s, |s, cx, v| {
                coordinates::read(cx, v, &["x", "y"], s, |s, cx, points| {
                    let Some(first) = points.first() else {
                        return Err(NativeError::Message("must set points"));
                    };
                    let mut bounds = [first[0], first[1], first[0], first[1]];
                    for p in &points[1..] {
                        bounds[0] = bounds[0].min(p[0]);
                        bounds[1] = bounds[1].min(p[1]);
                        bounds[2] = bounds[2].max(p[0]);
                        bounds[3] = bounds[3].max(p[1]);
                    }
                    let data = Radial {
                        read: s,
                        center: [
                            ((bounds[0] + bounds[2]) / 2.) as f32,
                            ((bounds[1] + bounds[3]) / 2.) as f32,
                        ],
                        radius: ((bounds[2] - bounds[0]).max(bounds[3] - bounds[1]) / 2.) as f32,
                        stops: Vec::new(),
                    };
                    get(
                        cx,
                        data.read.input,
                        "centerColor",
                        Value::Obj(data.read.missing.into()),
                        data,
                        |mut s, cx, v| {
                            if s.read.present(v) {
                                s.stops.push((0., number(cx, v)?));
                            }
                            get(
                                cx,
                                s.read.input,
                                "surroundColors",
                                Value::Obj(s.read.missing.into()),
                                s,
                                |s, cx, v| {
                                    if !s.read.present(v) {
                                        return s.finish(cx);
                                    }
                                    get(
                                        cx,
                                        v,
                                        "count",
                                        Value::Int(0),
                                        (s, v),
                                        |(s, colors), cx, v| {
                                            let count = value::to_integer(cx.heap(), v)? as i32;
                                            Colors {
                                                radial: s,
                                                colors,
                                                count,
                                                index: 0,
                                            }
                                            .advance(cx)
                                        },
                                    )
                                },
                            )
                        },
                    )
                })
            }),
            4 => get(
                cx,
                s.input,
                "point1",
                Value::Obj(s.missing.into()),
                s,
                |s, cx, v| {
                    if s.present(v) {
                        coordinates::one(cx, v, &["x", "y"], (s, v), |(s, first), cx, p| {
                            let p1 = [p[0][0] as f32, p[0][1] as f32];
                            get(cx, s.input, "point2", first, (s, p1), |(s, p1), cx, v| {
                                coordinates::one(cx, v, &["x", "y"], (s, p1), |(s, p1), cx, p| {
                                    linear_colors(cx, s, p1, [p[0][0] as f32, p[0][1] as f32])
                                })
                            })
                        })
                    } else {
                        get(
                            cx,
                            s.input,
                            "rect",
                            Value::Obj(s.missing.into()),
                            s,
                            |s, cx, v| {
                                if !s.present(v) {
                                    return Err(NativeError::Message("must set point1,2 or rect"));
                                }
                                coordinates::one(
                                    cx,
                                    v,
                                    &["x", "y", "width", "height"],
                                    s,
                                    |s, cx, p| {
                                        get(
                                            cx,
                                            s.input,
                                            "angle",
                                            Value::Real(0.),
                                            (s, p),
                                            |(s, p), cx, v| {
                                                let angle = value::to_real(cx.heap(), v)? as f32;
                                                let r = &p[0];
                                                let center = [
                                                    (r[0] + r[2] / 2.) as f32,
                                                    (r[1] + r[3] / 2.) as f32,
                                                ];
                                                let length = ((r[2] * r[2] + r[3] * r[3]) as f32)
                                                    .sqrt()
                                                    / 2.;
                                                let rad = angle * std::f32::consts::PI / 180.;
                                                let (sn, cs) = rad.sin_cos();
                                                linear_colors(
                                                    cx,
                                                    s,
                                                    [
                                                        center[0] - cs * length,
                                                        center[1] - sn * length,
                                                    ],
                                                    [
                                                        center[0] + cs * length,
                                                        center[1] + sn * length,
                                                    ],
                                                )
                                            },
                                        )
                                    },
                                )
                            },
                        )
                    }
                },
            ),
            _ => Err(NativeError::Message("invalid brush type")),
        },
    )
}
impl Read {
    fn present(&self, v: Value) -> bool {
        !matches!(v,Value::Obj(r) if r.object==Some(self.missing))
    }
}
fn linear_colors(
    cx: &mut NativeCx<'_>,
    s: Read,
    start: [f32; 2],
    end: [f32; 2],
) -> NativeResult<NativeStep> {
    get(
        cx,
        s.input,
        "color1",
        Value::Int(0),
        (s, (start, end)),
        |(s, (start, end)), cx, v| {
            let first = number(cx, v)?;
            get(
                cx,
                s.input,
                "color2",
                Value::Int(0),
                ((s, (start, end)), first),
                |((s, (start, end)), first), cx, v| {
                    s.next.resume(
                        cx,
                        Brush::Linear {
                            start,
                            end,
                            colors: [first, number(cx, v)?],
                        },
                    )
                },
            )
        },
    )
}
#[derive(tjs_bind::Trace)]
struct Radial {
    read: Read,
    center: [f32; 2],
    radius: f32,
    stops: Vec<(f32, u32)>,
}
impl Radial {
    fn finish(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        self.read.next.resume(
            cx,
            Brush::Radial {
                center: self.center,
                radius: self.radius,
                stops: self.stops,
            },
        )
    }
}
#[derive(tjs_bind::Trace)]
struct Colors {
    radial: Radial,
    colors: Value,
    count: i32,
    index: i32,
}
impl Colors {
    fn advance(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index >= self.count {
            return self.radial.finish(cx);
        }
        if self.count > 1_000_000 {
            return Err(NativeError::Message("gradient color count exceeds budget"));
        }
        Ok(super::properties::index(
            self.colors,
            i64::from(self.index),
            Value::Int(0),
            self,
            |mut s, cx, v| {
                let color = number(cx, v)?;
                if s.index == 0 {
                    s.radial.stops.push((1., color));
                }
                s.index += 1;
                s.advance(cx)
            },
        ))
    }
}
#[derive(tjs_bind::Trace)]
struct TextureReady(Read);
impl extensions::WorkContinuation<Texture> for TextureReady {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        texture: Texture,
    ) -> NativeResult<NativeStep> {
        let s = self.0;
        get(
            cx,
            s.input,
            "wrapMode",
            Value::Int(0),
            TextureOptions {
                read: s,
                texture: Arc::new(texture),
                plain: false,
            },
            |mut s, cx, v| {
                s.plain = number(cx, v)? == 4;
                get(
                    cx,
                    s.read.input,
                    "dstRect",
                    Value::Obj(s.read.missing.into()),
                    s,
                    |s, cx, v| {
                        if !s.read.present(v) {
                            return s.finish(cx, Matrix::default());
                        }
                        coordinates::one(cx, v, &["x", "y", "width", "height"], s, |s, cx, p| {
                            let r = &p[0];
                            let mut matrix = Matrix::default();
                            matrix.scale(
                                r[2] / f64::from(s.texture.size.width),
                                r[3] / f64::from(s.texture.size.height),
                                0,
                            );
                            matrix.translate(r[0], r[1], 0);
                            s.finish(cx, matrix)
                        })
                    },
                )
            },
        )
    }
}
struct TextureOptions {
    read: Read,
    texture: Arc<Texture>,
    plain: bool,
}
impl Trace for TextureOptions {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.read.trace(visit);
    }
}
impl TextureOptions {
    fn finish(self, cx: &mut NativeCx<'_>, matrix: Matrix) -> NativeResult<NativeStep> {
        self.read.next.resume(
            cx,
            Brush::Texture {
                image: self.texture,
                matrix,
                plain: self.plain,
            },
        )
    }
}
