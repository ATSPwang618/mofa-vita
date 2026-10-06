//! Portable visual approximations of the remaining Nagano handlers.
//! Geometry and lookup tables are prepared on CPU; image pixels stay on GPU.
use super::*;
pub(super) fn convert(
    cx: &mut NativeCx<'_>,
    kind: Kind,
    i: usize,
    v: Value,
) -> NativeResult<Value> {
    if matches!(v, Value::Void) {
        return Ok(v);
    }
    if matches!(kind, Kind::Morph) || matches!(kind, Kind::Universal) && (i == 0 || i == 11) {
        return Ok(v);
    }
    if matches!(kind, Kind::Blur) && i == 8
        || matches!(kind, Kind::Ripple) && i >= 4
        || matches!(kind, Kind::Universal) && (1..9).contains(&i)
    {
        let n = value::to_real(cx.heap(), v)?;
        if !n.is_finite() {
            return Err(NativeError::Message("non-finite transition parameter"));
        }
        Ok(Value::Real(n))
    } else {
        integer(cx, v)
    }
}
fn number(v: Value, default: f64) -> NativeResult<f64> {
    match v {
        Value::Void => Ok(default),
        Value::Int(i) => Ok(i as f64),
        Value::Real(r) if r.is_finite() => Ok(r),
        _ => Err(NativeError::Type("a finite transition parameter")),
    }
}
pub(super) fn create(
    cx: &mut NativeCx<'_>,
    kind: Kind,
    size: Size,
    options: &[Value],
    rule: Option<Arc<Pixels>>,
) -> NativeResult<Arc<dyn Instance>> {
    let n = |i, default| number(options[i], default);
    let mut geometry = vec![];
    let v = match kind {
        Kind::Flutter => vec![
            n(0, 0.)?,
            n(1, 255.)? as i32 as u8 as f64,
            n(2, 8.)?.clamp(0., size.width.min(size.height) as f64),
        ],
        Kind::Honey => vec![n(0, 32.)?.max(4.), n(1, 0.)?, n(2, 2.)?, n(3, 6.)?],
        Kind::Ripple => {
            let v = vec![
                n(0, 1.)?.clamp(1., 20.),
                n(1, 2.)?.clamp(1., 128.),
                n(2, 32.)?.clamp(1., 4096.),
                n(3, 24.)?.clamp(0., size.height as f64),
                n(4, 1.)?,
                n(5, 1.)?.max(0.01),
            ];
            if v[4] <= 0. {
                return Err(NativeError::Message("roundness must be positive"));
            }
            // Fixed per-instance origins: stable between renderers and frames.
            let mut seed = 0x6d2b79f5u32;
            for i in 0..v[0] as usize {
                let mut next = |extent: u32| {
                    seed = seed.wrapping_mul(214013).wrapping_add(2531011);
                    ((seed >> 16) % extent) as i32
                };
                geometry.extend(if i + 1 == v[0] as usize {
                    [size.width as i32 / 2, size.height as i32 / 2]
                } else {
                    [next(size.width), next(size.height)]
                });
            }
            v
        }
        Kind::Universal => {
            let hsb = if matches!(options[0], Value::Void) {
                false
            } else {
                let Value::Str(s) = value::to_string(cx.heap_mut(), options[0])? else {
                    unreachable!()
                };
                cx.heap().string(s)? == [72, 83, 66]
            };
            let rule = rule
                .as_ref()
                .ok_or(NativeError::Message("3duniversal requires a rule image"))?;
            if rule.size.width == 0 || rule.size.height == 0 || rule.main.is_none() {
                return Err(NativeError::Message(
                    "3duniversal requires RGBA rule pixels",
                ));
            }
            vec![
                u8::from(hsb) as f64,
                n(2, n(1, 0.)?)?,
                n(4, n(3, 0.)?)?,
                n(6, n(5, 0.)?)?,
                n(8, n(7, 0.)?)?,
                n(9, 0.)?.clamp(0., 127.),
                n(10, 0.)?.clamp(0., 127.),
            ]
        }
        Kind::Morph => {
            let read = |cx: &NativeCx<'_>, v: Value| -> NativeResult<Vec<i32>> {
                let Value::Obj(o) = v else {
                    return Err(NativeError::Type("an Array of triangle coordinates"));
                };
                let a = cx.heap().array(o.object.ok_or(NativeError::This)?)?;
                a.iter()
                    .take(256 * 6)
                    .map(|&v| Ok(value::to_integer(cx.heap(), v)? as i32))
                    .collect()
            };
            let before = read(cx, options[0])?;
            let after = read(cx, options[1])?;
            let count = (before.len() / 6).min(after.len() / 6);
            for i in 0..count {
                geometry.extend_from_slice(&before[i * 6..i * 6 + 6]);
                geometry.extend_from_slice(&after[i * 6..i * 6 + 6]);
            }
            vec![count as f64]
        }
        Kind::Blur => {
            let first = n(0, 0.)?.max(0.);
            let second = n(1, 0.)?.max(0.);
            let v = vec![
                n(2, first)?.clamp(0., size.width as f64),
                n(3, first)?.clamp(0., size.height as f64),
                n(4, second)?.clamp(0., size.width as f64),
                n(5, second)?.clamp(0., size.height as f64),
                n(6, 0.)?,
                n(7, 0.)?,
                n(8, 1.)?,
            ];
            if v[6] <= 0. {
                return Err(NativeError::Message("blurfade exponent must be positive"));
            }
            if !(0..=1).contains(&(v[4] as i32)) || !(0..=2).contains(&(v[5] as i32)) {
                return Err(NativeError::Message("invalid blurfade type or prerender"));
            }
            v
        }
        _ => unreachable!(),
    };
    Ok(Arc::new(Effect {
        kind,
        size,
        values: v,
        table: OnceLock::new(),
        rule,
        geometry,
    }))
}
pub(super) fn prepare(
    e: &Effect,
    t: u64,
    d: u64,
    p: &mut [u32; 16],
    budget: &Budget,
) -> Result<Arc<Bytes>, String> {
    let r = t as f64 / d as f64;
    let v = &e.values;
    match e.kind {
        Kind::Flutter => {
            p[3] = ((e.size.width + e.size.height) as f64 * r * r) as u32;
            p[4] = (v[2] * r * r * r) as u32;
            p[2] = v[0] as i64 as u32;
            p[6] = v[1] as u32;
        }
        Kind::Honey => {
            for (out, &v) in p[3..7].iter_mut().zip(v) {
                *out = v as i64 as u32;
            }
        }
        Kind::Blur => {
            let f = ((r * 10000.).floor() / 10000.).powf(v[6]);
            p[1] = (255. * f + 0.45) as u32;
            for i in 0..4 {
                let radius = (v[i] * if i < 2 { f } else { 1. - f } + 0.45) as u32;
                p[3 + i] = if v[5] == 2. { radius / 2 * 2 } else { radius };
            }
            p[7] = v[4] as u32;
        }
        Kind::Morph => return morph::prepare(e, r, p, budget),
        Kind::Ripple => return ripple(e, r, p, budget),
        Kind::Universal => return universal(e, p, budget),
        _ => unreachable!(),
    }
    e.table.get_or_init(|| table(1, budget, |_| {})).clone()
}
fn ripple(e: &Effect, r: f64, p: &mut [u32; 16], budget: &Budget) -> Result<Arc<Bytes>, String> {
    let v = &e.values;
    let count = v[0] as usize;
    let travel = (v[1] * v[2]) as usize;
    let max_dist = (e.size.width as f64).hypot(e.size.height as f64 * v[4]);
    p[3] = count as u32;
    p[4] = travel as u32;
    p[5] = (v[4] * 65536.).min(i32::MAX as f64) as u32;
    p[6] = (765 / count) as u32;
    table(count * 4 + travel * 2, budget, |bytes| {
        for i in 0..count {
            let delay = if count == 1 {
                0.
            } else {
                0.45 * i as f64 / (count - 1) as f64 * if i + 1 == count { v[5] } else { 1. }
            }
            .min(0.95);
            let local = ((r - delay) / (1. - delay)).clamp(0., 1.);
            let at = i * 4;
            put(bytes, at, e.geometry[i * 2]);
            put(bytes, at + 1, e.geometry[i * 2 + 1]);
            put(
                bytes,
                at + 2,
                if r < delay {
                    -1
                } else {
                    ((max_dist + travel as f64) * local) as i32
                },
            );
            put(
                bytes,
                at + 3,
                (255. * (std::f64::consts::PI * local).sin()).round() as i32,
            );
        }
        for i in 0..travel {
            let amplitude = (v[3]
                * (i as f64 / v[2] * std::f64::consts::TAU).sin()
                * (1. - i as f64 / travel as f64))
                .round() as i32;
            put(bytes, count * 4 + i * 2, amplitude);
            put(
                bytes,
                count * 4 + i * 2 + 1,
                (i * (765 / count) / travel) as i32,
            );
        }
    })
}
fn universal(e: &Effect, p: &mut [u32; 16], budget: &Budget) -> Result<Arc<Bytes>, String> {
    let rule = e.rule.as_ref().ok_or("missing 3d rule")?;
    p[3] = rule.size.width;
    p[4] = rule.size.height;
    e.table
        .get_or_init(|| {
            let source = rule.main.as_ref().ok_or("missing RGBA rule")?.as_slice();
            let words = source.len() / 4;
            let v = &e.values;
            table(1024 + words, budget, |out| {
                for i in 0..256 {
                    let angle = i as f64 * std::f64::consts::TAU / 255.;
                    put(out, i * 4, (angle.cos() * 1024.).round() as i32);
                    put(out, i * 4 + 1, (angle.sin() * 1024.).round() as i32);
                    for side in 0..2 {
                        let bound = v[5 + side] as usize;
                        let time = if bound == 0 { i } else { i % (255 - bound) } as f64;
                        let mv = (v[1 + side * 2] * time * time / 2. + v[2 + side * 2] * time)
                            .round()
                            .clamp(-8388608., 8388607.) as i32;
                        put(out, i * 4 + 2 + side, mv);
                    }
                }
                out[4096..].copy_from_slice(source);
                if v[0] != 0. {
                    for pixel in out[4096..].as_chunks_mut::<4>().0.iter_mut() {
                        let (red, green, blue) = (
                            f64::from(pixel[0]) / 255.,
                            f64::from(pixel[1]) / 255.,
                            f64::from(pixel[2]) / 255.,
                        );
                        let hi = red.max(green).max(blue);
                        let lo = red.min(green).min(blue);
                        let delta = hi - lo;
                        let hue = if delta == 0. {
                            0.
                        } else if hi == red {
                            ((green - blue) / delta).rem_euclid(6.)
                        } else if hi == green {
                            (blue - red) / delta + 2.
                        } else {
                            (red - green) / delta + 4.
                        };
                        pixel[0] = (hue * 255. / 6.).round() as u8;
                        pixel[1] = if hi == 0. {
                            0
                        } else {
                            (delta / hi * 255.).round() as u8
                        };
                        pixel[2] = (hi * 255.).round() as u8;
                    }
                }
            })
        })
        .clone()
}
