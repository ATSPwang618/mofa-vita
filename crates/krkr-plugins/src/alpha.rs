//! Modified Rust adaptation of krkrsdl3/plugins/LayerExBTOA.cpp.
//! Upstream notices: raster/LICENSE.txt. No script/native buffer pointers.
use krkr_engine::{
    extensions,
    protocol::pixels::{Bytes, Pixels},
};
use std::sync::Arc;
use tjs_core::{NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Value, value};
krkr_engine::native_plugin! {
    pub(crate) Alpha {
        names: ["layerExBTOA.dll", "layerExBTOA.tpm"],
        classes: [],
        extensions: [
            ("Layer", "copyRightBlueToLeftAlpha", right::CALL),
            ("Layer", "copyBottomBlueToTopAlpha", bottom::CALL),
            ("Layer", "fillAlpha", fill::CALL),
            ("Layer", "copyAlphaToProvince", province::CALL),
            ("Layer", "clipAlphaRect", clip::CALL),
            ("Layer", "fillByProvince", fill_province::CALL),
        ],
    }
}
#[derive(Clone, Copy, tjs_bind::Trace)]
enum Kind {
    Right,
    Bottom,
    Fill,
    Province,
    Clip,
    FillProvince,
}
macro_rules! entry {
    ($name:ident, $kind:ident, $count:expr) => {
        #[tjs_bind::function(resumable = true)]
        fn $name(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<NativeStep> {
            begin(cx, args, Kind::$kind, $count)
        }
    };
}
entry!(right, Right, 0usize);
entry!(bottom, Bottom, 0usize);
entry!(fill, Fill, 0usize);
entry!(province, Province, 0usize);
entry!(clip, Clip, 7usize);
entry!(fill_province, FillProvince, 2usize);
#[derive(tjs_bind::Trace)]
struct Work {
    owner: Value,
    source: Value,
    kind: Kind,
    args: [i64; 7],
    same: bool,
    #[trace(skip = "Budgeted CPU pixels contain no VM values")]
    dest: Option<Arc<Pixels>>,
    #[trace(skip = "Budgeted CPU pixels contain no VM values")]
    input: Option<Arc<Pixels>>,
    #[trace(skip = "Budgeted plane buffer contains no VM values")]
    output: Option<Bytes>,
    rect: [i64; 6],
    row: usize,
}
fn begin(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    kind: Kind,
    count: usize,
) -> NativeResult<NativeStep> {
    if args.len() < count {
        return Err(NativeError::Missing(count - 1));
    }
    let number = |i: usize, default| -> NativeResult<i64> {
        Ok(args
            .get(i)
            .filter(|v| !matches!(v, Value::Void))
            .map(|&v| value::to_integer(cx.heap(), v).map(|n| n as i32 as i64))
            .unwrap_or(Ok(default))?)
    };
    let mut p = [0; 7];
    let owner = Value::Obj(cx.this().into());
    let mut source = owner;
    match kind {
        Kind::Province => p[0] = number(0, -1)?,
        Kind::FillProvince => {
            p[0] = number(0, 0)?;
            p[1] = number(1, 0)?;
        }
        Kind::Clip => {
            p[0] = number(0, 0)?;
            p[1] = number(1, 0)?;
            source = args[2];
            crate::exports::object(source)?;
            for (i, out) in p[2..6].iter_mut().enumerate() {
                *out = number(i + 3, 0)?;
            }
            p[6] = number(7, -1)?;
            if p[4] <= 0 || p[5] <= 0 {
                return Err(NativeError::Message("invalid clipAlphaRect size"));
            }
        }
        _ => {}
    }
    let same = crate::exports::object(source)? == cx.this();
    extensions::layer_read_planes(
        cx,
        owner,
        Box::new(Work {
            owner,
            source,
            kind,
            args: p,
            same,
            dest: None,
            input: None,
            output: None,
            rect: [0; 6],
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
        if pixels.main.is_none() {
            return Err(NativeError::Message("Layer has no main image"));
        }
        if self.dest.is_none() {
            self.dest = Some(pixels);
            if matches!(self.kind, Kind::Clip) {
                return extensions::layer_read_planes(cx, self.source, self);
            }
        } else {
            self.input = Some(pixels);
        }
        let dest = self.dest.as_ref().unwrap();
        if matches!(self.kind, Kind::FillProvince) && dest.province.is_none() {
            return Err(NativeError::Message("no province image"));
        }
        if matches!(self.kind, Kind::Clip) {
            let src = self.input.as_ref().unwrap();
            let [dx, dy, sx, sy, w, h, _] = self.args;
            let left = 0.max(-sx).max(-dx);
            let top = 0.max(-sy).max(-dy);
            let right = w
                .min(i64::from(src.size.width) - sx)
                .min(i64::from(dest.size.width) - dx);
            let bottom = h
                .min(i64::from(src.size.height) - sy)
                .min(i64::from(dest.size.height) - dy);
            self.rect = [
                dx + left,
                dy + top,
                sx + left,
                sy + top,
                (right - left).max(0),
                (bottom - top).max(0),
            ];
        }
        let budget = extensions::layer_pixel_budget(cx, self.owner)?;
        let len = if matches!(self.kind, Kind::Province) {
            dest.size.width as usize * dest.size.height as usize
        } else {
            dest.main.as_ref().unwrap().as_slice().len()
        };
        let mut output =
            Bytes::zeroed(len, &budget).map_err(|e| NativeError::Detail(e.to_string()))?;
        if !matches!(self.kind, Kind::Province) {
            output
                .as_mut_slice()
                .copy_from_slice(dest.main.as_ref().unwrap().as_slice());
        }
        if matches!(self.kind, Kind::Clip) && (0..256).contains(&self.args[6]) {
            // Native clipAlphaRect clears top and bottom bands before any
            // multiplication, which matters when the source is this same Layer.
            let [_, dy, _, _, _, rh] = self.rect;
            let width = dest.size.width as usize;
            for y in 0..dest.size.height as usize {
                if (y as i64) < dy || (y as i64) >= dy + rh {
                    for x in 0..width {
                        output.as_mut_slice()[(y * width + x) * 4 + 3] = self.args[6] as u8;
                    }
                }
            }
        }
        self.output = Some(output);
        self.resume(cx, Value::Void)
    }
}
impl NativeContinuation for Work {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let dest = self.dest.as_ref().unwrap();
        let (w, h) = (dest.size.width as usize, dest.size.height as usize);
        if self.row < h {
            let y = self.row;
            let input = dest.main.as_ref().unwrap().as_slice();
            let out = self.output.as_mut().unwrap().as_mut_slice();
            for x in 0..w {
                let i = (y * w + x) * 4;
                match self.kind {
                    Kind::Right if x < w / 2 => out[i + 3] = input[(y * w + x + w / 2) * 4 + 2],
                    Kind::Bottom if y < h / 2 => out[i + 3] = input[((y + h / 2) * w + x) * 4 + 2],
                    Kind::Fill => out[i + 3] = 255,
                    Kind::Province => {
                        out[y * w + x] = match self.args[0] {
                            n if n < 0 => input[i + 3],
                            n if n < 256 => u8::from(i64::from(input[i + 3]) >= n),
                            _ => 0,
                        }
                    }
                    Kind::FillProvince => {
                        if dest.province.as_ref().unwrap().as_slice()[y * w + x]
                            == self.args[0] as u8
                        {
                            // This SDL source explicitly typedefs DWORD as unsigned
                            // short: retain its 16-bit BGRA word stepping, without
                            // aliasing or unaligned pointer access.
                            let byte = x * 2;
                            let pixel = (y * w + byte / 4) * 4;
                            let value = self.args[1] as u16;
                            if byte % 4 == 0 {
                                out[pixel + 2] = value as u8;
                                out[pixel + 1] = (value >> 8) as u8;
                            } else {
                                out[pixel] = value as u8;
                                out[pixel + 3] = (value >> 8) as u8;
                            }
                        }
                    }
                    Kind::Clip => {
                        let [dx, dy, sx, sy, rw, rh] = self.rect;
                        if (x as i64) >= dx
                            && (x as i64) < dx + rw
                            && (y as i64) >= dy
                            && (y as i64) < dy + rh
                        {
                            let source = self.input.as_ref().unwrap();
                            let si = (((y as i64 - dy + sy) as usize) * source.size.width as usize
                                + (x as i64 - dx + sx) as usize)
                                * 4
                                + 3;
                            let alpha = if self.same {
                                out[si]
                            } else {
                                source.main.as_ref().unwrap().as_slice()[si]
                            };
                            let n = u32::from(out[i + 3]) * u32::from(alpha);
                            out[i + 3] = ((n + (n >> 7)) >> 8) as u8;
                        } else if (0..256).contains(&self.args[6]) {
                            out[i + 3] = self.args[6] as u8;
                        }
                    }
                    _ => {}
                }
            }
            self.row += 1;
            return Ok(NativeStep::Continue(self));
        }
        let (main, province) = if matches!(self.kind, Kind::Province) {
            (None, self.output.take())
        } else {
            (self.output.take(), None)
        };
        let pixels = Arc::new(Pixels {
            size: dest.size,
            main,
            province,
        });
        extensions::layer_patch_pixels(cx, self.owner, pixels)
    }
}
