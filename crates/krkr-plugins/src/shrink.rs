//! Modified Rust adaptation of krkrsdl3/plugins/shrinkCopy.cpp.
//! Upstream notices: raster/LICENSE.txt. Both exports operate on the main plane.
mod filter;
use krkr_engine::{
    extensions,
    protocol::{
        graphics::Size,
        pixels::{Bytes, Pixels},
    },
};
use std::sync::Arc;
use tjs_core::{NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Value, value};
krkr_engine::native_plugin! {
    pub(crate) Shrink {
        names:["shrinkCopy.dll","shrinkCopy.tpm"],
        classes:[],
        extensions:[("Layer","shrinkCopy",general::CALL),("Layer","shrinkCopyFast",fast::CALL)],
    }
}
#[tjs_bind::function(resumable = true)]
fn general(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, false)
}
#[tjs_bind::function(resumable = true)]
fn fast(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, true)
}
#[derive(tjs_bind::Trace)]
struct Work {
    owner: Value,
    source: Value,
    fast: bool,
    same: bool,
    resizing: bool,
    #[trace(skip = "Pure numeric filter geometry")]
    mapping: filter::Mapping,
    #[trace(skip = "Budgeted pixels carry no VM references")]
    input: Option<Arc<Pixels>>,
    #[trace(skip = "Budgeted pixels carry no VM references")]
    output: Option<Bytes>,
    size: [u32; 2],
    row: usize,
}
fn begin(cx: &mut NativeCx<'_>, args: &[Value], fast: bool) -> NativeResult<NativeStep> {
    let count = if fast { 2 } else { 9 };
    if args.len() < count {
        return Err(NativeError::Missing(count - 1));
    }
    let owner = Value::Obj(cx.this().into());
    let mut mapping = filter::Mapping::default();
    let source = if fast {
        crate::exports::object(args[0])?;
        mapping.s[2] = value::to_integer(cx.heap(), args[1])? as i32 as i64;
        mapping.s[3] = args
            .get(2)
            .map(|&v| value::to_integer(cx.heap(), v))
            .transpose()?
            .unwrap_or(0) as i32 as i64;
        if mapping.s[3] == 0 {
            mapping.s[3] = mapping.s[2];
        }
        if mapping.s[2] <= 0 || mapping.s[3] <= 0 {
            return Err(NativeError::Message("invalid shrink step"));
        }
        args[0]
    } else {
        for (v, &a) in mapping.d.iter_mut().zip(args) {
            *v = value::to_real(cx.heap(), a)?;
        }
        crate::exports::object(args[4])?;
        for (v, &a) in mapping.s.iter_mut().zip(&args[5..]) {
            *v = value::to_integer(cx.heap(), a)? as i32 as i64;
        }
        if mapping
            .d
            .iter()
            .any(|v| !v.is_finite() || v.abs() > i32::MAX as f64)
            || mapping.d[2] <= 0.
            || mapping.d[3] <= 0.
            || mapping.s[2] <= 0
            || mapping.s[3] <= 0
            || mapping.s[2] < (mapping.d[2] as i64)
            || mapping.s[3] < (mapping.d[3] as i64)
        {
            return Err(NativeError::Message("invalid shrinkCopy rectangle"));
        }
        args[4]
    };
    let same = crate::exports::object(source)? == cx.this();
    read(
        cx,
        source,
        Box::new(Work {
            owner,
            source,
            fast,
            same,
            resizing: false,
            mapping,
            input: None,
            output: None,
            size: [0; 2],
            row: 0,
        }),
    )
}
impl extensions::PixelContinuation for Work {
    fn pixels(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<Pixels>,
    ) -> NativeResult<NativeStep> {
        if self.input.is_none() {
            self.input = Some(pixels);
            if self.fast {
                extensions::layer_size(cx, self.owner)?;
                let key = key(cx, "hasImage");
                self.resizing = true;
                return Ok(NativeStep::Get {
                    object: self.owner,
                    key,
                    continuation: self,
                });
            }
            return read(cx, self.owner, self);
        }
        let input = self.input.as_ref().unwrap();
        let size = pixels.size;
        if self.fast {
            let expected = Size {
                width: (input.size.width as u64).div_ceil(self.mapping.s[2] as u64) as u32,
                height: (input.size.height as u64).div_ceil(self.mapping.s[3] as u64) as u32,
            };
            if size != expected {
                return Err(NativeError::Message(
                    "setImageSize did not set shrink dimensions",
                ));
            }
        } else if !self.mapping.clip(size, input.size) {
            return Ok(NativeStep::Return(Value::Void));
        }
        let budget = extensions::layer_pixel_budget(cx, self.owner)?;
        let mut output = Bytes::zeroed(
            size.rgba_bytes()
                .ok_or(NativeError::Message("image too large"))?,
            &budget,
        )
        .map_err(|e| NativeError::Detail(e.to_string()))?;
        output
            .as_mut_slice()
            .copy_from_slice(pixels.main.as_ref().unwrap().as_slice());
        self.output = Some(output);
        self.size = [size.width, size.height];
        self.resume(cx, Value::Void)
    }
}
impl NativeContinuation for Work {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<NativeStep> {
        if self.resizing {
            self.resizing = false;
            if value::to_integer(cx.heap(), v)? == 0 {
                return Err(NativeError::Message("destination has no image"));
            }
            let input = self.input.as_ref().unwrap();
            let dimensions = [
                (u64::from(input.size.width).div_ceil(self.mapping.s[2] as u64)) as i64,
                (u64::from(input.size.height).div_ceil(self.mapping.s[3] as u64)) as i64,
            ];
            let key = key(cx, "setImageSize");
            return Ok(NativeStep::CallMemberOr {
                object: self.owner,
                key,
                arguments: dimensions.map(Value::Int).to_vec(),
                result_needed: false,
                continuation: tjs_bind::flow::callback(self, |work, cx, _| {
                    read(cx, work.owner, work)
                }),
            });
        }
        let size = Size {
            width: self.size[0],
            height: self.size[1],
        };
        let height = if self.fast {
            size.height as usize
        } else {
            (self.mapping.end[1] - self.mapping.start[1]) as usize
        };
        if self.row < height {
            let input = self.input.as_ref().unwrap();
            let out = self.output.as_mut().unwrap().as_mut_slice();
            if self.fast {
                filter::fast_row(
                    out,
                    input,
                    self.mapping.s[2] as usize,
                    self.mapping.s[3] as usize,
                    size,
                    self.row,
                );
            } else {
                self.mapping.row(out, input, self.same, size, self.row);
            }
            self.row += 1;
            return Ok(NativeStep::Continue(self));
        }
        extensions::layer_copy_pixels(
            cx,
            self.owner,
            Arc::new(Pixels {
                size,
                main: self.output.take(),
                province: None,
            }),
            false,
            size,
        )
    }
}
fn key(cx: &mut NativeCx<'_>, s: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(s.encode_utf16().collect::<Vec<_>>()),
    )
}
#[derive(tjs_bind::Trace)]
struct Read {
    owner: Value,
    next: Box<dyn extensions::PixelContinuation>,
    index: usize,
    size: [u32; 2],
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
        size: [0; 2],
    })
    .resume(cx, Value::Void)
}
impl NativeContinuation for Read {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<NativeStep> {
        if self.index > 0 {
            let n = value::to_integer(cx.heap(), v)? as i32;
            if n <= 0 {
                return Err(NativeError::Message("invalid Layer image"));
            }
            if self.index > 1 {
                self.size[self.index - 2] = n as u32;
            }
        }
        if self.index < 3 {
            let key = key(cx, ["hasImage", "imageWidth", "imageHeight"][self.index]);
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
    fn pixels(self: Box<Self>, cx: &mut NativeCx<'_>, p: Arc<Pixels>) -> NativeResult<NativeStep> {
        if self.size != [p.size.width, p.size.height] || p.main.is_none() {
            return Err(NativeError::Message("Layer dimensions disagree with image"));
        }
        self.next.pixels(cx, p)
    }
}
