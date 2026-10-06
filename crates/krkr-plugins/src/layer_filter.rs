//! Recovered from the supplied layerExFilter.dll (registration 0x10012210).
mod haze;
use krkr_engine::{
    extensions,
    protocol::{
        filter::{Filter, Kind},
        graphics::{Adjustment, Rect},
        pixels::Bytes,
    },
};
use std::sync::Arc;
use tjs_bind::{RestArgs, flow};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjRef, Value, value,
};

krkr_engine::native_plugin! {
    pub(crate) LayerFilter {
        names: ["layerExFilter.dll", "layerExFilter.tpm"],
        link(cx, exports) {
            let layer = crate::exports::class(cx, "Layer")?;
            let state = haze::Shared::default();
            for (name, call) in [
                ("drawNoise", noise::CALL), ("doContrast", contrast::CALL),
                ("initHazeCopy", haze::init::CALL), ("uninitHazeCopy", haze::uninit::CALL),
                ("hazeCopy", haze_copy::CALL),
            ] { exports.captured_function(cx, layer, name, call, state.clone(), false)?; }
            Ok(())
        }
    }
}
#[tjs_bind::function(resumable = true)]
fn noise(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    start(cx, args, Operation::Noise)
}
#[tjs_bind::function(resumable = true)]
fn contrast(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    start(cx, args, Operation::Contrast)
}
#[tjs_bind::function(resumable = true)]
fn haze_copy(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    if args.len() < 2 {
        return Err(NativeError::Missing(1));
    }
    let state = haze::shared(cx)?;
    let slot = int(cx, args, 0, -1)?;
    let waves = state.waves(slot)?;
    start(cx, args, Operation::Haze(waves))
}
#[derive(tjs_bind::Trace)]
enum Operation {
    Noise,
    Contrast,
    Haze(haze::Waves),
}
#[derive(tjs_bind::Trace)]
struct Read {
    layer: Value,
    args: Vec<Value>,
    operation: Operation,
    dimensions: [i32; 4],
    index: usize,
    fallback: bool,
}
fn start(cx: &mut NativeCx<'_>, args: &[Value], operation: Operation) -> NativeResult<NativeStep> {
    let count = match operation {
        Operation::Noise => 9,
        Operation::Contrast => 1,
        Operation::Haze(_) => 14,
    };
    Read {
        layer: Value::Obj(ObjRef::bound(cx.this())),
        args: args.iter().take(count).copied().collect(),
        operation,
        dimensions: [0; 4],
        index: 0,
        fallback: false,
    }
    .next(cx)
}
impl Read {
    fn next(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index == 2 {
            extensions::layer_prepare_draw(cx, self.layer)?;
        }
        let count = if matches!(self.operation, Operation::Haze(_)) {
            4
        } else {
            2
        };
        if self.index < count {
            let name = if self.index >= 2 || self.fallback {
                ["imageWidth", "imageHeight"][self.index % 2]
            } else {
                ["realImageWidth", "realImageHeight"][self.index]
            };
            return Ok(NativeStep::GetOr {
                object: if self.index < 2 {
                    self.layer
                } else {
                    self.args[1]
                },
                key: key(cx, name),
                raw: false,
                fallback: Value::Void,
                continuation: Box::new(self),
            });
        }
        match self.operation {
            Operation::Haze(waves) => {
                haze::draw(cx, self.layer, &self.args, self.dimensions, waves)
            }
            Operation::Contrast | Operation::Noise => {
                let [w, h, _, _] = self.dimensions;
                let full = Rect {
                    left: 0,
                    top: 0,
                    width: w.max(0) as u32,
                    height: h.max(0) as u32,
                };
                let budget = extensions::layer_pixel_budget(cx, self.layer)?;
                let (rectangle, kind, table) = if matches!(self.operation, Operation::Contrast) {
                    let c = int(cx, &self.args, 0, 0)?.clamp(-127, 127);
                    if c == 0 {
                        return Ok(NativeStep::Return(Value::Void));
                    }
                    let mut table = Bytes::zeroed(1024, &budget).map_err(error)?;
                    for (v, cell) in table
                        .as_mut_slice()
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .enumerate()
                    {
                        let v = v as i32;
                        let output = if c > 0 {
                            if v < c {
                                0
                            } else if v >= 255 - c {
                                255
                            } else {
                                (v - c) * 255 / (255 - 2 * c)
                            }
                        } else {
                            (255 + 2 * c) * v / 255 - c
                        };
                        cell.copy_from_slice(&(output as u32).to_le_bytes());
                    }
                    (full, Kind::Lookup, table)
                } else {
                    let mono = int(cx, &self.args, 0, 1)? != 0;
                    let mut under = int(cx, &self.args, 1, 0)?;
                    let mut upper = int(cx, &self.args, 2, 255)?;
                    let seed = if supplied(&self.args, 3) {
                        int(cx, &self.args, 3, 0)? as u32
                    } else {
                        extensions::tick_count(cx)? as u32
                    };
                    let left = int(cx, &self.args, 4, 0)?.max(0);
                    let top = int(cx, &self.args, 5, 0)?.max(0);
                    let width = int(cx, &self.args, 6, w)?
                        .min(w.saturating_sub(left))
                        .max(0) as u32;
                    let height =
                        int(cx, &self.args, 7, h)?.min(h.saturating_sub(top)).max(0) as u32;
                    let hold_alpha = int(cx, &self.args, 8, 0)? != 0;
                    if upper < under {
                        std::mem::swap(&mut under, &mut upper);
                    }
                    let rectangle = Rect {
                        left,
                        top,
                        width,
                        height,
                    };
                    (
                        rectangle,
                        Kind::RandomFill {
                            legacy: false,
                            seed,
                            under,
                            range: upper.wrapping_sub(under),
                            monochrome: mono,
                            hold_alpha,
                            rectangle,
                        },
                        Bytes::zeroed(0, &budget).map_err(error)?,
                    )
                };
                let step = extensions::layer_adjust(
                    cx,
                    self.layer,
                    rectangle,
                    Adjustment::Filter(Filter {
                        kind,
                        table: Arc::new(table),
                    }),
                    false,
                )?;
                redraw(cx, step, self.layer, [0, 0, w, h])
            }
        }
    }
}
impl NativeContinuation for Read {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if self.index < 2 && !self.fallback && matches!(result, Value::Void) {
            self.fallback = true;
        } else {
            self.dimensions[self.index] = value::to_integer(cx.heap(), result)? as i32;
            self.index += 1;
            self.fallback = false;
        }
        self.next(cx)
    }
}
fn redraw(
    _cx: &mut NativeCx<'_>,
    step: NativeStep,
    layer: Value,
    rect: [i32; 4],
) -> NativeResult<NativeStep> {
    Ok(flow::then(
        step,
        flow::callback((layer, rect), |(layer, rect), cx, _| {
            Ok(NativeStep::CallMember {
                object: layer,
                key: key(cx, "update"),
                arguments: rect.map(|v| Value::Int(i64::from(v))).to_vec(),
                continuation: flow::complete(Value::Void),
            })
        }),
    ))
}
fn key(cx: &mut NativeCx<'_>, name: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    )
}
fn supplied(args: &[Value], index: usize) -> bool {
    args.get(index).is_some_and(|v| !matches!(v, Value::Void))
}
fn int(cx: &NativeCx<'_>, args: &[Value], index: usize, default: i32) -> NativeResult<i32> {
    if supplied(args, index) {
        Ok(value::to_integer(cx.heap(), args[index])? as i32)
    } else {
        Ok(default)
    }
}
fn fixed(v: f64) -> i32 {
    if (i32::MIN as f64..i32::MAX as f64 + 1.0).contains(&v) {
        v as i32
    } else {
        i32::MIN
    }
}
fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
