//! The supplied filter.dll: fourteen global functions (V2Link 0x10002ce0).
//! All pixel access goes through managed image commands; no foreign addresses.
mod haze;
use krkr_engine::{
    extensions,
    protocol::{
        budget::Budget,
        filter::{Filter, Kind},
        graphics::{Adjustment, Rect, Size},
        pixels::Bytes,
        warp::Warp,
    },
};
use std::{cell::RefCell, rc::Rc, sync::Arc};
use tjs_bind::{RestArgs, flow};
use tjs_core::{NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Value, value};

#[derive(Clone, Default, tjs_bind::Trace)]
struct Shared(
    #[trace(skip = "Numeric effect state and owned tables contain no VM references")]
    Rc<RefCell<State>>,
);
#[derive(Default)]
struct State {
    lens: Option<Arc<Bytes>>,
    haze: Option<haze::State>,
}
fn shared(cx: &mut NativeCx<'_>) -> NativeResult<Shared> {
    let function = cx.function().ok_or(NativeError::This)?;
    cx.heap_mut()
        .with_native_state::<Shared, _>(function, |s| s.clone())
}
krkr_engine::native_plugin! {
    pub(crate) LegacyFilter {
        names:["filter.dll","filter.tpm"],
        link(cx,exports){
            let state=Shared::default();
            for (name,call) in [
                ("Smudge",smudge::CALL),("Blur",blur::CALL),("Lens",lens::CALL),
                ("InitLens",init_lens::CALL),("ReleaseLens",release_lens::CALL),
                ("Noise",noise::CALL),("Contrast",contrast::CALL),
                ("initHaze",init_haze::CALL),("doHaze",do_haze::CALL),("endHaze",end_haze::CALL),
                ("Stretch",stretch::CALL),("Vortex",vortex::CALL),("fillXor",fill_xor::CALL),("dithering",dithering::CALL),
            ] {exports.captured_function(cx,cx.global,name,call,state.clone(),false)?;}
            Ok(())
        }
    }
}
#[derive(Clone, Copy, tjs_bind::Trace)]
enum Operation {
    Smudge,
    Blur,
    Lens,
    Noise,
    Contrast,
    InitHaze,
    Haze,
    Stretch,
    Vortex,
}
macro_rules! entry {
    ($name:ident,$op:ident) => {
        #[tjs_bind::function(resumable = true)]
        fn $name(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            start(cx, args, Operation::$op)
        }
    };
}
entry!(smudge, Smudge);
entry!(blur, Blur);
entry!(lens, Lens);
entry!(noise, Noise);
entry!(contrast, Contrast);
entry!(init_haze, InitHaze);
entry!(do_haze, Haze);
entry!(stretch, Stretch);
entry!(vortex, Vortex);
#[tjs_bind::function]
fn init_lens(cx: &mut NativeCx<'_>) -> NativeResult<()> {
    let state = shared(cx)?;
    if state.0.borrow().lens.is_none() {
        let mut table = Bytes::zeroed(8192 * 4, &budget(cx)?).map_err(error)?;
        for (i, cell) in table
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            let v = (((i as f64 / 8191.0).asin() * 0.5).tan() * 65535.0) as i32;
            cell.copy_from_slice(&v.to_le_bytes());
        }
        state.0.borrow_mut().lens = Some(Arc::new(table));
    }
    Ok(())
}
#[tjs_bind::function]
fn release_lens(cx: &mut NativeCx<'_>) -> NativeResult<()> {
    shared(cx)?.0.borrow_mut().lens = None;
    Ok(())
}
#[tjs_bind::function]
fn end_haze(cx: &mut NativeCx<'_>) -> NativeResult<()> {
    shared(cx)?.0.borrow_mut().haze = None;
    Ok(())
}
#[derive(tjs_bind::Trace)]
struct Options {
    object: Value,
    state: Shared,
    operation: Operation,
    values: Vec<Value>,
}
fn start(cx: &mut NativeCx<'_>, args: &[Value], operation: Operation) -> NativeResult<NativeStep> {
    let object = crate::exports::arg(args, 0)?;
    crate::exports::object(object)?;
    let state = shared(cx)?;
    match operation {
        Operation::InitHaze if state.0.borrow().haze.is_some() => {
            return Err(NativeError::Message("filter haze is already initialized"));
        }
        Operation::Haze if state.0.borrow().haze.is_none() => {
            return Err(NativeError::Message("filter haze is not initialized"));
        }
        _ => {}
    }
    Options {
        object,
        state,
        operation,
        values: Vec::new(),
    }
    .next(cx)
}
impl Operation {
    fn names(self) -> &'static [&'static str] {
        match self {
            Self::Smudge => &["layer", "level"],
            Self::Blur => &["layer", "level", "type"],
            Self::Lens => &["src", "dest", "zoom", "power"],
            Self::Vortex => &["src", "dest", "rad"],
            Self::Noise => &["layer", "monocro", "seed", "under", "upper"],
            Self::Contrast => &["layer", "level"],
            Self::InitHaze => &[
                "time",
                "intime",
                "outtime",
                "speed",
                "cycle",
                "upper",
                "center",
                "lower",
                "upperpow",
                "centerpow",
                "lowerpow",
                "waves",
                "lwaves",
            ],
            Self::Haze => &["src", "dest", "tick", "rad", "bgcolor", "blend"],
            Self::Stretch => &[
                "src", "sleft", "stop", "swidth", "sheight", "dest", "dleft", "dtop", "dwidth",
                "dheight", "opa",
            ],
        }
    }
}
impl Options {
    fn next(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let index = self.values.len();
        if let Some(name) = self.operation.names().get(index) {
            let default = match (self.operation, index) {
                (Operation::Smudge | Operation::Blur, 1)
                | (Operation::Noise, 1)
                | (Operation::Lens, 3) => Value::Int(1),
                (Operation::Lens, 2) => Value::Real(1.0),
                (Operation::Noise, 2) => Value::Int(extensions::tick_count(cx)? as i64),
                (Operation::Noise, 4) | (Operation::Stretch, 10) => Value::Int(255),
                (Operation::Stretch, 3 | 4) => {
                    let size = extensions::layer_image_size(cx, self.values[0])?;
                    Value::Int(i64::from(if index == 3 { size.width } else { size.height }) - 1)
                }
                (Operation::Stretch, 8 | 9) => {
                    let size = extensions::layer_image_size(cx, self.values[5])?;
                    Value::Int(i64::from(if index == 8 { size.width } else { size.height }))
                }
                (Operation::InitHaze, 0 | 5 | 6 | 7) => Value::Int(-1),
                (Operation::InitHaze, 1) => Value::Int(800),
                (Operation::InitHaze, 2) => self.values[1],
                (Operation::InitHaze, 3) => Value::Real(std::f64::consts::TAU / 4000.0),
                (Operation::InitHaze, 4) => Value::Real(6.0),
                (Operation::InitHaze, 8..=10) => Value::Real(1.0),
                (Operation::InitHaze, 11 | 12) => Value::Void,
                _ if matches!(*name, "layer" | "src" | "dest") => Value::Void,
                _ => Value::Int(0),
            };
            return Ok(NativeStep::GetRequiredOr {
                object: self.object,
                key: key(cx, name),
                fallback: default,
                continuation: Box::new(self),
            });
        }
        self.execute(cx)
    }
    fn execute(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let v = &self.values;
        if matches!(self.operation, Operation::InitHaze) {
            return haze::init(cx, self.state, v);
        }
        if matches!(self.operation, Operation::Haze) {
            return haze::draw(cx, &self.state, v);
        }
        let target = match self.operation {
            Operation::Lens | Operation::Vortex => v[1],
            Operation::Stretch => v[5],
            _ => v[0],
        };
        let size = extensions::layer_image_size(cx, target)?;
        extensions::layer_prepare_draw(cx, target)?;
        let rectangle = size.rect();
        let image_budget = extensions::layer_pixel_budget(cx, target)?;
        let kind = match self.operation {
            Operation::Smudge | Operation::Blur => {
                let passes = int(cx, v[1])?.max(0) as u32;
                let disabled = matches!(self.operation, Operation::Blur) && int(cx, v[2])? != 0;
                Kind::Smudge {
                    passes: if disabled { 0 } else { passes },
                }
            }
            Operation::Noise => {
                let mut under = int(cx, v[3])?;
                let mut upper = int(cx, v[4])?;
                if upper < under {
                    std::mem::swap(&mut under, &mut upper);
                }
                Kind::RandomFill {
                    legacy: true,
                    seed: int(cx, v[2])? as u32,
                    under,
                    range: upper.wrapping_sub(under),
                    monochrome: int(cx, v[1])? != 0,
                    hold_alpha: false,
                    rectangle,
                }
            }
            Operation::Contrast => {
                let c = int(cx, v[1])?.clamp(-127, 127);
                if c == 0 {
                    return Ok(NativeStep::Return(Value::Void));
                }
                let mut table = Bytes::zeroed(1024, &image_budget).map_err(error)?;
                for (i, cell) in table
                    .as_mut_slice()
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .enumerate()
                {
                    let n = i as i32;
                    let out = if c > 0 {
                        if n < c {
                            0
                        } else if n >= 255 - c {
                            255
                        } else {
                            (n - c) * 255 / (255 - 2 * c)
                        }
                    } else {
                        (255 + 2 * c) * n / 255 - c
                    };
                    cell.copy_from_slice(&(out as u32).to_le_bytes());
                }
                let step = adjust(cx, target, rectangle, Kind::Lookup, table)?;
                return redraw(cx, step, target, rectangle);
            }
            Operation::Lens | Operation::Vortex | Operation::Stretch => {
                let source = extensions::layer_image_size(cx, v[0])?;
                let effect =
                    match self.operation {
                        Operation::Lens => Warp::Lens {
                            radius: (f64::from(size.width) * real(cx, v[2])? * 0.5) as f32,
                            power: int(cx, v[3])?.max(1) as u32,
                            table: self.state.0.borrow().lens.clone().ok_or(
                                NativeError::Message("InitLens must be called before Lens"),
                            )?,
                        },
                        Operation::Vortex => Warp::Vortex {
                            radians: real(cx, v[2])? as f32,
                        },
                        Operation::Stretch => {
                            let src = [
                                int(cx, v[1])?,
                                int(cx, v[2])?,
                                int(cx, v[3])?,
                                int(cx, v[4])?,
                            ];
                            let dst = [
                                int(cx, v[6])?,
                                int(cx, v[7])?,
                                int(cx, v[8])?,
                                int(cx, v[9])?,
                            ];
                            let Some((source, destination)) = clip_stretch(src, dst, source, size)
                            else {
                                return Ok(NativeStep::Return(Value::Void));
                            };
                            Warp::Stretch {
                                source,
                                destination,
                                opacity: int(cx, v[10])?,
                            }
                        }
                        _ => unreachable!(),
                    };
                let update = if let Warp::Stretch { destination, .. } = &effect {
                    *destination
                } else {
                    rectangle
                };
                let step = extensions::layer_warp(cx, target, v[0], Arc::new(effect))?;
                return redraw(cx, step, target, update);
            }
            _ => unreachable!(),
        };
        let step = adjust(
            cx,
            target,
            rectangle,
            kind,
            Bytes::zeroed(0, &image_budget).map_err(error)?,
        )?;
        redraw(cx, step, target, rectangle)
    }
}
impl NativeContinuation for Options {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        self.values.push(value);
        self.next(cx)
    }
}
#[tjs_bind::function(resumable = true)]
fn fill_xor(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    rectangle_filter(cx, args, false)
}
#[tjs_bind::function(resumable = true)]
fn dithering(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    rectangle_filter(cx, args, true)
}
fn rectangle_filter(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    dither: bool,
) -> NativeResult<NativeStep> {
    if args.len() < 5 {
        return Err(NativeError::Missing(4));
    }
    let target = args[0];
    let size = extensions::layer_image_size(cx, target)?;
    extensions::layer_prepare_draw(cx, target)?;
    if size.width == 0 || size.height == 0 {
        return Ok(NativeStep::Return(Value::Void));
    }
    let left = int(cx, args[1])?.clamp(0, size.width as i32 - 1);
    let top = int(cx, args[2])?.clamp(0, size.height as i32 - 1);
    let width = int(cx, args[3])?.clamp(0, size.width as i32 - left) as u32;
    let height = int(cx, args[4])?.clamp(0, size.height as i32 - top) as u32;
    let update = Rect {
        left,
        top,
        width,
        height,
    };
    // Both supplied callbacks start writing at row zero, but update the requested top.
    let rectangle = Rect { top: 0, ..update };
    let kind = if dither {
        Kind::Dither { width, height }
    } else {
        Kind::Xor {
            color: int(cx, args.get(5).copied().unwrap_or(Value::Int(0xffffff)))? as u32,
        }
    };
    let table = Bytes::zeroed(0, &extensions::layer_pixel_budget(cx, target)?).map_err(error)?;
    let step = adjust(cx, target, rectangle, kind, table)?;
    redraw(cx, step, target, update)
}
fn adjust(
    cx: &mut NativeCx<'_>,
    layer: Value,
    rectangle: Rect,
    kind: Kind,
    table: Bytes,
) -> NativeResult<NativeStep> {
    extensions::layer_adjust(
        cx,
        layer,
        rectangle,
        Adjustment::Filter(Filter {
            kind,
            table: Arc::new(table),
        }),
        false,
    )
}
fn redraw(
    _cx: &mut NativeCx<'_>,
    step: NativeStep,
    layer: Value,
    rectangle: Rect,
) -> NativeResult<NativeStep> {
    Ok(flow::then(
        step,
        flow::callback(
            (
                layer,
                [
                    rectangle.left as i64,
                    rectangle.top as i64,
                    rectangle.width as i64,
                    rectangle.height as i64,
                ],
            ),
            |(layer, rect), cx, _| {
                Ok(NativeStep::CallMember {
                    object: layer,
                    key: key(cx, "update"),
                    arguments: rect.map(Value::Int).to_vec(),
                    continuation: flow::complete(Value::Void),
                })
            },
        ),
    ))
}
fn key(cx: &mut NativeCx<'_>, name: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    )
}
fn int(cx: &NativeCx<'_>, v: Value) -> NativeResult<i32> {
    Ok(value::to_integer(cx.heap(), v)? as i32)
}
fn real(cx: &NativeCx<'_>, v: Value) -> NativeResult<f64> {
    Ok(value::to_real(cx.heap(), v)?)
}
fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
fn budget(cx: &mut NativeCx<'_>) -> NativeResult<Budget> {
    extensions::image_staging_budget(cx.heap_mut())?
        .ok_or(NativeError::Message("image staging budget is unavailable"))
}
fn clip_stretch(
    mut s: [i32; 4],
    mut d: [i32; 4],
    source: Size,
    dest: Size,
) -> Option<(Rect, Rect)> {
    if s[2] <= 0 || s[3] <= 0 || d[2] <= 0 || d[3] <= 0 {
        return None;
    }
    let ratios = [s[2] as f32 / d[2] as f32, s[3] as f32 / d[3] as f32];
    for axis in 0..2 {
        let n = axis + 2;
        let inv = d[n] as f64 / s[n] as f64;
        let source_limit = [source.width, source.height][axis] as i32 - 1;
        let dest_limit = [dest.width, dest.height][axis] as i32;
        if s[axis] < 0 {
            let amount = (-f64::from(s[axis]) * inv) as i32;
            s[n] += s[axis];
            s[axis] = 0;
            d[axis] = d[axis].saturating_add(amount);
            d[n] = d[n].saturating_sub(amount);
        }
        let overflow = i64::from(s[n]) + i64::from(s[axis]) - i64::from(source_limit);
        if overflow > 0 {
            s[n] = s[n].saturating_sub(overflow as i32);
            d[n] = d[n].saturating_sub((overflow as f64 * inv) as i32);
        }
        if d[axis] < 0 {
            let amount = (-f64::from(d[axis]) * f64::from(ratios[axis])) as i32;
            d[n] += d[axis];
            d[axis] = 0;
            s[axis] = s[axis].saturating_add(amount);
            s[n] = s[n].saturating_sub(amount);
        }
        let overflow = i64::from(d[n]) + i64::from(d[axis]) - i64::from(dest_limit);
        if overflow > 0 {
            d[n] = d[n].saturating_sub(overflow as i32);
            s[n] = s[n].saturating_sub((overflow as f64 * f64::from(ratios[axis])) as i32);
        }
    }
    if s[2] <= 0 || s[3] <= 0 || d[2] <= 0 || d[3] <= 0 {
        return None;
    }
    let rect = |r: [i32; 4]| Rect {
        left: r[0],
        top: r[1],
        width: r[2] as u32,
        height: r[3] as u32,
    };
    Some((rect(s), rect(d)))
}
