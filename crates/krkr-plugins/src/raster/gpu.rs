//! Between-layer raster copies need only one displacement record per row.
//! Keep pixels resident on the render host; the self-copy path retains the
//! original forward-loop feedback semantics in the CPU implementation.
use super::*;
use krkr_engine::protocol::{graphics::Size, scanlines::Scanlines};

#[derive(tjs_bind::Trace)]
struct Copy {
    owner: Value,
    source: Value,
    dimensions: [u32; 2],
    index: usize,
    wave: [f64; 3],
}
pub(super) fn begin(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let owner = Value::Obj(cx.this().into());
    extensions::layer_prepare_draw(cx, owner)?;
    let size = extensions::layer_image_size(cx, owner)?;
    let amplitude = integer(cx, args[1])?;
    let lines = integer(cx, args[2])?;
    let cycle = integer(cx, args[3])?;
    let time = value::to_integer(cx.heap(), args[4])?;
    if lines == 0 || cycle == 0 {
        return Err(NativeError::Message(
            "copyRaster requires nonzero lines and cycle",
        ));
    }
    extensions::layer_size(cx, args[0])?;
    let omega = std::f64::consts::TAU / lines as f64;
    Box::new(Copy {
        owner,
        source: args[0],
        dimensions: [0; 2],
        index: 0,
        wave: [
            amplitude as f64,
            omega,
            -omega * time as f64 / cycle as f64 * (size.height / 2) as f64,
        ],
    })
    .resume(cx, Value::Void)
}
impl NativeContinuation for Copy {
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
                object: self.source,
                key,
                continuation: self,
            });
        }
        let source = extensions::layer_image_size(cx, self.source)?;
        if source
            != (Size {
                width: self.dimensions[0],
                height: self.dimensions[1],
            })
        {
            return Err(NativeError::Message(
                "Layer dimensions disagree with managed image",
            ));
        }
        let size = extensions::layer_image_size(cx, self.owner)?;
        if source != size {
            return Ok(NativeStep::Return(Value::Void));
        }
        let count = (size.height as usize + 1)
            .checked_mul(8)
            .ok_or(NativeError::Message("raster row count overflow"))?;
        let budget = extensions::layer_pixel_budget(cx, self.owner)?;
        let permit = budget
            .reserve(
                count
                    .checked_mul(4)
                    .ok_or(NativeError::Message("raster table overflow"))?,
            )
            .map_err(|e| NativeError::Detail(e.to_string()))?;
        let mut words = Vec::with_capacity(count);
        words.extend_from_slice(&[
            0,
            size.height as i32,
            0,
            size.width as i32,
            size.height as i32,
            0,
            0,
            0,
        ]);
        let mut phase = self.wave[2];
        for y in 0..size.height {
            let shift = (phase.sin() * self.wave[0]) as i32 as i64;
            let width = i64::from(size.width) - shift.abs();
            if width > 0 {
                words.extend_from_slice(&[
                    shift.max(0) as i32,
                    width as i32,
                    (-shift).max(0) as i32,
                    y as i32,
                    0,
                    0,
                    0,
                    0,
                ]);
            } else {
                words.extend_from_slice(&[0; 8]);
            }
            phase += self.wave[1];
        }
        extensions::layer_copy_scanlines(
            cx,
            self.owner,
            self.source,
            Arc::new(Scanlines {
                rectangle: size.rect(),
                words,
                _permit: permit,
            }),
        )
    }
}
