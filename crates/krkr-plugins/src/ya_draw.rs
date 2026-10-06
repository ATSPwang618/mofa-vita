//! Recovered from the supplied layerExYADraw.dll: registration 0x10002c50,
//! line/focus wrappers 0x10002750/0x100028f0, rasterizers 0x10001990..0x100024a0.
use krkr_engine::{
    extensions,
    protocol::{
        budget::Budget,
        graphics::{Adjustment, Rect},
        lines::{Lines, TILE_SIZE},
    },
};
use std::sync::Arc;
use tjs_bind::{RestArgs, flow};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjRef, Value, value,
};

krkr_engine::native_plugin! {
    pub(crate) YaDraw {
        names: ["layerExYADraw.dll", "layerExYADraw.tpm"],
        classes: [],
        extensions: [("Layer", "YAdrawLine", line::CALL), ("Layer", "drawFocusLines", focus::CALL)],
    }
}
#[tjs_bind::function(resumable = true)]
fn line(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    start(cx, args, false)
}
#[tjs_bind::function(resumable = true)]
fn focus(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    start(cx, args, true)
}
fn start(cx: &mut NativeCx<'_>, args: &[Value], focus: bool) -> NativeResult<NativeStep> {
    let required = if focus { 2 } else { 4 };
    if args.len() < required {
        return Err(NativeError::Missing(required - 1));
    }
    let mut parameters = if focus {
        [0, 0, 50, 100, 25, 50, 2000, 2000, 0xff000000u32 as i32]
    } else {
        [0, 0, 0, 0, 0xff000000u32 as i32, 1, 0, 0, 0]
    };
    for (index, (&arg, result)) in args
        .iter()
        .take(if focus { 9 } else { 7 })
        .zip(&mut parameters)
        .enumerate()
    {
        // Only drawFocusLines treats an explicitly supplied void as a default.
        if !focus || index < required || !matches!(arg, Value::Void) {
            *result = value::to_integer(cx.heap(), arg)? as i32;
        }
    }
    if focus {
        for pair in parameters[2..8].as_chunks_mut::<2>().0.iter_mut() {
            if pair[0] > pair[1] {
                pair.swap(0, 1);
            }
        }
    }
    Read {
        layer: Value::Obj(ObjRef::bound(cx.this())),
        parameters,
        focus,
        values: [0; 6],
        index: 0,
    }
    .next(cx)
}
#[derive(tjs_bind::Trace)]
struct Read {
    layer: Value,
    parameters: [i32; 9],
    focus: bool,
    values: [i32; 6],
    index: usize,
}
impl Read {
    fn next(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index == 2 {
            extensions::layer_prepare_draw(cx, self.layer)?;
        }
        let names = [
            "imageWidth",
            "imageHeight",
            "clipLeft",
            "clipTop",
            "clipWidth",
            "clipHeight",
        ];
        if let Some(name) = names.get(self.index) {
            return Ok(NativeStep::GetOr {
                object: self.layer,
                key: key(cx, name),
                raw: false,
                fallback: Value::Void,
                continuation: Box::new(self),
            });
        }
        let [width, height, left, top, cw, ch] = self.values;
        if width <= 0 || height <= 0 {
            return Ok(NativeStep::Return(Value::Void));
        }
        // This DLL clamps both inclusive endpoints independently and uses
        // left + clipWidth (not width - 1); retain that one-pixel convention.
        let left = left.clamp(0, width - 1);
        let top = top.clamp(0, height - 1);
        let right = self.values[2].saturating_add(cw).clamp(0, width - 1);
        let bottom = self.values[3].saturating_add(ch).clamp(0, height - 1);
        if right < left || bottom < top {
            return Ok(NativeStep::Return(Value::Void));
        }
        let bounds = Rect {
            left,
            top,
            width: (right - left + 1) as u32,
            height: (bottom - top + 1) as u32,
        };
        let actual = extensions::layer_image_size(cx, self.layer)?.rect();
        let Some(bounds) = bounds.intersection(actual) else {
            return Ok(NativeStep::Return(Value::Void));
        };
        // The native algorithm uses signed 16.16 coordinates.
        if (i64::from(bounds.left) + i64::from(bounds.width)) > 32767
            || (i64::from(bounds.top) + i64::from(bounds.height)) > 32767
        {
            return Err(NativeError::Message(
                "YADraw coordinates exceed signed 16.16 range",
            ));
        }
        let budget = extensions::layer_pixel_budget(cx, self.layer)?;
        let p = self.parameters;
        let mut rng = Random(if self.focus {
            (extensions::tick_count(cx)? as i32).wrapping_sub(0x6d29735e)
        } else {
            0
        });
        let count = if self.focus {
            rng.range(p[4], p[5]).max(0) as usize
        } else {
            1
        };
        let _geometry = budget
            .reserve(
                count
                    .checked_mul(32)
                    .ok_or(NativeError::Message("line count overflow"))?,
            )
            .map_err(error)?;
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let (ends, color, aa, fade) = if self.focus {
                let inner = rng.range(p[2], p[3]) as f64;
                let outer = rng.range(p[6], p[7]) as f64;
                let angle = rng.next() as f64 * (1.0 / 4294967296.0) * std::f64::consts::TAU;
                let (sin, cos) = angle.sin_cos();
                (
                    [
                        integer(p[0] as f64 + inner * cos),
                        integer(p[1] as f64 + inner * sin),
                        integer(p[0] as f64 + outer * cos),
                        integer(p[1] as f64 + outer * sin),
                    ],
                    p[8],
                    1,
                    p[3].wrapping_sub(p[2]).max(0),
                )
            } else {
                ([p[0], p[1], p[2], p[3]], p[4], p[5], p[6].max(0))
            };
            if let Some([x0, y0, x1, y1]) = clip(ends, bounds) {
                records.push([
                    x0 as u32,
                    y0 as u32,
                    x1 as u32,
                    y1 as u32,
                    color as u32,
                    u32::from(aa != 0),
                    fade as u32,
                    0,
                ]);
            }
        }
        let ends = if self.focus {
            [
                p[0].saturating_sub(p[7]).max(bounds.left),
                p[1].saturating_sub(p[7]).max(bounds.top),
                p[0].saturating_add(p[7])
                    .min(bounds.left + bounds.width as i32 - 1),
                p[1].saturating_add(p[7])
                    .min(bounds.top + bounds.height as i32 - 1),
            ]
        } else {
            clip([p[0], p[1], p[2], p[3]], bounds).unwrap_or([p[0], p[1], p[2], p[3]])
        };
        let redraw = [
            i64::from(ends[0].min(ends[2])),
            i64::from(ends[1].min(ends[3])),
            (i64::from(ends[0]) - i64::from(ends[2])).abs() + 1,
            (i64::from(ends[1]) - i64::from(ends[3])).abs() + 1,
        ];
        let step = if records.is_empty() {
            NativeStep::Return(Value::Void)
        } else {
            // Snapshot only the line envelope, including the AA neighbor.
            let mut envelope = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
            for record in &records {
                for (x, y) in [
                    (record[0] as i32, record[1] as i32),
                    (record[2] as i32, record[3] as i32),
                ] {
                    envelope[0] = envelope[0].min(x);
                    envelope[1] = envelope[1].min(y);
                    envelope[2] = envelope[2].max(x + 1);
                    envelope[3] = envelope[3].max(y + 1);
                }
            }
            let region = Rect {
                left: envelope[0],
                top: envelope[1],
                width: (envelope[2] - envelope[0] + 1) as u32,
                height: (envelope[3] - envelope[1] + 1) as u32,
            };
            let region = region.intersection(bounds).expect("clipped lines");
            let lines = index_lines(region, &records, &budget)?;
            extensions::layer_adjust(
                cx,
                self.layer,
                region,
                Adjustment::Lines(Arc::new(lines)),
                false,
            )?
        };
        Ok(flow::then(
            step,
            flow::callback((self.layer, redraw), |(layer, redraw), cx, _| {
                Ok(NativeStep::CallMember {
                    object: layer,
                    key: key(cx, "update"),
                    arguments: redraw.map(Value::Int).to_vec(),
                    continuation: flow::complete(Value::Void),
                })
            }),
        ))
    }
}
impl NativeContinuation for Read {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        self.values[self.index] = value::to_integer(cx.heap(), result)? as i32;
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
fn error(error: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(error.to_string())
}
fn integer(value: f64) -> i32 {
    if (i32::MIN as f64..i32::MAX as f64 + 1.0).contains(&value) {
        value as i32
    } else {
        i32::MIN
    }
}
struct Random(i32);
impl Random {
    fn next(&mut self) -> i32 {
        self.0 ^= self.0.wrapping_shl(13);
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0.wrapping_shl(5);
        self.0
    }
    fn range(&mut self, min: i32, max: i32) -> i32 {
        min.wrapping_add(
            max.wrapping_sub(min)
                .wrapping_mul((self.next() >> 16) & 65535)
                >> 16,
        )
    }
}
fn clip([x0, y0, x1, y1]: [i32; 4], rect: Rect) -> Option<[i32; 4]> {
    let [l, t, r, b] = [
        i64::from(rect.left),
        i64::from(rect.top),
        (i64::from(rect.left) + i64::from(rect.width)) - 1,
        (i64::from(rect.top) + i64::from(rect.height)) - 1,
    ];
    let code = |x: i64, y: i64| {
        u8::from(x < l) | (u8::from(x > r) << 1) | (u8::from(y < t) << 2) | (u8::from(y > b) << 3)
    };
    let a = [i64::from(x0), i64::from(y0)];
    let z = [i64::from(x1), i64::from(y1)];
    if code(a[0], a[1]) & code(z[0], z[1]) != 0 {
        return None;
    }
    let endpoint = |a: [i64; 2], z: [i64; 2]| -> Option<[i64; 2]> {
        let bits = code(a[0], a[1]);
        if bits == 0 {
            return Some(a);
        }
        // Original edge priority: left, right, top, bottom. Widen products
        // so offscreen coordinates cannot overflow into an in-bounds address.
        for (flag, edge, axis) in [(1, l, 0), (2, r, 0), (4, t, 1), (8, b, 1)] {
            if bits & flag == 0 {
                continue;
            }
            let other = 1 - axis;
            let denominator = z[axis] - a[axis];
            if denominator == 0 {
                continue;
            }
            let value = a[other] as i128
                + (edge - a[axis]) as i128 * (z[other] - a[other]) as i128 / denominator as i128;
            let point = if axis == 0 {
                [edge, value as i64]
            } else {
                [value as i64, edge]
            };
            if code(point[0], point[1]) == 0 {
                return Some(point);
            }
        }
        None
    };
    let first = endpoint(a, z)?;
    let last = endpoint(z, first)?;
    Some([
        first[0] as i32,
        first[1] as i32,
        last[0] as i32,
        last[1] as i32,
    ])
}
fn visit_tiles(line: &[u32; 8], rect: Rect, columns: u32, mut visit: impl FnMut(usize)) {
    let [x0, y0, x1, y1] = [
        line[0] as i32,
        line[1] as i32,
        line[2] as i32,
        line[3] as i32,
    ];
    let horizontal = (x1 - x0).abs() >= (y1 - y0).abs();
    let (major, minor, delta, dm) = if horizontal {
        (x0, y0, x1 - x0, y1 - y0)
    } else {
        (y0, x0, y1 - y0, x1 - x0)
    };
    let distance = delta.abs().max(i32::from(line[5] != 0));
    let step = i64::from(dm) * 65536 / i64::from(distance.max(1));
    for at in 0..=distance {
        let m = major + at * if delta >= 0 { 1 } else { -1 };
        let n = ((i64::from(minor) * 65536 + i64::from(at) * step) >> 16) as i32;
        // Include the symmetric Bresenham tie as well as both AA neighbors.
        for n in n - 1..=n + 2 {
            let (x, y) = if horizontal { (m, n) } else { (n, m) };
            if x < rect.left
                || y < rect.top
                || i64::from(x) >= (i64::from(rect.left) + i64::from(rect.width))
                || i64::from(y) >= (i64::from(rect.top) + i64::from(rect.height))
            {
                continue;
            }
            visit(
                (((y - rect.top) as u32 / TILE_SIZE) * columns + (x - rect.left) as u32 / TILE_SIZE)
                    as usize,
            );
        }
    }
}
fn index_lines(rectangle: Rect, records: &[[u32; 8]], budget: &Budget) -> NativeResult<Lines> {
    let columns = rectangle.width.div_ceil(TILE_SIZE);
    let rows = rectangle.height.div_ceil(TILE_SIZE);
    let tiles = columns as usize * rows as usize;
    let _scratch = budget.reserve(tiles * 8).map_err(error)?;
    let mut counts = vec![0u32; tiles];
    let mut seen = vec![usize::MAX as u32; tiles];
    for (i, line) in records.iter().enumerate() {
        visit_tiles(line, rectangle, columns, |tile| {
            if seen[tile] != i as u32 {
                seen[tile] = i as u32;
                counts[tile] += 1;
            }
        });
    }
    let record_offset = 8 + tiles * 2;
    let refs_offset = record_offset + records.len() * 8;
    let length = counts
        .iter()
        .fold(refs_offset, |n, &count| n.saturating_add(count as usize));
    let permit = budget
        .reserve(
            length
                .checked_mul(4)
                .ok_or(NativeError::Message("line index size overflow"))?,
        )
        .map_err(error)?;
    let mut words = vec![0; length];
    words[..8].copy_from_slice(&[
        rectangle.left as u32,
        rectangle.top as u32,
        columns,
        rows,
        record_offset as u32,
        refs_offset as u32,
        rectangle.left as u32,
        rectangle.top as u32,
    ]);
    let mut offset = refs_offset as u32;
    for (tile, &count) in counts.iter().enumerate() {
        words[8 + tile * 2] = offset;
        words[9 + tile * 2] = count;
        offset += count;
    }
    for (i, record) in records.iter().enumerate() {
        words[record_offset + i * 8..record_offset + (i + 1) * 8].copy_from_slice(record);
    }
    seen.fill(u32::MAX);
    counts.fill(0);
    for (i, line) in records.iter().enumerate() {
        visit_tiles(line, rectangle, columns, |tile| {
            if seen[tile] != i as u32 {
                seen[tile] = i as u32;
                let at = words[8 + tile * 2] + counts[tile];
                words[at as usize] = i as u32;
                counts[tile] += 1;
            }
        });
    }
    Ok(Lines {
        rectangle,
        words,
        _permit: permit,
    })
}
