use super::*;
use krkr_engine::protocol::{budget::Budget, scanlines::Scanlines};
use std::{cell::RefCell, rc::Rc};
#[derive(Clone, Default, tjs_bind::Trace)]
pub(super) struct Shared(
    #[trace(skip = "Per-engine numeric wave tables contain no VM handles")]
    Rc<RefCell<[Option<Slot>; 10]>>,
);
#[derive(Default)]
struct Slot {
    waves: Waves,
    powers: Option<Bytes>,
}
#[derive(Clone, Default, tjs_bind::Trace)]
pub(super) struct Waves(
    #[trace(skip = "Budgeted immutable numeric tables contain no VM handles")]
    [Option<Arc<Bytes>>; 2],
);
pub(super) fn shared(cx: &mut NativeCx<'_>) -> NativeResult<Shared> {
    let function = cx.function().ok_or(NativeError::This)?;
    cx.heap_mut()
        .with_native_state::<Shared, _>(function, |s| s.clone())
}
impl Shared {
    pub(super) fn waves(&self, slot: i32) -> NativeResult<Waves> {
        let mut slots = self.0.borrow_mut();
        let slot = slots
            .get_mut(slot as usize)
            .and_then(Option::as_mut)
            .ok_or(NativeError::Message("haze slot is not initialized"))?;
        // 0x10012120 is called at the start of hazeCopy and frees this table.
        slot.powers = None;
        if slot.waves.0[0].is_none() {
            return Err(NativeError::Message("haze X wave is not initialized"));
        }
        Ok(slot.waves.clone())
    }
}
#[tjs_bind::function(resumable = true)]
pub(super) fn init(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    if !cx.result_needed() {
        return Err(NativeError::Message("initHazeCopy requires a result slot"));
    }
    let shared = shared(cx)?;
    // The supplied DLL's condition always selects the first vacant slot;
    // argument zero is not a replacement handle in this binary.
    let index = shared
        .0
        .borrow()
        .iter()
        .position(Option::is_none)
        .ok_or(NativeError::Message("all ten haze slots are in use"))?;
    let budget = extensions::image_staging_budget(cx.heap_mut())?
        .ok_or(NativeError::Message("image staging budget is unavailable"))?;
    shared.0.borrow_mut()[index] = Some(Slot::default());
    let inputs = std::array::from_fn(|i| args.get(i + 1).copied().unwrap_or(Value::Void));
    ReadTable {
        shared,
        index,
        inputs,
        axis: 0,
        stage: 0,
        outer: Value::Void,
        inner: Value::Void,
        n: 0,
        row: 0,
        part: 0,
        parts: 0,
        waves: [[0.0; 3]; 32],
        powers: None,
        budget,
    }
    .axis(cx)
}
#[tjs_bind::function]
pub(super) fn uninit(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<Value> {
    if !supplied(args, 0) {
        return Err(NativeError::Missing(0));
    }
    let index = int(cx, args, 0, -1)?;
    let shared = shared(cx)?;
    let removed = shared
        .0
        .borrow_mut()
        .get_mut(index as usize)
        .is_some_and(|s| s.take().is_some());
    Ok(Value::Int(i64::from(removed)))
}
#[derive(tjs_bind::Trace)]
struct ReadTable {
    shared: Shared,
    index: usize,
    inputs: [Value; 3],
    axis: usize,
    stage: u8,
    outer: Value,
    inner: Value,
    n: usize,
    row: usize,
    part: usize,
    parts: usize,
    waves: [[f64; 3]; 32],
    #[trace(skip = "Numeric curve allocation contains no VM handles")]
    powers: Option<Bytes>,
    #[trace(skip = "Allocation budget contains no VM handles")]
    budget: Budget,
}
impl ReadTable {
    fn get(self, cx: &mut NativeCx<'_>, object: Value, key: Value) -> NativeResult<NativeStep> {
        let _ = cx;
        Ok(NativeStep::Get {
            object,
            key,
            continuation: Box::new(self),
        })
    }
    fn axis(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.axis == 3 {
            return Ok(NativeStep::Return(Value::Int(self.index as i64)));
        }
        self.waves = [[0.0; 3]; 32];
        self.waves[0] = [1.0, 0.0, 1.0];
        self.outer = self.inputs[self.axis];
        self.row = 0;
        if matches!(self.outer, Value::Void) {
            if self.axis == 0 {
                self.n = 1;
                return self.finish_axis(cx);
            }
            self.axis += 1;
            return self.axis(cx);
        }
        self.stage = 0;
        let outer = self.outer;
        let key = key(cx, "count");
        self.get(cx, outer, key)
    }
    fn row(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.row == self.n {
            return self.finish_axis(cx);
        }
        self.stage = 1;
        let outer = self.outer;
        let key = Value::Int(self.row as i64);
        self.get(cx, outer, key)
    }
    fn part(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.part == self.parts {
            self.row += 1;
            return self.row(cx);
        }
        self.stage = 3;
        let inner = self.inner;
        let key = Value::Int(self.part as i64);
        self.get(cx, inner, key)
    }
    fn finish_axis(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.axis < 2 {
            let mut table = Bytes::zeroed(16384 * 4, &self.budget).map_err(error)?;
            let waves = self.waves;
            let n = self.n;
            return extensions::run_work(
                cx,
                move |cancelled| {
                    let mut angle = 0.0;
                    for (i, cell) in table
                        .as_mut_slice()
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .enumerate()
                    {
                        if i.is_multiple_of(128)
                            && cancelled.load(std::sync::atomic::Ordering::Relaxed)
                        {
                            return Err(NativeError::Message("haze initialization cancelled"));
                        }
                        let mut sum = 0.0;
                        for [frequency, phase, amplitude] in &waves[..n] {
                            sum += (angle * frequency + phase).sin() * amplitude;
                            // The increment is inside the wave loop in this DLL.
                            angle += std::f64::consts::TAU / 4096.0;
                        }
                        cell.copy_from_slice(&fixed(sum * 256.0).to_le_bytes());
                    }
                    Ok(table)
                },
                Box::new(self),
            );
        } else {
            self.shared.0.borrow_mut()[self.index]
                .as_mut()
                .ok_or(NativeError::Message(
                    "haze slot released during initialization",
                ))?
                .powers = self.powers.take();
        }
        self.axis += 1;
        self.axis(cx)
    }
}
impl extensions::WorkContinuation<Bytes> for ReadTable {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        table: Bytes,
    ) -> NativeResult<NativeStep> {
        self.shared.0.borrow_mut()[self.index]
            .as_mut()
            .ok_or(NativeError::Message(
                "haze slot released during initialization",
            ))?
            .waves
            .0[self.axis] = Some(Arc::new(table));
        self.axis += 1;
        self.axis(cx)
    }
}
impl NativeContinuation for ReadTable {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        match self.stage {
            0 => {
                self.n = (value::to_integer(cx.heap(), result)? as i32).max(0) as usize;
                if self.axis < 2 {
                    self.n = self.n.min(32);
                } else {
                    self.powers = Some(
                        Bytes::zeroed(
                            self.n
                                .checked_mul(8)
                                .ok_or(NativeError::Message("haze curve size overflow"))?,
                            &self.budget,
                        )
                        .map_err(error)?,
                    );
                }
                self.row(cx)
            }
            1 => {
                self.inner = result;
                self.part = 0;
                if self.axis == 2 {
                    self.parts = 2;
                    self.part(cx)
                } else if matches!(result, Value::Void) {
                    self.row += 1;
                    self.row(cx)
                } else {
                    self.stage = 2;
                    let key = key(cx, "count");
                    self.get(cx, result, key)
                }
            }
            2 => {
                self.parts = (value::to_integer(cx.heap(), result)? as i32).clamp(0, 3) as usize;
                self.part(cx)
            }
            _ => {
                if self.axis < 2 {
                    self.waves[self.row][self.part] = value::to_real(cx.heap(), result)?;
                } else {
                    let v = if self.part == 0 {
                        value::to_integer(cx.heap(), result)? as i32
                    } else {
                        fixed(value::to_real(cx.heap(), result)? * 256.0)
                    };
                    let at = self.row * 8 + self.part * 4;
                    self.powers.as_mut().expect("curve buffer").as_mut_slice()[at..at + 4]
                        .copy_from_slice(&v.to_le_bytes());
                }
                self.part += 1;
                self.part(cx)
            }
        }
    }
}
pub(super) fn draw(
    cx: &mut NativeCx<'_>,
    layer: Value,
    args: &[Value],
    dimensions: [i32; 4],
    waves: Waves,
) -> NativeResult<NativeStep> {
    let [dw, dh, sw, sh] = dimensions;
    let sx = int(cx, args, 2, 0)?;
    let sy = int(cx, args, 3, 0)?;
    let rad = int(cx, args, 4, 0)?;
    let delta = int(cx, args, 5, 7)?;
    let per = if supplied(args, 6) {
        fixed(value::to_real(cx.heap(), args[6])? * 256.0)
    } else {
        2560
    };
    let linear = int(cx, args, 7, 1)? != 0;
    let left = int(cx, args, 8, 0)?.max(0);
    let top = int(cx, args, 9, 0)?.max(0);
    let width = int(cx, args, 10, dw)?;
    let height = int(cx, args, 11, dh)?;
    let boundw = int(cx, args, 12, dw)?;
    let boundh = int(cx, args, 13, dh)?;
    let width = width.min(boundw.saturating_sub(left)).max(0);
    let height = height
        .min(boundh.saturating_sub(top))
        .min(sh.saturating_sub(sy))
        .max(0);
    let actual = extensions::layer_image_size(cx, layer)?;
    let source = extensions::layer_image_size(cx, args[1])?;
    if sw != source.width as i32 || sh != source.height as i32 {
        return Err(NativeError::Message(
            "haze source dimensions do not match its image",
        ));
    }
    let rows = height
        .min((actual.height as i32).saturating_sub(top))
        .max(0);
    if width == 0 || rows == 0 || sw <= 0 || sh <= 0 {
        return redraw(
            cx,
            NativeStep::Return(Value::Void),
            layer,
            [left, top, width, top.saturating_add(height)],
        );
    }
    let budget = extensions::layer_pixel_budget(cx, layer)?;
    let count = (rows as usize + 1)
        .checked_mul(8)
        .ok_or(NativeError::Message("haze row count overflow"))?;
    let permit = budget.reserve(count * 4).map_err(error)?;
    let mut words = Vec::with_capacity(count);
    words.extend_from_slice(&[top, rows, i32::from(linear), sw, sh, 0, 0, 0]);
    for row in 0..rows {
        let index = (rad.wrapping_add(row.wrapping_mul(delta)) & 16383) as usize;
        let lookup = |axis: usize| {
            waves.0[axis].as_ref().map_or(0, |table| {
                i32::from_le_bytes(
                    table.as_slice()[index * 4..index * 4 + 4]
                        .try_into()
                        .unwrap(),
                )
            })
        };
        let mut shift = lookup(0).wrapping_mul(per) >> 8;
        if row == 0 || row == height - 1 {
            let edge = sw.wrapping_sub(width);
            shift = if shift > edge {
                edge
            } else if shift < edge.wrapping_neg() {
                edge.wrapping_neg()
            } else {
                shift
            };
        }
        let source_y = if waves.0[1].is_some() {
            let displacement = lookup(1).wrapping_mul(per) >> 16;
            sy.saturating_add(row.saturating_sub(displacement).clamp(0, height - 1))
                .clamp(0, (sh - 2).max(0))
        } else {
            sy.saturating_add(row)
        };
        let offset = sx.wrapping_mul(256).wrapping_add(shift);
        let integer = offset >> 8;
        let (dx, x, w) = if offset <= 0 && sx < integer {
            (
                left.saturating_sub(integer),
                0,
                width.saturating_add(integer).min(sw),
            )
        } else {
            (left, integer, width.min(sw.saturating_sub(integer)))
        };
        // Negative native pointers can refer to a preceding source scanline;
        // the shader preserves addresses inside the image and discards OOB.
        words.extend_from_slice(&[dx, w.max(0), x, source_y, offset & 255, 0, 0, 0]);
    }
    let rectangle = Rect {
        left: 0,
        top,
        width: actual.width,
        height: rows as u32,
    };
    let step = extensions::layer_copy_scanlines(
        cx,
        layer,
        args[1],
        Arc::new(Scanlines {
            rectangle,
            words,
            _permit: permit,
        }),
    )?;
    // Native update passes the ending y as its fourth argument.
    redraw(
        cx,
        step,
        layer,
        [left, top, width, top.saturating_add(height)],
    )
}
