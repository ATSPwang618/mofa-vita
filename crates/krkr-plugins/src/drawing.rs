use crate::exports::{Exports, arg, class};
use krkr_engine::{
    extensions,
    plugins::Context,
    protocol::graphics::{Adjustment, Rect, Size},
};
use tjs_core::{
    NativeCallable, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjRef,
    Trace, Value, value,
};

pub(crate) fn install(cx: &mut Context<'_>, exports: &mut Exports) -> NativeResult<()> {
    let layer = class(cx, "Layer")?;
    for (name, call) in [
        (
            "fillHSV",
            fill_hsv as fn(&mut NativeCx<'_>, &[Value]) -> NativeResult<NativeStep>,
        ),
        ("fillRGB", fill_rgb),
        ("copyWrappedRect", wrapped_copy),
        ("fillGradientRectLR", fill_lr),
        ("fillGradientRectUD", fill_ud),
        ("colorGradientRectLR", color_lr),
        ("colorGradientRectUD", color_ud),
    ] {
        exports.function(cx, layer, name, NativeCallable::Resumable(call))?;
    }
    exports.function(cx, layer, "RGB2HSV", NativeCallable::Leaf(rgb_to_hsv))?;
    exports.function(cx, layer, "HSV2RGB", NativeCallable::Leaf(hsv_to_rgb))
}
fn fill_hsv(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    field(cx, args, true)
}
fn fill_rgb(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    field(cx, args, false)
}
fn fill_lr(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    gradient(cx, args, false, false)
}
fn fill_ud(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    gradient(cx, args, true, false)
}
fn color_lr(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    gradient(cx, args, false, true)
}
fn color_ud(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    gradient(cx, args, true, true)
}
fn field(cx: &mut NativeCx<'_>, args: &[Value], hsv: bool) -> NativeResult<NativeStep> {
    if args.len() < 3 {
        return Err(NativeError::Missing(2));
    }
    let mut axes = [0; 3];
    let mut values = [0.; 3];
    for i in 0..3 {
        // LayerSupport accepts ttstr, then calls AsInteger on that string.
        // Objects stringify too; this is not a numeric Variant cast. Octets
        // retain the language's string-conversion error.
        let string = value::to_string(cx.heap_mut(), args[i])?;
        let Value::Str(id) = string else {
            unreachable!()
        };
        let text = tjs_core::string::c_string(cx.heap().string(id)?);
        axes[i] = if text == [120] {
            1
        } else if text == [121] {
            2
        } else {
            0
        };
        if axes[i] == 0 {
            let n = value::to_integer(cx.heap(), string)?;
            values[i] = if hsv {
                n as f64
            } else {
                (n as i32 & 255) as f64
            };
        }
    }
    Read::start(cx, Operation::Field { hsv, axes, values })
}
fn gradient(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    vertical: bool,
    blend: bool,
) -> NativeResult<NativeStep> {
    if args.len() < 6 {
        return Err(NativeError::Missing(5));
    }
    let mut p = [0; 6];
    for (v, arg) in p.iter_mut().zip(args) {
        *v = value::to_integer(cx.heap(), *arg)? as i32;
    }
    if p[4] == p[5] {
        // This dispatch precedes width/clip/image access and uses the layer's
        // current face, including province/mask and script overrides.
        let mut arguments = p[..4]
            .iter()
            .map(|&v| Value::Int(v as i64))
            .collect::<Vec<_>>();
        arguments.push(Value::Int(if blend {
            (p[4] as u32 & 0xffffff) as i64
        } else {
            p[4] as i64
        }));
        if blend {
            arguments.push(Value::Int((p[4] as u32 >> 24) as i64));
        }
        let layer = Value::Obj(ObjRef::bound(cx.this()));
        return Ok(call(
            cx,
            layer,
            if blend { "colorRect" } else { "fillRect" },
            arguments,
        ));
    }
    Read::start(
        cx,
        Operation::Gradient {
            bounds: rectangle(&p),
            from: p[4] as u32,
            to: p[5] as u32,
            vertical,
            blend,
        },
    )
}
fn wrapped_copy(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    if args.len() < 11 {
        return Err(NativeError::Missing(10));
    }
    let mut arguments = args[..11].to_vec();
    for (i, v) in arguments.iter_mut().enumerate() {
        if i != 4 {
            *v = Value::Int(value::to_integer(cx.heap(), *v)? as i32 as i64);
        }
    }
    Read::start(cx, Operation::Wrapped { arguments })
}
fn rectangle(p: &[i32]) -> Rect {
    Rect {
        left: p[0],
        top: p[1],
        width: p[2].max(0) as u32,
        height: p[3].max(0) as u32,
    }
}
enum Operation {
    Field {
        hsv: bool,
        axes: [i32; 3],
        values: [f64; 3],
    },
    Gradient {
        bounds: Rect,
        from: u32,
        to: u32,
        vertical: bool,
        blend: bool,
    },
    Wrapped {
        arguments: Vec<Value>,
    },
}
impl Trace for Operation {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Self::Wrapped { arguments } = self {
            arguments.trace(visit);
        }
    }
}
struct Read {
    layer: Value,
    operation: Operation,
    index: usize,
    values: [i32; 6],
}
impl Trace for Read {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.layer.trace(visit);
        self.operation.trace(visit);
    }
}
impl Read {
    fn start(cx: &mut NativeCx<'_>, operation: Operation) -> NativeResult<NativeStep> {
        Self {
            layer: Value::Obj(ObjRef::bound(cx.this())),
            operation,
            index: 0,
            values: [0; 6],
        }
        .next(cx)
    }
    fn next(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index == 2 && !matches!(self.operation, Operation::Wrapped { .. }) {
            extensions::layer_prepare_draw(cx, self.layer)?;
        }
        let names: &[&str] = match self.operation {
            Operation::Field { .. } => &["width", "height"],
            Operation::Gradient { .. } => &[
                "width",
                "height",
                "clipLeft",
                "clipTop",
                "clipWidth",
                "clipHeight",
            ],
            // Preserve the repeated reads in LayerSupport::copyWrappedRect.
            Operation::Wrapped { .. } => &[
                "clipLeft",
                "clipTop",
                "clipLeft",
                "clipWidth",
                "clipTop",
                "clipHeight",
            ],
        };
        if let Some(name) = names.get(self.index) {
            return Ok(NativeStep::GetOr {
                object: self.layer,
                key: key(cx, name),
                raw: false,
                fallback: Value::Void,
                continuation: Box::new(self),
            });
        }
        let step = match self.operation {
            Operation::Field {
                mut hsv,
                axes,
                mut values,
            } => {
                if self.values[0] < 0 || self.values[1] < 0 {
                    return Err(NativeError::Message(
                        "color field dimensions cannot be negative",
                    ));
                }
                let size = Size {
                    width: self.values[0] as u32,
                    height: self.values[1] as u32,
                };
                if size.width > 0 && size.height > 0 {
                    // The C++ loops divide by zero on a mapped singleton axis.
                    // Reject before issuing any GPU write, instead of inventing a color.
                    let singleton = axes.iter().enumerate().any(|(channel, &axis)| {
                        let unused_hue = hsv && channel == 0 && axes[1] == 0 && values[1] == 0.;
                        !unused_hue
                            && (axis == 1 && size.width == 1 || axis == 2 && size.height == 1)
                    });
                    if singleton {
                        return Err(NativeError::Message(
                            "mapped color field axis requires at least two pixels",
                        ));
                    }
                    if hsv {
                        // Check all corners before writing; out-of-range C++ float
                        // casts and uninitialized negative sectors have no portable result.
                        for corner in 0..8 {
                            let sample = std::array::from_fn(|i| {
                                if axes[i] == 0 {
                                    values[i]
                                } else if corner & (1 << i) == 0 {
                                    0.
                                } else if i == 0 {
                                    360.
                                } else {
                                    100.
                                }
                            });
                            hsv_rgb(sample)?;
                        }
                        if axes == [0; 3] {
                            // Constant fields can retain the reference's double precision.
                            values = hsv_rgb(values)?.map(|v| (v & 255) as f64);
                            hsv = false;
                        } else if axes[0] == 0 && !(axes[1] == 0 && values[1] == 0.) {
                            // Reduce a constant hue in double precision before
                            // upload, including C++'s unusual negative sector 0.
                            // Large positive cycles otherwise lose their sector
                            // when narrowed to the GPU's f32 arithmetic.
                            let h = values[0];
                            let sector = checked_integer(h.floor())? / 60 % 6;
                            values[0] = sector as f64 * 60. + (h / 60. - (h / 60.).floor()) * 60.;
                        }
                    }
                }
                extensions::layer_adjust(
                    cx,
                    self.layer,
                    size.rect(),
                    Adjustment::ColorField {
                        size,
                        hsv,
                        axes,
                        values,
                    },
                    false,
                )?
            }
            Operation::Gradient {
                bounds,
                from,
                to,
                vertical,
                blend,
            } => {
                let Some(rect) = bounds.intersection(rectangle(&self.values[2..])) else {
                    return Ok(NativeStep::Return(Value::Void));
                };
                if if vertical {
                    bounds.height
                } else {
                    bounds.width
                } == 1
                {
                    return Err(NativeError::Message(
                        "gradient axis requires at least two pixels",
                    ));
                }
                extensions::layer_adjust(
                    cx,
                    self.layer,
                    rect,
                    Adjustment::Gradient {
                        bounds,
                        from,
                        to,
                        vertical,
                        blend,
                    },
                    false,
                )?
            }
            Operation::Wrapped { arguments } => {
                extensions::layer_prepare_draw(cx, self.layer)?;
                let left = i64::from(self.values[0]);
                let top = i64::from(self.values[1]);
                let right = i64::from(self.values[2]) + i64::from(self.values[3]);
                let bottom = i64::from(self.values[4]) + i64::from(self.values[5]);
                let clip = Rect {
                    left: left as i32,
                    top: top as i32,
                    width: (right - left).clamp(0, u32::MAX as i64) as u32,
                    height: (bottom - top).clamp(0, u32::MAX as i64) as u32,
                };
                extensions::layer_wrapped_copy(cx, &arguments, clip)?
            }
        };
        after_draw(cx, self.layer, step)
    }
}
impl NativeContinuation for Read {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        self.values[self.index] = value::to_integer(cx.heap(), value)? as i32;
        self.index += 1;
        self.next(cx)
    }
}
fn key(cx: &mut NativeCx<'_>, name: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    )
}
fn call(cx: &mut NativeCx<'_>, layer: Value, name: &str, arguments: Vec<Value>) -> NativeStep {
    NativeStep::CallMemberOr {
        object: layer,
        key: key(cx, name),
        arguments,
        result_needed: false,
        continuation: tjs_bind::flow::complete(Value::Void),
    }
}
fn after_draw(_cx: &mut NativeCx<'_>, layer: Value, step: NativeStep) -> NativeResult<NativeStep> {
    Ok(tjs_bind::flow::then(
        step,
        tjs_bind::flow::callback(layer, |layer, cx, _| {
            Ok(call(cx, layer, "update", Vec::new()))
        }),
    ))
}

fn triplet(cx: &mut NativeCx<'_>, keys: [&str; 3], values: [i32; 3]) -> NativeResult<Value> {
    tjs_bind::IntoTjs::into_tjs(
        tjs_bind::Dictionary(keys.into_iter().zip(values.map(i64::from))),
        cx.heap_mut(),
    )
}
fn checked_integer(value: f64) -> NativeResult<i32> {
    if value.is_finite() && (i32::MIN as f64..i32::MAX as f64 + 1.).contains(&value.trunc()) {
        Ok(value as i32)
    } else {
        Err(NativeError::Message(
            "color conversion exceeds the signed 32-bit result range",
        ))
    }
}
fn rgb_to_hsv(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    if args.len() < 3 {
        return Err(NativeError::Missing(2));
    }
    let mut rgb = [0.; 3];
    for (i, out) in rgb.iter_mut().enumerate() {
        *out = value::to_integer(cx.heap(), arg(args, i)?)? as i32 as f64;
    }
    let [r, g, b] = rgb;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max == 0. {
        return triplet(cx, ["h", "s", "v"], [0; 3]);
    }
    let h = if max == min {
        0.
    } else {
        let h = if max == r {
            (g - b) / (max - min) * 60.
        } else if max == g {
            (b - r) / (max - min) * 60. + 120.
        } else {
            (r - g) / (max - min) * 60. + 240.
        };
        if h < 0. { h + 360. } else { h }
    };
    triplet(
        cx,
        ["h", "s", "v"],
        [
            checked_integer(h)?,
            checked_integer((max - min) / max * 100.)?,
            checked_integer(max * 100. / 255.)?,
        ],
    )
}
fn hsv_rgb([mut h, s, v]: [f64; 3]) -> NativeResult<[i32; 3]> {
    let s = s / 100.;
    let v = v / 100.;
    if s == 0. {
        return Ok([checked_integer(v * 255.)?; 3]);
    }
    if h == 360. {
        h = 0.;
    }
    // C++ integer division truncates toward zero: (-59)/60 == 0.
    let sector = checked_integer(h.floor())? / 60 % 6;
    let f = h / 60. - (h / 60.).floor();
    let p = v * (1. - s);
    let q = v * (1. - f * s);
    let t = v * (1. - (1. - f) * s);
    let rgb = match sector {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        5 => [v, p, q],
        _ => {
            return Err(NativeError::Message(
                "negative HSV sector has no defined reference color",
            ));
        }
    };
    Ok([
        checked_integer(rgb[0] * 255.)?,
        checked_integer(rgb[1] * 255.)?,
        checked_integer(rgb[2] * 255.)?,
    ])
}
fn hsv_to_rgb(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    if args.len() < 3 {
        return Err(NativeError::Missing(2));
    }
    let rgb = hsv_rgb([
        value::to_real(cx.heap(), args[0])?,
        value::to_real(cx.heap(), args[1])?,
        value::to_real(cx.heap(), args[2])?,
    ])?;
    triplet(cx, ["r", "g", "b"], rgb)
}
