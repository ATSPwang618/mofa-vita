//! Portable adaptation of krkrsdl3/plugins/LayerExRaster.cpp and
//! LayerExAreaAverage.cpp. Managed main-plane access replaces their pointer ABI.
//! This Rust adaptation is modified; upstream notices are in raster/LICENSE.txt.
mod average;
mod gpu;
use krkr_engine::{
    extensions,
    protocol::pixels::{Bytes, Pixels},
};
use std::sync::Arc;
use tjs_core::{NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Value, value};

krkr_engine::native_plugin! {
    pub(crate) Raster {
        names: ["layerExRaster.dll", "layerExRaster.tpm"],
        classes: [],
        extensions: [("Layer", "copyRaster", raster::CALL)],
    }
}
krkr_engine::native_plugin! {
    pub(crate) AreaAverage {
        names: ["layerExAreaAverage.dll", "layerExAreaAverage.tpm"],
        classes: [],
        extensions: [("Layer", "stretchCopyAA", average::entry::CALL)],
    }
}

#[tjs_bind::function(resumable = true)]
fn raster(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<NativeStep> {
    if args.len() >= 5 && crate::exports::object(args[0])? != cx.this() {
        return gpu::begin(cx, args);
    }
    begin(cx, args, false)
}
#[derive(tjs_bind::Trace)]
struct Work {
    owner: Value,
    source: Value,
    args: Vec<Value>,
    area: bool,
    same: bool,
    #[trace(skip = "Budgeted immutable CPU image contains no VM handles")]
    dest: Option<Arc<Pixels>>,
    #[trace(skip = "Budgeted immutable CPU image contains no VM handles")]
    input: Option<Arc<Pixels>>,
    #[trace(skip = "Budgeted output contains no VM handles")]
    output: Option<Bytes>,
    rect: [i64; 8],
    wave: [f64; 3],
    row: usize,
}
fn begin(cx: &mut NativeCx<'_>, args: &[Value], area: bool) -> NativeResult<NativeStep> {
    let count = if area { 9 } else { 5 };
    if args.len() < count {
        return Err(NativeError::Missing(count - 1));
    }
    let owner = Value::Obj(cx.this().into());
    let work = Box::new(Work {
        owner,
        source: Value::Void,
        args: args[..count].to_vec(),
        area,
        same: false,
        dest: None,
        input: None,
        output: None,
        rect: [0; 8],
        wave: [0.; 3],
        row: 0,
    });
    if area {
        read(cx, owner, work)
    } else {
        // layerExBase_GL resets using native Layer dimensions before conversion.
        extensions::layer_prepare_draw(cx, owner)?;
        extensions::layer_read_pixels(cx, owner, work)
    }
}
impl extensions::PixelContinuation for Work {
    fn pixels(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<Pixels>,
    ) -> NativeResult<NativeStep> {
        if self.dest.is_none() {
            self.dest = Some(pixels);
            if self.area {
                extensions::layer_prepare_draw(cx, self.owner)?;
                for i in 0..4 {
                    self.rect[i] = integer(cx, self.args[i])?;
                }
                self.source = self.args[4];
            } else {
                let amplitude = integer(cx, self.args[1])?;
                let lines = integer(cx, self.args[2])?;
                let cycle = integer(cx, self.args[3])?;
                let time = value::to_integer(cx.heap(), self.args[4])?;
                self.source = self.args[0];
                // Reject undefined division/cast behavior instead of exposing C++ UB.
                if lines == 0 || cycle == 0 {
                    return Err(NativeError::Message(
                        "copyRaster requires nonzero lines and cycle",
                    ));
                }
                let omega = std::f64::consts::TAU / lines as f64;
                self.wave = [
                    amplitude as f64,
                    omega,
                    -omega * time as f64 / cycle as f64
                        * (self.dest.as_ref().unwrap().size.height / 2) as f64,
                ];
            }
            self.same = crate::exports::object(self.source)? == crate::exports::object(self.owner)?;
            return read(cx, self.source, self);
        }
        if !self.area && self.dest.as_ref().unwrap().size != pixels.size {
            return Ok(NativeStep::Return(Value::Void));
        }
        self.input = Some(pixels);
        if self.area {
            for i in 4..8 {
                self.rect[i] = integer(cx, self.args[i + 1])?;
            }
            self.rect = average::clip(
                self.rect,
                self.dest.as_ref().unwrap().size,
                self.input.as_ref().unwrap().size,
            )?;
        }
        let dest = self.dest.as_ref().unwrap();
        let budget = extensions::layer_pixel_budget(cx, self.owner)?;
        let mut output = Bytes::zeroed(
            dest.size
                .rgba_bytes()
                .ok_or(NativeError::Message("image too large"))?,
            &budget,
        )
        .map_err(|e| NativeError::Detail(e.to_string()))?;
        output.as_mut_slice().copy_from_slice(
            dest.main
                .as_ref()
                .ok_or(NativeError::Message("Layer has no main image"))?
                .as_slice(),
        );
        self.output = Some(output);
        self.resume(cx, Value::Void)
    }
}
impl NativeContinuation for Work {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let size = self.dest.as_ref().unwrap().size;
        let height = if self.area {
            self.rect[3] as usize
        } else {
            size.height as usize
        };
        if self.row < height {
            let source = self.input.as_ref().unwrap();
            let input = source
                .main
                .as_ref()
                .ok_or(NativeError::Message("Layer has no main image"))?
                .as_slice();
            let output = self.output.as_mut().unwrap().as_mut_slice();
            if self.area {
                average::row(
                    output,
                    input,
                    self.same,
                    size,
                    source.size,
                    self.rect,
                    self.row,
                );
            } else {
                let width = size.width as usize;
                let d = (self.wave[2].sin() * self.wave[0]) as i32 as i64;
                // The original forward loop smears overlapping rightward self-copies.
                if d.unsigned_abs() < width as u64 {
                    let (sx, dx) = ((-d).max(0) as usize, d.max(0) as usize);
                    for x in 0..width - d.unsigned_abs() as usize {
                        let s = (self.row * width + sx + x) * 4;
                        let t = (self.row * width + dx + x) * 4;
                        let p: [u8; 4] = if self.same {
                            &output[s..s + 4]
                        } else {
                            &input[s..s + 4]
                        }
                        .try_into()
                        .unwrap();
                        output[t..t + 4].copy_from_slice(&p);
                    }
                }
                self.wave[2] += self.wave[1];
            }
            self.row += 1;
            return Ok(NativeStep::Continue(self));
        }
        let pixels = Arc::new(Pixels {
            size,
            main: self.output.take(),
            province: None,
        });
        let step = extensions::layer_copy_pixels(cx, self.owner, pixels, false, size)?;
        if !self.area {
            // No update call: this is explicitly commented out in copyRaster.
            return Ok(step);
        }
        let rect = self.rect[..4]
            .iter()
            .map(|&n| Value::Int(n))
            .collect::<Vec<_>>();
        Ok(tjs_bind::flow::then(
            step,
            tjs_bind::flow::callback((self.owner, rect), |(owner, arguments), cx, _| {
                let key = key(cx, "update");
                Ok(NativeStep::CallMemberOr {
                    object: owner,
                    key,
                    arguments,
                    result_needed: false,
                    continuation: tjs_bind::flow::complete(Value::Void),
                })
            }),
        ))
    }
}
fn integer(cx: &NativeCx<'_>, v: Value) -> NativeResult<i64> {
    Ok(i64::from(value::to_integer(cx.heap(), v)? as i32))
}
fn key(cx: &mut NativeCx<'_>, name: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    )
}
// Source and area-average destination use ordinary property lookup, including
// script overrides. Validate dimensions against the managed allocation.
#[derive(tjs_bind::Trace)]
struct Read {
    owner: Value,
    next: Box<dyn extensions::PixelContinuation>,
    index: usize,
    dimensions: [u32; 2],
}
fn read(
    cx: &mut NativeCx<'_>,
    owner: Value,
    next: Box<dyn extensions::PixelContinuation>,
) -> NativeResult<NativeStep> {
    extensions::layer_size(cx, owner)?;
    Box::new(Read {
        owner,
        next,
        index: 0,
        dimensions: [0; 2],
    })
    .resume(cx, Value::Void)
}
impl NativeContinuation for Read {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        if self.index > 0 {
            let n = integer(cx, value)?;
            if n <= 0 {
                return Err(NativeError::Message("invalid Layer image dimensions"));
            }
            self.dimensions[self.index - 1] = n as u32;
        }
        if self.index < 2 {
            let key = key(cx, ["imageWidth", "imageHeight"][self.index]);
            self.index += 1;
            return Ok(NativeStep::Get {
                object: self.owner,
                key,
                continuation: self,
            });
        }
        extensions::layer_read_pixels(cx, self.owner, self)
    }
}
impl extensions::PixelContinuation for Read {
    fn pixels(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<Pixels>,
    ) -> NativeResult<NativeStep> {
        if [pixels.size.width, pixels.size.height] != self.dimensions {
            return Err(NativeError::Message(
                "Layer dimensions disagree with managed image",
            ));
        }
        self.next.pixels(cx, pixels)
    }
}
