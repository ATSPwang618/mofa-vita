use super::*;
use krkr_engine::protocol::scanlines::Scanlines;
#[derive(Clone)]
pub(super) struct State {
    speed: f64,
    cycle: f64,
    bounds: [i32; 3],
    powers: [i32; 3],
    waves: [Option<Arc<Bytes>>; 2],
}
pub(super) fn init(cx: &mut NativeCx<'_>, shared: Shared, v: &[Value]) -> NativeResult<NativeStep> {
    // These three values are accepted/stored by the DLL, but doHaze never reads them.
    for &value in &v[..3] {
        int(cx, value)?;
    }
    if matches!(v[11], Value::Void) {
        return Err(NativeError::Message("initHaze requires a waves array"));
    }
    let state = State {
        speed: real(cx, v[3])?,
        cycle: real(cx, v[4])?,
        bounds: [int(cx, v[5])?, int(cx, v[6])?, int(cx, v[7])?],
        powers: [
            fixed(real(cx, v[8])? * 1048575.0),
            fixed(real(cx, v[9])? * 1048575.0),
            fixed(real(cx, v[10])? * 1048575.0),
        ],
        waves: [None, None],
    };
    Read {
        shared,
        result: state,
        inputs: [v[11], v[12]],
        axis: 0,
        outer: Value::Void,
        inner: Value::Void,
        count: 0,
        row: 0,
        part: 0,
        stage: 0,
        waves: [[0.0; 3]; 32],
        budget: budget(cx)?,
    }
    .axis(cx)
}
#[derive(tjs_bind::Trace)]
struct Read {
    shared: Shared,
    #[trace(skip = "Only numeric wave tables and parameters")]
    result: State,
    inputs: [Value; 2],
    axis: usize,
    outer: Value,
    inner: Value,
    count: usize,
    row: usize,
    part: usize,
    stage: u8,
    waves: [[f64; 3]; 32],
    #[trace(skip = "Shared allocation budget")]
    budget: Budget,
}
impl Read {
    fn get(self, object: Value, key: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Get {
            object,
            key,
            continuation: Box::new(self),
        })
    }
    fn axis(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.axis == 2 {
            self.shared.0.borrow_mut().haze = Some(self.result);
            return Ok(NativeStep::Return(Value::Void));
        }
        self.outer = self.inputs[self.axis];
        self.row = 0;
        self.waves = [[0.0; 3]; 32];
        if matches!(self.outer, Value::Void) {
            self.axis += 1;
            return self.axis(cx);
        }
        self.stage = 0;
        let outer = self.outer;
        self.get(outer, key(cx, "count"))
    }
    fn row(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.row == self.count {
            let mut table = Bytes::zeroed(16384 * 8, &self.budget).map_err(error)?;
            let waves = self.waves;
            let count = self.count;
            return extensions::run_work(
                cx,
                move |cancelled| {
                    let mut angle = 0.0;
                    for (i, cell) in table
                        .as_mut_slice()
                        .as_chunks_mut::<8>()
                        .0
                        .iter_mut()
                        .enumerate()
                    {
                        if i.is_multiple_of(128)
                            && cancelled.load(std::sync::atomic::Ordering::Relaxed)
                        {
                            return Err(NativeError::Message(
                                "filter haze initialization cancelled",
                            ));
                        }
                        let mut sum = 0.0;
                        for [frequency, phase, amplitude] in &waves[..count] {
                            sum += (angle * frequency + phase).cos() * amplitude;
                            angle += std::f64::consts::TAU / 4096.0;
                        }
                        cell.copy_from_slice(&(sum * 2.0).to_le_bytes());
                    }
                    Ok(table)
                },
                Box::new(self),
            );
        }
        self.stage = 1;
        let outer = self.outer;
        let index = self.row;
        self.get(outer, Value::Int(index as i64))
    }
    fn part(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.part == 3 {
            self.row += 1;
            return self.row(cx);
        }
        self.stage = 3;
        let inner = self.inner;
        let index = self.part;
        self.get(inner, Value::Int(index as i64))
    }
}
impl NativeContinuation for Read {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        match self.stage {
            0 => {
                self.count = int(cx, result)?.clamp(0, 32) as usize;
                self.row(cx)
            }
            1 => {
                self.inner = result;
                self.stage = 2;
                self.get(result, key(cx, "count"))
            }
            2 => {
                if int(cx, result)? < 3 {
                    return Err(NativeError::Message(
                        "filter wave requires frequency, phase and amplitude",
                    ));
                }
                self.part = 0;
                self.part(cx)
            }
            _ => {
                self.waves[self.row][self.part] = real(cx, result)?;
                self.part += 1;
                self.part(cx)
            }
        }
    }
}
impl extensions::WorkContinuation<Bytes> for Read {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        table: Bytes,
    ) -> NativeResult<NativeStep> {
        self.result.waves[self.axis] = Some(Arc::new(table));
        self.axis += 1;
        self.axis(cx)
    }
}
pub(super) fn draw(
    cx: &mut NativeCx<'_>,
    shared: &Shared,
    v: &[Value],
) -> NativeResult<NativeStep> {
    let state = shared
        .0
        .borrow()
        .haze
        .clone()
        .ok_or(NativeError::Message("filter haze is not initialized"))?;
    let source = extensions::layer_image_size(cx, v[0])?;
    let target = extensions::layer_image_size(cx, v[1])?;
    extensions::layer_prepare_draw(cx, v[1])?;
    let tick = int(cx, v[2])?;
    let radius = real(cx, v[3])?;
    int(cx, v[4])?; // bgcolor is read but unused in the supplied callback.
    let blend = int(cx, v[5])? != 0;
    let height = target.height as i32;
    let width = target.width as i32;
    if width <= 0 || height <= 0 || source.width == 0 || source.height == 0 {
        return Ok(NativeStep::Return(Value::Void));
    }
    let [upper, center, lower] = state.bounds;
    let shaped = upper >= 0 || lower >= 0;
    let begin = if shaped { upper.max(0).min(height) } else { 0 };
    let end = if shaped && lower >= 0 {
        lower.min(height).max(begin)
    } else {
        height
    };
    if end <= begin {
        return Ok(NativeStep::Return(Value::Void));
    }
    let split = if center > begin && center < end {
        Some(center)
    } else {
        None
    };
    let rows = (end - begin) as usize;
    let length = (rows + 1) * 8;
    let permit = extensions::layer_pixel_budget(cx, v[1])?
        .reserve(length * 4)
        .map_err(error)?;
    let mut words = Vec::with_capacity(length);
    words.extend_from_slice(&[
        begin,
        rows as i32,
        if blend { 3 } else { 2 },
        source.width as i32,
        source.height as i32,
        0,
        0,
        0,
    ]);
    let mut phase =
        f64::from(fixed(f64::from(tick) * state.speed * (4096.0 / std::f64::consts::TAU)) & 16383);
    let wave = |axis: usize, index: usize| {
        state.waves[axis].as_ref().map_or(0.0, |table| {
            f64::from_le_bytes(
                table.as_slice()[index * 8..index * 8 + 8]
                    .try_into()
                    .unwrap(),
            )
        })
    };
    for y in begin..end {
        let index = (fixed(phase) & 16383) as usize;
        phase += state.cycle;
        let (offset, vertical) = if shaped {
            let (start, stop, a, b) = if let Some(center) = split {
                if y < center {
                    (begin, center, state.powers[0], state.powers[1])
                } else {
                    (center, end, state.powers[1], state.powers[2])
                }
            } else {
                (begin, end, state.powers[0], state.powers[2])
            };
            let strength = a.wrapping_add(
                b.wrapping_sub(a)
                    .wrapping_div(stop - start)
                    .wrapping_mul(y - start),
            );
            (
                fixed(f64::from(strength) * wave(0, index) * radius) >> 20,
                fixed(f64::from(strength) / 1048576.0 * wave(1, index) * radius + 0.5),
            )
        } else {
            (
                fixed(radius * wave(0, index)),
                fixed(radius * wave(1, index)),
            )
        };
        let sy = y
            .saturating_sub(vertical)
            .clamp(0, height - 1)
            .min(source.height as i32 - 1);
        let shift = offset >> 1;
        let dx = shift.max(0);
        let sx = shift.saturating_neg().max(0);
        let span = width
            .saturating_sub(shift.saturating_abs())
            .min(source.width as i32 - sx)
            .max(0);
        words.extend_from_slice(&[dx, span, sx, sy, offset & 1, 0, 0, 0]);
    }
    let rectangle = Rect {
        left: 0,
        top: begin,
        width: target.width,
        height: rows as u32,
    };
    let step = extensions::layer_copy_scanlines(
        cx,
        v[1],
        v[0],
        Arc::new(Scanlines {
            rectangle,
            words,
            _permit: permit,
        }),
    )?;
    redraw(cx, step, v[1], rectangle)
}
fn fixed(v: f64) -> i32 {
    if (i32::MIN as f64..i32::MAX as f64 + 1.0).contains(&v) {
        v as i32
    } else {
        i32::MIN
    }
}
